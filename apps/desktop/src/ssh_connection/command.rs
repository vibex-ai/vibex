//! Fixed-argument SSH child processes.
//!
//! Nothing here goes through a shell. Local options are discrete argv entries,
//! the destination is validated before it can look like an option, and remote
//! work is a fixed `sh -s` script supplied on stdin.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use vibex_core::{VibexError, VibexResult};

use super::error::SshFailure;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT_SECONDS: u32 = 10;
const ALIVE_INTERVAL_SECONDS: u32 = 15;
const ALIVE_COUNT_MAX: u32 = 3;
const MAX_SCRIPT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshDestination {
    pub host: String,
    pub user: Option<String>,
    pub port: u16,
}

impl SshDestination {
    pub fn validate(&self) -> VibexResult<()> {
        if !host_is_valid(&self.host) {
            return Err(invalid("host"));
        }
        if let Some(user) = &self.user
            && (user.is_empty()
                || user.len() > 64
                || user.starts_with('-')
                || user.contains('@')
                || !user.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
                }))
        {
            return Err(invalid("user"));
        }
        if self.port == 0 {
            return Err(invalid("port"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub stdin: Option<Vec<u8>>,
}

#[derive(Debug)]
pub struct SshOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl SshOutput {
    pub fn success(&self) -> bool {
        self.status.success()
    }

    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

impl SshCommand {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            stdin: None,
        }
    }

    /// Builds the common preflight command for one destination.
    pub fn for_destination(
        program: impl Into<PathBuf>,
        destination: &SshDestination,
        config_path: Option<&Path>,
    ) -> VibexResult<Self> {
        destination.validate()?;
        let mut args = Vec::new();
        if let Some(path) = config_path {
            args.push(OsString::from("-F"));
            args.push(path.as_os_str().to_owned());
        }
        for option in [
            "BatchMode=yes".to_string(),
            format!("ConnectTimeout={CONNECT_TIMEOUT_SECONDS}"),
            format!("ServerAliveInterval={ALIVE_INTERVAL_SECONDS}"),
            format!("ServerAliveCountMax={ALIVE_COUNT_MAX}"),
            "StrictHostKeyChecking=yes".to_string(),
        ] {
            args.push(OsString::from("-o"));
            args.push(OsString::from(option));
        }
        args.push(OsString::from("-p"));
        args.push(OsString::from(destination.port.to_string()));
        args.push(OsString::from("--"));
        let target = match &destination.user {
            Some(user) => format!("{user}@{}", destination.host),
            None => destination.host.clone(),
        };
        args.push(OsString::from(target));
        Ok(Self {
            program: program.into(),
            args,
            stdin: None,
        })
    }

    /// Appends the fixed remote command. User input must go through `stdin`,
    /// never into the remote command string.
    pub fn remote_script(&mut self, script: &str) -> VibexResult<&mut Self> {
        if script.len() > MAX_SCRIPT_BYTES {
            return Err(VibexError::validation(
                "ssh_script_too_large",
                "the SSH script is too large",
            ));
        }
        if script.contains('\0') {
            return Err(VibexError::validation(
                "ssh_script_invalid",
                "the SSH script contains a NUL byte",
            ));
        }
        self.args.push(OsString::from("sh"));
        self.args.push(OsString::from("-s"));
        self.stdin = Some(script.as_bytes().to_vec());
        Ok(self)
    }

    pub fn with_remote_script(mut self, script: &str) -> VibexResult<Self> {
        self.remote_script(script)?;
        Ok(self)
    }

    pub fn run(&self) -> Result<SshOutput, SshFailure> {
        self.run_with_timeout(DEFAULT_TIMEOUT)
    }

    pub fn run_with_timeout(&self, timeout: Duration) -> Result<SshOutput, SshFailure> {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command.env("LC_ALL", "C");
        command.stdin(if self.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                SshFailure::client_missing(&error.to_string())
            } else {
                SshFailure::spawn(&error.to_string())
            }
        })?;

        let stdin_thread = match &self.stdin {
            Some(input) => {
                let Some(mut stdin) = child.stdin.take() else {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(SshFailure::spawn("the SSH stdin pipe is unavailable"));
                };
                let input = input.clone();
                Some(thread::spawn(move || {
                    let _ = stdin.write_all(&input);
                }))
            }
            None => None,
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SshFailure::spawn("the SSH stdout pipe is unavailable"));
        };
        let Some(stderr) = child.stderr.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SshFailure::spawn("the SSH stderr pipe is unavailable"));
        };
        let stdout_reader = thread::spawn(move || read_all(stdout));
        let stderr_reader = thread::spawn(move || read_all(stderr));

        let started = Instant::now();
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(SshFailure::spawn(&error.to_string()));
                }
            }
            if started.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break None;
            }
            thread::sleep(Duration::from_millis(25));
        };

        let stdout = stdout_reader.join().unwrap_or_default();
        let stderr = stderr_reader.join().unwrap_or_default();
        if let Some(handle) = stdin_thread {
            let _ = handle.join();
        }
        if timed_out {
            return Err(SshFailure::timeout(&String::from_utf8_lossy(&stderr)));
        }
        let Some(status) = status else {
            return Err(SshFailure::spawn("the SSH process status is unavailable"));
        };
        if status.success() {
            Ok(SshOutput {
                status,
                stdout,
                stderr,
            })
        } else {
            Err(SshFailure::from_parts(
                status.code(),
                &String::from_utf8_lossy(&stderr),
            ))
        }
    }
}

/// Hostnames are DNS labels; bracketed IPv6 literals are the one exception.
/// Shell metacharacters are rejected even though no shell is involved, so a
/// hostile value cannot be mistaken for a command by a future caller.
pub(crate) fn host_is_valid(host: &str) -> bool {
    if host.is_empty() || host.len() > 255 || host.starts_with('-') {
        return false;
    }
    if let Some(inner) = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        return !inner.is_empty()
            && inner.chars().all(|character| {
                character.is_ascii_hexdigit() || matches!(character, ':' | '.' | '%' | '-' | '_')
            });
    }
    host.chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_'))
}

fn invalid(field: &str) -> VibexError {
    VibexError::validation("ssh_destination_invalid", "the SSH destination is invalid")
        .with_diagnostic("field", field)
}

fn read_all(mut reader: impl Read) -> Vec<u8> {
    let mut buffer = Vec::new();
    let _ = reader.read_to_end(&mut buffer);
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh_connection::error::SshFailureKind;
    use std::fs;

    fn has_pair(args: &[String], left: &str, right: &str) -> bool {
        args.windows(2)
            .any(|pair| pair[0] == left && pair[1] == right)
    }

    fn destination(host: &str) -> SshDestination {
        SshDestination {
            host: host.to_string(),
            user: Some("dev".to_string()),
            port: 22,
        }
    }

    #[test]
    fn builds_discrete_batch_mode_arguments() {
        let command = SshCommand::for_destination(
            Path::new("ssh"),
            &destination("build.example.com"),
            Some(Path::new("/tmp/ssh_config")),
        )
        .unwrap()
        .with_remote_script("set -eu\nuname -s\n")
        .unwrap();
        let args = command
            .args
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args[0], "-F");
        assert_eq!(args[1], "/tmp/ssh_config");
        assert!(has_pair(&args, "-o", "BatchMode=yes"));
        assert!(has_pair(&args, "-o", "ConnectTimeout=10"));
        assert!(has_pair(&args, "-o", "ServerAliveInterval=15"));
        assert!(has_pair(&args, "-o", "ServerAliveCountMax=3"));
        assert!(has_pair(&args, "-o", "StrictHostKeyChecking=yes"));
        assert!(has_pair(&args, "-p", "22"));
        assert!(args.contains(&"--".to_string()));
        assert!(args.contains(&"dev@build.example.com".to_string()));
        assert_eq!(args[args.len() - 2], "sh");
        assert_eq!(args[args.len() - 1], "-s");
        assert_eq!(
            command.stdin.as_deref(),
            Some(b"set -eu\nuname -s\n".as_slice())
        );
        assert!(!args.contains(&"-c".to_string()));
    }

    #[test]
    fn rejects_injection_shaped_destinations_before_spawning() {
        for host in [
            "host; rm -rf /",
            "$(id)",
            "a\nb",
            "-oProxyCommand=id",
            "user@host",
            "host name",
            "host*",
            "-host",
        ] {
            let error = SshCommand::for_destination(Path::new("ssh"), &destination(host), None)
                .unwrap_err();
            assert_eq!(error.code, "ssh_destination_invalid", "host={host:?}");
        }
        for user in ["dev;id", "dev user", "-dev", "dev@host"] {
            let error = SshCommand::for_destination(
                Path::new("ssh"),
                &SshDestination {
                    host: "build.example.com".to_string(),
                    user: Some(user.to_string()),
                    port: 22,
                },
                None,
            )
            .unwrap_err();
            assert_eq!(error.code, "ssh_destination_invalid", "user={user:?}");
        }
        let error = SshCommand::for_destination(
            Path::new("ssh"),
            &SshDestination {
                host: "build.example.com".to_string(),
                user: None,
                port: 0,
            },
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "ssh_destination_invalid");
    }

    #[test]
    fn rejects_a_script_with_a_nul_byte() {
        let mut command = SshCommand::new("ssh");
        assert_eq!(
            command.remote_script("echo \0").unwrap_err().code,
            "ssh_script_invalid"
        );
    }

    #[cfg(unix)]
    fn fake_ssh(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let path = dir.join("fake-ssh");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn captures_stdout_and_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = fake_ssh(dir.path(), "printf 'out\\n'; printf 'err\\n' >&2");
        let output = SshCommand::new(&ssh).run().unwrap();
        assert!(output.success());
        assert_eq!(output.stdout_lossy(), "out\n");
        assert_eq!(output.stderr_lossy(), "err\n");
    }

    #[cfg(unix)]
    #[test]
    fn classifies_auth_and_remote_failures() {
        let dir = tempfile::tempdir().unwrap();
        let auth = fake_ssh(
            dir.path(),
            "printf 'dev@host: Permission denied (publickey).\\n' >&2; exit 255",
        );
        let failure = SshCommand::new(&auth).run().unwrap_err();
        assert_eq!(failure.kind, SshFailureKind::Authentication);

        let remote = fake_ssh(
            dir.path(),
            "printf 'bash: nope: command not found\\n' >&2; exit 1",
        );
        let failure = SshCommand::new(&remote).run().unwrap_err();
        assert_eq!(failure.kind, SshFailureKind::RemoteCommand);
    }

    #[cfg(unix)]
    #[test]
    fn kills_a_command_that_exceeds_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = fake_ssh(dir.path(), "exec sleep 5");
        let started = Instant::now();
        let failure = SshCommand::new(&ssh)
            .run_with_timeout(Duration::from_millis(100))
            .unwrap_err();
        assert_eq!(failure.kind, SshFailureKind::Timeout);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn reports_a_missing_client() {
        let failure = SshCommand::new("/definitely/not/ssh")
            .run_with_timeout(Duration::from_millis(100))
            .unwrap_err();
        assert_eq!(failure.kind, SshFailureKind::ClientMissing);
    }
}

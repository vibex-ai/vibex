//! Runtime paths, env file and detached start for an installed server.
//!
//! The scripts are fixed POSIX `sh`; paths, ports and env values arrive as
//! positional arguments. `setsid` is used on Linux so the runtime survives the
//! SSH session that started it.

use std::fmt;
use std::time::Duration;

use super::command::{SshCommand, SshOutput};
use super::error::SshFailure;

const MAX_PATH_CHARS: usize = 1024;
const MAX_WORKSPACE_ROOTS: usize = 64;
const ENV_TIMEOUT: Duration = Duration::from_secs(30);
const START_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchConfig {
    pub remote_install_root: String,
    pub remote_home: String,
    pub runtime_home: String,
    pub env_file: String,
    pub log_file: String,
    pub pid_file: String,
    pub binary: String,
    pub workspace_roots: Vec<String>,
    pub remote_port: u16,
    /// macOS-only: use a user LaunchAgent instead of `nohup`.
    pub launch_agent_label: Option<String>,
}

impl LaunchConfig {
    pub fn new(
        remote_home: &str,
        remote_install_root: &str,
        workspace_roots: &[String],
        remote_port: u16,
    ) -> Result<Self, LaunchError> {
        validate_remote_path("remoteHome", remote_home)?;
        validate_remote_path("remoteInstallRoot", remote_install_root)?;
        if workspace_roots.is_empty() || workspace_roots.len() > MAX_WORKSPACE_ROOTS {
            return Err(LaunchError::new(
                "ssh_launch_workspace_invalid",
                "at least one workspace root is required",
            ));
        }
        for root in workspace_roots {
            validate_remote_path("workspaceRoot", root)?;
            if root.contains(':') {
                return Err(LaunchError::new(
                    "ssh_launch_workspace_invalid",
                    "workspace roots must not contain ':'",
                ));
            }
        }
        if remote_port == 0 {
            return Err(LaunchError::new(
                "ssh_launch_port_invalid",
                "the remote runtime port must not be zero",
            ));
        }
        let runtime_home = format!("{remote_install_root}/home");
        let state_root = format!("{remote_home}/.local/state/vibex/server");
        Ok(Self {
            remote_install_root: remote_install_root.to_string(),
            remote_home: remote_home.to_string(),
            runtime_home,
            env_file: format!("{state_root}/runtime.env"),
            log_file: format!("{state_root}/server.log"),
            pid_file: format!("{state_root}/runtime.pid"),
            binary: format!("{remote_install_root}/current/vibex-server"),
            workspace_roots: workspace_roots.to_vec(),
            remote_port,
            launch_agent_label: None,
        })
    }

    pub fn with_launch_agent(mut self, label: impl Into<String>) -> Self {
        self.launch_agent_label = Some(label.into());
        self
    }

    pub fn env_pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("VIBEX_HOME", self.runtime_home.clone()),
            ("VIBEX_DB_PATH", format!("{}/vibex.db", self.runtime_home)),
            ("VIBEX_BIND_ADDR", format!("127.0.0.1:{}", self.remote_port)),
            ("VIBEX_DEPLOYMENT_MODE", "lan".to_string()),
            ("VIBEX_TLS_MODE", "pinned_certificate".to_string()),
            (
                "VIBEX_PUBLIC_HOST",
                format!("127.0.0.1:{}", self.remote_port),
            ),
            ("VIBEX_WORKSPACE_ROOTS", self.workspace_roots.join(":")),
            ("VIBEX_PROVIDER_SECRET_STORE", "file".to_string()),
            ("VIBEX_ACQUIRE_HOME_LOCK", "true".to_string()),
        ]
    }

    pub fn with_port(&self, remote_port: u16) -> Self {
        let mut next = self.clone();
        next.remote_port = remote_port;
        next
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchState {
    Started,
    AlreadyRunning,
    PidAlive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartOutcome {
    pub state: LaunchState,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl LaunchError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
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

impl fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for LaunchError {}

/// Writes the runtime env file with mode 0600.
pub fn write_env(command: &SshCommand, config: &LaunchConfig) -> Result<(), LaunchError> {
    let mut args = vec![config.env_file.clone()];
    args.extend(
        config
            .env_pairs()
            .into_iter()
            .map(|(key, value)| format!("{key}={}", shell_quote(&value))),
    );
    run_script(command, ENV_SCRIPT, &args, ENV_TIMEOUT).map(|_| ())
}

/// Writes env, then starts the runtime if it is not already running.
pub fn start(command: &SshCommand, config: &LaunchConfig) -> Result<StartOutcome, LaunchError> {
    write_env(command, config)?;
    let output = run_script(
        command,
        START_SCRIPT,
        &[
            config.env_file.clone(),
            config.binary.clone(),
            config.log_file.clone(),
            config.pid_file.clone(),
        ],
        START_TIMEOUT,
    )?;
    parse_start_output(&output.stdout_lossy())
}

pub(crate) fn run_script(
    command: &SshCommand,
    script: &str,
    args: &[String],
    timeout: Duration,
) -> Result<SshOutput, LaunchError> {
    let mut command = command.clone();
    command
        .remote_script(script)
        .map_err(|error| LaunchError::new("ssh_launch_script_invalid", error.to_string()))?;
    for arg in args {
        command.args.push(arg.into());
    }
    command.run_with_timeout(timeout).map_err(map_ssh_failure)
}

pub(crate) fn map_ssh_failure(failure: SshFailure) -> LaunchError {
    let retryable = failure.is_retryable();
    let message = if failure.stderr_tail.is_empty() {
        failure.message
    } else {
        format!("{}: {}", failure.message, failure.stderr_tail)
    };
    if retryable {
        LaunchError::retryable("ssh_launch_failed", message)
    } else {
        LaunchError::new("ssh_launch_failed", message)
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub const ENV_SCRIPT: &str = r#"set -eu
file=$1
shift
umask 077
mkdir -p "$(dirname "$file")"
: > "$file"
chmod 600 "$file"
printf '%s\n' "$@" > "$file"
echo env=ok
"#;

pub const START_SCRIPT: &str = r#"set -eu
envfile=$1
binary=$2
log=$3
pidfile=$4
test -x "$binary"
set -a
. "$envfile"
set +a
if "$binary" status --json 2>/dev/null | grep -q '"running":true'; then
  echo "launch=already_running"
  exit 0
fi
if [ -f "$pidfile" ]; then
  pid=$(cat "$pidfile" 2>/dev/null || true)
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    echo "launch=pid_alive pid=$pid"
    exit 0
  fi
  rm -f "$pidfile"
fi
mkdir -p "$(dirname "$log")"
if command -v setsid >/dev/null 2>&1; then
  nohup setsid "$binary" serve --no-pairing >> "$log" 2>&1 < /dev/null &
else
  nohup "$binary" serve --no-pairing >> "$log" 2>&1 < /dev/null &
fi
pid=$!
umask 077
echo "$pid" > "$pidfile"
echo "launch=started pid=$pid"
"#;

fn parse_start_output(stdout: &str) -> Result<StartOutcome, LaunchError> {
    for line in stdout.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        if key != "launch" {
            continue;
        }
        let mut parts = value.split_whitespace();
        let state = match parts.next() {
            Some("started") => LaunchState::Started,
            Some("already_running") => LaunchState::AlreadyRunning,
            Some("pid_alive") => LaunchState::PidAlive,
            _ => continue,
        };
        let pid = parts
            .find_map(|part| part.strip_prefix("pid="))
            .and_then(|pid| pid.parse::<u32>().ok());
        return Ok(StartOutcome { state, pid });
    }
    Err(LaunchError::new(
        "ssh_launch_unexpected",
        "the remote runtime did not report a start result",
    ))
}

fn validate_remote_path(label: &'static str, value: &str) -> Result<(), LaunchError> {
    if value.is_empty()
        || value.chars().count() > MAX_PATH_CHARS
        || !value.starts_with('/')
        || value.contains('\0')
        || value.contains('\n')
        || value.contains('\r')
        || value.split('/').any(|part| part == "..")
    {
        return Err(LaunchError::new(
            "ssh_launch_path_invalid",
            format!("{label} is not a safe absolute remote path"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    fn config(port: u16) -> LaunchConfig {
        LaunchConfig::new(
            "/home/dev",
            "/home/dev/.local/share/vibex/server",
            &["/home/dev".to_string(), "/home/dev/work".to_string()],
            port,
        )
        .unwrap()
    }

    #[test]
    fn builds_env_and_paths_from_the_profile_roots() {
        let config = config(8765);
        assert_eq!(
            config.runtime_home,
            "/home/dev/.local/share/vibex/server/home"
        );
        assert_eq!(
            config.env_file,
            "/home/dev/.local/state/vibex/server/runtime.env"
        );
        assert_eq!(
            config.binary,
            "/home/dev/.local/share/vibex/server/current/vibex-server"
        );
        let env = config.env_pairs();
        assert!(env.contains(&("VIBEX_BIND_ADDR", "127.0.0.1:8765".to_string())));
        assert!(env.contains(&(
            "VIBEX_WORKSPACE_ROOTS",
            "/home/dev:/home/dev/work".to_string()
        )));
        assert!(env.contains(&("VIBEX_PROVIDER_SECRET_STORE", "file".to_string())));
        assert!(env.contains(&("VIBEX_ACQUIRE_HOME_LOCK", "true".to_string())));
    }

    #[test]
    fn rejects_unsafe_paths_and_workspace_roots() {
        for (home, root, workspaces) in [
            ("/home/dev", "relative", vec!["/home/dev".to_string()]),
            ("/home/dev", "/home/../root", vec!["/home/dev".to_string()]),
            ("/home/dev", "/home/dev", vec![]),
            ("/home/dev", "/home/dev", vec!["relative".to_string()]),
            ("/home/dev", "/home/dev", vec!["/home/a:b".to_string()]),
            ("/home/dev\n", "/home/dev", vec!["/home/dev".to_string()]),
        ] {
            assert!(
                LaunchConfig::new(home, root, &workspaces, 8765).is_err(),
                "{home:?} {root:?} {workspaces:?}"
            );
        }
    }

    #[test]
    fn env_script_writes_mode_0600_and_quotes_values_as_arguments() {
        let _process_guard = crate::ssh_connection::test_support::process_guard();
        let dir = tempfile::tempdir().unwrap();
        let env_file = dir.path().join("runtime.env");
        let home = format!("VIBEX_HOME={}", shell_quote("/home/dev/a b"));
        let bind = format!("VIBEX_BIND_ADDR={}", shell_quote("127.0.0.1:8765"));
        let mut child = std::process::Command::new("sh")
            .arg("-s")
            .args([env_file.to_str().unwrap(), home.as_str(), bind.as_str()])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(ENV_SCRIPT.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let mode = fs::metadata(&env_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            fs::read_to_string(&env_file).unwrap(),
            "VIBEX_HOME='/home/dev/a b'\nVIBEX_BIND_ADDR='127.0.0.1:8765'\n"
        );
        assert_eq!(shell_quote("/home/dev/a'b"), "'/home/dev/a'\\''b'");
    }

    #[test]
    fn start_script_keeps_detach_and_idempotence_markers() {
        assert!(START_SCRIPT.contains("nohup setsid"));
        assert!(START_SCRIPT.contains("command -v setsid"));
        assert!(START_SCRIPT.contains("nohup \"$binary\" serve"));
        assert!(START_SCRIPT.contains("status --json"));
        assert!(START_SCRIPT.contains("launch=already_running"));
        assert!(START_SCRIPT.contains("launch=pid_alive"));
        assert!(START_SCRIPT.contains("launch=started"));
        assert!(START_SCRIPT.contains("umask 077"));
    }

    #[test]
    fn parses_start_outcomes() {
        assert_eq!(
            parse_start_output("launch=started pid=42\n").unwrap(),
            StartOutcome {
                state: LaunchState::Started,
                pid: Some(42)
            }
        );
        assert_eq!(
            parse_start_output("launch=already_running\n")
                .unwrap()
                .state,
            LaunchState::AlreadyRunning
        );
        assert_eq!(
            parse_start_output("launch=pid_alive pid=7\n")
                .unwrap()
                .state,
            LaunchState::PidAlive
        );
        assert_eq!(
            parse_start_output("noise\n").unwrap_err().code,
            "ssh_launch_unexpected"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_env_passes_the_file_path_as_the_first_script_argument() {
        let dir = tempfile::tempdir().unwrap();
        let capture = dir.path().join("args.txt");
        let ssh = dir.path().join("fake-ssh");
        fs::write(
            &ssh,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\ncat >/dev/null\n",
                capture.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
        let command = SshCommand::new(&ssh);
        let config = config(8765);
        write_env(&command, &config).unwrap();
        let args = fs::read_to_string(&capture).unwrap();
        let lines = args.lines().collect::<Vec<_>>();
        assert!(
            lines
                .windows(2)
                .any(|pair| { pair[0] == "-s" && pair[1].trim_matches('\'') == config.env_file }),
            "env file must follow sh -s: {args}"
        );
        assert!(args.contains("VIBEX_HOME='"));
    }

    #[cfg(unix)]
    #[test]
    fn start_runs_the_fixed_script_through_ssh() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join("fake-ssh");
        fs::write(
            &ssh,
            "#!/bin/sh\ncat >/dev/null\necho 'launch=started pid=99'\n",
        )
        .unwrap();
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
        let command = SshCommand::new(&ssh);
        let outcome = start(&command, &config(8765)).unwrap();
        assert_eq!(outcome.state, LaunchState::Started);
        assert_eq!(outcome.pid, Some(99));
    }
}

//! macOS user LaunchAgent lifecycle.
//!
//! A LaunchAgent is the macOS equivalent of the Linux detached `nohup` start:
//! the plist lives under `~/Library/LaunchAgents`, runs at login, and keeps the
//! runtime independent of the SSH session. `KeepAlive` stays false so the
//! server's own `stop` is authoritative; the next login/bootstrap starts it.

use std::fmt;
use std::time::{Duration, Instant};

use super::command::SshCommand;
use super::launch::{LaunchConfig, LaunchError, write_env};
use super::supervisor::{self, ServerStatus, SupervisorError};

const STATUS_TIMEOUT: Duration = Duration::from_secs(30);
const START_DEADLINE: Duration = Duration::from_secs(20);
const STATUS_POLL: Duration = Duration::from_millis(400);
const MAX_LABEL_CHARS: usize = 96;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchdError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl LaunchdError {
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

impl fmt::Display for LaunchdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for LaunchdError {}

impl From<LaunchError> for LaunchdError {
    fn from(error: LaunchError) -> Self {
        Self {
            code: "ssh_launchd_env_failed",
            message: format!("{}: {}", error.code, error.message),
            retryable: error.retryable,
        }
    }
}

impl From<SupervisorError> for LaunchdError {
    fn from(error: SupervisorError) -> Self {
        Self {
            code: "ssh_launchd_status_failed",
            message: format!("{}: {}", error.code, error.message),
            retryable: error.retryable,
        }
    }
}

pub fn label_for_profile(profile_id: &str) -> String {
    let mut label = String::from("dev.vibex.server.");
    for character in profile_id.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
            label.push(character);
        } else {
            label.push('_');
        }
    }
    label
}

pub fn plist_path(config: &LaunchConfig, label: &str) -> String {
    format!("{}/Library/LaunchAgents/{label}.plist", config.remote_home)
}

/// Writes the plist, reloads the agent, and waits for the runtime health check.
pub fn ensure_started(
    command: &SshCommand,
    config: &LaunchConfig,
    label: &str,
) -> Result<ServerStatus, LaunchdError> {
    validate_label(label)?;
    if let Ok(current) = supervisor::status(command, config)
        && current.is_healthy()
    {
        return Ok(current);
    }
    write_env(command, config)?;
    let plist = plist_content(config, label);
    let path = plist_path(config, label);
    upload_plist(command, &path, &plist)?;

    let mut reload = command.clone();
    reload
        .remote_script(RELOAD_SCRIPT)
        .map_err(|error| LaunchdError::new("ssh_launchd_script_invalid", error.to_string()))?;
    reload
        .args
        .extend(["gui".into(), label.into(), path.clone().into()]);
    let output = reload.run_with_timeout(STATUS_TIMEOUT).map_err(|error| {
        LaunchdError::new(
            "ssh_launchd_bootstrap_failed",
            format!("{}: {}", error.code(), error.message),
        )
    })?;
    if !output.status.success() {
        return Err(LaunchdError::new(
            "ssh_launchd_bootstrap_failed",
            format!(
                "launchctl bootstrap failed: {}",
                output.stderr_lossy().trim()
            ),
        ));
    }

    let deadline = Instant::now() + START_DEADLINE;
    loop {
        let current = supervisor::status(command, config)?;
        if current.is_healthy() {
            return Ok(current);
        }
        if Instant::now() >= deadline {
            return Err(LaunchdError::retryable(
                "ssh_launchd_health_timeout",
                "the LaunchAgent runtime did not become healthy in time",
            ));
        }
        std::thread::sleep(STATUS_POLL);
    }
}

/// Unloads the agent and removes its plist.
pub fn remove(
    command: &SshCommand,
    config: &LaunchConfig,
    label: &str,
) -> Result<(), LaunchdError> {
    stop(command, label)?;
    let mut remove = command.clone();
    remove
        .remote_script("rm -f \"$1\"")
        .map_err(|error| LaunchdError::new("ssh_launchd_script_invalid", error.to_string()))?;
    remove.args.push(plist_path(config, label).into());
    remove.run_with_timeout(STATUS_TIMEOUT).map_err(|error| {
        LaunchdError::new(
            "ssh_launchd_remove_failed",
            format!("{}: {}", error.code(), error.message),
        )
    })?;
    Ok(())
}

/// Unloads the agent. The server's own stop remains the process-level action.
pub fn stop(command: &SshCommand, label: &str) -> Result<(), LaunchdError> {
    validate_label(label)?;
    let mut bootout = command.clone();
    bootout
        .remote_script(BOOTOUT_SCRIPT)
        .map_err(|error| LaunchdError::new("ssh_launchd_script_invalid", error.to_string()))?;
    bootout.args.extend(["gui".into(), label.into()]);
    bootout.run_with_timeout(STATUS_TIMEOUT).map_err(|error| {
        LaunchdError::new(
            "ssh_launchd_bootout_failed",
            format!("{}: {}", error.code(), error.message),
        )
    })?;
    Ok(())
}

fn upload_plist(command: &SshCommand, path: &str, contents: &str) -> Result<(), LaunchdError> {
    let mut upload = command.clone();
    upload.args.extend([
        "sh".into(),
        "-c".into(),
        shell_quote(UPLOAD_SCRIPT).into(),
        "sh".into(),
        shell_quote(path).into(),
    ]);
    upload.stdin = Some(contents.as_bytes().to_vec());
    upload.run_with_timeout(STATUS_TIMEOUT).map_err(|error| {
        LaunchdError::new(
            "ssh_launchd_upload_failed",
            format!("{}: {}", error.code(), error.message),
        )
    })?;
    Ok(())
}

fn plist_content(config: &LaunchConfig, label: &str) -> String {
    let mut env = String::new();
    for (key, value) in config.env_pairs() {
        env.push_str(&format!(
            "    <key>{}</key>\n    <string>{}</string>\n",
            xml_escape(key),
            xml_escape(&value)
        ));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{binary}</string>
    <string>serve</string>
    <string>--no-pairing</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
{env}  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <false/>
  <key>WorkingDirectory</key>
  <string>{home}</string>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        label = xml_escape(label),
        binary = xml_escape(&config.binary),
        home = xml_escape(&config.runtime_home),
        log = xml_escape(&config.log_file),
        env = env,
    )
}

fn validate_label(label: &str) -> Result<(), LaunchdError> {
    if label.is_empty()
        || label.len() > MAX_LABEL_CHARS
        || !label.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
    {
        return Err(LaunchdError::new(
            "ssh_launchd_label_invalid",
            "the LaunchAgent label is invalid",
        ));
    }
    Ok(())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

const UPLOAD_SCRIPT: &str = r#"set -eu; umask 077; mkdir -p "$(dirname "$1")"; cat > "$1""#;

const RELOAD_SCRIPT: &str = r#"set -eu
domain=$1
label=$2
plist=$3
uid=$(id -u)
launchctl bootout "$domain/$uid/$label" >/dev/null 2>&1 || true
launchctl bootstrap "$domain/$uid" "$plist"
launchctl kickstart -k "$domain/$uid/$label"
"#;

const BOOTOUT_SCRIPT: &str = r#"set -eu
domain=$1
label=$2
uid=$(id -u)
launchctl bootout "$domain/$uid/$label" >/dev/null 2>&1 || true
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LaunchConfig {
        LaunchConfig::new(
            "/Users/dev",
            "/Users/dev/.local/share/vibex/server",
            &["/Users/dev".to_string()],
            8765,
        )
        .unwrap()
        .with_launch_agent("dev.vibex.server.ssh-1")
    }

    #[test]
    fn plist_sets_login_start_and_no_keepalive() {
        let plist = plist_content(&config(), "dev.vibex.server.ssh-1");
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <false/>"));
        assert!(plist.contains("<string>serve</string>"));
        assert!(plist.contains("VIBEX_HOME"));
        assert!(!plist.contains("/Users/dev/Library/LaunchAgents/dev.vibex.server.ssh-1.plist"));
        assert!(plist.contains("StandardOutPath"));
    }

    #[test]
    fn values_are_xml_escaped_and_labels_are_safe() {
        assert_eq!(xml_escape("a&b<c>\"'"), "a&amp;b&lt;c&gt;&quot;&apos;");
        assert!(validate_label("dev.vibex.server.ssh-1").is_ok());
        assert!(validate_label("bad label").is_err());
        assert_eq!(label_for_profile("ssh/1"), "dev.vibex.server.ssh_1");
    }

    #[test]
    fn fixed_scripts_do_not_interpolate_paths() {
        assert!(UPLOAD_SCRIPT.contains("dirname"));
        assert!(RELOAD_SCRIPT.contains("launchctl bootstrap"));
        assert!(RELOAD_SCRIPT.contains("kickstart -k"));
        assert!(BOOTOUT_SCRIPT.contains("launchctl bootout"));
        assert!(!RELOAD_SCRIPT.contains("${"));
    }
}

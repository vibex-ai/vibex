//! Runtime upgrade and rollback over SSH.
//!
//! Upgrade is intentionally a stop/snapshot/start transaction:
//!
//! 1. install the new version (the installer swaps `current`, leaves `previous`);
//! 2. stop the old runtime;
//! 3. snapshot `VIBEX_HOME/vibex.db*` with the runtime stopped;
//! 4. start the new runtime and wait for health.
//!
//! If any step after the install fails, the runtime is stopped, the database
//! snapshot is restored, `current` is switched back to `previous`, and the old
//! runtime is started again. The home, identity, certificate and pairing grant
//! are never rewritten, so a rollback does not require re-pairing.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;

use super::artifact::LocalArtifact;
use super::command::SshCommand;
use super::install::{InstallError, InstallOutcome, install_artifact};
use super::launch::LaunchConfig;
use super::launchd;
use super::profile::SshRuntimeChannel;
use super::supervisor::{self, SupervisorError};

const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(120);
const RESTORE_TIMEOUT: Duration = Duration::from_secs(120);
const SWITCH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradePhase {
    Preflight,
    Installing,
    Stopping,
    Snapshotting,
    Starting,
    RollingBack,
}

impl UpgradePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Installing => "installing",
            Self::Stopping => "stopping",
            Self::Snapshotting => "snapshotting",
            Self::Starting => "starting",
            Self::RollingBack => "rolling_back",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeError {
    pub code: &'static str,
    pub message: String,
    pub phase: UpgradePhase,
    pub retryable: bool,
    /// `None` when no rollback was needed; `Some("ok")` or a failure detail
    /// when the upgrade attempted to restore the previous version.
    pub rollback: Option<String>,
}

impl UpgradeError {
    fn new(code: &'static str, phase: UpgradePhase, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            phase,
            retryable: false,
            rollback: None,
        }
    }
}

impl fmt::Display for UpgradeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for UpgradeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeOutcome {
    pub from_version: String,
    pub to_version: String,
    pub install: InstallOutcome,
    pub snapshot_dir: String,
    /// Version directories removed after the new runtime became healthy.
    pub cleaned_versions: usize,
}

/// Installs `artifact`, switches the runtime to it, and rolls back on failure.
///
/// The caller is expected to stop accepting new Agent turns and warn about
/// terminal interruption before calling this. The database snapshot is taken
/// after the old runtime stops, which is the only point where SQLite and its
/// WAL are guaranteed to be at rest.
pub fn upgrade_runtime(
    command: &SshCommand,
    launch: &LaunchConfig,
    artifact: &LocalArtifact,
    channel: SshRuntimeChannel,
    progress: impl FnMut(u64, u64) + Send + 'static,
) -> Result<UpgradeOutcome, UpgradeError> {
    let before = supervisor::status(command, launch).map_err(|error| {
        UpgradeError::new(
            "ssh_upgrade_preflight_failed",
            UpgradePhase::Preflight,
            supervisor_message(error),
        )
    })?;
    let target = parse_version(&artifact.version).ok_or_else(|| {
        UpgradeError::new(
            "ssh_upgrade_version_invalid",
            UpgradePhase::Preflight,
            "the target runtime version is not valid semver",
        )
    })?;
    let current = parse_version(&before.version).ok_or_else(|| {
        UpgradeError::new(
            "ssh_upgrade_current_version_invalid",
            UpgradePhase::Preflight,
            "the installed runtime version is not valid semver",
        )
    })?;
    validate_upgrade(&current, &target, channel).map_err(|message| {
        UpgradeError::new("ssh_upgrade_incompatible", UpgradePhase::Preflight, message)
    })?;

    let install = install_artifact(command, artifact, &launch.remote_install_root, progress)
        .map_err(|error| {
            UpgradeError::new(
                "ssh_upgrade_install_failed",
                UpgradePhase::Installing,
                install_message(error),
            )
        })?;

    if let Err(error) = supervisor::stop(command, launch) {
        return Err(rollback(
            command,
            launch,
            None,
            UpgradeError::new(
                "ssh_upgrade_stop_failed",
                UpgradePhase::Stopping,
                supervisor_message(error),
            ),
        ));
    }

    let snapshot = match snapshot_database(command, launch) {
        Ok(snapshot) => snapshot,
        Err(error) => return Err(rollback(command, launch, None, error)),
    };

    if let Err(error) = ensure_started(command, launch) {
        return Err(rollback(
            command,
            launch,
            Some(&snapshot),
            UpgradeError::new("ssh_upgrade_start_failed", UpgradePhase::Starting, error),
        ));
    }

    let cleaned_versions = cleanup_old_versions(command, launch).unwrap_or_default();

    Ok(UpgradeOutcome {
        from_version: before.version,
        to_version: artifact.version.clone(),
        install,
        snapshot_dir: snapshot,
        cleaned_versions,
    })
}

/// Restores `current` to `previous` and starts the old runtime. `snapshot`
/// is the directory returned by the snapshot script, if one was taken.
fn rollback(
    command: &SshCommand,
    launch: &LaunchConfig,
    snapshot: Option<&str>,
    mut error: UpgradeError,
) -> UpgradeError {
    let mut steps = Vec::new();
    match supervisor::stop(command, launch) {
        Ok(_) => steps.push("stopped"),
        Err(_) => steps.push("stop_failed"),
    }
    if let Some(snapshot) = snapshot {
        match restore_database(command, &launch.runtime_home, snapshot) {
            Ok(()) => steps.push("db_restored"),
            Err(_) => steps.push("db_restore_failed"),
        }
    }
    match switch_to_previous(command, &launch.remote_install_root) {
        Ok(()) => steps.push("current_previous"),
        Err(_) => steps.push("switch_failed"),
    }
    match ensure_started(command, launch) {
        Ok(_) => steps.push("old_started"),
        Err(_) => steps.push("old_start_failed"),
    }
    error.phase = UpgradePhase::RollingBack;
    error.rollback = Some(steps.join(","));
    error
}

fn ensure_started(command: &SshCommand, launch: &LaunchConfig) -> Result<(), String> {
    if let Some(label) = launch.launch_agent_label.as_deref() {
        launchd::ensure_started(command, launch, label)
            .map(|_| ())
            .map_err(|error| format!("{}: {}", error.code, error.message))
    } else {
        supervisor::ensure_started(command, launch)
            .map(|_| ())
            .map_err(|error| format!("{}: {}", error.code, error.message))
    }
}

fn snapshot_database(command: &SshCommand, launch: &LaunchConfig) -> Result<String, UpgradeError> {
    let tag = format!(
        "upgrade-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default()
    );
    let dir = format!("{}/backups/{tag}", launch.remote_install_root);
    let mut snapshot = command.clone();
    snapshot.remote_script(SNAPSHOT_SCRIPT).map_err(|error| {
        UpgradeError::new(
            "ssh_upgrade_snapshot_script",
            UpgradePhase::Snapshotting,
            error.to_string(),
        )
    })?;
    snapshot
        .args
        .extend([launch.runtime_home.clone().into(), dir.clone().into()]);
    let output = snapshot
        .run_with_timeout(SNAPSHOT_TIMEOUT)
        .map_err(|error| {
            UpgradeError::new(
                "ssh_upgrade_snapshot_failed",
                UpgradePhase::Snapshotting,
                ssh_message(error),
            )
        })?;
    if !output.stdout_lossy().contains("snapshot=ok") {
        return Err(UpgradeError::new(
            "ssh_upgrade_snapshot_unconfirmed",
            UpgradePhase::Snapshotting,
            "the database snapshot did not confirm",
        ));
    }
    Ok(dir)
}

fn restore_database(
    command: &SshCommand,
    home: &str,
    snapshot_dir: &str,
) -> Result<(), UpgradeError> {
    let mut restore = command.clone();
    restore.remote_script(RESTORE_SCRIPT).map_err(|error| {
        UpgradeError::new(
            "ssh_upgrade_restore_script",
            UpgradePhase::RollingBack,
            error.to_string(),
        )
    })?;
    restore.args.extend([home.into(), snapshot_dir.into()]);
    let output = restore.run_with_timeout(RESTORE_TIMEOUT).map_err(|error| {
        UpgradeError::new(
            "ssh_upgrade_restore_failed",
            UpgradePhase::RollingBack,
            ssh_message(error),
        )
    })?;
    if !output.stdout_lossy().contains("restore=ok") {
        return Err(UpgradeError::new(
            "ssh_upgrade_restore_unconfirmed",
            UpgradePhase::RollingBack,
            "the database restore did not confirm",
        ));
    }
    Ok(())
}

/// Removes version directories other than `current` and `previous`. The
/// runtime home, backups, and symlinks are preserved. Best effort: cleanup
/// failure never turns a healthy upgrade into a failed one.
pub fn cleanup_old_versions(
    command: &SshCommand,
    launch: &LaunchConfig,
) -> Result<usize, UpgradeError> {
    let mut cleanup = command.clone();
    cleanup.remote_script(CLEANUP_SCRIPT).map_err(|error| {
        UpgradeError::new(
            "ssh_upgrade_cleanup_script",
            UpgradePhase::Starting,
            error.to_string(),
        )
    })?;
    cleanup.args.push(launch.remote_install_root.clone().into());
    let output = cleanup.run_with_timeout(SWITCH_TIMEOUT).map_err(|error| {
        UpgradeError::new(
            "ssh_upgrade_cleanup_failed",
            UpgradePhase::Starting,
            ssh_message(error),
        )
    })?;
    let stdout = output.stdout_lossy();
    let count = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("cleanup="))
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| {
            UpgradeError::new(
                "ssh_upgrade_cleanup_unconfirmed",
                UpgradePhase::Starting,
                "the version cleanup did not confirm",
            )
        })?;
    Ok(count)
}

fn switch_to_previous(command: &SshCommand, root: &str) -> Result<(), UpgradeError> {
    let mut switch = command.clone();
    switch.remote_script(SWITCH_SCRIPT).map_err(|error| {
        UpgradeError::new(
            "ssh_upgrade_switch_script",
            UpgradePhase::RollingBack,
            error.to_string(),
        )
    })?;
    switch.args.push(root.into());
    let output = switch.run_with_timeout(SWITCH_TIMEOUT).map_err(|error| {
        UpgradeError::new(
            "ssh_upgrade_switch_failed",
            UpgradePhase::RollingBack,
            ssh_message(error),
        )
    })?;
    if !output.stdout_lossy().contains("current=") {
        return Err(UpgradeError::new(
            "ssh_upgrade_switch_unconfirmed",
            UpgradePhase::RollingBack,
            "the previous runtime link did not switch",
        ));
    }
    Ok(())
}

pub const SNAPSHOT_SCRIPT: &str = r#"set -eu
home=$1
dir=$2
mkdir -p "$dir"
chmod 700 "$dir"
files=""
for name in vibex.db vibex.db-wal vibex.db-shm; do
  if [ -e "$home/$name" ]; then
    files="$files $name"
  fi
done
if [ -z "$files" ]; then
  echo "snapshot=missing"
  exit 2
fi
tar -czf "$dir/db.tar.gz" -C "$home" $files
echo "snapshot=ok"
"#;

pub const RESTORE_SCRIPT: &str = r#"set -eu
home=$1
dir=$2
rm -f "$home/vibex.db" "$home/vibex.db-wal" "$home/vibex.db-shm"
tar -xzf "$dir/db.tar.gz" -C "$home"
chmod 600 "$home/vibex.db"
echo "restore=ok"
"#;

pub const CLEANUP_SCRIPT: &str = r#"set -eu
root=$1
current=$(readlink "$root/current" 2>/dev/null || true)
previous=$(readlink "$root/previous" 2>/dev/null || true)
removed=0
for entry in "$root"/*; do
  if [ ! -d "$entry" ]; then
    continue
  fi
  name=$(basename "$entry")
  case "$name" in
    backups|home|current|previous) continue ;;
    *[!0-9A-Za-z._-]*) continue ;;
  esac
  if [ "$name" = "$current" ] || [ "$name" = "$previous" ]; then
    continue
  fi
  rm -rf "$entry"
  removed=$((removed + 1))
done
echo "cleanup=$removed"
"#;

pub const SWITCH_SCRIPT: &str = r#"set -eu
root=$1
previous=$(readlink "$root/previous" 2>/dev/null || true)
if [ -z "$previous" ]; then
  echo "current=missing"
  exit 2
fi
ln -sfn "$previous" "$root/current"
echo "current=$previous"
"#;

fn parse_version(value: &str) -> Option<Version> {
    Version::parse(value).ok()
}

pub fn channel_for_version_str(value: &str) -> Option<SshRuntimeChannel> {
    parse_version(value).map(|version| channel_for_version(&version))
}

fn channel_for_version(version: &Version) -> SshRuntimeChannel {
    if version.pre.is_empty() {
        SshRuntimeChannel::Stable
    } else if version.pre.starts_with("rc") {
        SshRuntimeChannel::Rc
    } else {
        SshRuntimeChannel::Preview
    }
}

/// Validates monotonicity and channel. Stable never accepts a prerelease and
/// RC/Preview never accept a stable artifact; this is stricter than semver's
/// ordering because the channel is a product boundary.
pub fn validate_upgrade(
    current: &Version,
    target: &Version,
    channel: SshRuntimeChannel,
) -> Result<(), String> {
    if target <= current {
        return Err("the target runtime is not newer than the installed runtime".to_string());
    }
    if channel_for_version(target) != channel {
        return Err("the target runtime channel does not match the connection".to_string());
    }
    Ok(())
}

fn supervisor_message(error: SupervisorError) -> String {
    format!("{}: {}", error.code, error.message)
}

fn install_message(error: InstallError) -> String {
    format!("{}: {}", error.code, error.message)
}

fn ssh_message(error: super::error::SshFailure) -> String {
    if error.stderr_tail.is_empty() {
        format!("{}: {}", error.code(), error.message)
    } else {
        format!("{}: {}: {}", error.code(), error.message, error.stderr_tail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(value: &str) -> Version {
        Version::parse(value).unwrap()
    }

    #[test]
    fn accepts_a_newer_same_channel_runtime() {
        assert!(
            validate_upgrade(
                &version("0.1.0-rc.8"),
                &version("0.1.0-rc.9"),
                SshRuntimeChannel::Rc
            )
            .is_ok()
        );
        assert!(
            validate_upgrade(
                &version("0.1.0"),
                &version("0.2.0"),
                SshRuntimeChannel::Stable
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_downgrades_equal_versions_and_cross_channel_artifacts() {
        assert!(
            validate_upgrade(
                &version("0.2.0"),
                &version("0.1.0"),
                SshRuntimeChannel::Stable
            )
            .is_err()
        );
        assert!(
            validate_upgrade(
                &version("0.1.0"),
                &version("0.1.0"),
                SshRuntimeChannel::Stable
            )
            .is_err()
        );
        assert!(
            validate_upgrade(
                &version("0.1.0-rc.8"),
                &version("0.1.0-rc.9"),
                SshRuntimeChannel::Stable
            )
            .is_err()
        );
        assert!(
            validate_upgrade(
                &version("0.1.0"),
                &version("0.2.0-rc.1"),
                SshRuntimeChannel::Stable
            )
            .is_err()
        );
        assert!(
            validate_upgrade(&version("0.1.0"), &version("0.2.0"), SshRuntimeChannel::Rc).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn upgrade_stops_snapshots_and_starts_the_new_runtime() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt as _;
        use std::process::Command;

        use sha2::{Digest as _, Sha256};

        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        fs::write(&state, b"1").unwrap();
        let ssh = dir.path().join("fake-ssh");
        fs::write(
            &ssh,
            format!(
                r#"#!/bin/sh
state={state}
fail={fail}
case "$*" in
  *"sh -c"*) cat >/dev/null; exit 0;;
esac
script=$(cat)
case "$script" in
  *"launch=started"*)
    if [ -e "$fail" ]; then
      rm -f "$fail"
      echo "launch=failed"
      exit 0
    fi
    echo 1 > "$state"
    echo "launch=started pid=123"
    ;;
  *"status --json"*)
    if [ "$(cat "$state")" = 1 ]; then
      printf '{{"schemaVersion":"vibex-server-status.v1","running":true,"pid":123,"port":8765,"version":"0.1.0-rc.8","home":"/home/dev","health":"ok"}}\n'
    else
      printf '{{"schemaVersion":"vibex-server-status.v1","running":false,"pid":null,"port":8765,"version":"0.1.0-rc.8","home":"/home/dev","health":"stopped"}}\n'
    fi
    ;;
  *" stop"*) echo 0 > "$state"; echo stopped=true;;
  *"snapshot=ok"*) echo snapshot=ok;;
  *"cleanup="*) echo cleanup=2;;
  *"restore=ok"*) echo restore=ok;;
  *"current="*) echo current=0.1.0-rc.8;;
  *) echo install=ok;;
esac
"#,
                state = state.display(),
                fail = dir.path().join("fail-start").display()
            ),
        )
        .unwrap();
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();

        let payload = dir.path().join("payload");
        fs::create_dir(&payload).unwrap();
        fs::write(payload.join("vibex-server"), b"server").unwrap();
        let artifact_path = dir.path().join("server.tar.gz");
        assert!(
            Command::new("tar")
                .args(["-czf", artifact_path.to_str().unwrap(), "-C"])
                .arg(&payload)
                .arg("vibex-server")
                .status()
                .unwrap()
                .success()
        );
        let bytes = fs::read(&artifact_path).unwrap();
        let artifact = LocalArtifact {
            path: artifact_path,
            version: "0.1.0-rc.9".to_string(),
            arch: crate::ssh_connection::RemoteArch::X86_64,
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        let root = dir.path().join("server-root");
        fs::create_dir(&root).unwrap();
        let launch = LaunchConfig::new(
            "/home/dev",
            root.to_str().unwrap(),
            &["/home/dev".to_string()],
            8765,
        )
        .unwrap();
        let command = SshCommand::new(&ssh);
        let outcome = upgrade_runtime(
            &command,
            &launch,
            &artifact,
            SshRuntimeChannel::Rc,
            |_, _| {},
        )
        .unwrap();
        assert_eq!(outcome.from_version, "0.1.0-rc.8");
        assert_eq!(outcome.to_version, "0.1.0-rc.9");
        assert!(outcome.snapshot_dir.contains("/backups/upgrade-"));
        assert_eq!(outcome.cleaned_versions, 2);
        assert_eq!(fs::read_to_string(&state).unwrap().trim(), "1");

        fs::write(dir.path().join("fail-start"), b"1").unwrap();
        let mut next = artifact.clone();
        next.version = "0.1.0-rc.10".to_string();
        let error = upgrade_runtime(&command, &launch, &next, SshRuntimeChannel::Rc, |_, _| {})
            .expect_err("the new runtime start should fail");
        assert_eq!(error.code, "ssh_upgrade_start_failed");
        assert!(
            error
                .rollback
                .as_deref()
                .is_some_and(|steps| steps.contains("old_started")),
            "rollback: {:?}",
            error.rollback
        );
        assert_eq!(fs::read_to_string(&state).unwrap().trim(), "1");
    }

    #[test]
    fn fixed_scripts_do_not_interpolate_user_values() {
        for script in [
            SNAPSHOT_SCRIPT,
            RESTORE_SCRIPT,
            SWITCH_SCRIPT,
            CLEANUP_SCRIPT,
        ] {
            assert!(script.starts_with("set -eu"));
            assert!(!script.contains("${"));
        }
        assert!(SNAPSHOT_SCRIPT.contains("vibex.db-wal"));
        assert!(RESTORE_SCRIPT.contains("tar -xzf"));
        assert!(SWITCH_SCRIPT.contains("previous"));
        assert!(CLEANUP_SCRIPT.contains("backups|home"));
    }
}

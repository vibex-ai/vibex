#![cfg(not(target_family = "wasm"))]

//! Opt-in end-to-end smoke for the SSH provisioning stack.
//!
//! Requires a disposable Linux sshd fixture, the signed-layout server tarball,
//! and an SSH config alias. Run with:
//!
//! ```text
//! VIBEX_SSH_E2E=1 \
//! VIBEX_SSH_E2E_CONFIG=/tmp/vibex-ssh-e2e/config \
//! VIBEX_SSH_E2E_ARTIFACT=target/server-artifacts-2.8/vibex-server-0.1.0-rc.8-linux-x86_64.tar.gz \
//! cargo test -p vibex-desktop --test ssh_provisioning_e2e -- --ignored --nocapture
//!
//! For macOS set VIBEX_SSH_E2E_ALIAS, PORT=22, REMOTE_HOME=/Users/<user>,
//! REMOTE_ROOT=/Users/<user>/.local/share/vibex/server, ARCH=aarch64, and
//! ARTIFACT_VERSION to the Mac-built server version.
//! ```

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest as _, Sha256};
use vibex_desktop::ssh_connection::{
    LaunchConfig, LocalArtifact, MIN_FREE_KB, ProvisioningCoordinator, ProvisioningRunOutcome,
    RemoteArch, RunCancellation, SshCommand, SshDestination, SshProvisioningEffects,
    SshRuntimeChannel, install::install_artifact, launchd, probe, run_provisioning,
    select_local_port, supervisor, uninstall::uninstall_runtime, upgrade::upgrade_runtime,
};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
}

fn fixture_config() -> Option<PathBuf> {
    std::env::var_os("VIBEX_SSH_E2E")?;
    Some(PathBuf::from(
        std::env::var("VIBEX_SSH_E2E_CONFIG").expect("VIBEX_SSH_E2E_CONFIG"),
    ))
}

fn fixture_destination() -> SshDestination {
    let alias = std::env::var("VIBEX_SSH_E2E_ALIAS").unwrap_or_else(|_| "vibex-e2e".to_string());
    let port = std::env::var("VIBEX_SSH_E2E_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2222);
    SshDestination {
        host: alias,
        user: None,
        port,
    }
}

fn command_for(config: &Path) -> SshCommand {
    SshCommand::for_destination("ssh", &fixture_destination(), Some(config))
        .expect("build SSH command")
}

fn remote_file_sha(command: &SshCommand, path: &str) -> String {
    let mut read = command.clone();
    read.remote_script("sha256sum \"$1\" | awk '{print $1}'")
        .unwrap();
    read.args.push(path.into());
    read.run()
        .expect("hash remote file")
        .stdout_lossy()
        .trim()
        .to_string()
}

fn write_config(
    dir: &Path,
    identity: &Path,
    known_hosts: &Path,
    strict_host_key_checking: &str,
) -> PathBuf {
    let config = dir.join("config");
    let mut text = String::new();
    writeln!(text, "Host vibex-e2e").unwrap();
    writeln!(text, "  HostName 127.0.0.1").unwrap();
    writeln!(text, "  Port 2222").unwrap();
    writeln!(text, "  User dev").unwrap();
    writeln!(text, "  IdentityFile {}", identity.display()).unwrap();
    writeln!(text, "  IdentitiesOnly yes").unwrap();
    writeln!(text, "  StrictHostKeyChecking {strict_host_key_checking}").unwrap();
    writeln!(text, "  UserKnownHostsFile {}", known_hosts.display()).unwrap();
    std::fs::write(&config, text).unwrap();
    config
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the Docker sshd fixture"]
async fn probe_classifies_authentication_failure() {
    let Some(fixture) = fixture_config() else {
        return;
    };
    let dir = tempfile::tempdir().expect("auth fixture");
    let known_hosts = fixture.with_file_name("known_hosts");
    let wrong_identity = dir.path().join("wrong-id");
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&wrong_identity)
        .status()
        .expect("ssh-keygen");
    assert!(status.success(), "generate wrong identity");
    let config = write_config(dir.path(), &wrong_identity, &known_hosts, "yes");
    let error = probe(&command_for(&config), 8765, MIN_FREE_KB).expect_err("wrong key must fail");
    assert_eq!(error.code(), "ssh_auth_failed", "{}", error.message());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the Docker sshd fixture"]
async fn probe_classifies_a_busy_remote_port() {
    let Some(fixture) = fixture_config() else {
        return;
    };
    let command = command_for(&fixture);
    let mut start = command.clone();
    start
        .remote_script("python3 -m http.server 8766 --bind 127.0.0.1 >/dev/null 2>&1 & echo $!")
        .unwrap();
    let output = start.run().expect("start port holder");
    let pid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert!(!pid.is_empty(), "port holder pid");
    let result = probe(&command, 8766, MIN_FREE_KB);
    let mut kill = command.clone();
    kill.remote_script("kill \"$1\"").unwrap();
    kill.args.push(pid.into());
    let _ = kill.run();
    let error = result.expect_err("busy port must fail");
    assert_eq!(
        error.code(),
        "ssh_environment_port_busy",
        "{}",
        error.message()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the Docker sshd fixture"]
async fn probe_classifies_a_changed_host_key() {
    let Some(_fixture) = fixture_config() else {
        return;
    };
    let identity =
        PathBuf::from(std::env::var("VIBEX_SSH_E2E_IDENTITY").expect("VIBEX_SSH_E2E_IDENTITY"));
    let public = std::fs::read_to_string(identity.with_extension("pub")).expect("identity public");
    let mut parts = public.split_whitespace();
    let key_type = parts.next().expect("key type");
    let key = parts.next().expect("key body");
    let dir = tempfile::tempdir().expect("host key fixture");
    let known_hosts = dir.path().join("known_hosts");
    std::fs::write(
        &known_hosts,
        format!("[127.0.0.1]:2222 {key_type} {key} wrong-host\n"),
    )
    .unwrap();
    let config = write_config(dir.path(), &identity, &known_hosts, "yes");
    let error = probe(&command_for(&config), 8765, MIN_FREE_KB).expect_err("host key must fail");
    assert_eq!(error.code(), "ssh_host_key_changed", "{}", error.message());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an SSH fixture and a server tarball"]
async fn uninstall_preserves_or_removes_the_runtime_home() {
    let Some(fixture) = fixture_config() else {
        return;
    };
    let artifact_path =
        PathBuf::from(std::env::var("VIBEX_SSH_E2E_ARTIFACT").expect("VIBEX_SSH_E2E_ARTIFACT"));
    let bytes = std::fs::read(&artifact_path).expect("read artifact");
    let arch = std::env::var("VIBEX_SSH_E2E_ARCH")
        .ok()
        .and_then(|value| match value.as_str() {
            "aarch64" | "arm64" => Some(RemoteArch::Aarch64),
            "x86_64" => Some(RemoteArch::X86_64),
            _ => None,
        })
        .unwrap_or(RemoteArch::X86_64);
    let base = std::env::var("VIBEX_SSH_E2E_UNINSTALL_HOME")
        .unwrap_or_else(|_| "/home/dev/uninstall-home".to_string());
    let root =
        std::env::var("VIBEX_SSH_E2E_UNINSTALL_ROOT").unwrap_or_else(|_| format!("{base}/server"));
    let port = std::env::var("VIBEX_SSH_E2E_UNINSTALL_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(8769);
    let command = command_for(&fixture);
    let launch =
        LaunchConfig::new(&base, &root, std::slice::from_ref(&base), port).expect("launch config");
    let mut prepare = command.clone();
    prepare.remote_script("mkdir -p \"$1\" \"$2\"").unwrap();
    prepare
        .args
        .extend([base.clone().into(), root.clone().into()]);
    prepare.run().expect("prepare uninstall dirs");
    let artifact = LocalArtifact {
        path: artifact_path,
        version: "0.1.0-rc.8".to_string(),
        arch,
        size: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
    };
    install_artifact(&command, &artifact, &root, |_, _| {}).expect("install runtime");
    supervisor::ensure_started(&command, &launch).expect("start runtime");
    let marker = format!("{root}/home/uninstall-marker");
    let mut touch = command.clone();
    touch.remote_script("printf marker > \"$1\"").unwrap();
    touch.args.push(marker.clone().into());
    touch.run().expect("write marker");

    let outcome = uninstall_runtime(&command, &launch, false).expect("uninstall preserving home");
    assert!(outcome.home_preserved);
    let mut check = command.clone();
    check
        .remote_script("test -f \"$1\" && test -d \"$2\"")
        .unwrap();
    check.args.extend([marker.into(), root.clone().into()]);
    check.run().expect("home should remain");

    uninstall_runtime(&command, &launch, true).expect("uninstall removing home");
    let mut gone = command.clone();
    gone.remote_script("test ! -e \"$1\"").unwrap();
    gone.args.push(root.into());
    gone.run().expect("install root should be removed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the Docker sshd fixture and a server tarball"]
async fn upgrade_preserves_the_home_and_rolls_back_a_bad_artifact() {
    let Some(fixture) = fixture_config() else {
        return;
    };
    let artifact_path =
        PathBuf::from(std::env::var("VIBEX_SSH_E2E_ARTIFACT").expect("VIBEX_SSH_E2E_ARTIFACT"));
    let old_bytes = std::fs::read(&artifact_path).expect("read old artifact");
    let old_sha = format!("{:x}", Sha256::digest(&old_bytes));
    let command = command_for(&fixture);
    let upgrade_home = std::env::var("VIBEX_SSH_E2E_UPGRADE_HOME")
        .unwrap_or_else(|_| "/home/dev/upgrade-home".to_string());
    let upgrade_root = std::env::var("VIBEX_SSH_E2E_UPGRADE_ROOT")
        .unwrap_or_else(|_| format!("{upgrade_home}/upgrade-server"));
    let upgrade_port = std::env::var("VIBEX_SSH_E2E_UPGRADE_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(8767);
    let arch = std::env::var("VIBEX_SSH_E2E_ARCH")
        .ok()
        .and_then(|value| match value.as_str() {
            "aarch64" | "arm64" => Some(RemoteArch::Aarch64),
            "x86_64" => Some(RemoteArch::X86_64),
            _ => None,
        })
        .unwrap_or(RemoteArch::X86_64);
    let old_version =
        std::env::var("VIBEX_SSH_E2E_OLD_VERSION").unwrap_or_else(|_| "0.1.0-rc.8".to_string());
    let launch = LaunchConfig::new(
        &upgrade_home,
        &upgrade_root,
        std::slice::from_ref(&upgrade_home),
        upgrade_port,
    )
    .expect("launch config");
    let mut prepare = command.clone();
    prepare.remote_script("mkdir -p \"$1\" \"$2\"").unwrap();
    prepare
        .args
        .extend([upgrade_home.clone().into(), upgrade_root.clone().into()]);
    prepare.run().expect("prepare upgrade dirs");

    let old7 = LocalArtifact {
        path: artifact_path.clone(),
        version: "0.1.0-rc.7".to_string(),
        arch,
        size: old_bytes.len() as u64,
        sha256: old_sha.clone(),
    };
    install_artifact(&command, &old7, &upgrade_root, |_, _| {}).expect("install rc.7 runtime");
    let old = LocalArtifact {
        path: artifact_path.clone(),
        version: old_version,
        arch,
        size: old_bytes.len() as u64,
        sha256: old_sha,
    };
    install_artifact(&command, &old, &upgrade_root, |_, _| {}).expect("install old runtime");
    supervisor::ensure_started(&command, &launch).expect("start old runtime");
    let identity_path = format!("{upgrade_root}/home/relay/desktop-identity.json");
    let identity_before = remote_file_sha(&command, &identity_path);

    let dir = tempfile::tempdir().expect("upgrade artifacts");
    let (new_path, real_new_binary) = match std::env::var_os("VIBEX_SSH_E2E_NEW_ARTIFACT") {
        Some(path) => (PathBuf::from(path), true),
        None => {
            let path = dir.path().join("new.tar.gz");
            std::fs::copy(&artifact_path, &path).expect("copy new artifact");
            (path, false)
        }
    };
    let new_bytes = std::fs::read(&new_path).expect("read new artifact");
    let new = LocalArtifact {
        path: new_path,
        version: "0.1.0-rc.9".to_string(),
        arch,
        size: new_bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(&new_bytes)),
    };
    let outcome = upgrade_runtime(&command, &launch, &new, SshRuntimeChannel::Rc, |_, _| {})
        .expect("upgrade to rc.9");
    assert!(outcome.cleaned_versions >= 1, "rc.7 should be cleaned");
    assert_eq!(
        remote_file_sha(&command, &identity_path),
        identity_before,
        "upgrade must preserve the runtime identity"
    );
    let mut check = command.clone();
    check
        .remote_script("test ! -d \"$1\"")
        .expect("cleanup check script");
    check.args.push(format!("{upgrade_root}/0.1.0-rc.7").into());
    check.run().expect("rc.7 directory should be removed");
    if real_new_binary {
        let status = supervisor::status(&command, &launch).expect("new binary status");
        assert_eq!(status.version, "0.1.0-rc.9");
    }

    let bad_binary = dir.path().join("vibex-server");
    std::fs::write(&bad_binary, b"#!/bin/sh\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&bad_binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bad_path = dir.path().join("bad.tar.gz");
    assert!(
        Command::new("tar")
            .args(["-czf", bad_path.to_str().unwrap(), "-C"])
            .arg(dir.path())
            .arg("vibex-server")
            .status()
            .unwrap()
            .success()
    );
    let bad_bytes = std::fs::read(&bad_path).unwrap();
    let bad = LocalArtifact {
        path: bad_path,
        version: "0.1.0-rc.10".to_string(),
        arch: RemoteArch::X86_64,
        size: bad_bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(&bad_bytes)),
    };
    let error = upgrade_runtime(&command, &launch, &bad, SshRuntimeChannel::Rc, |_, _| {})
        .expect_err("bad runtime must fail and roll back");
    assert!(
        error
            .rollback
            .as_deref()
            .is_some_and(|steps| steps.contains("old_started")),
        "rollback: {:?} ({})",
        error.rollback,
        error.message
    );
    let status = supervisor::status(&command, &launch).expect("status after rollback");
    assert!(
        status.is_healthy(),
        "runtime should be healthy after rollback"
    );
    supervisor::ensure_stopped(&command, &launch).expect("stop upgraded runtime");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the Docker sshd fixture and a server tarball"]
async fn provisioning_reaches_ready_over_sshd() {
    if std::env::var_os("VIBEX_SSH_E2E").is_none() {
        return;
    }
    let config_path =
        PathBuf::from(std::env::var("VIBEX_SSH_E2E_CONFIG").expect("VIBEX_SSH_E2E_CONFIG"));
    let artifact_path =
        PathBuf::from(std::env::var("VIBEX_SSH_E2E_ARTIFACT").expect("VIBEX_SSH_E2E_ARTIFACT"));
    let bytes = std::fs::read(&artifact_path).expect("read server artifact");
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let destination = fixture_destination();
    let command = command_for(&config_path);
    let remote_home =
        std::env::var("VIBEX_SSH_E2E_REMOTE_HOME").unwrap_or_else(|_| "/home/dev".to_string());
    let remote_root = std::env::var("VIBEX_SSH_E2E_REMOTE_ROOT")
        .unwrap_or_else(|_| format!("{remote_home}/.local/share/vibex/server"));
    let arch = std::env::var("VIBEX_SSH_E2E_ARCH")
        .ok()
        .and_then(|value| match value.as_str() {
            "aarch64" | "arm64" => Some(RemoteArch::Aarch64),
            "x86_64" => Some(RemoteArch::X86_64),
            _ => None,
        })
        .unwrap_or(RemoteArch::X86_64);
    let version = std::env::var("VIBEX_SSH_E2E_ARTIFACT_VERSION")
        .unwrap_or_else(|_| "0.1.0-rc.8".to_string());
    let mut launch = LaunchConfig::new(
        &remote_home,
        &remote_root,
        std::slice::from_ref(&remote_home),
        8765,
    )
    .expect("launch config");
    let launchd_lifecycle = std::env::var("VIBEX_SSH_E2E_LIFECYCLE").as_deref() == Ok("launchd");
    if launchd_lifecycle {
        launch = launch.with_launch_agent(launchd::label_for_profile("e2e-profile"));
    }
    let local_port = select_local_port(0, 1).expect("local tunnel port");
    let desktop_home = tempfile::tempdir().expect("desktop home");
    let artifact = LocalArtifact {
        path: artifact_path,
        version,
        arch,
        size: bytes.len() as u64,
        sha256,
    };
    let now = now_ms();
    let mut effects = SshProvisioningEffects::new(
        command.clone(),
        destination,
        Some(config_path),
        launch.clone(),
        artifact,
        desktop_home.path().to_path_buf(),
        local_port,
        now,
    )
    .expect("effects");
    let mut coordinator = ProvisioningCoordinator::default();
    let cancellation = RunCancellation::new();
    let outcome = run_provisioning(
        &mut coordinator,
        "e2e-profile",
        now,
        &mut effects,
        &cancellation,
        false,
    )
    .await
    .expect("provisioning should run");
    assert!(
        matches!(outcome, ProvisioningRunOutcome::Ready(_)),
        "provisioning outcome: {outcome:?}"
    );
    assert!(effects.backend().is_some(), "backend should be connected");
    assert!(
        effects.runtime_id().is_some(),
        "runtime should be registered"
    );
    eprintln!("provisioning ready: {outcome:?}");
    eprintln!(
        "runtime id={:?} status={:?}",
        effects.runtime_id(),
        effects.runtime_status()
    );
    // The pairing code is fetched over SSH and claimed directly; it must never
    // land in the server log, which the desktop shows through View log.
    let mut read_log = command.clone();
    read_log.remote_script("cat \"$1\"").unwrap();
    read_log.args.push(launch.log_file.clone().into());
    let log = read_log.run().expect("read server log").stdout_lossy();
    assert!(log.contains("runtime=ready"), "server log: {log}");
    assert!(!log.contains("pairing_code="), "server log: {log}");
    assert!(!log.contains("vibex://pair"), "server log: {log}");
    let stop = supervisor::ensure_stopped(&command, &launch);
    let _ = effects.take_tunnel_process();
    stop.expect("runtime should stop");
    if launchd_lifecycle {
        launchd::remove(&command, &launch, "dev.vibex.server.e2e-profile")
            .expect("unload and remove LaunchAgent");
    }
}

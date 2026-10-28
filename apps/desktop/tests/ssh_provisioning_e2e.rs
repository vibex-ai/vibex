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
//! ```

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest as _, Sha256};
use vibex_desktop::ssh_connection::{
    LaunchConfig, LocalArtifact, MIN_FREE_KB, ProvisioningCoordinator, ProvisioningRunOutcome,
    RemoteArch, RunCancellation, SshCommand, SshDestination, SshProvisioningEffects, probe,
    run_provisioning, select_local_port, supervisor,
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

fn command_for(config: &Path) -> SshCommand {
    SshCommand::for_destination(
        "ssh",
        &SshDestination {
            host: "vibex-e2e".to_string(),
            user: None,
            port: 2222,
        },
        Some(config),
    )
    .expect("build SSH command")
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
#[ignore = "requires the Docker sshd fixture and a server tarball"]
async fn provisioning_reaches_ready_over_docker_sshd() {
    if std::env::var_os("VIBEX_SSH_E2E").is_none() {
        return;
    }
    let config_path =
        PathBuf::from(std::env::var("VIBEX_SSH_E2E_CONFIG").expect("VIBEX_SSH_E2E_CONFIG"));
    let artifact_path =
        PathBuf::from(std::env::var("VIBEX_SSH_E2E_ARTIFACT").expect("VIBEX_SSH_E2E_ARTIFACT"));
    let bytes = std::fs::read(&artifact_path).expect("read server artifact");
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let destination = SshDestination {
        host: "vibex-e2e".to_string(),
        user: None,
        port: 2222,
    };
    let command = SshCommand::for_destination("ssh", &destination, Some(&config_path))
        .expect("build SSH command");
    let launch = LaunchConfig::new(
        "/home/dev",
        "/home/dev/.local/share/vibex/server",
        &["/home/dev".to_string()],
        8765,
    )
    .expect("launch config");
    let local_port = select_local_port(0, 1).expect("local tunnel port");
    let desktop_home = tempfile::tempdir().expect("desktop home");
    let artifact = LocalArtifact {
        path: artifact_path,
        version: "0.1.0-rc.8".to_string(),
        arch: RemoteArch::X86_64,
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
    let stop = supervisor::ensure_stopped(&command, &launch);
    let _ = effects.take_tunnel_process();
    stop.expect("runtime should stop");
}

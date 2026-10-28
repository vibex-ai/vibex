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

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest as _, Sha256};
use vibex_desktop::ssh_connection::{
    LaunchConfig, LocalArtifact, ProvisioningCoordinator, ProvisioningRunOutcome, RemoteArch,
    RunCancellation, SshCommand, SshDestination, SshProvisioningEffects, run_provisioning,
    select_local_port, supervisor,
};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
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

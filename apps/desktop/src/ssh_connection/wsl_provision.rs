//! WSL2 provisioning orchestration.
//!
//! WSL has no SSH hop and no tunnel: the Linux runtime is installed inside the
//! distro and reached through WSL2 localhost forwarding. This module reuses the
//! same fixed install/launch/status/pairing scripts as the SSH path, executed by
//! `WslCommand`.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::artifact::LocalArtifact;
use super::environment::{MIN_FREE_KB, RemoteEnvironment};
use super::install::INSTALL_SCRIPT;
use super::launch::{ENV_SCRIPT, LaunchConfig, START_SCRIPT};
use super::pairing::{
    PAIRING_CODE_SCRIPT, PairingOutcome, PairingPayload, claim_payload_and_register,
    parse_pairing_payload,
};
use super::supervisor::{STATUS_SCRIPT, STOP_SCRIPT, ServerStatus, parse_status};
use super::wsl::{WslCommand, WslError, WslKeepalive, localhost_forwarded, probe_environment};
use crate::remote_client::{DesktopRemoteCredential, DesktopRuntimeRegistryStore};
use vibex_remote_client::WebRemoteBackend;

const START_TIMEOUT: Duration = Duration::from_secs(20);
const FORWARDING_DEADLINE: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslProvisionError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl WslProvisionError {
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

    fn from_wsl(error: WslError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            retryable: error.retryable,
        }
    }
}

impl fmt::Display for WslProvisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WslProvisionError {}

#[derive(Debug, Clone)]
pub struct WslProvisioner {
    program: PathBuf,
    distro: String,
    launch: LaunchConfig,
    desktop_home: PathBuf,
}

pub struct WslProvisionOutcome {
    pub pairing: PairingOutcome,
    pub backend: Arc<WebRemoteBackend>,
    pub environment: RemoteEnvironment,
    /// Keeps the distro alive after the provisioning `wsl.exe` commands exit.
    pub keepalive: WslKeepalive,
}

impl WslProvisioner {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        program: impl Into<PathBuf>,
        distro: impl Into<String>,
        remote_home: &str,
        install_root: &str,
        workspace_roots: &[String],
        remote_port: u16,
        desktop_home: impl Into<PathBuf>,
    ) -> Result<Self, WslProvisionError> {
        let launch = LaunchConfig::new(remote_home, install_root, workspace_roots, remote_port)
            .map_err(|error| WslProvisionError::new("wsl_launch_invalid", error.message))?;
        Ok(Self {
            program: program.into(),
            distro: distro.into(),
            launch,
            desktop_home: desktop_home.into(),
        })
    }

    pub fn launch(&self) -> &LaunchConfig {
        &self.launch
    }

    fn command(&self) -> WslCommand {
        WslCommand::new(&self.program, &self.distro)
    }

    pub fn probe(&self) -> Result<RemoteEnvironment, WslProvisionError> {
        probe_environment(&self.program, &self.distro, self.launch.remote_port)
            .map_err(WslProvisionError::from_wsl)
    }

    /// Uploads the server tarball and runs the shared installer.
    pub fn install(&self, artifact: &LocalArtifact) -> Result<(), WslProvisionError> {
        let bytes = std::fs::read(&artifact.path).map_err(|_| {
            WslProvisionError::new("wsl_artifact_read", "the artifact could not be read")
        })?;
        if bytes.len() as u64 != artifact.size {
            return Err(WslProvisionError::new(
                "wsl_artifact_size",
                "the artifact size changed before upload",
            ));
        }
        let incoming = format!(
            "{}/.incoming-{}.tar.gz",
            self.launch.remote_install_root, artifact.version
        );
        let mut upload = self.command();
        upload
            .upload(&incoming, &bytes)
            .map_err(WslProvisionError::from_wsl)?;
        let mut install = self.command();
        install
            .run_script(
                INSTALL_SCRIPT,
                &[
                    incoming,
                    self.launch.remote_install_root.clone(),
                    artifact.version.clone(),
                    artifact.sha256.clone(),
                ],
            )
            .map_err(WslProvisionError::from_wsl)?;
        Ok(())
    }

    pub fn write_env(&self) -> Result<(), WslProvisionError> {
        let mut args = vec![self.launch.env_file.clone()];
        args.extend(
            self.launch
                .env_pairs()
                .into_iter()
                .map(|(key, value)| format!("{key}={value}")),
        );
        self.command()
            .run_script(ENV_SCRIPT, &args)
            .map_err(WslProvisionError::from_wsl)?;
        Ok(())
    }

    pub fn status(&self) -> Result<ServerStatus, WslProvisionError> {
        let mut command = self.command();
        let output = command
            .run_script(
                STATUS_SCRIPT,
                &[self.launch.env_file.clone(), self.launch.binary.clone()],
            )
            .map_err(WslProvisionError::from_wsl)?;
        parse_status(&output.stdout_lossy())
            .map_err(|error| WslProvisionError::new("wsl_status_invalid", error.message))
    }

    pub fn stop(&self) -> Result<(), WslProvisionError> {
        self.command()
            .run_script(
                STOP_SCRIPT,
                &[self.launch.env_file.clone(), self.launch.binary.clone()],
            )
            .map_err(WslProvisionError::from_wsl)?;
        Ok(())
    }

    pub fn ensure_started(&self) -> Result<ServerStatus, WslProvisionError> {
        if let Ok(current) = self.status()
            && current.is_healthy()
        {
            return Ok(current);
        }
        self.write_env()?;
        self.command()
            .run_script(
                START_SCRIPT,
                &[
                    self.launch.env_file.clone(),
                    self.launch.binary.clone(),
                    self.launch.log_file.clone(),
                    self.launch.pid_file.clone(),
                ],
            )
            .map_err(WslProvisionError::from_wsl)?;
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            if let Ok(current) = self.status()
                && current.is_healthy()
            {
                return Ok(current);
            }
            if Instant::now() >= deadline {
                return Err(WslProvisionError::retryable(
                    "wsl_health_timeout",
                    "the WSL runtime did not become healthy in time",
                ));
            }
            std::thread::sleep(Duration::from_millis(400));
        }
    }

    pub async fn pair(&self, now_ms: i64) -> Result<PairingOutcome, WslProvisionError> {
        let mut command = self.command();
        let output = command
            .run_script(
                PAIRING_CODE_SCRIPT,
                &[self.launch.env_file.clone(), self.launch.binary.clone()],
            )
            .map_err(WslProvisionError::from_wsl)?;
        let payload: PairingPayload = parse_pairing_payload(&output.stdout_lossy(), now_ms)
            .map_err(|error| WslProvisionError::new("wsl_pairing_invalid", error.message))?;
        claim_payload_and_register(
            payload,
            &self.desktop_home,
            self.launch.remote_port,
            false,
            now_ms,
        )
        .await
        .map_err(|error| WslProvisionError {
            code: "wsl_pairing_failed",
            message: error.message,
            retryable: error.retryable,
        })
    }

    pub async fn provision(
        &self,
        artifact: &LocalArtifact,
        now_ms: i64,
    ) -> Result<WslProvisionOutcome, WslProvisionError> {
        let environment = self.probe()?;
        if environment.free_kb < MIN_FREE_KB {
            return Err(WslProvisionError::new(
                "wsl_disk_low",
                "the WSL distro does not have enough free disk",
            ));
        }
        self.install(artifact)?;
        self.write_env()?;
        let keepalive = WslKeepalive::spawn_runtime(
            &self.program,
            &self.distro,
            &self.launch.env_file,
            &self.launch.binary,
        )
        .map_err(WslProvisionError::from_wsl)?;
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            if let Ok(status) = self.status()
                && status.is_healthy()
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err(WslProvisionError::retryable(
                    "wsl_health_timeout",
                    "the WSL runtime did not become healthy in time",
                ));
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        let deadline = Instant::now() + FORWARDING_DEADLINE;
        while !localhost_forwarded(self.launch.remote_port) {
            if Instant::now() >= deadline {
                return Err(WslProvisionError::retryable(
                    "wsl_forwarding_unavailable",
                    "WSL2 localhost forwarding is unavailable; check .wslconfig localhostForwarding",
                ));
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        let pairing = self.pair(now_ms).await?;
        let backend = pairing
            .credential
            .backend()
            .map_err(|error| WslProvisionError::new("wsl_backend_failed", error.message))?;
        backend
            .connect()
            .await
            .map_err(|error| WslProvisionError::new("wsl_connect_failed", error.message))?;
        let store = DesktopRuntimeRegistryStore::new(&self.desktop_home);
        let mut registry = store.load_or_migrate();
        registry.set_active(&pairing.runtime_id);
        store
            .save(&registry)
            .map_err(|error| WslProvisionError::new("wsl_registry_failed", error.message))?;
        Ok(WslProvisionOutcome {
            pairing,
            backend,
            environment,
            keepalive,
        })
    }
}

/// Accessor used by callers that only need the pinned credential.
pub fn credential_backend(
    credential: &DesktopRemoteCredential,
) -> Result<Arc<WebRemoteBackend>, WslProvisionError> {
    credential
        .backend()
        .map_err(|error| WslProvisionError::new("wsl_backend_failed", error.message))
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::path::{Path, PathBuf};

    use sha2::{Digest as _, Sha256};

    use super::*;
    use crate::ssh_connection::environment::RemoteArch;

    #[test]
    #[ignore = "requires Windows WSL and VIBEX_WSL_E2E_ARTIFACT"]
    fn real_wsl_install_and_start() {
        let Ok(artifact_path) = std::env::var("VIBEX_WSL_E2E_ARTIFACT") else {
            return;
        };
        let distro =
            std::env::var("VIBEX_WSL_E2E_DISTRO").unwrap_or_else(|_| "archlinux".to_string());
        let home =
            std::env::var("VIBEX_WSL_E2E_HOME").unwrap_or_else(|_| "/home/peatboy".to_string());
        let root = format!("{home}/.local/share/vibex/wsl-e2e-server");
        let bytes = std::fs::read(&artifact_path).unwrap();
        let artifact = LocalArtifact {
            path: PathBuf::from(artifact_path),
            version: "0.1.0-rc.8".to_string(),
            arch: RemoteArch::X86_64,
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        let provisioner = WslProvisioner::new(
            "wsl.exe",
            &distro,
            &home,
            &root,
            &[home.clone()],
            8770,
            std::env::temp_dir(),
        )
        .unwrap();
        provisioner.install(&artifact).unwrap();
        let _keepalive = WslKeepalive::spawn(Path::new("wsl.exe"), &distro).unwrap();
        let status = provisioner.ensure_started().unwrap();
        assert!(status.is_healthy());
        provisioner.stop().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Windows WSL and VIBEX_WSL_E2E_ARTIFACT"]
    async fn real_wsl_full_provisioning_reaches_pairing_and_connect() {
        let Ok(artifact_path) = std::env::var("VIBEX_WSL_E2E_ARTIFACT") else {
            return;
        };
        let distro =
            std::env::var("VIBEX_WSL_E2E_DISTRO").unwrap_or_else(|_| "archlinux".to_string());
        let home =
            std::env::var("VIBEX_WSL_E2E_HOME").unwrap_or_else(|_| "/home/peatboy".to_string());
        let desktop = tempfile::tempdir().unwrap();
        let bytes = std::fs::read(&artifact_path).unwrap();
        let artifact = LocalArtifact {
            path: PathBuf::from(artifact_path),
            version: "0.1.0-rc.8".to_string(),
            arch: RemoteArch::X86_64,
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        let provisioner = WslProvisioner::new(
            "wsl.exe",
            &distro,
            &home,
            &format!("{home}/.local/share/vibex/wsl-e2e-full"),
            &[home.clone()],
            8771,
            desktop.path(),
        )
        .unwrap();
        let outcome = provisioner
            .provision(&artifact, vibex_core::unix_timestamp_ms())
            .await
            .unwrap();
        assert!(!outcome.pairing.runtime_id.is_empty());
        provisioner.stop().unwrap();
        drop(outcome.keepalive);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::process::Command;

    use sha2::{Digest as _, Sha256};

    use super::*;
    use crate::ssh_connection::environment::RemoteArch;
    use crate::ssh_connection::test_support;

    fn fake_wsl(dir: &std::path::Path, body: &str) -> PathBuf {
        let path = test_support::unique_path(dir, "wsl");
        test_support::write_executable(&path, &format!("#!/bin/sh\n{body}\n"));
        path
    }

    fn provisioner() -> (tempfile::TempDir, WslProvisioner) {
        let dir = tempfile::tempdir().unwrap();
        let wsl = fake_wsl(
            dir.path(),
            "case \"$*\" in
  *\" -c \"*) cat >/dev/null; exit 0 ;;
esac
script=$(cat)
case \"$script\" in
  *'install=ok'*) echo install=ok ;;
  *'env=ok'*) echo env=ok ;;
  *) echo ok ;;
esac",
        );
        let root = dir.path().join("server").display().to_string();
        let provisioner = WslProvisioner::new(
            wsl,
            "Ubuntu",
            &dir.path().display().to_string(),
            &root,
            &[dir.path().display().to_string()],
            8765,
            dir.path(),
        )
        .unwrap();
        (dir, provisioner)
    }

    #[test]
    fn install_uploads_then_runs_the_shared_installer() {
        let (dir, provisioner) = provisioner();
        std::fs::write(dir.path().join("payload"), b"server").unwrap();
        let artifact_path = dir.path().join("server.tar.gz");
        assert!(
            Command::new("tar")
                .args(["-czf", artifact_path.to_str().unwrap(), "-C"])
                .arg(dir.path())
                .arg("payload")
                .status()
                .unwrap()
                .success()
        );
        // The installer script expects a top-level vibex-server; the fake does
        // not inspect the archive, only that both stages were invoked.
        let bytes = std::fs::read(&artifact_path).unwrap();
        let artifact = LocalArtifact {
            path: artifact_path,
            version: "0.1.0-rc.8".to_string(),
            arch: RemoteArch::X86_64,
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        provisioner.install(&artifact).unwrap();
    }

    #[test]
    fn ensure_started_writes_env_and_waits_for_health() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::write(&state, b"0").unwrap();
        let wsl = fake_wsl(
            dir.path(),
            &format!(
                "script=$(cat)
case \"$script\" in
  *'launch=started'*) echo 1 > \"{state}\"; echo 'launch=started pid=1' ;;
  *'status --json'*)
    if [ \"$(cat \"{state}\")\" = 1 ]; then
      echo '{{\"schemaVersion\":\"vibex-server-status.v1\",\"running\":true,\"pid\":1,\"port\":8765,\"version\":\"0.1.0-rc.8\",\"home\":\"/home/dev\",\"health\":\"ok\"}}'
    else
      echo '{{\"schemaVersion\":\"vibex-server-status.v1\",\"running\":false,\"pid\":null,\"port\":8765,\"version\":\"0.1.0-rc.8\",\"home\":\"/home/dev\",\"health\":\"stopped\"}}'
    fi ;;
  *'env=ok'*) echo env=ok ;;
  *) echo ok ;;
esac",
                state = state.display()
            ),
        );
        let provisioner = WslProvisioner::new(
            wsl,
            "Ubuntu",
            "/home/dev",
            "/home/dev/.local/share/vibex/server",
            &["/home/dev".to_string()],
            8765,
            dir.path(),
        )
        .unwrap();
        let status = provisioner.ensure_started().unwrap();
        assert!(status.is_healthy());
        assert_eq!(std::fs::read_to_string(&state).unwrap().trim(), "1");
    }
}

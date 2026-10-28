//! Real effects for the provisioning runner.
//!
//! The adapter owns one profile's SSH command, launch config, downloaded
//! artifact and tunnel. It intentionally stops at a connected backend: the
//! workbench installs that backend into its own authority slots after Ready.

use std::path::PathBuf;
use std::sync::Arc;

use vibex_backend::BackendError;
use vibex_remote_client::WebRemoteBackend;

use super::artifact::LocalArtifact;
use super::command::{SshCommand, SshDestination};
use super::environment::{self, MIN_FREE_KB, RemoteEnvironment};
use super::install::{self, InstallError, InstallOutcome};
use super::launch::LaunchConfig;
use super::pairing::{self, PairingError, PairingOutcome};
use super::provisioning_run::{ProvisioningEffectError, ProvisioningEffects};
use super::supervisor::{self, ServerStatus, SupervisorError};
use super::tunnel::{TunnelError, TunnelProcess, TunnelSpec};
use crate::remote_client::{DesktopRemoteCredential, DesktopRuntimeRegistryStore};

pub struct SshProvisioningEffects {
    command: SshCommand,
    launch: LaunchConfig,
    artifact: LocalArtifact,
    desktop_home: PathBuf,
    tunnel_spec: TunnelSpec,
    now_ms: i64,
    environment: Option<RemoteEnvironment>,
    installed: Option<InstallOutcome>,
    runtime_status: Option<ServerStatus>,
    tunnel: Option<TunnelProcess>,
    pairing: Option<PairingOutcome>,
    backend: Option<Arc<WebRemoteBackend>>,
    runtime_id: Option<String>,
}

impl SshProvisioningEffects {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        command: SshCommand,
        destination: SshDestination,
        config_path: Option<PathBuf>,
        launch: LaunchConfig,
        artifact: LocalArtifact,
        desktop_home: PathBuf,
        local_port: u16,
        now_ms: i64,
    ) -> Result<Self, ProvisioningEffectError> {
        if local_port == 0 {
            return Err(ProvisioningEffectError::new(
                "ssh_effects_local_port_invalid",
                "the tunnel port must not be zero",
            ));
        }
        let tunnel_spec = TunnelSpec::new(
            command.program.clone(),
            destination,
            config_path,
            local_port,
            launch.remote_port,
        )
        .map_err(map_tunnel)?;
        Ok(Self {
            command,
            launch,
            artifact,
            desktop_home,
            tunnel_spec,
            now_ms,
            environment: None,
            installed: None,
            runtime_status: None,
            tunnel: None,
            pairing: None,
            backend: None,
            runtime_id: None,
        })
    }

    pub fn environment(&self) -> Option<&RemoteEnvironment> {
        self.environment.as_ref()
    }

    pub fn installed(&self) -> Option<&InstallOutcome> {
        self.installed.as_ref()
    }

    pub fn runtime_status(&self) -> Option<&ServerStatus> {
        self.runtime_status.as_ref()
    }

    pub fn pairing(&self) -> Option<&PairingOutcome> {
        self.pairing.as_ref()
    }

    pub fn backend(&self) -> Option<Arc<WebRemoteBackend>> {
        self.backend.clone()
    }

    pub fn credential(&self) -> Option<&DesktopRemoteCredential> {
        self.pairing.as_ref().map(|pairing| &pairing.credential)
    }

    pub fn runtime_id(&self) -> Option<&str> {
        self.runtime_id.as_deref()
    }

    pub fn tunnel_process(&mut self) -> Option<&mut TunnelProcess> {
        self.tunnel.as_mut()
    }

    pub fn take_tunnel_process(&mut self) -> Option<TunnelProcess> {
        self.tunnel.take()
    }
}

#[allow(async_fn_in_trait)]
impl ProvisioningEffects for SshProvisioningEffects {
    async fn resolve(&mut self) -> Result<(), ProvisioningEffectError> {
        // The profile is already resolved into SshCommand/LaunchConfig by the UI.
        Ok(())
    }

    async fn check_environment(&mut self) -> Result<(), ProvisioningEffectError> {
        let environment = environment::probe(&self.command, self.launch.remote_port, MIN_FREE_KB)
            .map_err(map_environment)?;
        self.environment = Some(environment);
        Ok(())
    }

    async fn upload(&mut self) -> Result<(), ProvisioningEffectError> {
        // The UI downloads the signed artifact before starting the run; the
        // install effect uploads and verifies it over SSH.
        Ok(())
    }

    async fn install(&mut self) -> Result<(), ProvisioningEffectError> {
        let outcome = install::install_artifact(
            &self.command,
            &self.artifact,
            &self.launch.remote_install_root,
            |_, _| {},
        )
        .map_err(map_install)?;
        self.installed = Some(outcome);
        Ok(())
    }

    async fn start(&mut self) -> Result<(), ProvisioningEffectError> {
        let status =
            supervisor::ensure_started(&self.command, &self.launch).map_err(map_supervisor)?;
        self.runtime_status = Some(status);
        Ok(())
    }

    async fn open_tunnel(&mut self) -> Result<(), ProvisioningEffectError> {
        let process = TunnelProcess::spawn(&self.tunnel_spec).map_err(map_tunnel)?;
        self.tunnel = Some(process);
        Ok(())
    }

    async fn pair(&mut self) -> Result<(), ProvisioningEffectError> {
        let outcome = pairing::claim_and_register(
            &self.command,
            &self.launch,
            &self.desktop_home,
            self.tunnel_spec.local_port,
            false,
            self.now_ms,
        )
        .await
        .map_err(map_pairing)?;
        self.pairing = Some(outcome);
        Ok(())
    }

    async fn connect(&mut self) -> Result<(), ProvisioningEffectError> {
        let pairing = self.pairing.as_ref().ok_or_else(|| {
            ProvisioningEffectError::new(
                "ssh_effects_pairing_missing",
                "the runtime was not paired before connecting",
            )
        })?;
        let backend = pairing.credential.backend().map_err(map_backend)?;
        backend.connect().await.map_err(map_backend)?;
        self.backend = Some(backend);
        Ok(())
    }

    async fn activate(&mut self) -> Result<String, ProvisioningEffectError> {
        let runtime_id = self
            .pairing
            .as_ref()
            .ok_or_else(|| {
                ProvisioningEffectError::new(
                    "ssh_effects_pairing_missing",
                    "the runtime was not paired before activation",
                )
            })?
            .runtime_id
            .clone();
        if self.backend.is_none() {
            return Err(ProvisioningEffectError::new(
                "ssh_effects_connect_missing",
                "the runtime was not connected before activation",
            ));
        }
        let store = DesktopRuntimeRegistryStore::new(&self.desktop_home);
        let mut registry = store.load_or_migrate();
        registry.set_active(&runtime_id);
        store.save(&registry).map_err(map_backend)?;
        self.runtime_id = Some(runtime_id.clone());
        Ok(runtime_id)
    }
}

fn map_environment(error: environment::EnvironmentError) -> ProvisioningEffectError {
    ProvisioningEffectError::new(error.code(), error.message()).retryable(error.retryable())
}

fn map_install(error: InstallError) -> ProvisioningEffectError {
    ProvisioningEffectError::new(error.code, error.message).retryable(error.retryable)
}

fn map_supervisor(error: SupervisorError) -> ProvisioningEffectError {
    ProvisioningEffectError::new(error.code, error.message).retryable(error.retryable)
}

fn map_tunnel(error: TunnelError) -> ProvisioningEffectError {
    ProvisioningEffectError::new(error.code, error.message).retryable(error.retryable)
}

fn map_pairing(error: PairingError) -> ProvisioningEffectError {
    ProvisioningEffectError::new(error.code, error.message).retryable(error.retryable)
}

fn map_backend(error: BackendError) -> ProvisioningEffectError {
    let retryable = matches!(
        error.kind,
        vibex_backend::BackendErrorKind::Offline | vibex_backend::BackendErrorKind::Failed
    );
    ProvisioningEffectError::new(error.code, error.message).retryable(retryable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh_connection::environment::RemoteArch;
    use std::fs;

    fn launch() -> LaunchConfig {
        LaunchConfig::new(
            "/home/dev",
            "/home/dev/.local/share/vibex/server",
            &["/home/dev".to_string()],
            8765,
        )
        .unwrap()
    }

    fn destination() -> SshDestination {
        SshDestination {
            host: "example.com".to_string(),
            user: Some("dev".to_string()),
            port: 22,
        }
    }

    fn artifact(dir: &std::path::Path) -> LocalArtifact {
        let path = dir.join("artifact.tar.gz");
        fs::write(&path, b"artifact").unwrap();
        LocalArtifact {
            path,
            version: "0.1.0-rc.8".to_string(),
            arch: RemoteArch::X86_64,
            size: 8,
            sha256: "a".repeat(64),
        }
    }

    #[cfg(unix)]
    fn command(dir: &std::path::Path, body: &str) -> SshCommand {
        let path = crate::ssh_connection::test_support::unique_path(dir, "fake-ssh");
        crate::ssh_connection::test_support::write_executable(
            &path,
            &format!("#!/bin/sh\n{body}\n"),
        );
        SshCommand {
            program: path,
            args: Vec::new(),
            stdin: None,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checks_environment_through_the_real_probe() {
        let dir = tempfile::tempdir().unwrap();
        let command = command(
            dir.path(),
            "cat >/dev/null\ncat <<'OUT'\nprobe=ok\nos=Linux\narch=x86_64\nhome=/home/dev\nshell=/bin/bash\ngit=yes\nsha256sum=yes\nfree_kb=1048576\nglibc=2.36\nhome_writable=yes\nport=free\nruntime=absent\nOUT",
        );
        let mut effects = SshProvisioningEffects::new(
            command,
            destination(),
            None,
            launch(),
            artifact(dir.path()),
            dir.path().to_path_buf(),
            18765,
            0,
        )
        .unwrap();
        effects.check_environment().await.unwrap();
        assert_eq!(effects.environment().unwrap().arch, RemoteArch::X86_64);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn open_tunnel_owns_a_child_and_activate_requires_pairing() {
        let dir = tempfile::tempdir().unwrap();
        let command = command(dir.path(), "exec sleep 30");
        let mut effects = SshProvisioningEffects::new(
            command,
            destination(),
            None,
            launch(),
            artifact(dir.path()),
            dir.path().to_path_buf(),
            18765,
            0,
        )
        .unwrap();
        effects.open_tunnel().await.unwrap();
        assert!(effects.tunnel_process().is_some());
        assert_eq!(
            effects.activate().await.unwrap_err().code,
            "ssh_effects_pairing_missing"
        );
    }

    #[test]
    fn effects_can_move_to_the_provisioning_worker_thread() {
        fn assert_send<T: Send>() {}
        assert_send::<SshProvisioningEffects>();
    }

    #[test]
    fn rejects_zero_local_port() {
        let dir = tempfile::tempdir().unwrap();
        let error = match SshProvisioningEffects::new(
            SshCommand::new("ssh"),
            destination(),
            None,
            launch(),
            artifact(dir.path()),
            dir.path().to_path_buf(),
            0,
            0,
        ) {
            Ok(_) => panic!("zero local port should be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.code, "ssh_effects_local_port_invalid");
    }
}

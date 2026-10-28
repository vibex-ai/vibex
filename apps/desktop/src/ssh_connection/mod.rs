//! SSH connection profiles and their on-disk store.
//!
//! The profile is deliberately local to the desktop: it names the host, the
//! user, and the ports, but never carries a private key, password, pairing
//! code, or auth token.

pub mod artifact;
pub mod command;
pub mod config;
pub mod environment;
pub mod error;
pub mod host_key;
pub mod install;
pub mod launch;
pub mod pairing;
pub mod profile;
pub mod provisioning;
pub mod supervisor;
#[cfg(test)]
mod test_support;
pub mod tunnel;

pub use artifact::{
    ArtifactError, LocalArtifact, download_server_artifact, select_server_artifact, verify_file,
};
pub use command::{SshCommand, SshDestination, SshOutput};
pub use config::{
    SshDiscovery, SshEffectiveConfig, SshHostEntry, default_ssh_config_path,
    discover_default_hosts, discover_hosts, resolve_effective, resolve_effective_default,
};
pub use environment::{
    EnvironmentError, EnvironmentWarning, GlibcVersion, MIN_FREE_KB, MIN_GLIBC, PortState,
    RemoteArch, RemoteEnvironment, parse_probe_output, probe, probe_script,
};
pub use error::{SshFailure, SshFailureKind};
pub use host_key::{default_known_hosts_path, known_hosts_contains};
pub use install::{INSTALL_SCRIPT, InstallError, InstallOutcome, install_artifact};
pub use launch::{LaunchConfig, LaunchError, LaunchState, StartOutcome, start, write_env};
pub use pairing::{
    PAIRING_CODE_SCRIPT, PAIRING_SCHEMA_VERSION, PairingError, PairingOutcome, PairingPayload,
    claim_and_register, fetch_pairing_code, parse_pairing_payload,
};
pub use profile::{
    SCHEMA_VERSION, STORE_FILE_NAME, SshConnectionProfile, SshProfileStore, SshRuntimeChannel,
};
pub use provisioning::{
    ProvisioningBegin, ProvisioningControlError, ProvisioningCoordinator, ProvisioningEvent,
    ProvisioningFailure, ProvisioningMachine, ProvisioningProgress, ProvisioningSnapshot,
    ProvisioningStatus, ProvisioningStep, ProvisioningUpdate,
};
pub use supervisor::{
    PORT_SCRIPT, ServerStatus, StopOutcome, SupervisorError, ensure_started, ensure_stopped,
    parse_status, select_free_port, status, stop,
};
pub use tunnel::{
    TunnelBackoff, TunnelError, TunnelProcess, TunnelSpec, TunnelState, TunnelSupervisor,
    select_local_port,
};

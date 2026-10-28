//! SSH connection profiles and their on-disk store.
//!
//! The profile is deliberately local to the desktop: it names the host, the
//! user, and the ports, but never carries a private key, password, pairing
//! code, or auth token.

pub mod command;
pub mod config;
pub mod environment;
pub mod error;
pub mod host_key;
pub mod profile;
pub mod provisioning;

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
pub use profile::{
    SCHEMA_VERSION, STORE_FILE_NAME, SshConnectionProfile, SshProfileStore, SshRuntimeChannel,
};
pub use provisioning::{
    ProvisioningBegin, ProvisioningControlError, ProvisioningCoordinator, ProvisioningEvent,
    ProvisioningFailure, ProvisioningMachine, ProvisioningProgress, ProvisioningSnapshot,
    ProvisioningStatus, ProvisioningStep, ProvisioningUpdate,
};

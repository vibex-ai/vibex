//! SSH connection profiles and their on-disk store.
//!
//! The profile is deliberately local to the desktop: it names the host, the
//! user, and the ports, but never carries a private key, password, pairing
//! code, or auth token.

pub mod command;
pub mod config;
pub mod error;
pub mod host_key;
pub mod profile;

pub use command::{SshCommand, SshDestination, SshOutput};
pub use config::{
    SshDiscovery, SshEffectiveConfig, SshHostEntry, default_ssh_config_path,
    discover_default_hosts, discover_hosts, resolve_effective, resolve_effective_default,
};
pub use error::{SshFailure, SshFailureKind};
pub use host_key::{default_known_hosts_path, known_hosts_contains};
pub use profile::{
    SCHEMA_VERSION, STORE_FILE_NAME, SshConnectionProfile, SshProfileStore, SshRuntimeChannel,
};

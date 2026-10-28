//! SSH connection profiles and their on-disk store.
//!
//! The profile is deliberately local to the desktop: it names the host, the
//! user, and the ports, but never carries a private key, password, pairing
//! code, or auth token.

pub mod config;
pub mod profile;

pub use config::{
    SshDiscovery, SshEffectiveConfig, SshHostEntry, default_ssh_config_path,
    discover_default_hosts, discover_hosts, resolve_effective, resolve_effective_default,
};
pub use profile::{
    SCHEMA_VERSION, STORE_FILE_NAME, SshConnectionProfile, SshProfileStore, SshRuntimeChannel,
};

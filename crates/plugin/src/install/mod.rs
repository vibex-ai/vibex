//! Downloading, verifying and activating plugin packages.

pub mod package;
pub mod store;

pub use package::{
    ExtractReport, extract_zip, safe_relative_path, sha256_hex, sha512_hex, verify_checksums,
};
pub use store::{
    DATA_DIR, PENDING_FILE, PLUGINS_DIR, PendingInstall, PluginStore, RecoveryOutcome, STAGING_DIR,
    VERSIONS_DIR,
};

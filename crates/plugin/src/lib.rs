//! Desktop plugin packages and the market that distributes them.
//!
//! The crate owns everything between "a catalog says a plugin exists" and "a
//! version is installed on disk":
//!
//! - the author's manifest (`vibex-plugin.json`) and its validation;
//! - the permission model and the consent fingerprint derived from it;
//! - the generated scripting-host manifest (`gpui-shell.json`);
//! - the market sources: a signed static index and GitHub topic discovery,
//!   merged into one catalog;
//! - the install pipeline: download, hash and signature verification, safe
//!   unpacking, version directories, atomic activation and rollback.
//!
//! It deliberately does not depend on the scripting host: the host is reached
//! through a runtime seam, so the market and package management can be built
//! and tested without it, and the host integration stays in one place.

pub mod error;
pub mod fingerprint;
pub mod install;
pub mod manifest;
pub mod market;
pub mod native;
pub mod permission;
pub mod runtime;
pub mod service;
pub mod shell;

pub use error::{ManifestError, PluginError};
pub use fingerprint::consent_fingerprint;
pub use install::{
    ExtractReport, PendingInstall, PluginStore, RecoveryOutcome, extract_zip, safe_relative_path,
    sha256_hex, sha512_hex, verify_checksums,
};
pub use manifest::{
    AUTHOR_MANIFEST_FILE, PluginCapabilities, PluginFsCapability, PluginManifest,
    PluginNetworkCapability, PluginSettingContribution,
};
pub use market::{
    DEFAULT_TOPIC, GithubTopicSource, MarketCache, MarketCatalog, MarketConfig, MarketHttpClient,
    MarketUrlPolicy, merge_entries, search_entries,
};
pub use permission::{
    PermissionSpec, PluginPermissionCatalog, default_permission_catalog, permissions_for,
};
pub use runtime::{LoadedPlugin, NullRuntime, PluginRuntime};
pub use service::{KEPT_VERSIONS, PluginService, is_newer};
pub use shell::{SHELL_MANIFEST_FILE, ShellManifest, build_shell_manifest};

//! Structured failures for manifest validation and plugin operations.

use thiserror::Error;
use vibex_core::{ErrorCategory, VibexError};

/// Why an author manifest was refused.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ManifestError {
    #[error("the plugin manifest is not valid JSON: {0}")]
    Json(String),
    #[error("the plugin manifest has an unknown field `{0}`")]
    UnknownField(String),
    #[error("the plugin id `{0}` is not valid")]
    InvalidId(String),
    #[error("the plugin name is empty")]
    EmptyName,
    #[error("the plugin name is too long")]
    NameTooLong,
    #[error("the plugin version `{0}` is not a semantic version")]
    InvalidVersion(String),
    #[error("the entry `{0}` is not a safe relative path")]
    InvalidEntry(String),
    #[error("the plugin icon `{0}` is not a safe relative path")]
    InvalidIcon(String),
    #[error("the license `{0}` is not in the allowed set")]
    LicenseNotAllowed(String),
    #[error("the requirement `{0}` is not a valid version requirement")]
    InvalidRequirement(String),
    #[error("the manifest schema version {0} is not supported")]
    UnsupportedSchema(u32),
    #[error("the plugin declares too many categories")]
    TooManyCategories,
}

/// Why a plugin operation failed.
#[derive(Debug, Error)]
pub enum PluginError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("the file could not be read or written: {0}")]
    Io(#[from] std::io::Error),
    #[error("the data is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the network request failed: {0}")]
    Network(String),
    #[error("the response is larger than the market accepts")]
    ResponseTooLarge,
    #[error("the index signature does not verify")]
    IndexSignatureInvalid,
    #[error("the package checksum does not match the catalog")]
    HashMismatch,
    #[error("the package signature does not verify")]
    SignatureInvalid,
    #[error("the package could not be unpacked: {0}")]
    Archive(String),
    #[error("the archive entry `{0}` was rejected")]
    UnpackRejected(String),
    #[error("the plugin `{0}` is not installed")]
    NotInstalled(String),
    #[error("the plugin `{0}` is already installed")]
    AlreadyInstalled(String),
    #[error("the plugin `{0}` cannot be uninstalled")]
    NotUninstallable(String),
    #[error("the plugin `{0}` is blocked: {1}")]
    Blocked(String, String),
    #[error("the plugin `{0}` is pinned to version {1}")]
    Pinned(String, String),
    #[error("the permission `{0}` was not granted")]
    PermissionDenied(String),
    #[error("the plugin id `{0}` conflicts with a built-in plugin")]
    BuiltinConflict(String),
    #[error("the version `{0}` was withdrawn")]
    Yanked(String),
    #[error("the plugin needs Vibex {required}, but this build is {current}")]
    HostTooOld { required: String, current: String },
    #[error("the plugin `{0}` was not found in the catalog")]
    NotFound(String),
    #[error("the market has no usable catalog yet")]
    NoCatalog,
    #[error("the catalog could not be built: {0}")]
    Catalog(String),
    #[error("the plugin package does not carry a manifest")]
    MissingManifest,
    #[error("the scripting host refused the plugin: {0}")]
    Host(String),
    #[error("the scripting host is not part of this build")]
    HostUnavailable,
    #[error("the operation was cancelled")]
    Cancelled,
}

impl From<PluginError> for VibexError {
    fn from(error: PluginError) -> Self {
        let code = match &error {
            PluginError::Manifest(inner) => manifest_code(inner),
            PluginError::Io(_) => "plugin_io_failed",
            PluginError::Json(_) => "plugin_json_invalid",
            PluginError::Network(_) => "plugin_network_failed",
            PluginError::ResponseTooLarge => "plugin_response_too_large",
            PluginError::IndexSignatureInvalid => "plugin_index_signature_invalid",
            PluginError::HashMismatch => "plugin_hash_mismatch",
            PluginError::SignatureInvalid => "plugin_signature_invalid",
            PluginError::Archive(_) => "plugin_archive_invalid",
            PluginError::UnpackRejected(_) => "plugin_unpack_rejected",
            PluginError::NotInstalled(_) => "plugin_not_installed",
            PluginError::AlreadyInstalled(_) => "plugin_already_installed",
            PluginError::NotUninstallable(_) => "plugin_not_uninstallable",
            PluginError::Blocked(_, _) => "plugin_blocked",
            PluginError::Pinned(_, _) => "plugin_pinned",
            PluginError::PermissionDenied(_) => "plugin_permission_denied",
            PluginError::BuiltinConflict(_) => "plugin_builtin_conflict",
            PluginError::Yanked(_) => "plugin_yanked",
            PluginError::HostTooOld { .. } => "plugin_host_too_old",
            PluginError::NotFound(_) => "plugin_not_found",
            PluginError::NoCatalog => "plugin_market_unavailable",
            PluginError::Catalog(_) => "plugin_catalog_invalid",
            PluginError::MissingManifest => "plugin_manifest_missing",
            PluginError::Host(_) => "plugin_host_rejected",
            PluginError::HostUnavailable => "plugin_host_unavailable",
            PluginError::Cancelled => "plugin_cancelled",
        };
        let category = match &error {
            PluginError::Manifest(_)
            | PluginError::Json(_)
            | PluginError::Archive(_)
            | PluginError::UnpackRejected(_)
            | PluginError::Catalog(_)
            | PluginError::MissingManifest => ErrorCategory::Validation,
            PluginError::HashMismatch
            | PluginError::SignatureInvalid
            | PluginError::IndexSignatureInvalid
            | PluginError::PermissionDenied(_)
            | PluginError::Blocked(_, _)
            | PluginError::Pinned(_, _)
            | PluginError::BuiltinConflict(_)
            | PluginError::Yanked(_) => ErrorCategory::Permission,
            PluginError::AlreadyInstalled(_) => ErrorCategory::Conflict,
            PluginError::Host(_) | PluginError::HostUnavailable => ErrorCategory::Process,
            PluginError::Network(_) | PluginError::ResponseTooLarge | PluginError::NoCatalog => {
                ErrorCategory::Provider
            }
            _ => ErrorCategory::Storage,
        };
        VibexError::new(category, code, error.to_string())
    }
}

fn manifest_code(error: &ManifestError) -> &'static str {
    match error {
        ManifestError::Json(_) => "plugin_manifest_invalid",
        ManifestError::UnknownField(_) => "plugin_manifest_unknown_field",
        ManifestError::InvalidId(_) => "plugin_manifest_id_invalid",
        ManifestError::EmptyName | ManifestError::NameTooLong => "plugin_manifest_name_invalid",
        ManifestError::InvalidVersion(_) => "plugin_manifest_version_invalid",
        ManifestError::InvalidEntry(_) => "plugin_manifest_entry_invalid",
        ManifestError::InvalidIcon(_) => "plugin_manifest_icon_invalid",
        ManifestError::LicenseNotAllowed(_) => "plugin_manifest_license_invalid",
        ManifestError::InvalidRequirement(_) => "plugin_manifest_requirement_invalid",
        ManifestError::UnsupportedSchema(_) => "plugin_manifest_schema_unsupported",
        ManifestError::TooManyCategories => "plugin_manifest_categories_invalid",
    }
}

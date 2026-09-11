//! Recovery domain payloads shared by the authority and its clients.
//!
//! Diagnostic export and database backup run on whichever side owns the data:
//! the local authority in native mode, the headless runtime in remote mode.
//! The payloads therefore carry only intent — a caller that has no opinion
//! about paths leaves them empty and the authority resolves them against its
//! own home directory, then reports the path it actually used.

use serde::{Deserialize, Serialize};

use crate::DiagnosticBundleRequest;

/// Diagnostic bundle export request.
///
/// The destination always comes from the authority: the bundle is written on
/// the machine that owns the runtime data, and its path is returned in
/// [`DiagnosticExportOutcome`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticExportPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<DiagnosticBundleRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticExportOutcome {
    /// Path of the written bundle, as reported by the authority.
    pub destination: String,
    /// Always true on success: the export aborts rather than write a bundle
    /// that fails the redaction sentinel gate.
    pub redaction_verified: bool,
}

/// Creates a database backup.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupCreatePayload {
    /// Empty means "the authority picks its default backup directory".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupCreateOutcome {
    pub backup_dir: String,
}

/// Inspects an existing backup directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInspectPayload {
    /// Empty means "the authority's default backup directory".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_dir: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupMigrationCompatibility {
    Ready,
    MigrationRequired,
    UnsupportedNewerSchema,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInspectOutcome {
    pub backup_dir: String,
    pub database_schema_version: i64,
    pub migration_compatibility: BackupMigrationCompatibility,
}

/// Restores a backup into a new, empty database.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRestorePayload {
    /// Empty means "the authority's default backup directory".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_dir: Option<String>,
    /// Empty means "a sibling of the authority's live database".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_db_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupRestoreOutcomeStatus {
    Restored,
    RestoredMigrated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRestoreOutcome {
    pub target_db_path: String,
    pub status: BackupRestoreOutcomeStatus,
}

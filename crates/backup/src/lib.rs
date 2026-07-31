use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vibex_core::{VibexError, VibexResult, unix_timestamp_ms};
use vibex_db::{
    CURRENT_SCHEMA_VERSION, apply_migrations, current_schema_version, open_database, run_smoke,
};

pub const BACKUP_MANIFEST_SCHEMA_VERSION: &str = "vibex_backup.v1";
pub const MANIFEST_FILE_NAME: &str = "manifest.json";
pub const DATABASE_ARTIFACT_PATH: &str = "data/vibex.db";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupArtifactKind {
    Database,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupArtifact {
    pub path: String,
    pub kind: BackupArtifactKind,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationCompatibility {
    Ready,
    MigrationRequired,
    UnsupportedNewerSchema,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupExcludedContent {
    Prompts,
    AgentMessages,
    TerminalOutput,
    FileContents,
    Secrets,
    EnvValues,
    RawHeaders,
    ProviderNativePayloads,
    NativeIds,
    RawGitDiffs,
    RawLogs,
    DevicePrivateKeys,
    AuthTokens,
    RelayPairingSecrets,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupManifest {
    pub schema_version: String,
    pub created_at_ms: i64,
    pub app_version: String,
    pub source_database_schema_version: i64,
    pub expected_database_schema_version: i64,
    pub migration_compatibility: MigrationCompatibility,
    pub artifacts: Vec<BackupArtifact>,
    pub excluded_content: Vec<BackupExcludedContent>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BackupCreateRequest {
    pub source_db_path: PathBuf,
    pub backup_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupCreateResult {
    pub backup_dir: PathBuf,
    pub database_artifact_path: PathBuf,
    pub manifest: BackupManifest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInspection {
    pub backup_dir: PathBuf,
    pub database_artifact_path: PathBuf,
    pub manifest: BackupManifest,
    pub database_schema_version: i64,
    pub migration_compatibility: MigrationCompatibility,
}

#[derive(Debug, Clone)]
pub struct BackupRestoreRequest {
    pub backup_dir: PathBuf,
    pub target_db_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupRestoreStatus {
    Restored,
    RestoredMigrated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRestoreResult {
    pub target_db_path: PathBuf,
    pub source_schema_version: i64,
    pub restored_schema_version: i64,
    pub migration_compatibility: MigrationCompatibility,
    pub status: BackupRestoreStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRestoreSmokeResult {
    pub status: String,
    pub source_db_path: String,
    pub backup_path: String,
    pub restored_db_path: String,
    pub source_schema_version: i64,
    pub restored_schema_version: i64,
    pub migration_compatibility: MigrationCompatibility,
    pub round_trip_verified: bool,
    pub redaction_verified: bool,
}

pub fn classify_migration_compatibility(source_schema_version: i64) -> MigrationCompatibility {
    if source_schema_version <= 0 {
        MigrationCompatibility::Invalid
    } else if source_schema_version < CURRENT_SCHEMA_VERSION {
        MigrationCompatibility::MigrationRequired
    } else if source_schema_version == CURRENT_SCHEMA_VERSION {
        MigrationCompatibility::Ready
    } else {
        MigrationCompatibility::UnsupportedNewerSchema
    }
}

pub fn create_backup(request: BackupCreateRequest) -> VibexResult<BackupCreateResult> {
    if !request.source_db_path.exists() {
        return Err(VibexError::validation(
            "backup_source_database_missing",
            "backup source database does not exist",
        ));
    }

    let data_dir = request.backup_dir.join("data");
    let database_artifact_path = request.backup_dir.join(DATABASE_ARTIFACT_PATH);
    let manifest_path = request.backup_dir.join(MANIFEST_FILE_NAME);
    if manifest_path.exists() || database_artifact_path.exists() {
        return Err(VibexError::conflict(
            "backup_target_exists",
            "backup target already contains Vibex backup artifacts",
        ));
    }

    fs::create_dir_all(&data_dir).map_err(|err| {
        VibexError::storage(
            "backup_directory_create_failed",
            "failed to create backup directory",
        )
        .with_diagnostic("error", err.to_string())
    })?;

    let mut conn = open_database(&request.source_db_path)?;
    apply_migrations(&mut conn)?;
    let source_schema_version = current_schema_version(&conn)?;
    conn.execute_batch("PRAGMA wal_checkpoint(FULL);")
        .map_err(storage_io_error(
            "backup_wal_checkpoint_failed",
            "failed to checkpoint database WAL before backup",
        ))?;
    let vacuum_sql = format!(
        "VACUUM main INTO '{}';",
        escape_sqlite_string(&database_artifact_path.display().to_string())
    );
    conn.execute_batch(&vacuum_sql).map_err(storage_io_error(
        "backup_database_copy_failed",
        "failed to copy SQLite database into backup artifact",
    ))?;
    drop(conn);

    let copied_schema_version = read_database_schema_version(&database_artifact_path, false)?;
    if copied_schema_version != source_schema_version {
        return Err(VibexError::storage(
            "backup_database_schema_mismatch",
            "backup database artifact schema did not match source schema",
        )
        .with_diagnostic("sourceSchemaVersion", source_schema_version.to_string())
        .with_diagnostic("artifactSchemaVersion", copied_schema_version.to_string()));
    }

    let artifact = BackupArtifact {
        path: DATABASE_ARTIFACT_PATH.to_string(),
        kind: BackupArtifactKind::Database,
        size_bytes: file_size(&database_artifact_path)?,
        sha256: sha256_file(&database_artifact_path)?,
    };
    let manifest = BackupManifest {
        schema_version: BACKUP_MANIFEST_SCHEMA_VERSION.to_string(),
        created_at_ms: unix_timestamp_ms(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        source_database_schema_version: source_schema_version,
        expected_database_schema_version: CURRENT_SCHEMA_VERSION,
        migration_compatibility: classify_migration_compatibility(source_schema_version),
        artifacts: vec![artifact],
        excluded_content: default_excluded_content(),
        notes: vec!["Restore requires explicit target path; device keys and auth material are non-exportable by default.".to_string()],
    };
    write_manifest(&manifest_path, &manifest)?;

    Ok(BackupCreateResult {
        backup_dir: request.backup_dir,
        database_artifact_path,
        manifest,
    })
}

pub fn inspect_backup(backup_dir: &Path) -> VibexResult<BackupInspection> {
    let manifest = read_manifest(&backup_dir.join(MANIFEST_FILE_NAME))?;
    validate_manifest(&manifest)?;

    let database_artifact = database_artifact(&manifest)?;
    let database_artifact_path = backup_dir.join(&database_artifact.path);
    if !database_artifact_path.exists() {
        return Err(VibexError::validation(
            "backup_database_artifact_missing",
            "backup database artifact is missing",
        ));
    }

    let actual_size = file_size(&database_artifact_path)?;
    if actual_size != database_artifact.size_bytes {
        return Err(VibexError::validation(
            "backup_database_artifact_size_mismatch",
            "backup database artifact size does not match manifest",
        ));
    }
    let actual_sha256 = sha256_file(&database_artifact_path)?;
    if actual_sha256 != database_artifact.sha256 {
        return Err(VibexError::validation(
            "backup_database_artifact_checksum_mismatch",
            "backup database artifact checksum does not match manifest",
        ));
    }

    let database_schema_version = read_database_schema_version(&database_artifact_path, false)?;
    if database_schema_version != manifest.source_database_schema_version {
        return Err(VibexError::validation(
            "backup_database_schema_mismatch",
            "backup manifest schema version does not match database artifact",
        )
        .with_diagnostic(
            "manifestSchemaVersion",
            manifest.source_database_schema_version.to_string(),
        )
        .with_diagnostic("databaseSchemaVersion", database_schema_version.to_string()));
    }
    let migration_compatibility =
        classify_migration_compatibility(manifest.source_database_schema_version);

    Ok(BackupInspection {
        backup_dir: backup_dir.to_path_buf(),
        database_artifact_path,
        manifest,
        database_schema_version,
        migration_compatibility,
    })
}

pub fn restore_backup(request: BackupRestoreRequest) -> VibexResult<BackupRestoreResult> {
    let inspection = inspect_backup(&request.backup_dir)?;
    match inspection.migration_compatibility {
        MigrationCompatibility::UnsupportedNewerSchema => {
            return Err(VibexError::validation(
                "backup_restore_newer_schema_unsupported",
                "backup was created by a newer Vibex schema and cannot be restored safely",
            )
            .with_diagnostic(
                "sourceSchemaVersion",
                inspection
                    .manifest
                    .source_database_schema_version
                    .to_string(),
            )
            .with_diagnostic("currentSchemaVersion", CURRENT_SCHEMA_VERSION.to_string()));
        }
        MigrationCompatibility::Invalid => {
            return Err(VibexError::validation(
                "backup_restore_invalid_schema",
                "backup schema metadata is invalid",
            ));
        }
        MigrationCompatibility::Ready | MigrationCompatibility::MigrationRequired => {}
    }

    if database_family_exists(&request.target_db_path) {
        return Err(VibexError::conflict(
            "backup_restore_target_exists",
            "restore target database already exists",
        )
        .with_recovery_hint(
            "Choose an empty disposable target path or move the existing database first.",
        ));
    }

    if let Some(parent) = request.target_db_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            VibexError::storage(
                "backup_restore_target_directory_failed",
                "failed to create restore target directory",
            )
            .with_diagnostic("error", err.to_string())
        })?;
    }

    fs::copy(&inspection.database_artifact_path, &request.target_db_path).map_err(|err| {
        VibexError::storage(
            "backup_restore_copy_failed",
            "failed to copy backup database artifact to restore target",
        )
        .with_diagnostic("error", err.to_string())
    })?;

    let mut conn = open_database(&request.target_db_path)?;
    let status = if inspection.migration_compatibility == MigrationCompatibility::MigrationRequired
    {
        apply_migrations(&mut conn)?;
        BackupRestoreStatus::RestoredMigrated
    } else {
        BackupRestoreStatus::Restored
    };
    let restored_schema_version = current_schema_version(&conn)?;
    if restored_schema_version != CURRENT_SCHEMA_VERSION {
        return Err(VibexError::storage(
            "backup_restore_schema_verification_failed",
            "restored database schema did not reach the current expected version",
        )
        .with_diagnostic("restoredSchemaVersion", restored_schema_version.to_string())
        .with_diagnostic("currentSchemaVersion", CURRENT_SCHEMA_VERSION.to_string()));
    }

    Ok(BackupRestoreResult {
        target_db_path: request.target_db_path,
        source_schema_version: inspection.manifest.source_database_schema_version,
        restored_schema_version,
        migration_compatibility: inspection.migration_compatibility,
        status,
    })
}

pub fn run_backup_restore_smoke() -> VibexResult<BackupRestoreSmokeResult> {
    let root = PathBuf::from("target").join("stage0");
    let source_db_path = root.join("vibex-backup-source.db");
    let backup_path = root.join("vibex-backup-smoke");
    let restored_db_path = root.join("vibex-backup-restored.db");

    remove_database_family(&source_db_path)?;
    remove_database_family(&restored_db_path)?;
    remove_dir_if_exists(&backup_path)?;

    let source_smoke = run_smoke(&source_db_path)?;
    let created = create_backup(BackupCreateRequest {
        source_db_path: source_db_path.clone(),
        backup_dir: backup_path.clone(),
    })?;
    let inspected = inspect_backup(&backup_path)?;
    let restored = restore_backup(BackupRestoreRequest {
        backup_dir: backup_path.clone(),
        target_db_path: restored_db_path.clone(),
    })?;
    let restored_marker = read_foundation_marker(&restored_db_path)?;
    if restored_marker != source_smoke.marker {
        return Err(VibexError::storage(
            "backup_restore_smoke_marker_mismatch",
            "restored database marker did not match source database marker",
        ));
    }

    let manifest_json = serde_json::to_string(&created.manifest).map_err(|err| {
        VibexError::storage(
            "backup_restore_smoke_manifest_encode_failed",
            "failed to encode backup manifest for smoke redaction check",
        )
        .with_diagnostic("error", err.to_string())
    })?;
    assert_no_sensitive_sentinels(&manifest_json)?;
    let result_json = serde_json::to_string(&restored).map_err(|err| {
        VibexError::storage(
            "backup_restore_smoke_result_encode_failed",
            "failed to encode backup restore result for redaction check",
        )
        .with_diagnostic("error", err.to_string())
    })?;
    assert_no_sensitive_sentinels(&result_json)?;

    Ok(BackupRestoreSmokeResult {
        status: "ok".to_string(),
        source_db_path: source_db_path.display().to_string(),
        backup_path: backup_path.display().to_string(),
        restored_db_path: restored_db_path.display().to_string(),
        source_schema_version: inspected.manifest.source_database_schema_version,
        restored_schema_version: restored.restored_schema_version,
        migration_compatibility: inspected.migration_compatibility,
        round_trip_verified: true,
        redaction_verified: true,
    })
}

fn validate_manifest(manifest: &BackupManifest) -> VibexResult<()> {
    if manifest.schema_version != BACKUP_MANIFEST_SCHEMA_VERSION {
        return Err(VibexError::validation(
            "backup_manifest_version_unsupported",
            "backup manifest schema version is unsupported",
        ));
    }
    if manifest.expected_database_schema_version != CURRENT_SCHEMA_VERSION {
        return Err(VibexError::validation(
            "backup_manifest_expected_schema_mismatch",
            "backup manifest expected schema version does not match this Vibex build",
        ));
    }
    let expected_compatibility =
        classify_migration_compatibility(manifest.source_database_schema_version);
    if manifest.migration_compatibility != expected_compatibility {
        return Err(VibexError::validation(
            "backup_manifest_compatibility_mismatch",
            "backup manifest migration compatibility does not match schema version",
        ));
    }
    if manifest.artifacts.is_empty() {
        return Err(VibexError::validation(
            "backup_manifest_artifacts_missing",
            "backup manifest does not list any artifacts",
        ));
    }
    let mut seen = BTreeSet::new();
    for artifact in &manifest.artifacts {
        validate_artifact_path(&artifact.path)?;
        if !seen.insert(artifact.path.clone()) {
            return Err(VibexError::validation(
                "backup_manifest_duplicate_artifact",
                "backup manifest contains duplicate artifact paths",
            ));
        }
        if artifact.kind != BackupArtifactKind::Database || artifact.path != DATABASE_ARTIFACT_PATH
        {
            return Err(VibexError::validation(
                "backup_manifest_unexpected_artifact",
                "backup manifest contains an unexpected artifact",
            ));
        }
        if artifact.size_bytes == 0 || artifact.sha256.len() != 64 {
            return Err(VibexError::validation(
                "backup_manifest_artifact_invalid",
                "backup artifact metadata is invalid",
            ));
        }
    }
    ensure_excluded_content(manifest)?;
    Ok(())
}

fn ensure_excluded_content(manifest: &BackupManifest) -> VibexResult<()> {
    let excluded: BTreeSet<BackupExcludedContent> =
        manifest.excluded_content.iter().copied().collect();
    for required in default_excluded_content() {
        if !excluded.contains(&required) {
            return Err(VibexError::validation(
                "backup_manifest_exclusion_missing",
                "backup manifest is missing a required sensitive-content exclusion",
            ));
        }
    }
    Ok(())
}

fn database_artifact(manifest: &BackupManifest) -> VibexResult<&BackupArtifact> {
    manifest
        .artifacts
        .iter()
        .find(|artifact| {
            artifact.kind == BackupArtifactKind::Database && artifact.path == DATABASE_ARTIFACT_PATH
        })
        .ok_or_else(|| {
            VibexError::validation(
                "backup_database_artifact_not_listed",
                "backup manifest does not list the database artifact",
            )
        })
}

fn validate_artifact_path(value: &str) -> VibexResult<()> {
    let path = Path::new(value);
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(VibexError::validation(
            "backup_artifact_path_unsafe",
            "backup artifact path must be a relative path",
        ));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(VibexError::validation(
                    "backup_artifact_path_unsafe",
                    "backup artifact path must not traverse outside the backup directory",
                ));
            }
        }
    }
    Ok(())
}

fn read_manifest(path: &Path) -> VibexResult<BackupManifest> {
    let raw = fs::read_to_string(path).map_err(|err| {
        VibexError::validation(
            "backup_manifest_read_failed",
            "failed to read backup manifest",
        )
        .with_diagnostic("error", err.to_string())
    })?;
    serde_json::from_str(&raw).map_err(|err| {
        VibexError::validation(
            "backup_manifest_decode_failed",
            "failed to decode backup manifest",
        )
        .with_diagnostic("error", err.to_string())
    })
}

fn write_manifest(path: &Path, manifest: &BackupManifest) -> VibexResult<()> {
    let json = serde_json::to_string_pretty(manifest).map_err(|err| {
        VibexError::storage(
            "backup_manifest_encode_failed",
            "failed to encode backup manifest",
        )
        .with_diagnostic("error", err.to_string())
    })?;
    fs::write(path, json).map_err(|err| {
        VibexError::storage(
            "backup_manifest_write_failed",
            "failed to write backup manifest",
        )
        .with_diagnostic("error", err.to_string())
    })
}

fn read_database_schema_version(path: &Path, apply_pending: bool) -> VibexResult<i64> {
    if !apply_pending {
        let conn =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|err| {
                VibexError::storage(
                    "backup_database_open_read_only_failed",
                    "failed to open backup database artifact read-only",
                )
                .with_diagnostic("error", err.to_string())
            })?;
        return current_schema_version(&conn);
    }

    let mut conn = open_database(path)?;
    if apply_pending {
        apply_migrations(&mut conn)?;
    }
    current_schema_version(&conn)
}

fn read_foundation_marker(path: &Path) -> VibexResult<String> {
    let conn = open_database(path)?;
    conn.query_row(
        "SELECT marker FROM foundation_smoke WHERE id = 1",
        [],
        |row| row.get::<_, String>(0),
    )
    .map_err(storage_io_error(
        "backup_restore_smoke_marker_read_failed",
        "failed to read restored smoke marker",
    ))
}

fn file_size(path: &Path) -> VibexResult<u64> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|err| {
            VibexError::storage(
                "backup_artifact_metadata_failed",
                "failed to read artifact metadata",
            )
            .with_diagnostic("error", err.to_string())
        })
}

fn sha256_file(path: &Path) -> VibexResult<String> {
    let mut file = fs::File::open(path).map_err(|err| {
        VibexError::storage(
            "backup_artifact_hash_failed",
            "failed to open artifact for hashing",
        )
        .with_diagnostic("error", err.to_string())
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|err| {
            VibexError::storage(
                "backup_artifact_hash_failed",
                "failed to read artifact for hashing",
            )
            .with_diagnostic("error", err.to_string())
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn database_family_exists(path: &Path) -> bool {
    path.exists() || wal_path(path).exists() || shm_path(path).exists()
}

fn remove_database_family(path: &Path) -> VibexResult<()> {
    for member in [path.to_path_buf(), wal_path(path), shm_path(path)] {
        if member.exists() {
            fs::remove_file(&member).map_err(|err| {
                VibexError::storage(
                    "backup_smoke_database_cleanup_failed",
                    "failed to remove disposable smoke database file",
                )
                .with_diagnostic("error", err.to_string())
            })?;
        }
    }
    Ok(())
}

fn remove_dir_if_exists(path: &Path) -> VibexResult<()> {
    if path.exists() {
        fs::remove_dir_all(path).map_err(|err| {
            VibexError::storage(
                "backup_smoke_directory_cleanup_failed",
                "failed to remove disposable smoke backup directory",
            )
            .with_diagnostic("error", err.to_string())
        })?;
    }
    Ok(())
}

fn wal_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}-wal", path.display()))
}

fn shm_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}-shm", path.display()))
}

fn default_excluded_content() -> Vec<BackupExcludedContent> {
    vec![
        BackupExcludedContent::Prompts,
        BackupExcludedContent::AgentMessages,
        BackupExcludedContent::TerminalOutput,
        BackupExcludedContent::FileContents,
        BackupExcludedContent::Secrets,
        BackupExcludedContent::EnvValues,
        BackupExcludedContent::RawHeaders,
        BackupExcludedContent::ProviderNativePayloads,
        BackupExcludedContent::NativeIds,
        BackupExcludedContent::RawGitDiffs,
        BackupExcludedContent::RawLogs,
        BackupExcludedContent::DevicePrivateKeys,
        BackupExcludedContent::AuthTokens,
        BackupExcludedContent::RelayPairingSecrets,
    ]
}

fn assert_no_sensitive_sentinels(value: &str) -> VibexResult<()> {
    for sentinel in [
        "super-secret",
        "secret-auth-token",
        "BEGIN PRIVATE KEY",
        "raw-provider-payload",
        "terminal output sentinel",
        "prompt body sentinel",
    ] {
        if value.contains(sentinel) {
            return Err(VibexError::storage(
                "backup_restore_smoke_redaction_failed",
                "backup/restore smoke evidence contained a sensitive sentinel",
            )
            .with_diagnostic("sentinel", sentinel));
        }
    }
    Ok(())
}

fn escape_sqlite_string(value: &str) -> String {
    value.replace('\'', "''")
}

fn storage_io_error<E>(code: &'static str, message: &'static str) -> impl FnOnce(E) -> VibexError
where
    E: std::fmt::Display,
{
    move |err| VibexError::storage(code, message).with_diagnostic("error", err.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use vibex_core::{
        AcpAdapterId, AgentId, AgentSession, AgentSessionSafety, AgentSessionState,
        AgentUsageCounterOrigin, AgentUsageExecutionContext, AgentUsageObservation,
        AgentUsageObservationSource, AgentUsageStreamAttribution, AgentUsageTokenValues,
        BindingState, NativeStateHomeId, ProviderProfileId, RuntimeBinding, RuntimeBindingId,
        SessionRuntimeConfigState, TransportKind, UsageExecutionId, VibexSessionId, WorkspaceMode,
    };
    use vibex_db::{
        AgentUsageRepository, DbConnection, RuntimeBindingRepository, SessionRepository,
        WorkspaceRepository,
    };

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn classifies_migration_compatibility() {
        assert_eq!(
            classify_migration_compatibility(CURRENT_SCHEMA_VERSION),
            MigrationCompatibility::Ready
        );
        assert_eq!(
            classify_migration_compatibility(CURRENT_SCHEMA_VERSION - 1),
            MigrationCompatibility::MigrationRequired
        );
        assert_eq!(
            classify_migration_compatibility(CURRENT_SCHEMA_VERSION + 1),
            MigrationCompatibility::UnsupportedNewerSchema
        );
        assert_eq!(
            classify_migration_compatibility(0),
            MigrationCompatibility::Invalid
        );
    }

    #[test]
    fn manifest_records_required_exclusions() {
        let root = test_root("manifest-exclusions");
        let created = seeded_backup(&root);
        let json = serde_json::to_string(&created.manifest).unwrap();

        assert!(json.contains("device_private_keys"));
        assert!(json.contains("auth_tokens"));
        assert!(json.contains("relay_pairing_secrets"));
        assert!(!json.contains("super-secret"));
        validate_manifest(&created.manifest).unwrap();
    }

    #[test]
    fn inspect_rejects_unsafe_artifact_paths() {
        let root = test_root("unsafe-artifact");
        let created = seeded_backup(&root);
        let mut manifest = created.manifest;
        manifest.artifacts[0].path = "../vibex.db".to_string();
        write_manifest(&root.join("backup").join(MANIFEST_FILE_NAME), &manifest).unwrap();

        let err = inspect_backup(&root.join("backup")).unwrap_err();
        assert_eq!(err.code, "backup_artifact_path_unsafe");
    }

    #[test]
    fn inspect_rejects_manifest_database_schema_mismatch() {
        let root = test_root("schema-mismatch");
        let created = seeded_backup(&root);
        let mut manifest = created.manifest;
        manifest.source_database_schema_version = CURRENT_SCHEMA_VERSION - 1;
        manifest.migration_compatibility =
            classify_migration_compatibility(manifest.source_database_schema_version);
        write_manifest(&root.join("backup").join(MANIFEST_FILE_NAME), &manifest).unwrap();

        let err = inspect_backup(&root.join("backup")).unwrap_err();
        assert_eq!(err.code, "backup_database_schema_mismatch");
    }

    #[test]
    fn restore_rejects_newer_schema_without_target_mutation() {
        let root = test_root("newer-schema");
        let created = seeded_backup(&root);
        let artifact_path = created.database_artifact_path;
        insert_schema_version(&artifact_path, CURRENT_SCHEMA_VERSION + 1);

        let mut manifest = created.manifest;
        manifest.source_database_schema_version = CURRENT_SCHEMA_VERSION + 1;
        manifest.migration_compatibility = MigrationCompatibility::UnsupportedNewerSchema;
        manifest.artifacts[0].size_bytes = file_size(&artifact_path).unwrap();
        manifest.artifacts[0].sha256 = sha256_file(&artifact_path).unwrap();
        write_manifest(&root.join("backup").join(MANIFEST_FILE_NAME), &manifest).unwrap();

        let target = root.join("target.db");
        let err = restore_backup(BackupRestoreRequest {
            backup_dir: root.join("backup"),
            target_db_path: target.clone(),
        })
        .unwrap_err();

        assert_eq!(err.code, "backup_restore_newer_schema_unsupported");
        assert!(!database_family_exists(&target));
    }

    #[test]
    fn restore_rejects_existing_target() {
        let root = test_root("existing-target");
        seeded_backup(&root);
        let target = root.join("target.db");
        fs::write(&target, b"existing").unwrap();

        let err = restore_backup(BackupRestoreRequest {
            backup_dir: root.join("backup"),
            target_db_path: target,
        })
        .unwrap_err();
        assert_eq!(err.code, "backup_restore_target_exists");
    }

    #[test]
    fn backup_restore_round_trips_disposable_database() {
        let root = test_root("roundtrip");
        let source = root.join("source.db");
        let backup = root.join("backup");
        let target = root.join("target.db");
        let source_smoke = run_smoke(&source).unwrap();
        let (usage_execution_id, expected_total_tokens) = seed_usage_fact(&source, &root);

        create_backup(BackupCreateRequest {
            source_db_path: source,
            backup_dir: backup.clone(),
        })
        .unwrap();
        let inspected = inspect_backup(&backup).unwrap();
        let restored = restore_backup(BackupRestoreRequest {
            backup_dir: backup,
            target_db_path: target.clone(),
        })
        .unwrap();

        assert_eq!(
            inspected.migration_compatibility,
            MigrationCompatibility::Ready
        );
        assert_eq!(restored.restored_schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(
            read_foundation_marker(&target).unwrap(),
            source_smoke.marker
        );
        let restored_connection = open_database(&target).unwrap();
        let restored_fact =
            AgentUsageRepository::get_fact(&restored_connection, &usage_execution_id)
                .unwrap()
                .unwrap();
        assert_eq!(
            restored_fact.delta.total_tokens,
            Some(expected_total_tokens)
        );
        let checkpoint_count: i64 = restored_connection
            .query_row(
                "SELECT COUNT(*) FROM agent_usage_checkpoints WHERE last_usage_execution_id = ?1",
                [usage_execution_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(checkpoint_count, 1);
    }

    fn seed_usage_fact(source: &Path, root: &Path) -> (UsageExecutionId, u64) {
        let connection = open_database(source).unwrap();
        let workspace_root = root.join("usage-workspace");
        fs::create_dir_all(&workspace_root).unwrap();
        let (project, workspace) = WorkspaceRepository::ensure(
            &connection,
            &workspace_root,
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: "Backup usage".to_string(),
            project_id: project.id.clone(),
            workspace_id: workspace.id.clone(),
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("opencode").unwrap(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&connection, &session).unwrap();
        let migration_applied_at_ms = connection
            .query_row(
                "SELECT applied_at_ms FROM schema_migrations WHERE version = 31",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        let binding_created_at_ms = migration_applied_at_ms.saturating_add(1);
        let binding_id = RuntimeBindingId::new();
        let provider_profile_id = ProviderProfileId::new();
        RuntimeBindingRepository::insert(
            &connection,
            &RuntimeBinding {
                binding_id: binding_id.clone(),
                session_id: session.id.clone(),
                agent_id: session.agent_id.clone(),
                transport_kind: TransportKind::Acp,
                provider_profile_id: provider_profile_id.clone(),
                adapter_id: AcpAdapterId::parse("usage-test-adapter").unwrap(),
                adapter_version: "1.0.0".to_string(),
                adapter_compatibility_identity: "usage-test-compatibility".to_string(),
                native_session_id: None,
                native_state_home_id: NativeStateHomeId::new(),
                provider_resume_identity: None,
                process_spawn_fingerprint: "usage-test-fingerprint".to_string(),
                session_runtime_config_state: SessionRuntimeConfigState::default(),
                capability_snapshot: None,
                restore_compatibility_key: None,
                profile_revision: 1,
                last_context_sequence: 0,
                last_summary_sequence: 0,
                context_bridge_version: 0,
                activation_generation: 1,
                binding_state: BindingState::Current,
                created_by_switch_id: None,
                created_at_ms: binding_created_at_ms,
                updated_at_ms: binding_created_at_ms,
            },
        )
        .unwrap();

        let usage_execution_id = UsageExecutionId::new();
        RuntimeBindingRepository::claim_usage_zero_baseline(
            &connection,
            &binding_id,
            1,
            &usage_execution_id,
        )
        .unwrap();
        let execution = AgentUsageExecutionContext {
            usage_execution_id: usage_execution_id.clone(),
            message_submission_id: None,
            project_id: project.id,
            workspace_id: workspace.id,
            stream: AgentUsageStreamAttribution {
                session_id: session.id,
                binding_id,
                activation_generation: 1,
                agent_id: session.agent_id,
                provider_profile_id,
                model_id: "backup-test-model".to_string(),
            },
        }
        .dispatched_at(binding_created_at_ms.saturating_add(1));
        let expected_total_tokens = 1_234;
        let mut mutable_connection = connection;
        AgentUsageRepository::apply_observation(
            &mut mutable_connection,
            &AgentUsageObservation {
                stream: execution.stream.clone(),
                execution: Some(execution),
                counter_origin: AgentUsageCounterOrigin::KnownZero,
                observation_sequence: 1,
                cumulative: AgentUsageTokenValues {
                    input_tokens: Some(800),
                    output_tokens: Some(434),
                    cached_read_tokens: Some(200),
                    total_tokens: Some(expected_total_tokens),
                    ..AgentUsageTokenValues::default()
                },
                context_window_used_tokens: Some(1_100),
                context_window_size_tokens: Some(200_000),
                source: AgentUsageObservationSource::PromptResponse,
                observed_at_ms: binding_created_at_ms.saturating_add(2),
            },
        )
        .unwrap();
        (usage_execution_id, expected_total_tokens)
    }

    fn seeded_backup(root: &Path) -> BackupCreateResult {
        fs::create_dir_all(root).unwrap();
        let source = root.join("source.db");
        run_smoke(&source).unwrap();
        create_backup(BackupCreateRequest {
            source_db_path: source,
            backup_dir: root.join("backup"),
        })
        .unwrap()
    }

    fn insert_schema_version(path: &Path, version: i64) {
        let conn = DbConnection::open(path).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            (version, "future_schema", unix_timestamp_ms()),
        )
        .unwrap();
    }

    fn test_root(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "vibex-backup-test-{name}-{}-{nanos}-{}",
            std::process::id(),
            NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst)
        ))
    }
}

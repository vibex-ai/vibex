//! One-time import of the release-candidate channel home into the channel home
//! that is currently authoritative.
//!
//! RC and stable are separate applications on purpose: each channel owns an
//! application id and a data directory, so a published-artifact rollback can
//! never race the other channel over the same files. That isolation also means
//! a stable install starts empty, and moving an RC user's work forward is an
//! explicit, one-time operation.
//!
//! The operation is split in two so it can never swap a database under a
//! running runtime:
//!
//! 1. [`stage_rc_import`] snapshots the RC database into the target home,
//!    migrates the snapshot, verifies it, and records a pending marker. The
//!    target keeps serving its own data, and a failure here changes nothing.
//! 2. [`apply_pending_rc_import`] runs at the next runtime start, after the
//!    home lock is held and before anything opens a database. It moves the
//!    current target artifacts into a rollback directory, moves the staged
//!    artifacts into place, and restores the rollback directory when any step
//!    fails.
//!
//! The RC home itself is never modified or deleted: the imported database can
//! keep referencing managed worktrees that still live under the RC home, and
//! the user keeps a working RC install to fall back to.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vibex_core::{VibexError, VibexResult, unix_timestamp_ms};
use vibex_db::{CURRENT_SCHEMA_VERSION, apply_migrations, current_schema_version, open_database};

use crate::RC_HOME_DIRECTORY;

/// Directory inside the target home that owns every import artifact.
pub const RC_IMPORT_DIRECTORY: &str = ".rc-import";
/// Marker naming the staged import that the next runtime start must apply.
pub const RC_IMPORT_PENDING_FILE: &str = "pending.json";
/// Record of the one-time first-launch prompt, so it is asked at most once.
pub const RC_IMPORT_PROMPT_FILE: &str = "prompt.json";
const RC_IMPORT_STAGED_DIRECTORY: &str = "staged";
const RC_IMPORT_DATABASE_FILE: &str = "vibex.db";
const RC_IMPORT_PENDING_SCHEMA_VERSION: &str = "rc-import.v1";
const RC_IMPORT_PROMPT_SCHEMA_VERSION: &str = "rc-import-prompt.v1";

/// Small artifacts that live next to the database and belong to the same user
/// data. Managed Agent installations and worktrees are deliberately excluded:
/// they are reinstallable or still referenced in place, and copying them would
/// either duplicate large trees or break the Git linkage of a worktree.
const RC_IMPORT_SIDECAR_FILES: &[&str] = &["provider-secrets.json"];
const RC_IMPORT_SIDECAR_NESTED_FILES: &[&[&str]] = &[&["relay", "desktop-identity.json"]];

/// Why the first-launch prompt will not be shown again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RcImportPromptDecision {
    /// The user chose not to import; the prompt never returns.
    Declined,
    /// RC data was staged, so there is nothing left to offer.
    Imported,
}

/// An RC home that holds a database worth importing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcImportSource {
    pub home: PathBuf,
    pub database_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcImportStage {
    pub source_home: PathBuf,
    pub source_schema_version: i64,
    pub target_schema_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcImportApply {
    pub source_home: String,
    pub source_schema_version: i64,
    pub applied_schema_version: i64,
    /// Directory holding the pre-import artifacts, kept as a safety net.
    pub rollback_directory: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingRcImport {
    schema_version: String,
    source_home: String,
    source_schema_version: i64,
    staged_schema_version: i64,
    staged_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RcImportPromptRecord {
    schema_version: String,
    decision: RcImportPromptDecision,
    source_home: String,
    decided_at_ms: i64,
}

/// The RC home that belongs to the base home holding `base_home`.
pub fn rc_import_source_home(base_home: &Path) -> PathBuf {
    base_home.join(RC_HOME_DIRECTORY)
}

/// The RC home to import from, or `None` when this machine has no RC data.
pub fn rc_import_source(base_home: &Path) -> Option<RcImportSource> {
    let home = rc_import_source_home(base_home);
    let database_path = home.join(RC_IMPORT_DATABASE_FILE);
    database_path.is_file().then_some(RcImportSource {
        home,
        database_path,
    })
}

fn import_root(target_home: &Path) -> PathBuf {
    target_home.join(RC_IMPORT_DIRECTORY)
}

fn staged_database_path(target_home: &Path) -> PathBuf {
    import_root(target_home)
        .join(RC_IMPORT_STAGED_DIRECTORY)
        .join(RC_IMPORT_DATABASE_FILE)
}

fn pending_marker_path(target_home: &Path) -> PathBuf {
    import_root(target_home).join(RC_IMPORT_PENDING_FILE)
}

fn prompt_record_path(target_home: &Path) -> PathBuf {
    import_root(target_home).join(RC_IMPORT_PROMPT_FILE)
}

/// True while a staged import is waiting for the next runtime start.
pub fn rc_import_pending(target_home: &Path) -> bool {
    pending_marker_path(target_home).is_file()
}

/// True once the first-launch prompt was answered, either way.
pub fn rc_import_prompt_answered(target_home: &Path) -> bool {
    read_prompt_record(target_home).is_some()
}

/// Records the first-launch answer so the prompt is asked at most once.
pub fn record_rc_import_prompt_decision(
    target_home: &Path,
    decision: RcImportPromptDecision,
    source_home: &Path,
) -> VibexResult<()> {
    let record = RcImportPromptRecord {
        schema_version: RC_IMPORT_PROMPT_SCHEMA_VERSION.to_string(),
        decision,
        source_home: source_home.display().to_string(),
        decided_at_ms: unix_timestamp_ms(),
    };
    let path = prompt_record_path(target_home);
    if let Some(parent) = path.parent() {
        create_directory(parent, "rc_import_directory_create_failed")?;
    }
    write_json_atomically(&path, &record)
}

fn read_prompt_record(target_home: &Path) -> Option<RcImportPromptRecord> {
    let bytes = fs::read(prompt_record_path(target_home)).ok()?;
    let record: RcImportPromptRecord = serde_json::from_slice(&bytes).ok()?;
    (record.schema_version == RC_IMPORT_PROMPT_SCHEMA_VERSION).then_some(record)
}

fn read_pending_import(target_home: &Path) -> VibexResult<Option<PendingRcImport>> {
    let path = pending_marker_path(target_home);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(storage_error(
                "rc_import_marker_read_failed",
                "failed to read the pending RC import marker",
                error,
            ));
        }
    };
    let marker: PendingRcImport = serde_json::from_slice(&bytes).map_err(|_| {
        VibexError::storage(
            "rc_import_marker_invalid",
            "the pending RC import marker is not valid JSON",
        )
        .with_diagnostic("path", path.display().to_string())
    })?;
    if marker.schema_version != RC_IMPORT_PENDING_SCHEMA_VERSION {
        return Err(VibexError::storage(
            "rc_import_marker_unsupported",
            "the pending RC import marker was written by another version",
        )
        .with_diagnostic("schemaVersion", marker.schema_version));
    }
    Ok(Some(marker))
}

/// Snapshots the RC database into the target home and records it as pending.
///
/// Nothing in the target home changes: the snapshot, its migration and its
/// verification all happen inside the import directory, so a failure leaves the
/// target exactly as it was and only the staging directory is removed.
pub fn stage_rc_import(source_home: &Path, target_home: &Path) -> VibexResult<RcImportStage> {
    let source_database_path = source_home.join(RC_IMPORT_DATABASE_FILE);
    if !source_database_path.is_file() {
        return Err(VibexError::validation(
            "rc_import_source_missing",
            "no RC database was found to import",
        )
        .with_diagnostic("sourceHome", source_home.display().to_string()));
    }
    if same_path(source_home, target_home) {
        return Err(VibexError::validation(
            "rc_import_source_is_target",
            "the RC home and the active home must differ",
        ));
    }
    if rc_import_pending(target_home) {
        return Err(VibexError::conflict(
            "rc_import_already_pending",
            "an RC import is already waiting for the next restart",
        ));
    }

    let root = import_root(target_home);
    let staged = root.join(RC_IMPORT_STAGED_DIRECTORY);
    // A failed earlier attempt can leave a partial snapshot behind; the marker
    // is the only thing that makes a snapshot authoritative.
    if staged.exists() {
        remove_path(&staged, "rc_import_staging_reset_failed")?;
    }
    create_directory(&staged, "rc_import_directory_create_failed")?;

    let staged_database = staged.join(RC_IMPORT_DATABASE_FILE);
    let staged_result = (|| -> VibexResult<(i64, i64)> {
        let source_schema_version = snapshot_database(&source_database_path, &staged_database)?;
        let mut connection = open_database(&staged_database)?;
        apply_migrations(&mut connection)?;
        let target_schema_version = current_schema_version(&connection)?;
        if target_schema_version != CURRENT_SCHEMA_VERSION {
            return Err(VibexError::storage(
                "rc_import_schema_verification_failed",
                "the staged RC database did not reach the current schema version",
            )
            .with_diagnostic("stagedSchemaVersion", target_schema_version.to_string())
            .with_diagnostic("currentSchemaVersion", CURRENT_SCHEMA_VERSION.to_string()));
        }
        verify_imported_database(&connection)?;
        drop(connection);
        copy_sidecar_files(source_home, &staged)?;
        Ok((source_schema_version, target_schema_version))
    })();

    let (source_schema_version, target_schema_version) = match staged_result {
        Ok(versions) => versions,
        Err(error) => {
            let _ = remove_path(&staged, "rc_import_staging_cleanup_failed");
            return Err(error);
        }
    };

    let marker = PendingRcImport {
        schema_version: RC_IMPORT_PENDING_SCHEMA_VERSION.to_string(),
        source_home: source_home.display().to_string(),
        source_schema_version,
        staged_schema_version: target_schema_version,
        staged_at_ms: unix_timestamp_ms(),
    };
    if let Err(error) = write_json_atomically(&pending_marker_path(target_home), &marker) {
        let _ = remove_path(&staged, "rc_import_staging_cleanup_failed");
        return Err(error);
    }
    // The prompt is answered by a successful staging: the RC data has been
    // consumed, so offering it again on the next launch would be wrong. The
    // record is best-effort because the staged marker, not this file, is what
    // makes the import authoritative; applying records it again.
    if let Err(error) =
        record_rc_import_prompt_decision(target_home, RcImportPromptDecision::Imported, source_home)
    {
        tracing::warn!(
            target: "vibex_desktop",
            error_code = %error.code,
            "RC import prompt record could not be written"
        );
    }

    Ok(RcImportStage {
        source_home: source_home.to_path_buf(),
        source_schema_version,
        target_schema_version,
    })
}

/// Applies a staged import into the target home.
///
/// Callers must hold the target home lock and must not have opened the target
/// database yet. Every replaced artifact is moved into a rollback directory
/// first, and any failure moves the originals back.
pub fn apply_pending_rc_import(target_home: &Path) -> VibexResult<Option<RcImportApply>> {
    let Some(marker) = read_pending_import(target_home)? else {
        return Ok(None);
    };
    let staged = import_root(target_home).join(RC_IMPORT_STAGED_DIRECTORY);
    let staged_database = staged_database_path(target_home);
    if !staged_database.is_file() {
        // A snapshot that is gone can never be applied, and keeping its marker
        // would block every later staging attempt.
        let _ = fs::remove_file(pending_marker_path(target_home));
        return Err(VibexError::storage(
            "rc_import_staged_database_missing",
            "the staged RC database disappeared before it could be applied",
        )
        .with_diagnostic("path", staged_database.display().to_string()));
    }

    let rollback = import_root(target_home).join(format!("rollback-{}", unix_timestamp_ms()));
    create_directory(&rollback, "rc_import_directory_create_failed")?;

    let outcome = (|| -> VibexResult<()> {
        for relative in replaced_relative_paths() {
            move_into_rollback(target_home, &rollback, &relative)?;
        }
        move_file(&staged_database, &target_home.join(RC_IMPORT_DATABASE_FILE))?;
        for relative in RC_IMPORT_SIDECAR_FILES {
            move_staged_sidecar(&staged, target_home, Path::new(relative))?;
        }
        for parts in RC_IMPORT_SIDECAR_NESTED_FILES {
            move_staged_sidecar(&staged, target_home, &parts.iter().collect::<PathBuf>())?;
        }
        Ok(())
    })();

    if let Err(error) = outcome {
        // The import must never leave a half-replaced home behind.
        let restored = restore_rollback(target_home, &rollback);
        let _ = remove_path(&staged, "rc_import_staging_cleanup_failed");
        let _ = fs::remove_file(pending_marker_path(target_home));
        return match restored {
            Ok(()) => {
                Err(error
                    .with_recovery_hint("The previous data was restored; nothing was imported."))
            }
            Err(restore_error) => Err(VibexError::storage(
                "rc_import_rollback_failed",
                "the RC import failed and the previous data could not be fully restored",
            )
            .with_diagnostic("importError", error.code)
            .with_diagnostic("rollbackError", restore_error.code)
            .with_diagnostic("rollbackDirectory", rollback.display().to_string())),
        };
    }

    let _ = remove_path(&staged, "rc_import_staging_cleanup_failed");
    let _ = fs::remove_file(pending_marker_path(target_home));
    // Applying is the last chance to record that the prompt was answered, so a
    // failed staging-time write cannot offer the same data a second time.
    if let Err(error) = record_rc_import_prompt_decision(
        target_home,
        RcImportPromptDecision::Imported,
        Path::new(&marker.source_home),
    ) {
        tracing::warn!(
            target: "vibex_desktop",
            error_code = %error.code,
            "RC import prompt record could not be written after applying"
        );
    }

    Ok(Some(RcImportApply {
        source_home: marker.source_home,
        source_schema_version: marker.source_schema_version,
        applied_schema_version: marker.staged_schema_version,
        rollback_directory: rollback,
    }))
}

/// Drops a staged import that will never be applied.
pub fn discard_pending_rc_import(target_home: &Path) -> VibexResult<bool> {
    let pending = rc_import_pending(target_home);
    let staged = import_root(target_home).join(RC_IMPORT_STAGED_DIRECTORY);
    if staged.exists() {
        remove_path(&staged, "rc_import_staging_cleanup_failed")?;
    }
    if pending {
        fs::remove_file(pending_marker_path(target_home)).map_err(|error| {
            storage_error(
                "rc_import_marker_remove_failed",
                "failed to remove the pending RC import marker",
                error,
            )
        })?;
    }
    Ok(pending)
}

/// Artifacts the import replaces, relative to the target home.
fn replaced_relative_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from(RC_IMPORT_DATABASE_FILE),
        PathBuf::from(format!("{RC_IMPORT_DATABASE_FILE}-wal")),
        PathBuf::from(format!("{RC_IMPORT_DATABASE_FILE}-shm")),
    ];
    paths.extend(RC_IMPORT_SIDECAR_FILES.iter().map(PathBuf::from));
    paths.extend(
        RC_IMPORT_SIDECAR_NESTED_FILES
            .iter()
            .map(|parts| parts.iter().collect::<PathBuf>()),
    );
    paths
}

fn move_into_rollback(target_home: &Path, rollback: &Path, relative: &Path) -> VibexResult<()> {
    let source = target_home.join(relative);
    if !source.is_file() {
        return Ok(());
    }
    let destination = rollback.join(relative);
    if let Some(parent) = destination.parent() {
        create_directory(parent, "rc_import_directory_create_failed")?;
    }
    move_file(&source, &destination)
}

fn move_staged_sidecar(staged: &Path, target_home: &Path, relative: &Path) -> VibexResult<()> {
    let source = staged.join(relative);
    if !source.is_file() {
        return Ok(());
    }
    let destination = target_home.join(relative);
    if let Some(parent) = destination.parent() {
        create_directory(parent, "rc_import_directory_create_failed")?;
    }
    move_file(&source, &destination)
}

/// Puts the target home back exactly as it was before an apply attempt.
///
/// Every replaced artifact is cleared first and then restored from the
/// rollback directory, so an artifact the import introduced for a path the
/// target never had (an RC sidecar file) cannot survive a failed apply.
fn restore_rollback(target_home: &Path, rollback: &Path) -> VibexResult<()> {
    for relative in replaced_relative_paths() {
        let destination = target_home.join(&relative);
        let source = rollback.join(&relative);
        if !destination.is_file() && !source.is_file() {
            continue;
        }
        if let Some(parent) = destination.parent() {
            create_directory(parent, "rc_import_directory_create_failed")?;
        }
        if destination.is_file() {
            fs::remove_file(&destination).map_err(|error| {
                storage_error(
                    "rc_import_rollback_failed",
                    "failed to clear a partially imported artifact",
                    error,
                )
            })?;
        }
        if source.is_file() {
            move_file(&source, &destination)?;
        }
    }
    Ok(())
}

fn copy_sidecar_files(source_home: &Path, staged: &Path) -> VibexResult<()> {
    let mut relatives: Vec<PathBuf> = RC_IMPORT_SIDECAR_FILES.iter().map(PathBuf::from).collect();
    relatives.extend(
        RC_IMPORT_SIDECAR_NESTED_FILES
            .iter()
            .map(|parts| parts.iter().collect::<PathBuf>()),
    );
    for relative in relatives {
        let source = source_home.join(&relative);
        if !source.is_file() {
            continue;
        }
        let destination = staged.join(&relative);
        if let Some(parent) = destination.parent() {
            create_directory(parent, "rc_import_directory_create_failed")?;
        }
        fs::copy(&source, &destination).map_err(|error| {
            storage_error(
                "rc_import_sidecar_copy_failed",
                "failed to copy an RC artifact into the import staging directory",
                error,
            )
            .with_diagnostic("artifact", relative.display().to_string())
        })?;
    }
    Ok(())
}

/// Copies the RC database into `destination` without migrating the source.
///
/// `VACUUM INTO` reads a consistent snapshot, including any WAL content, and
/// writes only to `destination`, so the RC install keeps its own rows and its
/// own schema version and stays usable after the import.
fn snapshot_database(source: &Path, destination: &Path) -> VibexResult<i64> {
    let connection = open_database(source)?;
    let source_schema_version = current_schema_version(&connection)?;
    if source_schema_version <= 0 {
        return Err(VibexError::validation(
            "rc_import_source_schema_invalid",
            "the RC database does not report a schema version",
        )
        .with_diagnostic("sourceSchemaVersion", source_schema_version.to_string()));
    }
    let sql = format!(
        "VACUUM main INTO '{}';",
        destination.display().to_string().replace('\'', "''")
    );
    connection.execute_batch(&sql).map_err(|error| {
        sqlite_error(
            "rc_import_snapshot_failed",
            "failed to snapshot the RC database",
            error,
        )
    })?;
    drop(connection);

    let copied = open_database(destination)?;
    let copied_schema_version = current_schema_version(&copied)?;
    if copied_schema_version != source_schema_version {
        return Err(VibexError::storage(
            "rc_import_snapshot_schema_mismatch",
            "the staged snapshot reports a different schema version than its source",
        )
        .with_diagnostic("sourceSchemaVersion", source_schema_version.to_string())
        .with_diagnostic("stagedSchemaVersion", copied_schema_version.to_string()));
    }
    Ok(source_schema_version)
}

/// Proves the migrated snapshot can answer the reads the workbench needs.
fn verify_imported_database(connection: &vibex_db::DbConnection) -> VibexResult<()> {
    for table in ["workspaces", "agent_sessions"] {
        let count: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(|error| {
                sqlite_error(
                    "rc_import_verification_failed",
                    "the staged RC database failed its read verification",
                    error,
                )
                .with_diagnostic("table", table)
            })?;
        if count < 0 {
            return Err(VibexError::storage(
                "rc_import_verification_failed",
                "the staged RC database returned an invalid row count",
            )
            .with_diagnostic("table", table));
        }
    }
    Ok(())
}

fn write_json_atomically<T: Serialize>(path: &Path, value: &T) -> VibexResult<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| {
        VibexError::storage(
            "rc_import_record_encode_failed",
            "failed to encode the RC import record",
        )
    })?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, &bytes).map_err(|error| {
        storage_error(
            "rc_import_record_write_failed",
            "failed to write the RC import record",
            error,
        )
    })?;
    fs::rename(&temporary, path).map_err(|error| {
        storage_error(
            "rc_import_record_write_failed",
            "failed to publish the RC import record",
            error,
        )
    })
}

fn move_file(source: &Path, destination: &Path) -> VibexResult<()> {
    fs::rename(source, destination).map_err(|error| {
        storage_error(
            "rc_import_move_failed",
            "failed to move an RC import artifact",
            error,
        )
        .with_diagnostic("source", source.display().to_string())
        .with_diagnostic("destination", destination.display().to_string())
    })
}

fn create_directory(path: &Path, code: &'static str) -> VibexResult<()> {
    fs::create_dir_all(path).map_err(|error| {
        storage_error(code, "failed to create an RC import directory", error)
            .with_diagnostic("path", path.display().to_string())
    })
}

fn remove_path(path: &Path, code: &'static str) -> VibexResult<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(
            storage_error(code, "failed to remove an RC import directory", error)
                .with_diagnostic("path", path.display().to_string()),
        ),
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn storage_error(code: &'static str, message: &'static str, error: std::io::Error) -> VibexError {
    VibexError::storage(code, message).with_diagnostic("errorKind", format!("{:?}", error.kind()))
}

fn sqlite_error(
    code: &'static str,
    message: &'static str,
    error: impl std::fmt::Display,
) -> VibexError {
    VibexError::storage(code, message).with_diagnostic("error", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_db::WorkspaceRepository;

    fn rc_home(base: &Path) -> PathBuf {
        base.join(RC_HOME_DIRECTORY)
    }

    fn seed_rc_database(home: &Path) -> PathBuf {
        fs::create_dir_all(home).unwrap();
        let path = home.join(RC_IMPORT_DATABASE_FILE);
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        WorkspaceRepository::ensure(
            &connection,
            home.join("imported-workspace"),
            vibex_core::WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        path
    }

    fn stable_home(base: &Path) -> PathBuf {
        let home = base.join("desktop-stable");
        fs::create_dir_all(&home).unwrap();
        home
    }

    #[test]
    fn detects_only_a_home_that_holds_a_database() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        assert!(rc_import_source(base).is_none());
        fs::create_dir_all(rc_home(base)).unwrap();
        assert!(rc_import_source(base).is_none());
        seed_rc_database(&rc_home(base));
        let source = rc_import_source(base).expect("RC data is detected");
        assert_eq!(source.home, rc_home(base));
        assert_eq!(source.database_path, rc_home(base).join("vibex.db"));
    }

    #[test]
    fn staging_validates_without_touching_the_target() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        let source_home = rc_home(base);
        let source_database = seed_rc_database(&source_home);
        let source_bytes = fs::read(&source_database).unwrap();
        let target = stable_home(base);

        let staged = stage_rc_import(&source_home, &target).unwrap();
        assert_eq!(staged.source_schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(staged.target_schema_version, CURRENT_SCHEMA_VERSION);
        assert!(rc_import_pending(&target));
        assert!(!target.join(RC_IMPORT_DATABASE_FILE).exists());
        // The RC database keeps its own schema and content.
        assert_eq!(fs::read(&source_database).unwrap(), source_bytes);
        assert!(rc_import_prompt_answered(&target));
    }

    #[test]
    fn staging_reports_a_missing_source_and_leaves_no_marker() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        let target = stable_home(base);
        let error = stage_rc_import(&rc_home(base), &target).unwrap_err();
        assert_eq!(error.code, "rc_import_source_missing");
        assert!(!rc_import_pending(&target));
    }

    #[test]
    fn staging_rejects_a_second_pending_import() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        let source_home = rc_home(base);
        seed_rc_database(&source_home);
        let target = stable_home(base);
        stage_rc_import(&source_home, &target).unwrap();
        let error = stage_rc_import(&source_home, &target).unwrap_err();
        assert_eq!(error.code, "rc_import_already_pending");
    }

    #[test]
    fn applying_moves_the_staged_database_into_place() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        let source_home = rc_home(base);
        seed_rc_database(&source_home);
        let target = stable_home(base);
        stage_rc_import(&source_home, &target).unwrap();

        let applied = apply_pending_rc_import(&target).unwrap().unwrap();
        assert_eq!(applied.source_home, source_home.display().to_string());
        assert_eq!(applied.applied_schema_version, CURRENT_SCHEMA_VERSION);
        assert!(target.join(RC_IMPORT_DATABASE_FILE).is_file());
        assert!(!rc_import_pending(&target));
        assert!(applied.rollback_directory.is_dir());

        let connection = open_database(&target.join(RC_IMPORT_DATABASE_FILE)).unwrap();
        let workspaces = WorkspaceRepository::list(&connection).unwrap();
        assert_eq!(workspaces.len(), 1);
        assert!(workspaces[0].1.root_path.ends_with("imported-workspace"));
    }

    #[test]
    fn applying_without_a_marker_is_a_no_op() {
        let directory = tempfile::tempdir().unwrap();
        let target = stable_home(directory.path());
        assert!(apply_pending_rc_import(&target).unwrap().is_none());
    }

    #[test]
    fn applying_restores_the_previous_data_when_the_staged_database_is_gone() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        let source_home = rc_home(base);
        seed_rc_database(&source_home);
        let target = stable_home(base);
        // The stable home already owns data the import must not lose.
        let existing = seed_rc_database(&target);
        let existing_bytes = fs::read(&existing).unwrap();
        stage_rc_import(&source_home, &target).unwrap();
        fs::remove_file(staged_database_path(&target)).unwrap();

        let error = apply_pending_rc_import(&target).unwrap_err();
        assert_eq!(error.code, "rc_import_staged_database_missing");
        assert_eq!(fs::read(&existing).unwrap(), existing_bytes);
        assert!(!rc_import_pending(&target));
    }

    #[test]
    fn rollback_removes_imported_artifacts_the_target_never_had() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let rollback = directory.path().join("rollback");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&rollback).unwrap();
        // The target never had a provider-secrets file, and the failed import
        // copied one in; the database it replaced is already in the rollback.
        fs::write(target.join("provider-secrets.json"), b"imported").unwrap();
        fs::write(target.join("vibex.db"), b"imported-db").unwrap();
        fs::write(rollback.join("vibex.db"), b"original-db").unwrap();

        restore_rollback(&target, &rollback).unwrap();

        assert_eq!(fs::read(target.join("vibex.db")).unwrap(), b"original-db");
        assert!(!target.join("provider-secrets.json").exists());
    }

    #[test]
    fn discarding_drops_the_staged_snapshot_and_marker() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        let source_home = rc_home(base);
        seed_rc_database(&source_home);
        let target = stable_home(base);
        stage_rc_import(&source_home, &target).unwrap();
        assert!(discard_pending_rc_import(&target).unwrap());
        assert!(!rc_import_pending(&target));
        assert!(!staged_database_path(&target).exists());
        assert!(!discard_pending_rc_import(&target).unwrap());
    }

    #[test]
    fn the_prompt_decision_is_remembered() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        let source_home = rc_home(base);
        seed_rc_database(&source_home);
        let target = stable_home(base);
        assert!(!rc_import_prompt_answered(&target));
        record_rc_import_prompt_decision(&target, RcImportPromptDecision::Declined, &source_home)
            .unwrap();
        assert!(rc_import_prompt_answered(&target));
        // A staging attempt after a decline still works: the record only
        // suppresses the prompt, it never blocks an explicit import.
        assert!(stage_rc_import(&source_home, &target).is_ok());
    }

    #[test]
    fn importing_from_the_target_home_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        let target = stable_home(base);
        seed_rc_database(&target);
        let error = stage_rc_import(&target, &target).unwrap_err();
        assert_eq!(error.code, "rc_import_source_is_target");
    }
}

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use vibex_core::{VibexError, VibexResult};
use vibex_db::{
    AdapterDiagnosticsRepository, TerminalSessionRepository, apply_migrations, open_database,
};

use crate::DesktopRuntime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageCleanupKind {
    SessionsAndAttachments,
    Terminals,
    Diagnostics,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StorageCleanupReport {
    pub removed_records: u64,
    pub removed_files: u64,
    pub reclaimed_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileCleanupKind {
    SessionsAndAttachments,
    Diagnostics,
}

pub(crate) async fn clear_storage(
    runtime: &DesktopRuntime,
    kind: StorageCleanupKind,
) -> VibexResult<StorageCleanupReport> {
    match kind {
        StorageCleanupKind::SessionsAndAttachments => {
            let sessions = runtime.agent.manager.list_sessions(true).await?;
            let mut report = StorageCleanupReport::default();
            for session in sessions {
                runtime.agent.manager.delete_session(&session.id).await?;
                report.removed_records = report.removed_records.saturating_add(1);
            }
            let (removed_files, reclaimed_bytes) = clear_files(
                &runtime.config.home_dir,
                FileCleanupKind::SessionsAndAttachments,
            )?;
            report.removed_files = removed_files;
            report.reclaimed_bytes = reclaimed_bytes;
            vacuum_database(&runtime.config.database_path)?;
            Ok(report)
        }
        StorageCleanupKind::Terminals => {
            let _lifecycle = runtime.terminals.lock_lifecycle()?;
            let shutdown = runtime.terminals.manager.shutdown_all()?;
            if let Some(error) = shutdown.failures.into_iter().next() {
                return Err(error);
            }
            let mut connection = open_database(&runtime.config.database_path)?;
            apply_migrations(&mut connection)?;
            let removed_records = TerminalSessionRepository::delete_all(&connection)?;
            vacuum_connection(&connection)?;
            Ok(StorageCleanupReport {
                removed_records,
                ..StorageCleanupReport::default()
            })
        }
        StorageCleanupKind::Diagnostics => {
            let mut connection = open_database(&runtime.config.database_path)?;
            apply_migrations(&mut connection)?;
            let removed_records = AdapterDiagnosticsRepository::delete_all(&connection)?;
            let (removed_files, reclaimed_bytes) =
                clear_files(&runtime.config.home_dir, FileCleanupKind::Diagnostics)?;
            vacuum_connection(&connection)?;
            Ok(StorageCleanupReport {
                removed_records,
                removed_files,
                reclaimed_bytes,
            })
        }
    }
}

fn vacuum_database(path: &Path) -> VibexResult<()> {
    let connection = open_database(path)?;
    vacuum_connection(&connection)
}

fn vacuum_connection(connection: &rusqlite::Connection) -> VibexResult<()> {
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
        .map_err(|error| {
            VibexError::storage(
                "desktop_storage_vacuum_failed",
                "failed to compact local storage",
            )
            .with_diagnostic("errorKind", format!("{:?}", error))
        })
}

fn clear_files(home: &Path, kind: FileCleanupKind) -> VibexResult<(u64, u64)> {
    let mut paths = Vec::new();
    collect_cleanup_paths(home, kind, &mut paths).map_err(|error| {
        VibexError::storage(
            "desktop_storage_cleanup_scan_failed",
            "failed to inspect local storage for cleanup",
        )
        .with_diagnostic("errorKind", format!("{:?}", error.kind()))
    })?;

    let mut removed_files = 0u64;
    let mut reclaimed_bytes = 0u64;
    for path in paths {
        let (files, bytes) = remove_path(&path).map_err(|error| {
            VibexError::storage(
                "desktop_storage_cleanup_failed",
                "failed to clear local storage",
            )
            .with_diagnostic("errorKind", format!("{:?}", error.kind()))
        })?;
        removed_files = removed_files.saturating_add(files);
        reclaimed_bytes = reclaimed_bytes.saturating_add(bytes);
    }
    Ok((removed_files, reclaimed_bytes))
}

fn collect_cleanup_paths(
    root: &Path,
    kind: FileCleanupKind,
    paths: &mut Vec<PathBuf>,
) -> io::Result<()> {
    // Cleanup owns a small, explicit set of top-level runtime directories. Do
    // not recursively match filenames in unrelated roots such as Relay or
    // provider state; those files may happen to contain a similar keyword.
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if is_agent_installation_path(relative) {
            continue;
        }
        if file_type.is_dir() {
            if should_remove_directory(relative, kind) {
                paths.push(path);
            } else {
                continue;
            }
        } else if file_type.is_file() && should_remove_file(relative, kind) {
            paths.push(path);
        }
    }
    Ok(())
}

fn is_agent_installation_path(relative: &Path) -> bool {
    relative
        .components()
        .next()
        .is_some_and(|component| component.as_os_str() == "acp-agents")
}

fn should_remove_directory(relative: &Path, kind: FileCleanupKind) -> bool {
    let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if relative.components().count() != 1 {
        return false;
    }
    match kind {
        FileCleanupKind::SessionsAndAttachments => matches!(
            name,
            "sessions" | "transcripts" | "clipboard-attachments" | "composer-edits"
        ),
        FileCleanupKind::Diagnostics => {
            name == "diagnostics" || name == "logs" || name.starts_with("backup-")
        }
    }
}

fn should_remove_file(relative: &Path, kind: FileCleanupKind) -> bool {
    if relative.components().count() != 1 {
        return false;
    }
    let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let folded = name.to_ascii_lowercase();
    match kind {
        FileCleanupKind::SessionsAndAttachments => false,
        FileCleanupKind::Diagnostics => {
            folded == "diagnostics.json"
                || folded.starts_with("diagnostic-")
                || folded.starts_with("diagnostics-")
                || folded.ends_with(".log")
                || folded.contains(".log.")
        }
    }
}

fn remove_path(path: &Path) -> io::Result<(u64, u64)> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || metadata.is_file() {
        let bytes = metadata.len();
        fs::remove_file(path)?;
        return Ok((1, bytes));
    }
    if !metadata.is_dir() {
        return Ok((0, 0));
    }

    let mut files = 0u64;
    let mut bytes = 0u64;
    for entry in fs::read_dir(path)? {
        let (child_files, child_bytes) = remove_path(&entry?.path())?;
        files = files.saturating_add(child_files);
        bytes = bytes.saturating_add(child_bytes);
    }
    fs::remove_dir(path)?;
    Ok((files, bytes))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn cleanup_paths_skip_managed_agent_installations() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("acp-agents/agents/codex")).unwrap();
        fs::write(
            root.path().join("acp-agents/agents/codex/session.log"),
            b"keep",
        )
        .unwrap();
        fs::create_dir_all(root.path().join("clipboard-attachments")).unwrap();
        fs::write(
            root.path().join("clipboard-attachments/image.png"),
            b"remove",
        )
        .unwrap();

        let (files, bytes) =
            clear_files(root.path(), FileCleanupKind::SessionsAndAttachments).unwrap();
        assert_eq!(files, 1);
        assert_eq!(bytes, 6);
        assert!(
            root.path()
                .join("acp-agents/agents/codex/session.log")
                .exists()
        );
        assert!(!root.path().join("clipboard-attachments").exists());
    }

    #[test]
    fn cleanup_paths_do_not_match_keywords_inside_unrelated_directories() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("relay")).unwrap();
        fs::write(root.path().join("relay/session.log"), b"keep").unwrap();
        fs::write(root.path().join("relay/upload.bin"), b"keep").unwrap();

        let (files, bytes) =
            clear_files(root.path(), FileCleanupKind::SessionsAndAttachments).unwrap();
        assert_eq!(files, 0);
        assert_eq!(bytes, 0);
        assert!(root.path().join("relay/session.log").exists());
        assert!(root.path().join("relay/upload.bin").exists());
    }

    #[test]
    fn diagnostic_cleanup_removes_exports_and_backup_directories() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("backup-latest")).unwrap();
        fs::write(root.path().join("backup-latest/state.json"), b"backup").unwrap();
        fs::write(root.path().join("diagnostics.json"), b"diagnostic").unwrap();
        fs::write(root.path().join("runtime.log"), b"log").unwrap();
        fs::write(root.path().join("keep.txt"), b"keep").unwrap();
        fs::write(root.path().join("catalog.json"), b"catalog").unwrap();

        let (files, bytes) = clear_files(root.path(), FileCleanupKind::Diagnostics).unwrap();
        assert_eq!(files, 3);
        assert_eq!(bytes, 19);
        assert!(!root.path().join("backup-latest").exists());
        assert!(!root.path().join("diagnostics.json").exists());
        assert!(!root.path().join("runtime.log").exists());
        assert!(root.path().join("keep.txt").exists());
        assert!(root.path().join("catalog.json").exists());
    }
}

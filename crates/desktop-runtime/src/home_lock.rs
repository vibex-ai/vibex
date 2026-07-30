use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::Serialize;
use vibex_core::{VibexError, VibexResult, unix_timestamp_ms};

pub const DESKTOP_RUNTIME_LOCK_FILE: &str = ".vibex-runtime.lock";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HomeLockMetadata<'a> {
    schema_version: &'static str,
    application_id: &'a str,
    process_id: u32,
    acquired_at_ms: i64,
}

#[derive(Debug)]
pub struct DesktopHomeLock {
    path: PathBuf,
    file: File,
}

impl DesktopHomeLock {
    pub fn acquire(home: &Path, application_id: &str) -> VibexResult<Self> {
        std::fs::create_dir_all(home).map_err(|error| {
            VibexError::storage(
                "desktop_runtime_home_create_failed",
                "failed to create the desktop runtime home",
            )
            .with_diagnostic("errorKind", format!("{:?}", error.kind()))
        })?;
        let path = home.join(DESKTOP_RUNTIME_LOCK_FILE);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|error| {
            VibexError::storage(
                "desktop_runtime_home_lock_open_failed",
                "failed to open the desktop runtime home lock",
            )
            .with_diagnostic("errorKind", format!("{:?}", error.kind()))
        })?;
        FileExt::try_lock_exclusive(&file).map_err(|error| {
            VibexError::conflict(
                "desktop_runtime_home_locked",
                "another Vibex desktop shell already controls this runtime home",
            )
            .with_diagnostic("applicationId", application_id)
            .with_diagnostic("errorKind", format!("{:?}", error.kind()))
        })?;
        let metadata = serde_json::to_vec(&HomeLockMetadata {
            schema_version: "desktop-home-lock.v1",
            application_id,
            process_id: std::process::id(),
            acquired_at_ms: unix_timestamp_ms(),
        })
        .map_err(|_| {
            VibexError::storage(
                "desktop_runtime_home_lock_encode_failed",
                "failed to encode desktop runtime home lock metadata",
            )
        })?;
        file.set_len(0)
            .and_then(|_| file.seek(SeekFrom::Start(0)))
            .and_then(|_| file.write_all(&metadata))
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                let _ = FileExt::unlock(&file);
                VibexError::storage(
                    "desktop_runtime_home_lock_write_failed",
                    "failed to publish desktop runtime home lock metadata",
                )
                .with_diagnostic("errorKind", format!("{:?}", error.kind()))
            })?;
        Ok(Self { path, file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DesktopHomeLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_home_is_exclusive_and_drop_releases_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let first = DesktopHomeLock::acquire(dir.path(), "dev.vibex.one").unwrap();
        let error = DesktopHomeLock::acquire(dir.path(), "dev.vibex.two").unwrap_err();
        assert_eq!(error.code, "desktop_runtime_home_locked");
        drop(first);
        DesktopHomeLock::acquire(dir.path(), "dev.vibex.two").unwrap();
    }

    #[test]
    fn separate_preview_home_does_not_contend_with_stable() {
        let dir = tempfile::tempdir().unwrap();
        let stable = dir.path().join("stable");
        let preview = dir.path().join("preview");
        let _stable = DesktopHomeLock::acquire(&stable, "dev.vibex.desktop").unwrap();
        let _preview = DesktopHomeLock::acquire(&preview, "dev.vibex.desktop.preview").unwrap();
    }
}

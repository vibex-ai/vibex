use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use vibex_core::{VibexError, VibexResult};

static PRIVATE_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn ensure_private_runtime_directory(path: &Path) -> VibexResult<()> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path).map_err(private_fs_error(
            "acp_private_directory_metadata_failed",
            "Private runtime directory metadata could not be read",
        ))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(VibexError::validation(
                "acp_private_directory_invalid",
                "Private runtime directory must be a real directory",
            ));
        }
    } else {
        fs::create_dir_all(path).map_err(private_fs_error(
            "acp_private_directory_create_failed",
            "Private runtime directory could not be created",
        ))?;
        let metadata = fs::symlink_metadata(path).map_err(private_fs_error(
            "acp_private_directory_metadata_failed",
            "Private runtime directory metadata could not be read",
        ))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(VibexError::validation(
                "acp_private_directory_invalid",
                "Private runtime directory must be a real directory",
            ));
        }
    }
    set_private_directory_permissions(path)
}

pub(crate) fn ensure_private_runtime_file(path: &Path) -> VibexResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(private_fs_error(
        "acp_private_file_metadata_failed",
        "Private runtime file metadata could not be read",
    ))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(VibexError::validation(
            "acp_private_file_invalid",
            "Private runtime file must be a real file",
        ));
    }
    set_private_file_permissions(path)
}

pub fn write_private_runtime_file_atomic(path: &Path, bytes: &[u8]) -> VibexResult<()> {
    let parent = path.parent().ok_or_else(|| {
        VibexError::validation(
            "acp_private_file_parent_missing",
            "Private runtime file must have a parent directory",
        )
    })?;
    ensure_private_runtime_directory(parent)?;
    if path.exists() {
        let metadata = fs::symlink_metadata(path).map_err(private_fs_error(
            "acp_private_file_metadata_failed",
            "Private runtime file metadata could not be read",
        ))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(VibexError::validation(
                "acp_private_file_invalid",
                "Private runtime file must be a real file",
            ));
        }
    }

    let temporary = private_temporary_path(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    configure_private_file_options(&mut options);
    let mut file = options.open(&temporary).map_err(private_fs_error(
        "acp_private_file_create_failed",
        "Private runtime temporary file could not be created",
    ))?;
    let result = (|| {
        file.write_all(bytes).map_err(private_fs_error(
            "acp_private_file_write_failed",
            "Private runtime file could not be written",
        ))?;
        file.sync_all().map_err(private_fs_error(
            "acp_private_file_sync_failed",
            "Private runtime file could not be synchronized",
        ))?;
        drop(file);
        fs::rename(&temporary, path).map_err(private_fs_error(
            "acp_private_file_publish_failed",
            "Private runtime file could not be published",
        ))?;
        ensure_private_runtime_file(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn private_temporary_path(path: &Path) -> VibexResult<PathBuf> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            VibexError::validation(
                "acp_private_file_name_invalid",
                "Private runtime file name is invalid",
            )
        })?;
    Ok(path.with_file_name(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        PRIVATE_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    )))
}

#[cfg(unix)]
fn configure_private_file_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn configure_private_file_options(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> VibexResult<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(private_fs_error(
        "acp_private_directory_permissions_failed",
        "Private runtime directory permissions could not be applied",
    ))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> VibexResult<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> VibexResult<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(private_fs_error(
        "acp_private_file_permissions_failed",
        "Private runtime file permissions could not be applied",
    ))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> VibexResult<()> {
    Ok(())
}

fn private_fs_error(
    code: &'static str,
    message: &'static str,
) -> impl Fn(std::io::Error) -> VibexError {
    move |error| {
        VibexError::storage(code, message).with_diagnostic("kind", error.kind().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[cfg(unix)]
    #[test]
    fn private_directory_and_atomic_file_tighten_modes() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let directory = temp.path().join("runtime-home");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
        ensure_private_runtime_directory(&directory).unwrap();
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let file = directory.join("config.json");
        write_private_runtime_file_atomic(&file, b"first").unwrap();
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        write_private_runtime_file_atomic(&file, b"second").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"second");
        assert!(fs::read_dir(&directory).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".config.json.tmp-")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn private_paths_reject_final_symlinks_without_touching_targets() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let target_directory = temp.path().join("target-directory");
        fs::create_dir(&target_directory).unwrap();
        let directory_link = temp.path().join("directory-link");
        symlink(&target_directory, &directory_link).unwrap();
        assert_eq!(
            ensure_private_runtime_directory(&directory_link)
                .unwrap_err()
                .code,
            "acp_private_directory_invalid"
        );

        let directory = temp.path().join("runtime-home");
        ensure_private_runtime_directory(&directory).unwrap();
        let target_file = temp.path().join("target-file");
        fs::write(&target_file, b"untouched").unwrap();
        let file_link = directory.join("config.json");
        symlink(&target_file, &file_link).unwrap();
        assert_eq!(
            write_private_runtime_file_atomic(&file_link, b"changed")
                .unwrap_err()
                .code,
            "acp_private_file_invalid"
        );
        assert_eq!(fs::read(target_file).unwrap(), b"untouched");
    }
}

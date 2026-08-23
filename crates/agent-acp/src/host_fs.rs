//! Containment and atomicity for the ACP client filesystem host
//! (`fs/read_text_file`, `fs/write_text_file`).
//!
//! Reads stay deliberately permissive: the same runtime also exposes
//! `terminal/*`, so an agent denied a read simply re-runs `cat` in a shell and
//! the restriction buys nothing except a harder-to-audit code path. Writes are
//! the real boundary, because a write escapes the workspace permanently.
//!
//! Write rules enforced here:
//! - the request path must be absolute;
//! - `..` never traverses outside a root: containment is checked against the
//!   canonical form of the nearest existing ancestor, before any directory is
//!   created;
//! - a final symlink is resolved and its target must itself be contained, so
//!   a link planted inside the workspace cannot redirect a write outside it;
//! - publication is atomic (temporary file in the destination directory,
//!   `sync_all`, then rename), so a crashed agent never leaves a half-written
//!   source file behind.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use vibex_core::{VibexError, VibexResult};

static HOST_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Upper bound on a single `fs/write_text_file` payload. Larger edits belong
/// to the agent's own tooling, not to the host bridge.
pub(crate) const ACP_FS_WRITE_BYTE_LIMIT: usize = 2 * 1024 * 1024;

/// Resolve `path` to the location that may actually be written.
///
/// Returns the canonical destination, or a validation error when the path
/// leaves every configured root.
pub(crate) fn resolve_contained_write_path(path: &Path, roots: &[PathBuf]) -> VibexResult<PathBuf> {
    if !path.is_absolute() {
        return Err(write_denied(path, "path must be absolute"));
    }
    let roots = canonical_roots(roots);
    if roots.is_empty() {
        return Err(write_denied(path, "no writable root is configured"));
    }

    // An existing destination is resolved through symlinks so a planted link
    // cannot redirect the write; a new file is resolved against its nearest
    // existing ancestor so containment is decided before any mkdir.
    let resolved = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::canonicalize(path).map_err(|error| {
                write_denied(path, &format!("symlink target is unresolvable: {error}"))
            })?
        }
        Ok(_) => fs::canonicalize(path)
            .map_err(|error| write_denied(path, &format!("path is unresolvable: {error}")))?,
        Err(_) => resolve_against_existing_ancestor(path)?,
    };

    if !roots.iter().any(|root| resolved.starts_with(root)) {
        return Err(write_denied(path, "path is outside every writable root"));
    }
    Ok(resolved)
}

/// Atomically publish `contents` at an already contained `path`.
pub(crate) fn write_host_file_atomic(path: &Path, contents: &str) -> VibexResult<()> {
    if contents.len() > ACP_FS_WRITE_BYTE_LIMIT {
        return Err(VibexError::validation(
            "acp_fs_write_too_large",
            "ACP write exceeds the host write size limit",
        )
        .with_diagnostic("bytes", contents.len().to_string()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| write_denied(path, "path has no parent directory"))?;
    fs::create_dir_all(parent).map_err(host_fs_error(
        "acp_fs_write_parent_failed",
        "ACP write parent directory could not be created",
    ))?;

    let temporary = host_temporary_path(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(&temporary).map_err(host_fs_error(
        "acp_fs_write_create_failed",
        "ACP write temporary file could not be created",
    ))?;
    let result = (|| {
        file.write_all(contents.as_bytes()).map_err(host_fs_error(
            "acp_fs_write_failed",
            "ACP write could not be completed",
        ))?;
        file.sync_all().map_err(host_fs_error(
            "acp_fs_write_sync_failed",
            "ACP write could not be synchronized",
        ))?;
        drop(file);
        fs::rename(&temporary, path).map_err(host_fs_error(
            "acp_fs_write_publish_failed",
            "ACP write could not be published",
        ))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn resolve_against_existing_ancestor(path: &Path) -> VibexResult<PathBuf> {
    let mut trailing: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cursor = path;
    loop {
        match fs::canonicalize(cursor) {
            Ok(base) => {
                let mut resolved = base;
                for component in trailing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(_) => {
                let name = cursor
                    .file_name()
                    .ok_or_else(|| write_denied(path, "no existing ancestor directory"))?;
                // `..` is only safe once the prefix is canonical; an
                // unresolvable parent therefore ends the walk.
                if matches!(
                    cursor.components().next_back(),
                    Some(Component::ParentDir | Component::CurDir)
                ) {
                    return Err(write_denied(path, "path traverses an unresolved parent"));
                }
                trailing.push(name);
                cursor = cursor
                    .parent()
                    .ok_or_else(|| write_denied(path, "no existing ancestor directory"))?;
            }
        }
    }
}

fn canonical_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut canonical = Vec::with_capacity(roots.len());
    for root in roots {
        if let Ok(resolved) = fs::canonicalize(root)
            && !canonical.contains(&resolved)
        {
            canonical.push(resolved);
        }
    }
    canonical
}

fn host_temporary_path(path: &Path) -> VibexResult<PathBuf> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| write_denied(path, "file name is invalid"))?;
    Ok(path.with_file_name(format!(
        ".{name}.acp-tmp-{}-{}",
        std::process::id(),
        HOST_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    )))
}

fn write_denied(path: &Path, reason: &str) -> VibexError {
    VibexError::validation(
        "acp_fs_write_denied",
        "ACP write target is outside the allowed roots",
    )
    .with_diagnostic("reason", reason.to_string())
    .with_diagnostic("path", path.display().to_string())
}

fn host_fs_error(
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

    #[test]
    fn writes_inside_a_root_resolve_and_publish_atomically() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let target = root.join("nested/dir/file.txt");

        let resolved = resolve_contained_write_path(&target, std::slice::from_ref(&root)).unwrap();
        write_host_file_atomic(&resolved, "hello").unwrap();
        assert_eq!(fs::read_to_string(&resolved).unwrap(), "hello");

        write_host_file_atomic(&resolved, "second").unwrap();
        assert_eq!(fs::read_to_string(&resolved).unwrap(), "second");
        assert!(
            fs::read_dir(resolved.parent().unwrap())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".acp-tmp-"))
        );
    }

    #[test]
    fn writes_outside_every_root_are_denied() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let outside = temp.path().join("outside.txt");

        let error =
            resolve_contained_write_path(&outside, std::slice::from_ref(&root)).unwrap_err();
        assert_eq!(error.code, "acp_fs_write_denied");

        let traversal = root.join("../outside.txt");
        assert_eq!(
            resolve_contained_write_path(&traversal, std::slice::from_ref(&root))
                .unwrap_err()
                .code,
            "acp_fs_write_denied"
        );
        assert_eq!(
            resolve_contained_write_path(Path::new("relative.txt"), std::slice::from_ref(&root))
                .unwrap_err()
                .code,
            "acp_fs_write_denied"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_redirect_a_write_outside_a_root() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let root = temp.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, "untouched").unwrap();
        let link = root.join("link.txt");
        symlink(&outside, &link).unwrap();

        assert_eq!(
            resolve_contained_write_path(&link, std::slice::from_ref(&root))
                .unwrap_err()
                .code,
            "acp_fs_write_denied"
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), "untouched");

        // A link that stays inside the root remains writable through its
        // resolved target.
        let inside = root.join("real.txt");
        fs::write(&inside, "old").unwrap();
        let inside_link = root.join("alias.txt");
        symlink(&inside, &inside_link).unwrap();
        let resolved =
            resolve_contained_write_path(&inside_link, std::slice::from_ref(&root)).unwrap();
        write_host_file_atomic(&resolved, "new").unwrap();
        assert_eq!(fs::read_to_string(&inside).unwrap(), "new");
    }

    #[test]
    fn oversized_payloads_are_rejected() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("big.txt");
        let payload = "x".repeat(ACP_FS_WRITE_BYTE_LIMIT + 1);
        assert_eq!(
            write_host_file_atomic(&target, &payload).unwrap_err().code,
            "acp_fs_write_too_large"
        );
        assert!(!target.exists());
    }
}

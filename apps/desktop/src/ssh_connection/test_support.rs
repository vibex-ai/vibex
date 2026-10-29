//! Test-only filesystem helpers.

#[cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
use std::sync::{Mutex, MutexGuard};

/// Writes an executable through a temporary path and renames it into place.
///
/// Executing a path that is still open for writing returns `ETXTBSY`; under a
/// parallel test binary that can race even after `fs::write` returns on some
/// filesystems. Renaming a fully written file avoids that class of flake.
static NEXT_SCRIPT: AtomicU64 = AtomicU64::new(0);

/// Returns a process-unique filename inside `dir`. Tests that rewrite the same
/// executable path can race a still-executing child and hit `ETXTBSY`; one
/// filename per fixture avoids that.
pub fn unique_path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(format!(
        "{stem}-{}",
        NEXT_SCRIPT.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(unix)]
pub fn write_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;

    use std::io::Write as _;

    let temporary = path.with_extension("tmp");
    let mut file = fs::File::create(&temporary).unwrap();
    file.write_all(body.as_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&temporary, path).unwrap();
}

#[cfg(test)]
static PROCESS_LOCK: Mutex<()> = Mutex::new(());

/// Serializes test subprocess spawns. Some filesystems report `ETXTBSY` when
/// many freshly written script fixtures are executed concurrently; production
/// SSH/keygen binaries are not rewritten, so this is test-only.
#[cfg(test)]
pub fn process_guard() -> MutexGuard<'static, ()> {
    PROCESS_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

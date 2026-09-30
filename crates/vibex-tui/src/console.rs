//! Console ownership while the interface is on screen.
//!
//! Entering the alternate screen takes the *screen*, not the *process*. The
//! authority seat boots a `DesktopRuntime` in this process, and components
//! report progress and failures with `eprintln!`: the runtime's startup stages,
//! for instance, finish on background tasks well after the first frame is
//! painted. A byte written like that lands in the middle of a frame, wraps at
//! the last column and can scroll the alternate screen — the residue a
//! character-grid client must never show.
//!
//! So the interface diverts the process's `stderr` into a spill file for as
//! long as it owns the terminal, and hands the terminal's own descriptor back
//! when it leaves. Nothing disappears in silence: [`captured`] reports the
//! spill file, and the client says where the output went once the screen is
//! restored.
//!
//! The diversion is process-wide because `eprintln!` writes to the descriptor
//! itself, not to a handle this crate could wrap. It is also best-effort: when
//! the spill file cannot be created the interface still starts, and the
//! previous behaviour — diagnostics on the terminal — stands.
//!
//! `stdout` is deliberately left alone: it is the renderer's, and diverting it
//! would divert the frames too.

use std::io;
use std::path::PathBuf;

/// Chooses the spill file. Without it the file is
/// `std::env::temp_dir()/vibex-tui-<pid>.log`.
pub const LOG_ENVIRONMENT: &str = "VIBEX_TUI_LOG";

/// The spill file for this process, installed once and reused across terminal
/// hand-offs.
///
/// It outlives a single [`divert_stderr`] / [`restore_stderr`] pair because
/// `$EDITOR` releases the guard and re-acquires it: the same file has to keep
/// collecting output instead of being truncated on the way back.
#[cfg(unix)]
static SPILL: std::sync::Mutex<Option<Spill>> = std::sync::Mutex::new(None);

#[cfg(unix)]
struct Spill {
    /// The terminal's own descriptor, kept for the restore.
    terminal: rustix::fd::OwnedFd,
    /// The open spill file; `stderr` is re-pointed at this descriptor.
    file: std::fs::File,
    path: PathBuf,
}

#[cfg(unix)]
fn spill() -> std::sync::MutexGuard<'static, Option<Spill>> {
    // A panic while the interface owns the terminal must not make the restore
    // impossible, so a poisoned lock is recovered rather than unwrapped.
    SPILL.lock().unwrap_or_else(|error| error.into_inner())
}

#[cfg(unix)]
fn spill_path() -> PathBuf {
    std::env::var_os(LOG_ENVIRONMENT)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("vibex-tui-{}.log", std::process::id()))
        })
}

/// Point the process's `stderr` at the spill file, creating it on first use.
///
/// Idempotent: a second call after [`restore_stderr`] re-points `stderr` at the
/// same file rather than starting a new one.
#[cfg(unix)]
pub fn divert_stderr() -> io::Result<()> {
    let mut spill = spill();
    if spill.is_none() {
        let path = spill_path();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)?;
        let terminal = rustix::io::dup(rustix::stdio::stderr()).map_err(io::Error::from)?;
        *spill = Some(Spill {
            terminal,
            file,
            path,
        });
    }
    let installed = spill.as_ref().expect("the spill was just installed");
    rustix::stdio::dup2_stderr(&installed.file).map_err(io::Error::from)
}

/// Hand `stderr` back to the terminal. Idempotent.
#[cfg(unix)]
pub fn restore_stderr() {
    let spill = spill();
    let Some(installed) = spill.as_ref() else {
        return;
    };
    // Failing here leaves the interface unable to report anything, but there is
    // no second strategy to fall back to, and the caller is on its way out.
    let _ = rustix::stdio::dup2_stderr(&installed.terminal);
}

/// The spill file and its size, once output has been diverted into it.
///
/// `None` means nothing was written to `stderr` while the interface owned the
/// terminal, which is the normal case for a remote seat.
#[cfg(unix)]
pub fn captured() -> Option<(PathBuf, u64)> {
    let spill = spill();
    let installed = spill.as_ref()?;
    let bytes = installed
        .file
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    (bytes > 0).then(|| (installed.path.clone(), bytes))
}

/// Point the process's `stderr` at the spill file.
///
/// The diversion is a descriptor operation; without one there is nothing to
/// divert and the interface keeps the historical behaviour.
#[cfg(not(unix))]
pub fn divert_stderr() -> io::Result<()> {
    Ok(())
}

/// Hand `stderr` back to the terminal.
#[cfg(not(unix))]
pub fn restore_stderr() {}

/// The spill file and its size, once output has been diverted into it.
///
/// Nothing is diverted without a descriptor layer, so there is never a file.
#[cfg(not(unix))]
pub fn captured() -> Option<(PathBuf, u64)> {
    None
}

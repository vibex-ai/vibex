//! The PTY a terminal-mode session runs in.
//!
//! `vibex-agent` must not depend on `vibex-terminal`: the manager decides
//! *which* Agent to run and *what* arguments it needs, while the composition
//! root owns the process. This module is the seam between them.
//!
//! The launcher contract is deliberately narrow. It starts one argv in one
//! workspace root and reports the terminal id; everything else about the
//! process — ring buffers, resize, kill escalation — belongs to the terminal
//! crate. A launcher that cannot honour the request says so with a typed error
//! instead of returning a handle to nothing.

use std::path::PathBuf;

use vibex_core::{
    TerminalId, TerminalSessionHandle, TerminalSessionLaunchSpec, VibexError, VibexResult,
    WorkspaceId,
};

/// Starts and stops the PTY behind a terminal-mode session.
pub trait SessionTerminalLauncher: Send + Sync + 'static {
    /// Starts the Agent's native interface.
    ///
    /// The implementation must not interpolate `argv` through a shell: the
    /// arguments come from Vibex, and a shell would turn a path with a space or
    /// a quote into a different command.
    fn launch(&self, spec: &TerminalSessionLaunchSpec) -> VibexResult<TerminalSessionHandle>;

    /// Stops the process group behind a terminal.
    ///
    /// Escalation from a graceful signal to a kill is the launcher's business;
    /// callers only need the process to be gone when this returns.
    fn kill(&self, terminal_id: &TerminalId) -> VibexResult<()>;

    /// Whether the PTY is still running.
    fn is_running(&self, terminal_id: &TerminalId) -> bool;

    /// Rebuilds a PTY whose process is gone, keeping the recorded argv.
    ///
    /// Returns `Ok(None)` when the launcher cannot restore this terminal, which
    /// a caller reports as an error session rather than as a silent no-op.
    fn restore(
        &self,
        spec: &TerminalSessionLaunchSpec,
        previous: &TerminalId,
    ) -> VibexResult<Option<TerminalSessionHandle>> {
        let _ = (spec, previous);
        Ok(None)
    }
}

/// A launcher installed by a test or by a composition root that has no PTY yet.
///
/// It records what it was asked to start, so a caller can prove the argv it
/// resolved without spawning a process.
#[derive(Debug, Default)]
pub struct RecordingTerminalLauncher {
    launches: std::sync::Mutex<Vec<TerminalSessionLaunchSpec>>,
    running: std::sync::Mutex<std::collections::HashSet<TerminalId>>,
}

impl RecordingTerminalLauncher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn launches(&self) -> Vec<TerminalSessionLaunchSpec> {
        self.launches
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl SessionTerminalLauncher for RecordingTerminalLauncher {
    fn launch(&self, spec: &TerminalSessionLaunchSpec) -> VibexResult<TerminalSessionHandle> {
        if spec.argv.is_empty() {
            return Err(VibexError::validation(
                "terminal_session_argv_empty",
                "a terminal session needs a command to run",
            ));
        }
        self.launches
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(spec.clone());
        let terminal_id = TerminalId::new();
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(terminal_id.clone());
        Ok(TerminalSessionHandle { terminal_id })
    }

    fn kill(&self, terminal_id: &TerminalId) -> VibexResult<()> {
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(terminal_id);
        Ok(())
    }

    fn is_running(&self, terminal_id: &TerminalId) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(terminal_id)
    }

    fn restore(
        &self,
        spec: &TerminalSessionLaunchSpec,
        previous: &TerminalId,
    ) -> VibexResult<Option<TerminalSessionHandle>> {
        let _ = self.kill(previous);
        self.launch(spec).map(Some)
    }
}

/// The workspace a terminal-mode session was started in.
///
/// A session's root is a path; the terminal domain keys on an id. Keeping the
/// pair together is what stops a launcher from inventing a `WorkspaceId`, which
/// the terminal-table foreign key would then reject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalWorkspace {
    pub workspace_id: WorkspaceId,
    pub root: PathBuf,
}

impl TerminalWorkspace {
    pub fn new(workspace_id: WorkspaceId, root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_id,
            root: root.into(),
        }
    }
}

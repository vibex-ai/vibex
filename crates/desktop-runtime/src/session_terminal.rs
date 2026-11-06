//! The PTY host for terminal-mode Agent sessions.
//!
//! A terminal session runs the Agent's own interactive CLI, so the runtime has
//! to start a process with an explicit argv — not a shell, and not a command
//! string. That is the one thing [`vibex_agent::SessionTerminalLauncher`] needs
//! and the one thing the ACP terminal host cannot provide, because its
//! terminals belong to a tool call rather than to a session the reader returns
//! to.
//!
//! The launcher is intentionally thin: argument validation, process start, and
//! kill escalation stay in `vibex-terminal`, which already owns process groups
//! and the PTY actor.

use std::path::PathBuf;
use std::sync::Arc;

use vibex_agent::SessionTerminalLauncher;
use vibex_core::{
    TerminalId, TerminalSessionHandle, TerminalSessionLaunchSpec, VibexError, VibexResult,
};
use vibex_terminal::{TerminalCommandRequest, TerminalManager};

/// Starts terminal-mode Agents in the runtime's shared terminal manager.
#[derive(Clone)]
pub struct TerminalManagerSessionLauncher {
    manager: TerminalManager,
    /// The observation set the runtime also reads for its periodic sweep.
    ///
    /// Shared rather than created here because the sweep and the byte tap must
    /// agree on which terminals are watched; two sets would publish from one and
    /// sweep the other.
    observations:
        std::sync::Arc<std::sync::Mutex<crate::terminal_observation::TerminalObservations>>,
}

impl TerminalManagerSessionLauncher {
    pub fn new(
        manager: TerminalManager,
        observations: std::sync::Arc<
            std::sync::Mutex<crate::terminal_observation::TerminalObservations>,
        >,
    ) -> Arc<Self> {
        Arc::new(Self {
            manager,
            observations,
        })
    }

    /// The same launcher behind the trait the Agent manager installs.
    pub fn shared(
        manager: TerminalManager,
        observations: std::sync::Arc<
            std::sync::Mutex<crate::terminal_observation::TerminalObservations>,
        >,
    ) -> Arc<dyn SessionTerminalLauncher> {
        Self::new(manager, observations)
    }

    fn request(spec: &TerminalSessionLaunchSpec) -> VibexResult<TerminalCommandRequest> {
        let (command, args) = spec.argv.split_first().ok_or_else(|| {
            VibexError::validation(
                "terminal_session_argv_empty",
                "a terminal session needs a command to run",
            )
        })?;
        if command.trim().is_empty() {
            return Err(VibexError::validation(
                "terminal_session_argv_empty",
                "a terminal session needs a command to run",
            ));
        }
        Ok(TerminalCommandRequest {
            workspace_id: spec.workspace_id.clone(),
            title: Some(spec.title.clone()),
            command: command.clone(),
            args: args.to_vec(),
            cwd: Some(spec.workspace_root.clone()),
            env: spec.env.clone(),
            rows: spec.rows.clamp(1, u16::MAX),
            cols: spec.cols.clamp(1, u16::MAX),
        })
    }
}

/// Registers a session's terminal for observation.
///
/// The tap runs on the PTY reader thread and only reads the bytes into the
/// emulator, queueing whatever reading changed; the runtime's sweep is what
/// persists and broadcasts. Kept as one function so the launcher and the
/// runtime's own `watch_session_terminal` cannot drift.
pub fn watch_session_terminal(
    manager: &TerminalManager,
    observations: &std::sync::Arc<
        std::sync::Mutex<crate::terminal_observation::TerminalObservations>,
    >,
    terminal_id: &TerminalId,
    session_id: &str,
    agent_id: &vibex_core::AgentId,
    rows: u16,
    cols: u16,
) {
    {
        let Ok(mut observations) = observations.lock() else {
            return;
        };
        observations.watch(
            terminal_id.clone(),
            session_id,
            agent_id.clone(),
            rows,
            cols,
        );
    }
    let observations = std::sync::Arc::clone(observations);
    let watched = terminal_id.clone();
    manager.set_output_tap(
        terminal_id,
        std::sync::Arc::new(move |_, bytes: &[u8]| {
            if let Ok(mut observations) = observations.lock() {
                observations.observe(&watched, bytes, vibex_core::unix_timestamp_ms());
            }
        }),
    );
}

impl SessionTerminalLauncher for TerminalManagerSessionLauncher {
    fn launch(&self, spec: &TerminalSessionLaunchSpec) -> VibexResult<TerminalSessionHandle> {
        let root = PathBuf::from(&spec.workspace_root);
        let session = self
            .manager
            .create_command_persistent(&root, Self::request(spec)?)?;
        Ok(TerminalSessionHandle {
            terminal_id: session.id,
        })
    }

    fn kill(&self, terminal_id: &TerminalId) -> VibexResult<()> {
        match self.manager.kill_retaining(terminal_id) {
            Ok(_) => Ok(()),
            Err(error) if error.code == "terminal_not_found" => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn watch_session(
        &self,
        terminal_id: &TerminalId,
        session_id: &str,
        agent_id: &vibex_core::AgentId,
        rows: u16,
        cols: u16,
    ) {
        watch_session_terminal(
            &self.manager,
            &self.observations,
            terminal_id,
            session_id,
            agent_id,
            rows,
            cols,
        );
    }

    fn unwatch_session(&self, terminal_id: &TerminalId) {
        self.manager.clear_output_tap(terminal_id);
        if let Ok(mut observations) = self.observations.lock() {
            observations.unwatch(terminal_id);
        }
    }

    fn is_running(&self, terminal_id: &TerminalId) -> bool {
        self.manager
            .snapshot(terminal_id)
            .map(|snapshot| snapshot.session.status == vibex_core::TerminalStatus::Running)
            .unwrap_or(false)
    }

    /// Rebuilds the PTY from the recorded argv.
    ///
    /// A terminal-mode Agent was started with arguments — a resume id, a model
    /// override — and rebuilding it from the shell name alone would silently
    /// start a different session.
    fn restore(
        &self,
        spec: &TerminalSessionLaunchSpec,
        previous: &TerminalId,
    ) -> VibexResult<Option<TerminalSessionHandle>> {
        if let Ok(previous_session) = self.manager.snapshot(previous) {
            let root = PathBuf::from(&previous_session.session.cwd);
            let argv = spec.argv.clone();
            let env = spec.env.clone();
            let mut session = previous_session.session;
            session.workspace_id = spec.workspace_id.clone();
            let restored = self.manager.restore_command(&root, session, &argv, env)?;
            return Ok(Some(TerminalSessionHandle {
                terminal_id: restored.id,
            }));
        }
        let root = PathBuf::from(&spec.workspace_root);
        let session = self
            .manager
            .create_command_persistent(&root, Self::request(spec)?)?;
        Ok(Some(TerminalSessionHandle {
            terminal_id: session.id,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_spec_needs_an_executable() {
        let launcher = TerminalManagerSessionLauncher {
            manager: TerminalManager::new(),
            observations: std::sync::Arc::new(std::sync::Mutex::new(
                crate::terminal_observation::TerminalObservations::new(),
            )),
        };
        let spec = TerminalSessionLaunchSpec {
            workspace_id: vibex_core::WorkspaceId::new(),
            workspace_root: std::env::temp_dir().to_string_lossy().into_owned(),
            argv: Vec::new(),
            env: Vec::new(),
            title: "terminal".to_string(),
            rows: 24,
            cols: 100,
        };
        let error = launcher.launch(&spec).unwrap_err();
        assert_eq!(error.code, "terminal_session_argv_empty");
    }

    #[test]
    fn an_explicit_argv_is_passed_through_without_a_shell() {
        let spec = TerminalSessionLaunchSpec {
            workspace_id: vibex_core::WorkspaceId::new(),
            workspace_root: "/tmp".to_string(),
            argv: vec![
                "claude".to_string(),
                "--resume".to_string(),
                "abc".to_string(),
            ],
            env: Vec::new(),
            title: "terminal".to_string(),
            rows: 24,
            cols: 100,
        };
        let request = TerminalManagerSessionLauncher::request(&spec).unwrap();
        assert_eq!(request.command, "claude");
        assert_eq!(
            request.args,
            vec!["--resume".to_string(), "abc".to_string()]
        );
    }
}

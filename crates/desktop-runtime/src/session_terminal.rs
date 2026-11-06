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
    TerminalCli, TerminalId, TerminalSessionHandle, TerminalSessionLaunchSpec, VibexError,
    VibexResult,
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
    /// Resolves an Agent's native CLI inside a Vibex-managed installation.
    ///
    /// `None` is the test and headless-host shape: the recorded command is run
    /// as written, exactly as an externally installed CLI would be.
    managed_clis: Option<Arc<crate::AgentInstallService>>,
}

impl TerminalManagerSessionLauncher {
    /// A launcher that can also start CLIs inside Vibex-managed installs.
    pub fn with_managed_clis(
        manager: TerminalManager,
        observations: std::sync::Arc<
            std::sync::Mutex<crate::terminal_observation::TerminalObservations>,
        >,
        managed_clis: Arc<crate::AgentInstallService>,
    ) -> Arc<Self> {
        Arc::new(Self {
            manager,
            observations,
            managed_clis: Some(managed_clis),
        })
    }

    /// The same launcher behind the trait the Agent manager installs.
    pub fn shared(
        manager: TerminalManager,
        observations: std::sync::Arc<
            std::sync::Mutex<crate::terminal_observation::TerminalObservations>,
        >,
        managed_clis: Arc<crate::AgentInstallService>,
    ) -> Arc<dyn SessionTerminalLauncher> {
        Self::with_managed_clis(manager, observations, managed_clis)
    }

    /// The argv and environment the PTY should actually receive.
    ///
    /// The recorded argv keeps the logical command name (`gemini`, `cline`, …)
    /// so a session created before an installation moved still describes the
    /// same command. Resolution happens here, at every launch, and only when the
    /// Agent's managed installation currently contains a native entry point.
    fn prepare(&self, spec: &TerminalSessionLaunchSpec) -> (Vec<String>, Vec<(String, String)>) {
        let Some(managed_clis) = self.managed_clis.as_ref() else {
            return (spec.argv.clone(), spec.env.clone());
        };
        let Some(cli) = TerminalCli::for_agent(&spec.agent_id) else {
            return (spec.argv.clone(), spec.env.clone());
        };
        let Some(command) = spec.argv.first() else {
            return (spec.argv.clone(), spec.env.clone());
        };
        if command != cli.executable() {
            // A caller-supplied argv that does not start with the Agent's own
            // command is already explicit; do not reinterpret it.
            return (spec.argv.clone(), spec.env.clone());
        }
        let Some(launch) = managed_clis.native_cli_launch(&spec.agent_id) else {
            return (spec.argv.clone(), spec.env.clone());
        };
        let mut argv = Vec::with_capacity(launch.arguments.len() + spec.argv.len());
        argv.push(launch.program);
        argv.extend(launch.arguments);
        argv.extend(spec.argv.iter().skip(1).cloned());
        let mut env = spec.env.clone();
        if !launch.path_entries.is_empty() {
            let separator = if cfg!(windows) { ';' } else { ':' };
            let inherited = env
                .iter()
                .find(|(key, _)| key == "PATH")
                .map(|(_, value)| value.clone())
                .or_else(|| std::env::var("PATH").ok())
                .unwrap_or_default();
            let mut path = launch.path_entries.join(&separator.to_string());
            if !inherited.is_empty() {
                path.push(separator);
                path.push_str(&inherited);
            }
            env.retain(|(key, _)| key != "PATH");
            env.push(("PATH".to_string(), path));
        }
        (argv, env)
    }

    fn request_for(&self, spec: &TerminalSessionLaunchSpec) -> VibexResult<TerminalCommandRequest> {
        let (argv, env) = self.prepare(spec);
        request_from_argv(spec, &argv, env)
    }
}

/// Builds the PTY request from the argv that will actually run.
fn request_from_argv(
    spec: &TerminalSessionLaunchSpec,
    argv: &[String],
    env: Vec<(String, String)>,
) -> VibexResult<TerminalCommandRequest> {
    let (command, args) = argv.split_first().ok_or_else(|| {
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
        env,
        rows: spec.rows.clamp(1, u16::MAX),
        cols: spec.cols.clamp(1, u16::MAX),
    })
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
            .create_command_persistent(&root, self.request_for(spec)?)?;
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
            let (argv, env) = self.prepare(spec);
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
            .create_command_persistent(&root, self.request_for(spec)?)?;
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
            managed_clis: None,
        };
        let spec = TerminalSessionLaunchSpec {
            agent_id: vibex_core::AgentId::parse("claude").unwrap(),
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

    /// A managed installation replaces the logical command with the tree's
    /// native CLI, and its `node_modules/.bin` reaches the child's `PATH`.
    #[cfg(unix)]
    #[test]
    fn a_managed_install_is_resolved_before_the_pty_starts() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("vibex.db");
        let root = temp.path().join("managed-agents");
        let service = crate::AgentInstallService::new(
            &db_path,
            &root,
            vibex_config_switch::ProviderConfigService::new(&db_path),
        )
        .unwrap();
        let node = root.join("runtimes/node/external/bin/node");
        std::fs::create_dir_all(node.parent().unwrap()).unwrap();
        std::fs::write(&node, b"fixture").unwrap();
        let install_root = root.join("agents/gemini/1.0.0-fixture");
        let script = install_root.join("node_modules/gemini.js");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, b"// gemini").unwrap();
        let launch_dir = install_root.join("node_modules/.bin");
        std::fs::create_dir_all(&launch_dir).unwrap();
        std::fs::write(
            install_root.join("vibex-install.json"),
            serde_json::json!({
                "registryAgentId": "gemini",
                "version": "1.0.0",
                "fingerprint": "fixture",
                "distributionKind": "npm",
                "launch": {
                    "type": "node",
                    "node": node.to_string_lossy(),
                    "script": "node_modules/gemini.js",
                    "args": ["--acp"],
                },
            })
            .to_string(),
        )
        .unwrap();
        {
            let mut conn = vibex_db::open_database(&db_path).unwrap();
            vibex_db::apply_migrations(&mut conn).unwrap();
            vibex_db::AgentManagedInstallationRepository::upsert(
                &conn,
                &vibex_db::AgentManagedInstallationRecord {
                    agent_id: vibex_core::AgentId::parse("gemini").unwrap(),
                    registry_agent_id: "gemini".to_string(),
                    state: vibex_core::AgentManagedInstallState {
                        managed: true,
                        status: vibex_core::AgentManagedInstallStatus::Installed,
                        distribution_kind: Some(vibex_core::AgentManagedDistributionKind::Npm),
                        installed_version: Some("1.0.0".to_string()),
                        available_version: Some("1.0.0".to_string()),
                        last_error_code: None,
                        last_error_message: None,
                        updated_at_ms: Some(vibex_core::unix_timestamp_ms()),
                    },
                    command: Some(vibex_core::AgentCommandConfig {
                        command: node.to_string_lossy().into_owned(),
                        args: vec![script.to_string_lossy().into_owned(), "--acp".to_string()],
                    }),
                    install_root: Some(install_root.to_string_lossy().into_owned()),
                    updated_at_ms: vibex_core::unix_timestamp_ms(),
                },
            )
            .unwrap();
        }
        let launcher = TerminalManagerSessionLauncher::with_managed_clis(
            TerminalManager::new(),
            std::sync::Arc::new(std::sync::Mutex::new(
                crate::terminal_observation::TerminalObservations::new(),
            )),
            std::sync::Arc::new(service),
        );
        let spec = TerminalSessionLaunchSpec {
            agent_id: vibex_core::AgentId::parse("gemini").unwrap(),
            workspace_id: vibex_core::WorkspaceId::new(),
            workspace_root: temp.path().to_string_lossy().into_owned(),
            argv: vec!["gemini".to_string(), "--model".to_string(), "m".to_string()],
            env: Vec::new(),
            title: "gemini".to_string(),
            rows: 24,
            cols: 100,
        };
        let (argv, env) = launcher.prepare(&spec);
        assert_eq!(argv[0], node.to_string_lossy());
        assert_eq!(argv[1], script.to_string_lossy());
        assert_eq!(&argv[2..], ["--model", "m"]);
        let path = env
            .iter()
            .find(|(key, _)| key == "PATH")
            .map(|(_, value)| value.clone())
            .expect("a managed launch prepends its bin directories");
        assert!(path.contains("node_modules/.bin"));
        assert!(path.contains("runtimes/node"));
    }

    #[test]
    fn an_explicit_argv_is_passed_through_without_a_shell() {
        let spec = TerminalSessionLaunchSpec {
            agent_id: vibex_core::AgentId::parse("claude").unwrap(),
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
        let request = request_from_argv(&spec, &spec.argv, spec.env.clone()).unwrap();
        assert_eq!(request.command, "claude");
        assert_eq!(
            request.args,
            vec!["--resume".to_string(), "abc".to_string()]
        );
    }
}

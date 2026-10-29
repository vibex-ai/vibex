//! Remove an installed SSH runtime.
//!
//! This is separate from removing a connection: it stops the remote process,
//! unloads a macOS LaunchAgent, and removes version directories. Runtime data
//! (`<install-root>/home`) is preserved unless the caller explicitly asks to
//! delete it; workspace files outside the install root are never touched.

use std::fmt;

use super::command::SshCommand;
use super::launch::LaunchConfig;
use super::launchd;
use super::supervisor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallOutcome {
    pub install_root: String,
    pub runtime_home: String,
    pub home_preserved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallError {
    pub code: &'static str,
    pub message: String,
}

impl UninstallError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for UninstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for UninstallError {}

/// Stops the runtime, unloads its LaunchAgent if present, and removes the
/// install root while optionally preserving `install-root/home`.
pub fn uninstall_runtime(
    command: &SshCommand,
    config: &LaunchConfig,
    remove_home: bool,
) -> Result<UninstallOutcome, UninstallError> {
    validate_root(&config.remote_install_root)?;
    if let Some(label) = config.launch_agent_label.as_deref() {
        let _ = launchd::remove(command, config, label);
    }
    let _ = supervisor::stop(command, config);
    if let Ok(status) = supervisor::status(command, config)
        && status.running
    {
        return Err(UninstallError::new(
            "ssh_uninstall_runtime_running",
            "the remote runtime is still running",
        ));
    }

    let mut remove = command.clone();
    remove
        .remote_script(UNINSTALL_SCRIPT)
        .map_err(|error| UninstallError::new("ssh_uninstall_script_invalid", error.to_string()))?;
    remove.args.extend([
        config.remote_install_root.clone().into(),
        if remove_home { "no" } else { "yes" }.into(),
    ]);
    let output = remove
        .run_with_timeout(std::time::Duration::from_secs(60))
        .map_err(|error| {
            UninstallError::new(
                "ssh_uninstall_failed",
                format!("{}: {}", error.code(), error.message),
            )
        })?;
    if !output.stdout_lossy().contains("uninstall=ok") {
        return Err(UninstallError::new(
            "ssh_uninstall_unconfirmed",
            "the remote uninstall did not confirm",
        ));
    }
    Ok(UninstallOutcome {
        install_root: config.remote_install_root.clone(),
        runtime_home: config.runtime_home.clone(),
        home_preserved: !remove_home,
    })
}

fn validate_root(root: &str) -> Result<(), UninstallError> {
    if root.is_empty()
        || root == "/"
        || !root.starts_with('/')
        || root.contains('\0')
        || root.contains('\n')
        || root.split('/').any(|part| part == "..")
    {
        return Err(UninstallError::new(
            "ssh_uninstall_root_invalid",
            "the install root is not a safe uninstall target",
        ));
    }
    Ok(())
}

pub const UNINSTALL_SCRIPT: &str = r#"set -eu
root=$1
keep_home=$2
if [ "$keep_home" = yes ]; then
  for entry in "$root"/* "$root"/.[!.]*; do
    if [ ! -e "$entry" ] && [ ! -L "$entry" ]; then
      continue
    fi
    if [ "$entry" = "$root/home" ]; then
      continue
    fi
    rm -rf "$entry"
  done
  echo "home_preserved=yes"
else
  rm -rf "$root"
fi
echo "uninstall=ok"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LaunchConfig {
        LaunchConfig::new(
            "/home/dev",
            "/home/dev/.local/share/vibex/server",
            &["/home/dev".to_string()],
            8765,
        )
        .unwrap()
    }

    #[test]
    fn rejects_root_and_parent_targets() {
        assert!(validate_root("/").is_err());
        assert!(validate_root("/home/dev/../etc").is_err());
        assert!(validate_root("relative").is_err());
        assert!(validate_root("/home/dev/.local/share/vibex/server").is_ok());
    }

    #[test]
    fn script_preserves_home_by_default_and_never_touches_workspaces() {
        assert!(UNINSTALL_SCRIPT.contains("$root/home"));
        assert!(UNINSTALL_SCRIPT.contains("uninstall=ok"));
        assert!(UNINSTALL_SCRIPT.contains("home_preserved=yes"));
        assert!(!UNINSTALL_SCRIPT.contains("/home/dev/src"));
        let config = config();
        assert!(validate_root(&config.remote_install_root).is_ok());
    }
}

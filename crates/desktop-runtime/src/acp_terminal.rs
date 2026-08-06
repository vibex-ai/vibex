use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use vibex_agent_acp::{
    AcpTerminalAuthRequest, AcpTerminalCreateRequest, AcpTerminalExitStatus, AcpTerminalHost,
    AcpTerminalOutput, redacted_terminal_auth_action_descriptor,
};
use vibex_core::{TerminalAuthActionDescriptor, TerminalId, VibexError, VibexResult, WorkspaceId};
use vibex_terminal::{TerminalCommandRequest, TerminalManager};

const ACP_TERMINAL_ROWS: u16 = 24;
const ACP_TERMINAL_COLS: u16 = 100;
const ACP_TERMINAL_WAIT_POLL: Duration = Duration::from_millis(50);

#[derive(Clone)]
pub(crate) struct DesktopAcpTerminalHost {
    manager: TerminalManager,
}

impl DesktopAcpTerminalHost {
    pub(crate) fn new(manager: TerminalManager) -> Self {
        Self { manager }
    }

    fn command_root(cwd: Option<&Path>) -> VibexResult<PathBuf> {
        let candidate = cwd
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(std::env::temp_dir);
        let candidate = if candidate.is_absolute() {
            candidate
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| std::env::temp_dir())
                .join(candidate)
        };
        let root = candidate.canonicalize().map_err(|error| {
            VibexError::validation(
                "acp_terminal_cwd_missing",
                "ACP terminal working directory does not exist",
            )
            .with_diagnostic("path", candidate.display().to_string())
            .with_diagnostic("error", error.to_string())
        })?;
        if !root.is_dir() {
            return Err(VibexError::validation(
                "acp_terminal_cwd_not_directory",
                "ACP terminal working directory must be a directory",
            ));
        }
        Ok(root)
    }

    fn create_command(
        &self,
        title: Option<String>,
        command: String,
        args: Vec<String>,
        cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
    ) -> VibexResult<TerminalId> {
        let root = Self::command_root(cwd.as_deref())?;
        let session = self.manager.create_command(
            &root,
            TerminalCommandRequest {
                workspace_id: WorkspaceId::new(),
                title,
                command,
                args,
                cwd: Some(root.to_string_lossy().into_owned()),
                env,
                rows: ACP_TERMINAL_ROWS,
                cols: ACP_TERMINAL_COLS,
            },
        )?;
        Ok(session.id)
    }
}

#[async_trait]
impl AcpTerminalHost for DesktopAcpTerminalHost {
    async fn create(&self, request: AcpTerminalCreateRequest) -> VibexResult<TerminalId> {
        self.create_command(
            request.title,
            request.command,
            request.args,
            request.cwd,
            request.env,
        )
    }

    async fn kill(&self, terminal_id: &TerminalId) -> VibexResult<()> {
        match self.manager.kill(terminal_id) {
            Ok(_) => Ok(()),
            Err(error) if error.code == "terminal_not_found" => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn release(&self, terminal_id: &TerminalId) -> VibexResult<()> {
        match self.manager.process_exit_status(terminal_id) {
            Ok(Some(_)) => self.manager.release(terminal_id).map(|_| ()),
            Ok(None) => self.manager.kill(terminal_id).map(|_| ()),
            Err(error) if error.code == "terminal_not_found" => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn output(
        &self,
        terminal_id: &TerminalId,
        limit: usize,
    ) -> VibexResult<AcpTerminalOutput> {
        let snapshot = self.manager.snapshot(terminal_id)?;
        let text = snapshot
            .chunks
            .iter()
            .map(|chunk| chunk.data.as_str())
            .collect::<String>();
        let (text, truncated) = tail_utf8(&text, limit);
        Ok(AcpTerminalOutput { text, truncated })
    }

    async fn wait_for_exit(&self, terminal_id: &TerminalId) -> VibexResult<AcpTerminalExitStatus> {
        loop {
            if let Some(status) = self.manager.process_exit_status(terminal_id)? {
                return Ok(AcpTerminalExitStatus {
                    exit_code: status.exit_code,
                    signal: status.signal,
                });
            }
            tokio::time::sleep(ACP_TERMINAL_WAIT_POLL).await;
        }
    }

    fn terminal_auth_descriptor(
        &self,
        request: AcpTerminalAuthRequest,
    ) -> VibexResult<TerminalAuthActionDescriptor> {
        let terminal_id = self.create_command(
            Some(request.title.clone()),
            request.command.clone(),
            request.args.clone(),
            request.cwd.as_deref().map(PathBuf::from),
            request.env.clone(),
        )?;
        let mut descriptor = redacted_terminal_auth_action_descriptor(request);
        descriptor.terminal_id = Some(terminal_id);
        Ok(descriptor)
    }
}

fn tail_utf8(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_string(), false);
    }
    let mut start = text.len().saturating_sub(limit);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    (text[start..].to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_output_tail_preserves_utf8_boundaries() {
        assert_eq!(tail_utf8("a你好", 4), ("好".to_string(), true));
        assert_eq!(tail_utf8("safe", 8), ("safe".to_string(), false));
    }

    #[cfg(unix)]
    #[test]
    fn terminal_auth_descriptor_is_backed_by_a_real_shared_terminal() {
        let manager = TerminalManager::new();
        let host = DesktopAcpTerminalHost::new(manager.clone());
        let descriptor = host
            .terminal_auth_descriptor(AcpTerminalAuthRequest {
                provider_profile_id: vibex_core::ProviderProfileId::new(),
                method_id: "terminal-login".to_string(),
                title: "Login".to_string(),
                command: "printf".to_string(),
                args: vec!["login".to_string()],
                cwd: Some(std::env::temp_dir().display().to_string()),
                env: vec![("AUTH_TOKEN".to_string(), "secret".to_string())],
            })
            .unwrap();
        let terminal_id = descriptor.terminal_id.clone().unwrap();
        assert_eq!(descriptor.env_keys, vec!["AUTH_TOKEN"]);
        assert!(format!("{descriptor:?}").contains("AUTH_TOKEN"));
        assert!(!format!("{descriptor:?}").contains("secret"));
        manager.kill(&terminal_id).unwrap();
    }
}

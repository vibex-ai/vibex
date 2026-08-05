use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use vibex_backend::{
    BackendCapabilitySnapshot, BackendError, BackendFacade, BackendOperation, BackendResult,
    CapabilityAvailability, DomainCapabilities,
};

use crate::{
    AgentWorkflowController, AgentWorkflowView, FileWorkflowController, FileWorkflowView,
    GitWorkflowController, GitWorkflowView, ShellKind,
};
use vibex_desktop_model::SidebarState;

pub const AGENT_FILE_GIT_WORKFLOW_SCHEMA_VERSION: &str = "vibex-agent-file-git-workflow.v1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkflowViewGeneration(pub u64);

impl WorkflowViewGeneration {
    pub fn advance(&mut self) -> Self {
        self.0 = self.0.saturating_add(1).max(1);
        *self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowDomain {
    Agent,
    File,
    Git,
}

/// Capability projection for the first shared Agent/File/Git workflow.
///
/// The underlying backend can expose a larger desktop surface. This projection
/// deliberately removes file move/delete and advanced Git operations so the
/// Compact/Web workflow cannot accidentally grow beyond the reviewed v1 scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentFileGitCapabilities {
    pub schema_version: String,
    pub backend_revision: u64,
    pub agent: DomainCapabilities,
    pub file: DomainCapabilities,
    pub git: DomainCapabilities,
}

impl AgentFileGitCapabilities {
    pub fn from_backend(snapshot: &BackendCapabilitySnapshot) -> Self {
        use BackendOperation::*;
        Self {
            schema_version: AGENT_FILE_GIT_WORKFLOW_SCHEMA_VERSION.to_string(),
            backend_revision: snapshot.revision,
            agent: filter_domain(
                &snapshot.agent,
                [
                    AgentListSessions,
                    AgentCreateSession,
                    AgentOpenSession,
                    AgentFetchTimeline,
                    AgentSendMessage,
                    AgentContinueTurn,
                    AgentInterrupt,
                    AgentResolveApproval,
                    AgentRespondElicitation,
                    AgentManageSession,
                    AgentSwitchRuntime,
                ],
            ),
            file: filter_domain(&snapshot.file, [FileTree, FileSearch, FileRead, FileWrite]),
            git: filter_domain(
                &snapshot.git,
                [GitStatus, GitDiff, GitStage, GitUnstage, GitCommit],
            ),
        }
    }

    pub fn supports(&self, operation: BackendOperation) -> bool {
        self.domain_for(operation).supports(operation)
    }

    pub fn require(&self, operation: BackendOperation) -> BackendResult<()> {
        let domain = self.domain_for(operation);
        if domain.supports(operation) {
            return Ok(());
        }
        let label = operation_label(operation);
        match domain.availability {
            CapabilityAvailability::Offline => Err(BackendError::offline(
                format!("{label}_offline"),
                "the authoritative desktop backend is offline",
            )),
            CapabilityAvailability::Degraded => Err(BackendError::loading(
                format!("{label}_degraded"),
                "the requested workflow operation is temporarily degraded",
            )),
            CapabilityAvailability::RequiresPermission => Err(BackendError::permission(
                format!("{label}_permission_required"),
                "the paired device is not permitted to perform this operation",
            )),
            CapabilityAvailability::Available | CapabilityAvailability::Unsupported => {
                Err(BackendError::unsupported(
                    format!("{label}_unsupported"),
                    "the requested operation is outside the shared Agent/File/Git workflow",
                ))
            }
        }
    }

    pub fn domain(&self, domain: WorkflowDomain) -> &DomainCapabilities {
        match domain {
            WorkflowDomain::Agent => &self.agent,
            WorkflowDomain::File => &self.file,
            WorkflowDomain::Git => &self.git,
        }
    }

    fn domain_for(&self, operation: BackendOperation) -> &DomainCapabilities {
        use BackendOperation::*;
        match operation {
            AgentListSessions
            | AgentCreateSession
            | AgentOpenSession
            | AgentFetchTimeline
            | AgentSendMessage
            | AgentContinueTurn
            | AgentInterrupt
            | AgentResolveApproval
            | AgentRespondElicitation
            | AgentManageSession
            | AgentSwitchRuntime => &self.agent,
            FileTree | FileSearch | FileRead | FileWrite | FileMove | FileDelete => &self.file,
            GitStatus | GitDiff | GitStage | GitUnstage | GitCommit => &self.git,
            _ => &self.agent,
        }
    }
}

fn filter_domain<const N: usize>(
    source: &DomainCapabilities,
    allowed: [BackendOperation; N],
) -> DomainCapabilities {
    let allowed = allowed.into_iter().collect::<BTreeSet<_>>();
    DomainCapabilities {
        availability: source.availability,
        operations: source.operations.intersection(&allowed).copied().collect(),
    }
}

fn operation_label(operation: BackendOperation) -> &'static str {
    use BackendOperation::*;
    match operation {
        AgentListSessions => "agent_list_sessions",
        AgentCreateSession => "agent_create_session",
        AgentOpenSession => "agent_open_session",
        AgentFetchTimeline => "agent_fetch_timeline",
        AgentSendMessage => "agent_send_message",
        AgentContinueTurn => "agent_continue_turn",
        AgentInterrupt => "agent_interrupt",
        AgentResolveApproval => "agent_resolve_approval",
        AgentRespondElicitation => "agent_respond_elicitation",
        AgentManageSession => "agent_manage_session",
        AgentSwitchRuntime => "agent_switch_runtime",
        FileTree => "file_tree",
        FileSearch => "file_search",
        FileRead => "file_read",
        FileWrite => "file_write",
        FileMove => "file_move",
        FileDelete => "file_delete",
        GitStatus => "git_status",
        GitDiff => "git_diff",
        GitStage => "git_stage",
        GitUnstage => "git_unstage",
        GitCommit => "git_commit",
        _ => "workflow_operation",
    }
}

#[derive(Clone)]
pub struct AgentFileGitController {
    pub capabilities: AgentFileGitCapabilities,
    pub agent: AgentWorkflowController,
    pub files: FileWorkflowController,
    pub git: GitWorkflowController,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFileGitView {
    pub agent: AgentWorkflowView,
    pub files: FileWorkflowView,
    pub git: GitWorkflowView,
}

impl AgentFileGitController {
    pub fn from_facade(facade: &BackendFacade) -> Self {
        let capabilities = AgentFileGitCapabilities::from_backend(&facade.capabilities());
        Self {
            agent: AgentWorkflowController::new(facade.agent().clone(), capabilities.agent.clone()),
            files: FileWorkflowController::new(facade.file().clone(), capabilities.file.clone()),
            git: GitWorkflowController::new(facade.git().clone(), capabilities.git.clone()),
            capabilities,
        }
    }

    pub fn refresh_capabilities(&mut self, snapshot: &BackendCapabilitySnapshot) {
        self.capabilities = AgentFileGitCapabilities::from_backend(snapshot);
        self.agent.set_capabilities(self.capabilities.agent.clone());
        self.files.set_capabilities(self.capabilities.file.clone());
        self.git.set_capabilities(self.capabilities.git.clone());
    }

    pub fn view(&self, sidebar: &SidebarState, query: &str, shell: ShellKind) -> AgentFileGitView {
        AgentFileGitView {
            agent: self.agent.state.view(sidebar, query, shell),
            files: self.files.state.view(),
            git: self.git.state.view(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_filters_dangerous_file_operations_without_weakening_safe_git_review() {
        let backend = BackendCapabilitySnapshot::desktop_native_v1();
        let workflow = AgentFileGitCapabilities::from_backend(&backend);

        assert!(workflow.supports(BackendOperation::FileRead));
        assert!(workflow.supports(BackendOperation::FileWrite));
        assert!(!workflow.supports(BackendOperation::FileMove));
        assert!(!workflow.supports(BackendOperation::FileDelete));
        assert!(workflow.supports(BackendOperation::GitStage));
        assert!(workflow.supports(BackendOperation::GitUnstage));
        assert!(workflow.supports(BackendOperation::GitCommit));
        assert_eq!(
            workflow
                .require(BackendOperation::FileDelete)
                .unwrap_err()
                .code,
            "file_delete_unsupported"
        );
    }

    #[test]
    fn offline_capability_returns_a_structured_recovery_state() {
        let mut backend = BackendCapabilitySnapshot::desktop_native_v1();
        backend.agent.availability = CapabilityAvailability::Offline;
        let workflow = AgentFileGitCapabilities::from_backend(&backend);
        let error = workflow
            .require(BackendOperation::AgentSendMessage)
            .unwrap_err();
        assert_eq!(error.code, "agent_send_message_offline");
        assert_eq!(error.kind, vibex_backend::BackendErrorKind::Offline);
    }

    #[test]
    fn view_generation_is_monotonic_and_never_zero_after_start() {
        let mut generation = WorkflowViewGeneration::default();
        assert_eq!(generation.advance(), WorkflowViewGeneration(1));
        assert_eq!(generation.advance(), WorkflowViewGeneration(2));
    }
}

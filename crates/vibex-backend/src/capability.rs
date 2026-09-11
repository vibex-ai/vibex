use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const BACKEND_CAPABILITY_SCHEMA_VERSION: &str = "vibex-backend-capabilities.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendOperation {
    AgentListSessions,
    AgentCreateSession,
    AgentOpenSession,
    AgentFetchTimeline,
    AgentGetTimelineDisplaySettings,
    AgentSendMessage,
    AgentContinueTurn,
    AgentInterrupt,
    AgentResolveApproval,
    AgentRespondElicitation,
    AgentManageSession,
    AgentForkSession,
    AgentSwitchRuntime,
    AgentAuthRead,
    AgentAuthManage,
    AgentSidebarOrganizationRead,
    AgentSidebarOrganizationMutate,
    WorkspaceList,
    WorkspaceOpen,
    WorkspaceDelete,
    FileTree,
    FileSearch,
    FileRead,
    FileWrite,
    FileMove,
    FileDelete,
    FileCreateDirectory,
    FileCopy,
    GitStatus,
    GitDiff,
    GitStage,
    GitUnstage,
    GitCommit,
    GitHistory,
    GitCommitDetail,
    GitBranchList,
    GitRevert,
    GitRemoteAction,
    GitWorktreeRead,
    GitWorktreeCreate,
    GitWorktreeLifecycleMutate,
    GitWorktreeRenameBranch,
    TerminalList,
    TerminalCreate,
    TerminalAttach,
    TerminalInput,
    TerminalResize,
    TerminalClose,
    ManagementAgents,
    ManagementProfiles,
    ManagementProfileSelect,
    ManagementProviderProjectionRead,
    ManagementProviderProjectionMutate,
    ManagementProviderSecretMutate,
    ManagementProviderDisplayOrderRead,
    ManagementProviderDisplayOrderMutate,
    ManagementProviderProfileTest,
    ManagementProviderModelFetch,
    ManagementCapabilityRead,
    ManagementCapabilityProbe,
    ManagementMcpRead,
    ManagementMcpMutate,
    ManagementSkillsRead,
    ManagementSkillsMutate,
    ManagementPromptsRead,
    ManagementPromptsMutate,
    ManagementHooksRead,
    ManagementHooksMutate,
    ManagementScheduledRead,
    ManagementScheduledMutate,
    ManagementAutomationRead,
    ManagementAutomationMutate,
    ManagementRuntimeProbeRead,
    ManagementRuntimeProbeMutate,
    ManagementHealth,
    ManagementRelay,
    DevicePairing,
    DeviceList,
    DeviceRevoke,
    DeviceAudit,
    /// Authority-local recovery: diagnostic export and database backup.
    RecoveryDiagnosticsExport,
    RecoveryBackupCreate,
    RecoveryBackupInspect,
    RecoveryBackupRestore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAvailability {
    Available,
    Degraded,
    Offline,
    RequiresPermission,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainCapabilities {
    pub availability: CapabilityAvailability,
    pub operations: BTreeSet<BackendOperation>,
    /// Operations the authority supports but this client's grant does not
    /// permit. Keeping them separate lets a client report "permission
    /// required" instead of claiming the operation is unsupported.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub permission_required: BTreeSet<BackendOperation>,
}

impl DomainCapabilities {
    pub fn available(operations: impl IntoIterator<Item = BackendOperation>) -> Self {
        Self {
            availability: CapabilityAvailability::Available,
            operations: operations.into_iter().collect(),
            permission_required: BTreeSet::new(),
        }
    }

    pub fn supports(&self, operation: BackendOperation) -> bool {
        self.availability == CapabilityAvailability::Available
            && self.operations.contains(&operation)
    }

    /// True when the authority recognizes `operation` but the paired device's
    /// permission level denies it.
    pub fn requires_permission(&self, operation: BackendOperation) -> bool {
        self.permission_required.contains(&operation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendCapabilitySnapshot {
    pub schema_version: String,
    pub revision: u64,
    pub agent: DomainCapabilities,
    pub workspace: DomainCapabilities,
    pub file: DomainCapabilities,
    pub git: DomainCapabilities,
    pub terminal: DomainCapabilities,
    pub management: DomainCapabilities,
    pub device: DomainCapabilities,
}

impl BackendCapabilitySnapshot {
    pub fn disconnected_v1() -> Self {
        let mut snapshot = Self::desktop_native_v1();
        snapshot.revision = 0;
        for domain in [
            &mut snapshot.agent,
            &mut snapshot.workspace,
            &mut snapshot.file,
            &mut snapshot.git,
            &mut snapshot.terminal,
            &mut snapshot.management,
            &mut snapshot.device,
        ] {
            domain.availability = CapabilityAvailability::Offline;
        }
        snapshot
    }

    pub fn desktop_native_v1() -> Self {
        use BackendOperation::*;
        Self {
            schema_version: BACKEND_CAPABILITY_SCHEMA_VERSION.to_string(),
            revision: 1,
            agent: DomainCapabilities::available([
                AgentListSessions,
                AgentCreateSession,
                AgentOpenSession,
                AgentFetchTimeline,
                AgentGetTimelineDisplaySettings,
                AgentSendMessage,
                AgentContinueTurn,
                AgentInterrupt,
                AgentResolveApproval,
                AgentRespondElicitation,
                AgentManageSession,
                AgentForkSession,
                AgentSwitchRuntime,
                AgentAuthRead,
                AgentAuthManage,
            ]),
            workspace: DomainCapabilities::available([
                WorkspaceList,
                WorkspaceOpen,
                WorkspaceDelete,
            ]),
            file: DomainCapabilities::available([
                FileTree,
                FileSearch,
                FileRead,
                FileWrite,
                FileMove,
                FileDelete,
                FileCreateDirectory,
                FileCopy,
            ]),
            git: DomainCapabilities::available([
                GitStatus,
                GitDiff,
                GitStage,
                GitUnstage,
                GitCommit,
                GitHistory,
                GitCommitDetail,
                GitBranchList,
                GitRevert,
                GitRemoteAction,
                GitWorktreeRead,
                GitWorktreeCreate,
                GitWorktreeLifecycleMutate,
                GitWorktreeRenameBranch,
            ]),
            terminal: DomainCapabilities::available([
                TerminalList,
                TerminalCreate,
                TerminalAttach,
                TerminalInput,
                TerminalResize,
                TerminalClose,
            ]),
            management: DomainCapabilities::available([
                ManagementAgents,
                ManagementProfiles,
                ManagementProfileSelect,
                ManagementProviderProjectionRead,
                ManagementProviderProjectionMutate,
                ManagementProviderSecretMutate,
                ManagementProviderDisplayOrderRead,
                ManagementProviderDisplayOrderMutate,
                ManagementProviderProfileTest,
                ManagementProviderModelFetch,
                ManagementCapabilityRead,
                ManagementCapabilityProbe,
                ManagementMcpRead,
                ManagementMcpMutate,
                ManagementSkillsRead,
                ManagementSkillsMutate,
                ManagementPromptsRead,
                ManagementPromptsMutate,
                ManagementHooksRead,
                ManagementHooksMutate,
                ManagementScheduledRead,
                ManagementScheduledMutate,
                ManagementAutomationRead,
                ManagementAutomationMutate,
                ManagementRuntimeProbeRead,
                ManagementRuntimeProbeMutate,
                ManagementHealth,
                ManagementRelay,
            ]),
            device: DomainCapabilities::available([
                DevicePairing,
                DeviceList,
                DeviceRevoke,
                DeviceAudit,
                RecoveryDiagnosticsExport,
                RecoveryBackupCreate,
                RecoveryBackupInspect,
                RecoveryBackupRestore,
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_snapshot_exposes_seven_separate_domains() {
        let snapshot = BackendCapabilitySnapshot::desktop_native_v1();
        assert_eq!(snapshot.schema_version, BACKEND_CAPABILITY_SCHEMA_VERSION);
        assert!(
            snapshot
                .agent
                .supports(BackendOperation::AgentFetchTimeline)
        );
        assert!(snapshot.file.supports(BackendOperation::FileWrite));
        assert!(snapshot.device.supports(BackendOperation::DevicePairing));
        assert!(snapshot.git.supports(BackendOperation::GitWorktreeCreate));
        assert!(
            snapshot
                .git
                .supports(BackendOperation::GitWorktreeLifecycleMutate)
        );
        assert!(!snapshot.git.supports(BackendOperation::TerminalCreate));
    }

    #[test]
    fn capability_snapshot_round_trips_as_a_remote_safe_contract() {
        let snapshot = BackendCapabilitySnapshot::desktop_native_v1();
        let encoded = serde_json::to_string(&snapshot).expect("capability snapshot serializes");
        let decoded: BackendCapabilitySnapshot =
            serde_json::from_str(&encoded).expect("capability snapshot deserializes");

        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn permission_required_operations_stay_out_of_the_allowed_set() {
        let mut domain = DomainCapabilities::available([BackendOperation::FileRead]);
        domain
            .permission_required
            .insert(BackendOperation::FileWrite);

        assert!(domain.supports(BackendOperation::FileRead));
        assert!(!domain.supports(BackendOperation::FileWrite));
        assert!(domain.requires_permission(BackendOperation::FileWrite));
        assert!(!domain.requires_permission(BackendOperation::FileRead));

        let encoded = serde_json::to_string(&domain).expect("domain serializes");
        assert!(encoded.contains("permissionRequired"));
        let decoded: DomainCapabilities =
            serde_json::from_str(&encoded).expect("domain deserializes");
        assert_eq!(decoded, domain);

        let legacy: DomainCapabilities =
            serde_json::from_str(r#"{"availability":"available","operations":["file_read"]}"#)
                .expect("legacy payloads without the field still decode");
        assert!(legacy.permission_required.is_empty());
    }
}

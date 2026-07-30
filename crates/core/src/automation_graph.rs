use serde::{Deserialize, Serialize};

use crate::agent::AgentSessionSafety;
use crate::error::RedactedDiagnostic;
use crate::ids::{
    AutomationEdgeId, AutomationGraphId, AutomationNodeId, AutomationRunId, AutomationRunStepId,
    ProjectId, ProviderProfileId, RequestId, ScheduledTaskId, VibexSessionId, WorkspaceId,
};
use crate::permission::{PermissionResponseKind, PermissionRiskCategory};
use crate::provider::ProviderKind;
use crate::workspace::WorkspaceMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationGraphStatus {
    Active,
    Paused,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationGraphScheduledTaskTrigger {
    pub scheduled_task_id: ScheduledTaskId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AutomationGraphTrigger {
    Manual,
    ScheduledTask(AutomationGraphScheduledTaskTrigger),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationNodeKind {
    AgentPrompt,
    ApprovalGate,
    FileCheck,
    GitCheck,
    TerminalCheck,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationAgentPromptConfig {
    pub prompt_template: String,
    pub provider_kind: Option<ProviderKind>,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub safety: Option<AgentSessionSafety>,
    pub workspace_root: Option<String>,
    pub workspace_mode: Option<WorkspaceMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationApprovalGateConfig {
    pub title: String,
    pub details: String,
    pub risk_category: PermissionRiskCategory,
    pub allowed_responses: Vec<PermissionResponseKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationFileCheckConfig {
    pub path_pattern: String,
    pub condition: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationGitCheckConfig {
    pub condition: String,
    pub path_pattern: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationTerminalCheckConfig {
    pub command_preview: String,
    pub condition: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AutomationNodeConfig {
    AgentPrompt(AutomationAgentPromptConfig),
    ApprovalGate(AutomationApprovalGateConfig),
    FileCheck(AutomationFileCheckConfig),
    GitCheck(AutomationGitCheckConfig),
    TerminalCheck(AutomationTerminalCheckConfig),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationNodePosition {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationNode {
    pub id: AutomationNodeId,
    pub graph_id: AutomationGraphId,
    pub kind: AutomationNodeKind,
    pub title: String,
    pub config: AutomationNodeConfig,
    pub position: Option<AutomationNodePosition>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationEdgeConditionKind {
    Always,
    OnSuccess,
    OnFailure,
    OnApproval,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationEdgeCondition {
    pub kind: AutomationEdgeConditionKind,
    pub expression: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationEdge {
    pub id: AutomationEdgeId,
    pub graph_id: AutomationGraphId,
    pub source_node_id: AutomationNodeId,
    pub target_node_id: AutomationNodeId,
    pub condition: AutomationEdgeCondition,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationGraph {
    pub id: AutomationGraphId,
    pub title: String,
    pub description: Option<String>,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
    pub workspace_root: String,
    pub workspace_mode: WorkspaceMode,
    pub provider_kind: Option<ProviderKind>,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub trigger: AutomationGraphTrigger,
    pub status: AutomationGraphStatus,
    pub version: u32,
    pub nodes: Vec<AutomationNode>,
    pub edges: Vec<AutomationEdge>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationGraphCreateRequest {
    pub title: String,
    pub description: Option<String>,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
    pub workspace_root: String,
    pub workspace_mode: WorkspaceMode,
    pub provider_kind: Option<ProviderKind>,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub trigger: AutomationGraphTrigger,
    pub nodes: Vec<AutomationNodeCreateRequest>,
    pub edges: Vec<AutomationEdgeCreateRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationGraphUpdateRequest {
    pub id: AutomationGraphId,
    pub title: Option<String>,
    pub description: Option<String>,
    pub clear_description: bool,
    pub project_id: Option<ProjectId>,
    pub clear_project_id: bool,
    pub workspace_id: Option<WorkspaceId>,
    pub clear_workspace_id: bool,
    pub workspace_root: Option<String>,
    pub workspace_mode: Option<WorkspaceMode>,
    pub provider_kind: Option<ProviderKind>,
    pub clear_provider_kind: bool,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub clear_provider_profile_id: bool,
    pub trigger: Option<AutomationGraphTrigger>,
    pub status: Option<AutomationGraphStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationGraphListRequest {
    pub workspace_id: Option<WorkspaceId>,
    pub status: Option<AutomationGraphStatus>,
    pub include_deleted: bool,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationNodeCreateRequest {
    pub id: Option<AutomationNodeId>,
    pub kind: AutomationNodeKind,
    pub title: String,
    pub config: AutomationNodeConfig,
    pub position: Option<AutomationNodePosition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationEdgeCreateRequest {
    pub source_node_id: AutomationNodeId,
    pub target_node_id: AutomationNodeId,
    pub condition: AutomationEdgeCondition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationGraphDefinitionUpdateRequest {
    pub graph_id: AutomationGraphId,
    pub nodes: Vec<AutomationNodeCreateRequest>,
    pub edges: Vec<AutomationEdgeCreateRequest>,
    // Optional compare-and-swap fence for editors that loaded an older graph.
    // `None` preserves the legacy command contract for trusted callers.
    #[serde(default)]
    pub expected_version: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunStatus {
    Queued,
    Running,
    WaitingForApproval,
    Succeeded,
    Failed,
    Canceled,
    Recovered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunTrigger {
    Manual,
    ScheduledTask,
    Recovery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRun {
    pub id: AutomationRunId,
    pub graph_id: AutomationGraphId,
    pub status: AutomationRunStatus,
    pub trigger: AutomationRunTrigger,
    pub scheduled_task_id: Option<ScheduledTaskId>,
    pub session_id: Option<VibexSessionId>,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub redacted_diagnostics: Vec<RedactedDiagnostic>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunCreateRequest {
    pub graph_id: AutomationGraphId,
    pub status: AutomationRunStatus,
    pub trigger: AutomationRunTrigger,
    pub scheduled_task_id: Option<ScheduledTaskId>,
    pub session_id: Option<VibexSessionId>,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub redacted_diagnostics: Vec<RedactedDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunUpdateRequest {
    pub id: AutomationRunId,
    pub status: Option<AutomationRunStatus>,
    pub scheduled_task_id: Option<ScheduledTaskId>,
    pub clear_scheduled_task_id: bool,
    pub session_id: Option<VibexSessionId>,
    pub clear_session_id: bool,
    pub started_at_ms: Option<i64>,
    pub clear_started_at_ms: bool,
    pub ended_at_ms: Option<i64>,
    pub clear_ended_at_ms: bool,
    pub error_code: Option<String>,
    pub clear_error_code: bool,
    pub error_message: Option<String>,
    pub clear_error_message: bool,
    pub redacted_diagnostics: Option<Vec<RedactedDiagnostic>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunListRequest {
    pub graph_id: Option<AutomationGraphId>,
    pub status: Option<AutomationRunStatus>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunStartRequest {
    pub graph_id: AutomationGraphId,
    pub trigger: AutomationRunTrigger,
    pub scheduled_task_id: Option<ScheduledTaskId>,
    pub now_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunResumeRequest {
    pub run_id: AutomationRunId,
    pub now_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunCancelRequest {
    pub run_id: AutomationRunId,
    pub now_ms: Option<i64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunStepStatus {
    Queued,
    Running,
    WaitingForApproval,
    Succeeded,
    Failed,
    Skipped,
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunStep {
    pub id: AutomationRunStepId,
    pub run_id: AutomationRunId,
    pub node_id: AutomationNodeId,
    pub status: AutomationRunStepStatus,
    pub session_id: Option<VibexSessionId>,
    pub permission_request_id: Option<RequestId>,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub redacted_diagnostics: Vec<RedactedDiagnostic>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunStepCreateRequest {
    pub run_id: AutomationRunId,
    pub node_id: AutomationNodeId,
    pub status: AutomationRunStepStatus,
    pub session_id: Option<VibexSessionId>,
    pub permission_request_id: Option<RequestId>,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub redacted_diagnostics: Vec<RedactedDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunStepUpdateRequest {
    pub id: AutomationRunStepId,
    pub status: Option<AutomationRunStepStatus>,
    pub session_id: Option<VibexSessionId>,
    pub clear_session_id: bool,
    pub permission_request_id: Option<RequestId>,
    pub clear_permission_request_id: bool,
    pub started_at_ms: Option<i64>,
    pub clear_started_at_ms: bool,
    pub ended_at_ms: Option<i64>,
    pub clear_ended_at_ms: bool,
    pub error_code: Option<String>,
    pub clear_error_code: bool,
    pub error_message: Option<String>,
    pub clear_error_message: bool,
    pub redacted_diagnostics: Option<Vec<RedactedDiagnostic>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunStepListRequest {
    pub run_id: Option<AutomationRunId>,
    pub node_id: Option<AutomationNodeId>,
    pub status: Option<AutomationRunStepStatus>,
    pub limit: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::PermissionMode;

    #[test]
    fn graph_trigger_uses_tagged_shape() {
        let trigger = AutomationGraphTrigger::ScheduledTask(AutomationGraphScheduledTaskTrigger {
            scheduled_task_id: ScheduledTaskId::parse("scheduled_task_mock").unwrap(),
        });

        let json = serde_json::to_value(trigger).unwrap();
        assert_eq!(json["type"], "scheduled_task");
        assert_eq!(json["data"]["scheduledTaskId"], "scheduled_task_mock");
    }

    #[test]
    fn node_config_uses_tagged_shape() {
        let config = AutomationNodeConfig::AgentPrompt(AutomationAgentPromptConfig {
            prompt_template: "Summarize current diff".to_string(),
            provider_kind: Some(ProviderKind::Codex),
            provider_profile_id: None,
            safety: Some(AgentSessionSafety {
                permission_mode: PermissionMode::WorkspaceWrite,
                ask_on_risk: true,
                bypass_all_permissions: false,
            }),
            workspace_root: Some("/tmp/vibex".to_string()),
            workspace_mode: Some(WorkspaceMode::CurrentCheckout),
        });

        let json = serde_json::to_value(config).unwrap();
        assert_eq!(json["type"], "agent_prompt");
        assert_eq!(json["data"]["promptTemplate"], "Summarize current diff");
        assert_eq!(json["data"]["providerKind"], "codex");
    }

    #[test]
    fn graph_serializes_camel_case_fields() {
        let graph = AutomationGraph {
            id: AutomationGraphId::parse("automation_graph_mock").unwrap(),
            title: "Nightly review".to_string(),
            description: Some("Run safe local review steps".to_string()),
            project_id: None,
            workspace_id: Some(WorkspaceId::parse("workspace_mock").unwrap()),
            workspace_root: "/tmp/vibex".to_string(),
            workspace_mode: WorkspaceMode::CurrentCheckout,
            provider_kind: Some(ProviderKind::Codex),
            provider_profile_id: None,
            trigger: AutomationGraphTrigger::Manual,
            status: AutomationGraphStatus::Active,
            version: 1,
            nodes: Vec::new(),
            edges: Vec::new(),
            created_at_ms: 1,
            updated_at_ms: 2,
            deleted_at_ms: None,
        };

        let json = serde_json::to_value(graph).unwrap();
        assert_eq!(json["workspaceId"], "workspace_mock");
        assert_eq!(json["workspaceMode"], "current_checkout");
        assert_eq!(json["providerKind"], "codex");
        assert_eq!(json["createdAtMs"], 1);
        assert!(json.get("workspace_id").is_none());
    }

    #[test]
    fn run_start_request_serializes_camel_case_fields() {
        let request = AutomationRunStartRequest {
            graph_id: AutomationGraphId::parse("automation_graph_mock").unwrap(),
            trigger: AutomationRunTrigger::Manual,
            scheduled_task_id: None,
            now_ms: Some(42),
        };

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["graphId"], "automation_graph_mock");
        assert_eq!(json["trigger"], "manual");
        assert_eq!(json["nowMs"], 42);
        assert!(json.get("graph_id").is_none());
    }
}

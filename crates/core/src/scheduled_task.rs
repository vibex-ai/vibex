use serde::{Deserialize, Serialize};

use crate::agent::AgentSessionSafety;
use crate::error::RedactedDiagnostic;
use crate::ids::{
    ProjectId, ProviderProfileId, ScheduledTaskId, ScheduledTaskRunId, VibexSessionId, WorkspaceId,
};
use crate::provider::ProviderKind;
use crate::workspace::WorkspaceMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledTaskStatus {
    Active,
    Paused,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskOneShotSchedule {
    pub run_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskIntervalSchedule {
    pub every_seconds: u32,
    pub start_at_ms: i64,
    pub end_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskDailySchedule {
    pub local_time_minutes: u16,
    pub timezone: String,
    pub start_at_ms: i64,
    pub end_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ScheduledTaskSchedule {
    OneShot(ScheduledTaskOneShotSchedule),
    Interval(ScheduledTaskIntervalSchedule),
    Daily(ScheduledTaskDailySchedule),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTask {
    pub id: ScheduledTaskId,
    pub title: String,
    pub prompt: String,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
    pub workspace_root: String,
    pub workspace_mode: WorkspaceMode,
    pub provider_kind: ProviderKind,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub schedule: ScheduledTaskSchedule,
    pub status: ScheduledTaskStatus,
    pub safety: AgentSessionSafety,
    pub next_run_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskCreateRequest {
    pub title: String,
    pub prompt: String,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
    pub workspace_root: String,
    pub workspace_mode: WorkspaceMode,
    pub provider_kind: ProviderKind,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub schedule: ScheduledTaskSchedule,
    pub safety: Option<AgentSessionSafety>,
    pub next_run_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskUpdateRequest {
    pub id: ScheduledTaskId,
    pub title: Option<String>,
    pub prompt: Option<String>,
    pub project_id: Option<ProjectId>,
    pub clear_project_id: bool,
    pub workspace_id: Option<WorkspaceId>,
    pub clear_workspace_id: bool,
    pub workspace_root: Option<String>,
    pub workspace_mode: Option<WorkspaceMode>,
    pub provider_kind: Option<ProviderKind>,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub clear_provider_profile_id: bool,
    pub schedule: Option<ScheduledTaskSchedule>,
    pub safety: Option<AgentSessionSafety>,
    pub next_run_at_ms: Option<i64>,
    pub clear_next_run_at_ms: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskListRequest {
    pub workspace_id: Option<WorkspaceId>,
    pub status: Option<ScheduledTaskStatus>,
    pub include_deleted: bool,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledTaskRunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Canceled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledTaskRunTrigger {
    Scheduler,
    Manual,
    Recovery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskRun {
    pub id: ScheduledTaskRunId,
    pub task_id: ScheduledTaskId,
    pub status: ScheduledTaskRunStatus,
    pub trigger: ScheduledTaskRunTrigger,
    pub session_id: Option<VibexSessionId>,
    pub due_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub attempt: u32,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub redacted_diagnostics: Vec<RedactedDiagnostic>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskRunCreateRequest {
    pub task_id: ScheduledTaskId,
    pub status: ScheduledTaskRunStatus,
    pub trigger: ScheduledTaskRunTrigger,
    pub session_id: Option<VibexSessionId>,
    pub due_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub attempt: u32,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub redacted_diagnostics: Vec<RedactedDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskRunUpdateRequest {
    pub id: ScheduledTaskRunId,
    pub status: Option<ScheduledTaskRunStatus>,
    pub session_id: Option<VibexSessionId>,
    pub clear_session_id: bool,
    pub started_at_ms: Option<i64>,
    pub clear_started_at_ms: bool,
    pub ended_at_ms: Option<i64>,
    pub clear_ended_at_ms: bool,
    pub attempt: Option<u32>,
    pub error_code: Option<String>,
    pub clear_error_code: bool,
    pub error_message: Option<String>,
    pub clear_error_message: bool,
    pub redacted_diagnostics: Option<Vec<RedactedDiagnostic>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskRunListRequest {
    pub task_id: Option<ScheduledTaskId>,
    pub session_id: Option<VibexSessionId>,
    pub status: Option<ScheduledTaskRunStatus>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledTaskAttentionKind {
    Failed,
    Skipped,
    Canceled,
    PermissionRequired,
    RecoveredStaleRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledTaskAuditOutcome {
    Queued,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Canceled,
    PermissionRequired,
    RecoveredStaleRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskAttentionListRequest {
    pub workspace_id: Option<WorkspaceId>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskAuditListRequest {
    pub workspace_id: Option<WorkspaceId>,
    pub status: Option<ScheduledTaskRunStatus>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskAttentionSummary {
    pub task_id: ScheduledTaskId,
    pub task_title: String,
    pub run_id: ScheduledTaskRunId,
    pub workspace_id: Option<WorkspaceId>,
    pub workspace_root: String,
    pub provider_kind: ProviderKind,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub trigger: ScheduledTaskRunTrigger,
    pub status: ScheduledTaskRunStatus,
    pub attention_kind: ScheduledTaskAttentionKind,
    pub session_id: Option<VibexSessionId>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskAuditRecord {
    pub audit_id: String,
    pub task_id: ScheduledTaskId,
    pub task_title: String,
    pub run_id: ScheduledTaskRunId,
    pub workspace_id: Option<WorkspaceId>,
    pub workspace_root: String,
    pub provider_kind: ProviderKind,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub trigger: ScheduledTaskRunTrigger,
    pub outcome: ScheduledTaskAuditOutcome,
    pub status: ScheduledTaskRunStatus,
    pub session_id: Option<VibexSessionId>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub redacted_diagnostics: Vec<RedactedDiagnostic>,
    pub created_at_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::PermissionMode;

    #[test]
    fn schedule_serializes_with_stable_tagged_shape() {
        let schedule = ScheduledTaskSchedule::OneShot(ScheduledTaskOneShotSchedule {
            run_at_ms: 1_800_000_000_000,
        });

        let json = serde_json::to_value(schedule).unwrap();
        assert_eq!(json["type"], "one_shot");
        assert_eq!(json["data"]["runAtMs"], 1_800_000_000_000_i64);
    }

    #[test]
    fn task_serializes_camel_case_fields_and_snake_case_enums() {
        let task = ScheduledTask {
            id: ScheduledTaskId::new(),
            title: "Morning summary".to_string(),
            prompt: "Summarize changes".to_string(),
            project_id: None,
            workspace_id: None,
            workspace_root: "/tmp/workspace".to_string(),
            workspace_mode: WorkspaceMode::CurrentCheckout,
            provider_kind: ProviderKind::Codex,
            provider_profile_id: None,
            schedule: ScheduledTaskSchedule::Daily(ScheduledTaskDailySchedule {
                local_time_minutes: 9 * 60,
                timezone: "Asia/Shanghai".to_string(),
                start_at_ms: 1_800_000_000_000,
                end_at_ms: None,
            }),
            status: ScheduledTaskStatus::Active,
            safety: AgentSessionSafety {
                permission_mode: PermissionMode::WorkspaceWrite,
                ask_on_risk: true,
                bypass_all_permissions: false,
            },
            next_run_at_ms: Some(1_800_000_100_000),
            created_at_ms: 1,
            updated_at_ms: 2,
            deleted_at_ms: None,
        };

        let json = serde_json::to_value(task).unwrap();
        assert_eq!(json["workspaceRoot"], "/tmp/workspace");
        assert_eq!(json["workspaceMode"], "current_checkout");
        assert_eq!(json["providerKind"], "codex");
        assert_eq!(json["status"], "active");
        assert_eq!(json["schedule"]["type"], "daily");
        assert_eq!(json["schedule"]["data"]["localTimeMinutes"], 540);
    }

    #[test]
    fn audit_projection_serializes_without_prompt_payload() {
        let record = ScheduledTaskAuditRecord {
            audit_id: "scheduled_audit:scheduled_run_mock".to_string(),
            task_id: ScheduledTaskId::parse("scheduled_task_mock").unwrap(),
            task_title: "Daily summary".to_string(),
            run_id: ScheduledTaskRunId::parse("scheduled_run_mock").unwrap(),
            workspace_id: None,
            workspace_root: "/tmp/workspace".to_string(),
            provider_kind: ProviderKind::Codex,
            provider_profile_id: None,
            trigger: ScheduledTaskRunTrigger::Scheduler,
            outcome: ScheduledTaskAuditOutcome::PermissionRequired,
            status: ScheduledTaskRunStatus::Skipped,
            session_id: None,
            error_code: Some("scheduler/permission_required".to_string()),
            error_message: Some("Open session to review the provider request.".to_string()),
            redacted_diagnostics: vec![RedactedDiagnostic {
                key: "state".to_string(),
                value: "needs_input".to_string(),
            }],
            created_at_ms: 1,
        };

        let json = serde_json::to_value(record).unwrap();
        assert_eq!(json["auditId"], "scheduled_audit:scheduled_run_mock");
        assert_eq!(json["outcome"], "permission_required");
        assert!(json.get("prompt").is_none());
    }
}

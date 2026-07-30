use serde::{Deserialize, Serialize};

use crate::{
    ProviderBindingMetadata, ProviderCapabilities, ProviderCapabilityProbeStatus,
    ProviderHealthProbeKind, ProviderHealthStatus, ProviderKind, ProviderProfileId,
    ProviderProfileStatus, ProviderSecretSetupState, ProviderUsageBalance, RedactedDiagnostic,
    ScheduledTaskAttentionKind, ScheduledTaskAuditOutcome, ScheduledTaskId, ScheduledTaskRunId,
    ScheduledTaskRunStatus, ScheduledTaskRunTrigger, VibexSessionId, WorkspaceId,
};

pub const DIAGNOSTIC_BUNDLE_SCHEMA_VERSION: &str = "diagnostic_bundle.v2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticBundleRequest {
    pub record_limit: Option<u32>,
    pub include_smoke_references: Option<bool>,
}

impl Default for DiagnosticBundleRequest {
    fn default() -> Self {
        Self {
            record_limit: None,
            include_smoke_references: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticBundle {
    pub metadata: DiagnosticBundleMetadata,
    pub redaction: DiagnosticBundleRedactionPolicy,
    pub storage: DiagnosticStorageSection,
    pub providers: DiagnosticProviderSection,
    pub scheduled_tasks: DiagnosticScheduledTaskSection,
    pub workbench: DiagnosticWorkbenchSection,
    pub runtime: DiagnosticRuntimeSection,
    pub smokes: DiagnosticSmokeSection,
    pub errors: DiagnosticErrorSection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticBundleMetadata {
    pub schema_version: String,
    pub generated_at_ms: i64,
    pub app_version: String,
    pub core_contract_version: String,
    pub os: String,
    pub arch: String,
    pub target_family: String,
    pub debug_assertions: bool,
    #[serde(default)]
    pub release_context: Option<DiagnosticReleaseContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticReleaseContext {
    pub gpui_revision: Option<String>,
    pub gpui_component_revision: Option<String>,
    pub renderer: Option<String>,
    pub window_size: Option<String>,
    pub dpi_scale: Option<String>,
    pub web_backend: Option<String>,
    pub pdf_backend: Option<String>,
    pub terminal_backend: Option<String>,
    pub ui_state_schema: Option<String>,
    pub last_clean_shutdown: Option<bool>,
    pub cache_budgets: Vec<DiagnosticCount>,
    pub crash_metadata: Vec<DiagnosticCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticBundleRedactionPolicy {
    pub default_safe: bool,
    pub policy_version: String,
    pub excluded_content: Vec<DiagnosticExcludedContent>,
    pub max_section_records: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticExcludedContent {
    Prompts,
    AgentMessages,
    TerminalOutput,
    FileContents,
    Secrets,
    EnvValues,
    RawHeaders,
    ProviderNativePayloads,
    NativeIds,
    RawGitDiffs,
    RawLogs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticDatabasePathKind {
    DefaultVibexHome,
    ExplicitOverride,
    Temporary,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticStorageSection {
    pub database_path_kind: DiagnosticDatabasePathKind,
    pub database_path_hint: String,
    pub current_schema_version: i64,
    pub expected_schema_version: i64,
    pub applied_migration_count: u32,
    pub counts: Vec<DiagnosticCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticCount {
    pub name: String,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticRuntimeSection {
    pub process_started_at_ms: i64,
    pub snapshot_at_ms: i64,
    pub series_limit: u32,
    pub series: Vec<DiagnosticRuntimeMetric>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticRuntimeMetric {
    pub name: String,
    pub operation: Option<String>,
    pub result: String,
    pub count: u64,
    pub duration_total_ms: Option<u64>,
    pub duration_min_ms: Option<u64>,
    pub duration_max_ms: Option<u64>,
    pub duration_last_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticProviderSection {
    pub record_limit: u32,
    pub health_summary_count: u32,
    pub capability_summary_count: u32,
    pub usage_summary_count: u32,
    pub health_summaries: Vec<DiagnosticProviderHealthSummary>,
    pub capability_summaries: Vec<DiagnosticProviderCapabilitySummary>,
    pub usage_summaries: Vec<DiagnosticProviderUsageSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticProviderProfileRef {
    pub id: ProviderProfileId,
    pub kind: ProviderKind,
    pub status: ProviderProfileStatus,
    pub secret_setup_state: ProviderSecretSetupState,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticProviderHealthSummary {
    pub profile: DiagnosticProviderProfileRef,
    pub overall_status: ProviderHealthStatus,
    pub last_checked_at_ms: Option<i64>,
    pub expires_at_ms: Option<i64>,
    pub probe_results: Vec<DiagnosticProviderHealthProbe>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticProviderHealthProbe {
    pub provider_kind: ProviderKind,
    pub probe_kind: ProviderHealthProbeKind,
    pub status: ProviderHealthStatus,
    pub summary: String,
    pub latency_ms: Option<u32>,
    pub checked_at_ms: i64,
    pub expires_at_ms: Option<i64>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticProviderCapabilitySummary {
    pub profile: DiagnosticProviderProfileRef,
    pub status: ProviderCapabilityProbeStatus,
    pub effective_capabilities: ProviderCapabilities,
    pub capability_source: String,
    pub fresh: bool,
    pub last_checked_at_ms: Option<i64>,
    pub expires_at_ms: Option<i64>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticProviderUsageSummary {
    pub profile: DiagnosticProviderProfileRef,
    pub balances: Vec<ProviderUsageBalance>,
    pub latest_recorded_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticScheduledTaskSection {
    pub record_limit: u32,
    pub attention_count: u32,
    pub audit_count: u32,
    pub attention: Vec<DiagnosticScheduledTaskAttentionRecord>,
    pub audit: Vec<DiagnosticScheduledTaskAuditRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticScheduledTaskAttentionRecord {
    pub task_id: ScheduledTaskId,
    pub run_id: ScheduledTaskRunId,
    pub workspace_id: Option<WorkspaceId>,
    pub provider_kind: ProviderKind,
    pub provider_profile_id_present: bool,
    pub trigger: ScheduledTaskRunTrigger,
    pub status: ScheduledTaskRunStatus,
    pub attention_kind: ScheduledTaskAttentionKind,
    pub session_id_present: bool,
    pub error_code: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticScheduledTaskAuditRecord {
    pub audit_id: String,
    pub task_id: ScheduledTaskId,
    pub run_id: ScheduledTaskRunId,
    pub workspace_id: Option<WorkspaceId>,
    pub provider_kind: ProviderKind,
    pub provider_profile_id_present: bool,
    pub trigger: ScheduledTaskRunTrigger,
    pub outcome: ScheduledTaskAuditOutcome,
    pub status: ScheduledTaskRunStatus,
    pub session_id: Option<VibexSessionId>,
    pub error_code: Option<String>,
    pub redacted_diagnostics: Vec<RedactedDiagnostic>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticWorkbenchSection {
    pub counts: Vec<DiagnosticCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticSmokeSection {
    pub references: Vec<DiagnosticSmokeCommandReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticSmokeCommandReference {
    pub name: String,
    pub command: String,
    pub kind: DiagnosticSmokeCommandKind,
    pub starts_real_provider: bool,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSmokeCommandKind {
    Deterministic,
    ExplicitManual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticErrorSection {
    pub scheduled_task_error_codes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_exclusion_list_covers_sensitive_classes() {
        let policy = DiagnosticBundleRedactionPolicy {
            default_safe: true,
            policy_version: "diagnostic_redaction.v1".to_string(),
            excluded_content: vec![
                DiagnosticExcludedContent::Prompts,
                DiagnosticExcludedContent::AgentMessages,
                DiagnosticExcludedContent::TerminalOutput,
                DiagnosticExcludedContent::FileContents,
                DiagnosticExcludedContent::Secrets,
                DiagnosticExcludedContent::EnvValues,
                DiagnosticExcludedContent::RawHeaders,
                DiagnosticExcludedContent::ProviderNativePayloads,
                DiagnosticExcludedContent::NativeIds,
                DiagnosticExcludedContent::RawGitDiffs,
                DiagnosticExcludedContent::RawLogs,
            ],
            max_section_records: 25,
        };

        let json = serde_json::to_value(policy).unwrap();
        assert_eq!(json["defaultSafe"], true);
        assert!(
            json["excludedContent"]
                .as_array()
                .unwrap()
                .contains(&serde_json::Value::String(
                    "provider_native_payloads".to_string()
                ))
        );
    }
}

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use vibex_core::{
    AdapterDiagnostic, AgentAuthCatalog, AgentCommandConfig, AgentConfig, AgentDiscoveryRecord,
    AgentId, AgentManagedInstallState, AgentModelListResponse, AgentModelProviderDefaultSelection,
    AgentModelProviderDisplayOrderEntry, AgentModelProviderFailoverEntry, AgentSession,
    AgentSessionConfigProbe, AgentSessionSafety, AgentSessionState, AutomationEdge,
    AutomationEdgeCreateRequest, AutomationEdgeId, AutomationGraph, AutomationGraphCreateRequest,
    AutomationGraphId, AutomationGraphListRequest, AutomationGraphStatus,
    AutomationGraphUpdateRequest, AutomationNode, AutomationNodeCreateRequest, AutomationNodeId,
    AutomationRun, AutomationRunCreateRequest, AutomationRunId, AutomationRunListRequest,
    AutomationRunStep, AutomationRunStepCreateRequest, AutomationRunStepId,
    AutomationRunStepListRequest, AutomationRunStepUpdateRequest, AutomationRunUpdateRequest,
    CorrelationId, DeviceId, ElicitationRequest, ElicitationRequestStatus, ElicitationResolution,
    ElicitationResolutionAction, GitManagedWorktreeRecord, GitManagedWorktreeStatus,
    GitWorktreeDiagnostic, GitWorktreeOperationCheckpoint, GitWorktreeOperationDetail,
    GitWorktreeOperationRecord, GitWorktreeOperationStatus, GitWorktreeReadinessRecord,
    GitWorktreeReconciliationState, Hook, HookCreateRequest, HookId, HookInstallPreview,
    HookInstallState, McpServer, McpServerAgentMatrix, McpServerCreateRequest, McpServerId,
    McpServerProviderMatrix, McpServerSecretReference, McpServerStatus, PermissionActionDetail,
    PermissionRequest, PermissionRequestStatus, PermissionResolution, PermissionResponseKind,
    PermissionResponseOption, ProjectId, ProjectRecord, Prompt, PromptCreateRequest, PromptId,
    PromptStatus, ProviderCapabilityProbeResult, ProviderHealthProbeResult,
    ProviderInjectionPreview, ProviderInjectionPreviewRequest, ProviderKind,
    ProviderNativeExportApplyResult, ProviderNativeExportFilePlan, ProviderNativeExportListRequest,
    ProviderNativeExportPreview, ProviderNativeExportRecordSummary,
    ProviderNativeExportRollbackResult, ProviderNetworkDefaults, ProviderOptions,
    ProviderPermissionDefaults, ProviderProfile, ProviderProfileCreateRequest,
    ProviderProfileDefaultScope, ProviderProfileDefaultSelection, ProviderProfileId,
    ProviderProfileSetDefaultRequest, ProviderProfileStatus, ProviderSandboxDefaults,
    ProviderSecretReference, ProviderUsageRecord, ProviderUsageWindow, RedactedDiagnostic,
    RemoteAuditListRequest, RemoteAuditRecord, RemoteDeviceDetail, RemoteDeviceStatus,
    RemotePairingCode, RequestId, ScheduledTask, ScheduledTaskAttentionKind,
    ScheduledTaskAttentionListRequest, ScheduledTaskAttentionSummary,
    ScheduledTaskAuditListRequest, ScheduledTaskAuditOutcome, ScheduledTaskAuditRecord,
    ScheduledTaskCreateRequest, ScheduledTaskId, ScheduledTaskListRequest, ScheduledTaskRun,
    ScheduledTaskRunCreateRequest, ScheduledTaskRunId, ScheduledTaskRunListRequest,
    ScheduledTaskRunStatus, ScheduledTaskRunTrigger, ScheduledTaskRunUpdateRequest,
    ScheduledTaskStatus, ScheduledTaskUpdateRequest, Skill, SkillAgentMatrix, SkillCreateRequest,
    SkillId, SkillProviderMatrix, SkillStatus, TerminalId, TerminalSession, TimelineItem,
    TimelineItemId, TimelinePage, TimelinePayload, TimelineRedactionState, TimelineSource,
    TurnExecutionAttribution, VibexError, VibexResult, VibexSessionId, WorkspaceId, WorkspaceMode,
    WorkspaceRecord, agent_id_for_provider_kind, unix_timestamp_ms,
};

mod remote_v2;
pub use remote_v2::*;
mod provider_projection;
pub use provider_projection::*;
mod agent_provider_probe;
pub use agent_provider_probe::*;
mod usage;
pub use usage::*;
mod agent_auth_context;
pub use agent_auth_context::*;

pub type DbConnection = Connection;

pub mod runtime;
pub use runtime::{
    AgentSessionRuntimeRepository, AgentSessionRuntimeState, ContextBridgePrepareRequest,
    ContextBridgeRecord, ContextBridgeRepository, DesiredRuntimeSwitchEnqueueRequest,
    DesiredRuntimeSwitchEnqueueResult, MessageSubmissionPayloadRecord, MessageSubmissionRecord,
    MessageSubmissionRepository, RequestedSwitchClaimOutcome, RuntimeBindingRepository,
    RuntimeSwitchCommitRequest, RuntimeSwitchEventRepository, RuntimeSwitchRecord,
    RuntimeSwitchRepository, RuntimeSwitchReserveRequest, SwitchOperationAppendRequest,
    SwitchOperationJournalRepository, SwitchOperationRecord,
};

pub const CURRENT_SCHEMA_VERSION: i64 = 49;
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "stage0_foundation_smoke",
        sql: "
            CREATE TABLE IF NOT EXISTS foundation_smoke (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                marker TEXT NOT NULL,
                last_seen_at_ms INTEGER NOT NULL
            );
        ",
    },
    Migration {
        version: 2,
        name: "agent_session_core",
        sql: "
            CREATE TABLE IF NOT EXISTS projects (
                project_id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                root_path TEXT NOT NULL UNIQUE,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS workspaces (
                workspace_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
                root_path TEXT NOT NULL,
                mode TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                UNIQUE(project_id, root_path, mode)
            );

            CREATE TABLE IF NOT EXISTS agent_sessions (
                session_id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                project_id TEXT NOT NULL REFERENCES projects(project_id),
                workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id),
                workspace_root TEXT NOT NULL,
                workspace_mode TEXT NOT NULL,
                provider_kind TEXT NOT NULL,
                provider_profile_id TEXT NOT NULL,
                state TEXT NOT NULL,
                permission_mode TEXT NOT NULL,
                ask_on_risk INTEGER NOT NULL,
                bypass_all_permissions INTEGER NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                archived_at_ms INTEGER NULL,
                deleted_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_agent_sessions_workspace
                ON agent_sessions(workspace_id, deleted_at_ms, archived_at_ms);

            CREATE TABLE IF NOT EXISTS provider_bindings (
                session_id TEXT PRIMARY KEY REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                provider_kind TEXT NOT NULL,
                provider_profile_id TEXT NOT NULL,
                native_session_id TEXT NULL,
                native_thread_id TEXT NULL,
                native_resume_token TEXT NULL,
                redacted_metadata_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS session_provider_bindings (
                session_id TEXT NOT NULL REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                provider_profile_id TEXT NOT NULL,
                provider_kind TEXT NOT NULL,
                native_session_id TEXT NULL,
                native_thread_id TEXT NULL,
                native_resume_token TEXT NULL,
                redacted_metadata_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY (session_id, provider_profile_id)
            );

            CREATE TABLE IF NOT EXISTS agent_timeline_items (
                session_id TEXT NOT NULL REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                sequence INTEGER NOT NULL,
                timeline_item_id TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                source TEXT NOT NULL,
                timestamp_ms INTEGER NOT NULL,
                correlation_id TEXT NULL,
                provider_correlation_id TEXT NULL,
                payload_json TEXT NOT NULL,
                redaction_state TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                source_event_id TEXT NULL,
                PRIMARY KEY (session_id, sequence),
                UNIQUE(session_id, source_event_id)
            );
            CREATE INDEX IF NOT EXISTS idx_agent_timeline_session_sequence
                ON agent_timeline_items(session_id, sequence);

            CREATE TABLE IF NOT EXISTS permission_requests (
                request_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                project_id TEXT NULL REFERENCES projects(project_id),
                workspace_id TEXT NULL REFERENCES workspaces(workspace_id),
                provider_request_id TEXT NULL,
                risk_category TEXT NOT NULL,
                title TEXT NOT NULL,
                details_json TEXT NOT NULL,
                allowed_responses_json TEXT NOT NULL,
                status TEXT NOT NULL,
                requested_at_ms INTEGER NOT NULL,
                expires_at_ms INTEGER NULL,
                resolution_json TEXT NULL,
                resolved_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_permission_requests_session_status
                ON permission_requests(session_id, status);

            CREATE TABLE IF NOT EXISTS adapter_diagnostics (
                diagnostic_id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NULL REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                provider_kind TEXT NOT NULL,
                level TEXT NOT NULL,
                code TEXT NOT NULL,
                message TEXT NOT NULL,
                redacted_details_json TEXT NOT NULL,
                timestamp_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_adapter_diagnostics_session
                ON adapter_diagnostics(session_id, timestamp_ms);
        ",
    },
    Migration {
        version: 3,
        name: "pc_workbench_slice",
        sql: "
            CREATE TABLE IF NOT EXISTS terminal_sessions (
                terminal_id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
                title TEXT NOT NULL,
                shell TEXT NOT NULL,
                cwd TEXT NOT NULL,
                rows INTEGER NOT NULL,
                cols INTEGER NOT NULL,
                status TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                closed_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_terminal_sessions_workspace
                ON terminal_sessions(workspace_id, updated_at_ms);

            CREATE TABLE IF NOT EXISTS workbench_recent_files (
                workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
                path TEXT NOT NULL,
                last_opened_at_ms INTEGER NOT NULL,
                PRIMARY KEY (workspace_id, path)
            );

            CREATE TABLE IF NOT EXISTS git_snapshots (
                workspace_id TEXT PRIMARY KEY REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
                branch TEXT NULL,
                short_commit TEXT NULL,
                dirty INTEGER NOT NULL,
                changed_files INTEGER NOT NULL,
                captured_at_ms INTEGER NOT NULL
            );
        ",
    },
    Migration {
        version: 4,
        name: "advanced_git_worktrees",
        sql: "
            CREATE TABLE IF NOT EXISTS git_managed_worktrees (
                worktree_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
                workspace_id TEXT NULL REFERENCES workspaces(workspace_id) ON DELETE SET NULL,
                repo_root TEXT NOT NULL,
                worktree_path TEXT NOT NULL UNIQUE,
                branch TEXT NULL,
                base_ref TEXT NULL,
                head TEXT NULL,
                status TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                closed_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_git_managed_worktrees_project
                ON git_managed_worktrees(project_id, status, updated_at_ms);

            CREATE TABLE IF NOT EXISTS git_worktree_operations (
                operation_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
                source_workspace_id TEXT NULL REFERENCES workspaces(workspace_id) ON DELETE SET NULL,
                target_workspace_id TEXT NULL REFERENCES workspaces(workspace_id) ON DELETE SET NULL,
                operation TEXT NOT NULL,
                status TEXT NOT NULL,
                worktree_path TEXT NULL,
                branch TEXT NULL,
                base_ref TEXT NULL,
                head_before TEXT NULL,
                head_after TEXT NULL,
                error TEXT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_git_worktree_operations_project
                ON git_worktree_operations(project_id, updated_at_ms);
        ",
    },
    Migration {
        version: 5,
        name: "provider_profile_core",
        sql: "
            CREATE TABLE IF NOT EXISTS provider_profiles (
                provider_profile_id TEXT PRIMARY KEY,
                provider_kind TEXT NOT NULL,
                display_name TEXT NOT NULL,
                status TEXT NOT NULL,
                account_alias TEXT NULL,
                base_url TEXT NULL,
                default_model TEXT NULL,
                small_model TEXT NULL,
                large_model TEXT NULL,
                reasoning_effort TEXT NULL,
                sandbox_defaults_json TEXT NOT NULL,
                network_defaults_json TEXT NOT NULL,
                permission_defaults_json TEXT NOT NULL,
                provider_options_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                deleted_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_provider_profiles_kind
                ON provider_profiles(provider_kind, deleted_at_ms, updated_at_ms);

            CREATE TABLE IF NOT EXISTS provider_secret_references (
                secret_ref_id TEXT PRIMARY KEY,
                provider_profile_id TEXT NOT NULL REFERENCES provider_profiles(provider_profile_id) ON DELETE CASCADE,
                secret_kind TEXT NOT NULL,
                backend TEXT NOT NULL,
                setup_state TEXT NOT NULL,
                lookup_key TEXT NOT NULL,
                display_label TEXT NOT NULL,
                redacted_hint TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_provider_secret_references_profile
                ON provider_secret_references(provider_profile_id);

            CREATE TABLE IF NOT EXISTS provider_default_profiles (
                scope_kind TEXT NOT NULL,
                scope_id TEXT NOT NULL,
                provider_kind TEXT NOT NULL,
                provider_profile_id TEXT NOT NULL REFERENCES provider_profiles(provider_profile_id),
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(scope_kind, scope_id, provider_kind)
            );

            CREATE TABLE IF NOT EXISTS provider_injection_previews (
                preview_id TEXT PRIMARY KEY,
                provider_profile_id TEXT NOT NULL REFERENCES provider_profiles(provider_profile_id),
                request_json TEXT NOT NULL,
                preview_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_provider_injection_previews_profile
                ON provider_injection_previews(provider_profile_id, created_at_ms);
        ",
    },
    Migration {
        version: 6,
        name: "provider_health_usage",
        sql: "
            CREATE TABLE IF NOT EXISTS provider_health_probe_records (
                health_record_id TEXT PRIMARY KEY,
                provider_profile_id TEXT NOT NULL REFERENCES provider_profiles(provider_profile_id),
                provider_kind TEXT NOT NULL,
                probe_kind TEXT NOT NULL,
                status TEXT NOT NULL,
                summary TEXT NOT NULL,
                latency_ms INTEGER NULL,
                checked_at_ms INTEGER NOT NULL,
                expires_at_ms INTEGER NULL,
                diagnostics_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_provider_health_probe_records_latest
                ON provider_health_probe_records(provider_profile_id, probe_kind, checked_at_ms);

            CREATE TABLE IF NOT EXISTS provider_usage_records (
                usage_record_id TEXT PRIMARY KEY,
                provider_profile_id TEXT NOT NULL REFERENCES provider_profiles(provider_profile_id),
                provider_kind TEXT NOT NULL,
                source TEXT NOT NULL,
                unit TEXT NOT NULL,
                label TEXT NOT NULL,
                used REAL NULL,
                limit_value REAL NULL,
                remaining REAL NULL,
                window_label TEXT NULL,
                window_started_at_ms INTEGER NULL,
                window_ends_at_ms INTEGER NULL,
                recorded_at_ms INTEGER NOT NULL,
                metadata_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_provider_usage_records_latest
                ON provider_usage_records(provider_profile_id, unit, recorded_at_ms);
        ",
    },
    Migration {
        version: 7,
        name: "mcp_resource_management",
        sql: "
            CREATE TABLE IF NOT EXISTS mcp_servers (
                mcp_server_id TEXT PRIMARY KEY,
                display_name TEXT NOT NULL,
                transport_kind TEXT NOT NULL,
                status TEXT NOT NULL,
                scope_kind TEXT NOT NULL,
                project_id TEXT NULL REFERENCES projects(project_id) ON DELETE SET NULL,
                workspace_id TEXT NULL REFERENCES workspaces(workspace_id) ON DELETE SET NULL,
                command TEXT NULL,
                args_json TEXT NOT NULL,
                url TEXT NULL,
                description TEXT NULL,
                tags_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                deleted_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_mcp_servers_scope
                ON mcp_servers(scope_kind, deleted_at_ms, updated_at_ms);

            CREATE TABLE IF NOT EXISTS mcp_server_secret_references (
                secret_ref_id TEXT PRIMARY KEY,
                mcp_server_id TEXT NOT NULL REFERENCES mcp_servers(mcp_server_id) ON DELETE CASCADE,
                secret_kind TEXT NOT NULL,
                backend TEXT NOT NULL,
                setup_state TEXT NOT NULL,
                lookup_key TEXT NOT NULL,
                display_label TEXT NOT NULL,
                redacted_hint TEXT NOT NULL,
                target TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_mcp_server_secret_references_server
                ON mcp_server_secret_references(mcp_server_id);

            CREATE TABLE IF NOT EXISTS mcp_server_provider_matrix (
                mcp_server_id TEXT NOT NULL REFERENCES mcp_servers(mcp_server_id) ON DELETE CASCADE,
                provider_kind TEXT NOT NULL,
                enabled INTEGER NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(mcp_server_id, provider_kind)
            );
            CREATE INDEX IF NOT EXISTS idx_mcp_server_provider_matrix_provider
                ON mcp_server_provider_matrix(provider_kind, enabled);
        ",
    },
    Migration {
        version: 8,
        name: "skills_prompts_hooks_resource_management",
        sql: "
            CREATE TABLE IF NOT EXISTS skills (
                skill_id TEXT PRIMARY KEY,
                display_name TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                status TEXT NOT NULL,
                scope_kind TEXT NOT NULL,
                project_id TEXT NULL REFERENCES projects(project_id) ON DELETE SET NULL,
                workspace_id TEXT NULL REFERENCES workspaces(workspace_id) ON DELETE SET NULL,
                source_uri TEXT NULL,
                description TEXT NULL,
                tags_json TEXT NOT NULL,
                content_preview TEXT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                deleted_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_skills_scope
                ON skills(scope_kind, deleted_at_ms, updated_at_ms);

            CREATE TABLE IF NOT EXISTS skill_provider_matrix (
                skill_id TEXT NOT NULL REFERENCES skills(skill_id) ON DELETE CASCADE,
                provider_kind TEXT NOT NULL,
                enabled INTEGER NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(skill_id, provider_kind)
            );
            CREATE INDEX IF NOT EXISTS idx_skill_provider_matrix_provider
                ON skill_provider_matrix(provider_kind, enabled);

            CREATE TABLE IF NOT EXISTS prompts (
                prompt_id TEXT PRIMARY KEY,
                display_name TEXT NOT NULL,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                scope_kind TEXT NOT NULL,
                project_id TEXT NULL REFERENCES projects(project_id) ON DELETE SET NULL,
                workspace_id TEXT NULL REFERENCES workspaces(workspace_id) ON DELETE SET NULL,
                body TEXT NOT NULL,
                description TEXT NULL,
                tags_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                deleted_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_prompts_scope_kind
                ON prompts(scope_kind, kind, deleted_at_ms, updated_at_ms);

            CREATE TABLE IF NOT EXISTS hooks (
                hook_id TEXT PRIMARY KEY,
                display_name TEXT NOT NULL,
                provider_kind TEXT NOT NULL,
                event_kind TEXT NOT NULL,
                status TEXT NOT NULL,
                install_state TEXT NOT NULL,
                command_preview TEXT NULL,
                managed_marker TEXT NOT NULL,
                description TEXT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                deleted_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_hooks_provider_event
                ON hooks(provider_kind, event_kind, deleted_at_ms, updated_at_ms);

            CREATE TABLE IF NOT EXISTS hook_install_previews (
                preview_id TEXT PRIMARY KEY,
                hook_id TEXT NOT NULL REFERENCES hooks(hook_id),
                target_path TEXT NOT NULL,
                marker TEXT NOT NULL,
                redacted_preview TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_hook_install_previews_hook
                ON hook_install_previews(hook_id, created_at_ms);
        ",
    },
    Migration {
        version: 9,
        name: "provider_native_export_records",
        sql: "
            CREATE TABLE IF NOT EXISTS provider_native_export_records (
                export_id TEXT PRIMARY KEY,
                provider_profile_id TEXT NOT NULL REFERENCES provider_profiles(provider_profile_id),
                source TEXT NOT NULL,
                mode TEXT NOT NULL,
                status TEXT NOT NULL,
                preview_json TEXT NOT NULL,
                applied_at_ms INTEGER NULL,
                rolled_back_at_ms INTEGER NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_provider_native_export_records_profile
                ON provider_native_export_records(provider_profile_id, created_at_ms);

            CREATE TABLE IF NOT EXISTS provider_native_export_file_operations (
                operation_id TEXT PRIMARY KEY,
                export_id TEXT NOT NULL REFERENCES provider_native_export_records(export_id) ON DELETE CASCADE,
                source TEXT NOT NULL,
                file_kind TEXT NOT NULL,
                operation_kind TEXT NOT NULL,
                target_path TEXT NOT NULL,
                backup_path TEXT NULL,
                temp_path TEXT NULL,
                marker TEXT NULL,
                status TEXT NOT NULL,
                redacted_diff TEXT NOT NULL,
                diagnostics_json TEXT NOT NULL,
                target_size_before INTEGER NULL,
                target_size_after INTEGER NULL,
                backup_size INTEGER NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_provider_native_export_file_operations_export
                ON provider_native_export_file_operations(export_id, status);
            CREATE INDEX IF NOT EXISTS idx_provider_native_export_file_operations_target
                ON provider_native_export_file_operations(target_path, updated_at_ms);
        ",
    },
    Migration {
        version: 10,
        name: "remote_devices_pairing_audit",
        sql: "
            CREATE TABLE IF NOT EXISTS remote_devices (
                device_id TEXT PRIMARY KEY,
                display_name TEXT NOT NULL,
                public_key TEXT NULL,
                auth_secret_hash TEXT NOT NULL,
                permission_level TEXT NOT NULL,
                status TEXT NOT NULL,
                paired_at_ms INTEGER NULL,
                last_seen_at_ms INTEGER NULL,
                revoked_at_ms INTEGER NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_remote_devices_status
                ON remote_devices(status, updated_at_ms);

            CREATE TABLE IF NOT EXISTS remote_pairing_codes (
                pairing_id TEXT PRIMARY KEY,
                code_hash TEXT NOT NULL UNIQUE,
                permission_level TEXT NOT NULL,
                expires_at_ms INTEGER NOT NULL,
                claimed_device_id TEXT NULL REFERENCES remote_devices(device_id) ON DELETE SET NULL,
                created_at_ms INTEGER NOT NULL,
                claimed_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_remote_pairing_codes_expiry
                ON remote_pairing_codes(expires_at_ms, claimed_at_ms);

            CREATE TABLE IF NOT EXISTS remote_audit_logs (
                audit_id TEXT PRIMARY KEY,
                device_id TEXT NULL REFERENCES remote_devices(device_id) ON DELETE SET NULL,
                action TEXT NOT NULL,
                target_kind TEXT NOT NULL,
                target_id TEXT NULL,
                outcome TEXT NOT NULL,
                redacted_summary TEXT NOT NULL,
                request_id TEXT NULL,
                correlation_id TEXT NULL,
                created_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_remote_audit_logs_device
                ON remote_audit_logs(device_id, created_at_ms);
            CREATE INDEX IF NOT EXISTS idx_remote_audit_logs_created
                ON remote_audit_logs(created_at_ms);
        ",
    },
    Migration {
        version: 11,
        name: "provider_capability_probe_records",
        sql: "
            CREATE TABLE IF NOT EXISTS provider_capability_probe_records (
                capability_record_id TEXT PRIMARY KEY,
                provider_profile_id TEXT NOT NULL REFERENCES provider_profiles(provider_profile_id),
                provider_kind TEXT NOT NULL,
                status TEXT NOT NULL,
                summary TEXT NOT NULL,
                capabilities_json TEXT NOT NULL,
                source TEXT NOT NULL,
                checked_at_ms INTEGER NOT NULL,
                expires_at_ms INTEGER NULL,
                diagnostics_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_provider_capability_probe_records_latest
                ON provider_capability_probe_records(provider_profile_id, checked_at_ms);
        ",
    },
    Migration {
        version: 12,
        name: "scheduled_task_contract_storage",
        sql: "
            CREATE TABLE IF NOT EXISTS scheduled_tasks (
                scheduled_task_id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                prompt TEXT NOT NULL,
                project_id TEXT NULL REFERENCES projects(project_id) ON DELETE SET NULL,
                workspace_id TEXT NULL REFERENCES workspaces(workspace_id) ON DELETE SET NULL,
                workspace_root TEXT NOT NULL,
                workspace_mode TEXT NOT NULL,
                provider_kind TEXT NOT NULL,
                provider_profile_id TEXT NULL REFERENCES provider_profiles(provider_profile_id) ON DELETE SET NULL,
                schedule_json TEXT NOT NULL,
                status TEXT NOT NULL,
                safety_json TEXT NOT NULL,
                next_run_at_ms INTEGER NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                deleted_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_due
                ON scheduled_tasks(status, next_run_at_ms);
            CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_workspace
                ON scheduled_tasks(workspace_id, status, updated_at_ms);

            CREATE TABLE IF NOT EXISTS scheduled_task_runs (
                scheduled_task_run_id TEXT PRIMARY KEY,
                scheduled_task_id TEXT NOT NULL REFERENCES scheduled_tasks(scheduled_task_id) ON DELETE CASCADE,
                status TEXT NOT NULL,
                trigger TEXT NOT NULL,
                session_id TEXT NULL REFERENCES agent_sessions(session_id) ON DELETE SET NULL,
                due_at_ms INTEGER NOT NULL,
                started_at_ms INTEGER NULL,
                ended_at_ms INTEGER NULL,
                attempt INTEGER NOT NULL,
                error_code TEXT NULL,
                error_message TEXT NULL,
                redacted_diagnostics_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_scheduled_task_runs_task
                ON scheduled_task_runs(scheduled_task_id, started_at_ms, created_at_ms);
            CREATE INDEX IF NOT EXISTS idx_scheduled_task_runs_session
                ON scheduled_task_runs(session_id);
        ",
    },
    Migration {
        version: 13,
        name: "automation_graph_contract_storage",
        sql: "
            CREATE TABLE IF NOT EXISTS automation_graphs (
                automation_graph_id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                description TEXT NULL,
                project_id TEXT NULL REFERENCES projects(project_id) ON DELETE SET NULL,
                workspace_id TEXT NULL REFERENCES workspaces(workspace_id) ON DELETE SET NULL,
                workspace_root TEXT NOT NULL,
                workspace_mode TEXT NOT NULL,
                provider_kind TEXT NULL,
                provider_profile_id TEXT NULL REFERENCES provider_profiles(provider_profile_id) ON DELETE SET NULL,
                trigger_json TEXT NOT NULL,
                status TEXT NOT NULL,
                version INTEGER NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                deleted_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_automation_graphs_workspace
                ON automation_graphs(workspace_id, status, updated_at_ms);
            CREATE INDEX IF NOT EXISTS idx_automation_graphs_status
                ON automation_graphs(status, updated_at_ms);

            CREATE TABLE IF NOT EXISTS automation_graph_nodes (
                automation_node_id TEXT PRIMARY KEY,
                automation_graph_id TEXT NOT NULL REFERENCES automation_graphs(automation_graph_id) ON DELETE CASCADE,
                kind TEXT NOT NULL,
                title TEXT NOT NULL,
                config_json TEXT NOT NULL,
                position_json TEXT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_automation_graph_nodes_graph
                ON automation_graph_nodes(automation_graph_id);

            CREATE TABLE IF NOT EXISTS automation_graph_edges (
                automation_edge_id TEXT PRIMARY KEY,
                automation_graph_id TEXT NOT NULL REFERENCES automation_graphs(automation_graph_id) ON DELETE CASCADE,
                source_node_id TEXT NOT NULL,
                target_node_id TEXT NOT NULL,
                condition_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_automation_graph_edges_graph
                ON automation_graph_edges(automation_graph_id);

            CREATE TABLE IF NOT EXISTS automation_graph_runs (
                automation_run_id TEXT PRIMARY KEY,
                automation_graph_id TEXT NOT NULL REFERENCES automation_graphs(automation_graph_id) ON DELETE CASCADE,
                status TEXT NOT NULL,
                trigger TEXT NOT NULL,
                scheduled_task_id TEXT NULL REFERENCES scheduled_tasks(scheduled_task_id) ON DELETE SET NULL,
                session_id TEXT NULL REFERENCES agent_sessions(session_id) ON DELETE SET NULL,
                started_at_ms INTEGER NULL,
                ended_at_ms INTEGER NULL,
                error_code TEXT NULL,
                error_message TEXT NULL,
                redacted_diagnostics_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_automation_graph_runs_graph
                ON automation_graph_runs(automation_graph_id, created_at_ms);
            CREATE INDEX IF NOT EXISTS idx_automation_graph_runs_status
                ON automation_graph_runs(status, updated_at_ms);

            CREATE TABLE IF NOT EXISTS automation_graph_run_steps (
                automation_run_step_id TEXT PRIMARY KEY,
                automation_run_id TEXT NOT NULL REFERENCES automation_graph_runs(automation_run_id) ON DELETE CASCADE,
                automation_node_id TEXT NOT NULL,
                status TEXT NOT NULL,
                session_id TEXT NULL REFERENCES agent_sessions(session_id) ON DELETE SET NULL,
                permission_request_id TEXT NULL,
                started_at_ms INTEGER NULL,
                ended_at_ms INTEGER NULL,
                error_code TEXT NULL,
                error_message TEXT NULL,
                redacted_diagnostics_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_automation_graph_run_steps_run
                ON automation_graph_run_steps(automation_run_id, created_at_ms);
            CREATE INDEX IF NOT EXISTS idx_automation_graph_run_steps_permission
                ON automation_graph_run_steps(permission_request_id);
        ",
    },
    Migration {
        version: 14,
        name: "right_rail_plugin_storage",
        sql: "
            CREATE TABLE IF NOT EXISTS right_rail_plugins (
                plugin_id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                system_key TEXT NULL,
                builtin_key TEXT NULL,
                display_name TEXT NOT NULL,
                url TEXT NULL,
                logo TEXT NULL,
                desktop_user_agent TEXT NULL,
                mobile_user_agent TEXT NULL,
                ua_mode TEXT NULL,
                status TEXT NOT NULL,
                order_index INTEGER NOT NULL,
                data_directory TEXT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                deleted_at_ms INTEGER NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_right_rail_plugins_system_key
                ON right_rail_plugins(system_key)
                WHERE system_key IS NOT NULL;
            CREATE UNIQUE INDEX IF NOT EXISTS idx_right_rail_plugins_builtin_key
                ON right_rail_plugins(builtin_key)
                WHERE builtin_key IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_right_rail_plugins_order
                ON right_rail_plugins(deleted_at_ms, order_index);
        ",
    },
    Migration {
        version: 15,
        name: "workspace_project_soft_delete",
        sql: "
            ALTER TABLE projects ADD COLUMN deleted_at_ms INTEGER NULL;
            ALTER TABLE workspaces ADD COLUMN deleted_at_ms INTEGER NULL;
            CREATE INDEX IF NOT EXISTS idx_projects_deleted
                ON projects(deleted_at_ms, updated_at_ms);
            CREATE INDEX IF NOT EXISTS idx_workspaces_project_deleted
                ON workspaces(project_id, deleted_at_ms, updated_at_ms);
        ",
    },
    Migration {
        version: 16,
        name: "agent_registry_snapshot",
        sql: "
            CREATE TABLE IF NOT EXISTS agent_configs (
                agent_id TEXT PRIMARY KEY,
                runtime_kind TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                label_override TEXT NULL,
                description_override TEXT NULL,
                enabled INTEGER NOT NULL,
                order_index INTEGER NOT NULL,
                command_json TEXT NULL,
                env_json TEXT NOT NULL,
                params_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                deleted_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_agent_configs_order
                ON agent_configs(deleted_at_ms, order_index, agent_id);

            CREATE TABLE IF NOT EXISTS agent_discovery_records (
                discovery_record_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                cwd_scope TEXT NOT NULL,
                install_status TEXT NOT NULL,
                config_status TEXT NOT NULL,
                runtime_status TEXT NOT NULL,
                binary_path TEXT NULL,
                version TEXT NULL,
                native_config_paths_json TEXT NOT NULL,
                models_json TEXT NOT NULL,
                modes_json TEXT NOT NULL,
                diagnostics_json TEXT NOT NULL,
                discovered_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_agent_discovery_records_latest
                ON agent_discovery_records(agent_id, cwd_scope, discovered_at_ms DESC);
        ",
    },
    Migration {
        version: 17,
        name: "agent_model_provider_profiles",
        sql: "
            ALTER TABLE provider_profiles ADD COLUMN agent_id TEXT NULL;

            UPDATE provider_profiles
            SET agent_id = CASE provider_kind
                WHEN 'mock' THEN 'mock'
                WHEN 'claude' THEN 'claude'
                WHEN 'codex' THEN 'codex'
                WHEN 'acp' THEN 'opencode'
                ELSE provider_kind
            END
            WHERE agent_id IS NULL;

            CREATE INDEX IF NOT EXISTS idx_provider_profiles_agent
                ON provider_profiles(agent_id, deleted_at_ms, updated_at_ms);

            CREATE TABLE IF NOT EXISTS agent_default_model_provider_profiles (
                scope_kind TEXT NOT NULL,
                scope_id TEXT NOT NULL DEFAULT '',
                agent_id TEXT NOT NULL,
                provider_profile_id TEXT NOT NULL REFERENCES provider_profiles(provider_profile_id),
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(scope_kind, scope_id, agent_id)
            );

            CREATE TABLE IF NOT EXISTS agent_model_provider_failover (
                agent_id TEXT NOT NULL,
                provider_profile_id TEXT NOT NULL REFERENCES provider_profiles(provider_profile_id),
                order_index INTEGER NOT NULL,
                enabled INTEGER NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(agent_id, provider_profile_id)
            );
            CREATE INDEX IF NOT EXISTS idx_agent_model_provider_failover_order
                ON agent_model_provider_failover(agent_id, order_index, provider_profile_id);
        ",
    },
    Migration {
        version: 18,
        name: "resource_agent_matrices",
        sql: "
            CREATE TABLE IF NOT EXISTS mcp_server_agent_matrix (
                mcp_server_id TEXT NOT NULL REFERENCES mcp_servers(mcp_server_id) ON DELETE CASCADE,
                agent_id TEXT NOT NULL,
                enabled INTEGER NOT NULL,
                source_kind TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(mcp_server_id, agent_id)
            );
            CREATE INDEX IF NOT EXISTS idx_mcp_server_agent_matrix_agent
                ON mcp_server_agent_matrix(agent_id, enabled);

            CREATE TABLE IF NOT EXISTS skill_agent_matrix (
                skill_id TEXT NOT NULL REFERENCES skills(skill_id) ON DELETE CASCADE,
                agent_id TEXT NOT NULL,
                enabled INTEGER NOT NULL,
                source_kind TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(skill_id, agent_id)
            );
            CREATE INDEX IF NOT EXISTS idx_skill_agent_matrix_agent
                ON skill_agent_matrix(agent_id, enabled);

            INSERT OR IGNORE INTO mcp_server_agent_matrix (
                mcp_server_id, agent_id, enabled, source_kind, created_at_ms, updated_at_ms
            )
            SELECT
                mcp_server_id,
                CASE provider_kind
                    WHEN 'mock' THEN 'mock'
                    WHEN 'claude' THEN 'claude'
                    WHEN 'codex' THEN 'codex'
                    WHEN 'acp' THEN 'opencode'
                    ELSE provider_kind
                END,
                enabled,
                'legacy_backfill',
                created_at_ms,
                updated_at_ms
            FROM mcp_server_provider_matrix;

            INSERT OR IGNORE INTO skill_agent_matrix (
                skill_id, agent_id, enabled, source_kind, created_at_ms, updated_at_ms
            )
            SELECT
                skill_id,
                CASE provider_kind
                    WHEN 'mock' THEN 'mock'
                    WHEN 'claude' THEN 'claude'
                    WHEN 'codex' THEN 'codex'
                    WHEN 'acp' THEN 'opencode'
                    ELSE provider_kind
                END,
                enabled,
                'legacy_backfill',
                created_at_ms,
                updated_at_ms
            FROM skill_provider_matrix;
        ",
    },
    Migration {
        version: 19,
        name: "remove_mock_agent_registry_state",
        sql: "
            UPDATE agent_configs
            SET deleted_at_ms = COALESCE(deleted_at_ms, updated_at_ms, created_at_ms, 0),
                enabled = 0
            WHERE agent_id = 'mock' OR runtime_kind = 'mock';

            DELETE FROM agent_discovery_records
            WHERE agent_id = 'mock';

            UPDATE provider_profiles
            SET deleted_at_ms = COALESCE(deleted_at_ms, updated_at_ms, created_at_ms, 0),
                status = 'disabled'
            WHERE agent_id = 'mock' OR provider_kind = 'mock';

            DELETE FROM agent_default_model_provider_profiles
            WHERE agent_id = 'mock'
               OR provider_profile_id IN (
                    SELECT provider_profile_id
                    FROM provider_profiles
                    WHERE agent_id = 'mock' OR provider_kind = 'mock'
               );

            DELETE FROM agent_model_provider_failover
            WHERE agent_id = 'mock'
               OR provider_profile_id IN (
                    SELECT provider_profile_id
                    FROM provider_profiles
                    WHERE agent_id = 'mock' OR provider_kind = 'mock'
               );

            DELETE FROM mcp_server_agent_matrix
            WHERE agent_id = 'mock';

            DELETE FROM skill_agent_matrix
            WHERE agent_id = 'mock';

            DELETE FROM mcp_server_provider_matrix
            WHERE provider_kind = 'mock';

            DELETE FROM skill_provider_matrix
            WHERE provider_kind = 'mock';
        ",
    },
    Migration {
        version: 20,
        name: "session_provider_bindings",
        sql: "
            CREATE TABLE IF NOT EXISTS session_provider_bindings (
                session_id TEXT NOT NULL REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                provider_profile_id TEXT NOT NULL,
                provider_kind TEXT NOT NULL,
                native_session_id TEXT NULL,
                native_thread_id TEXT NULL,
                native_resume_token TEXT NULL,
                redacted_metadata_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY (session_id, provider_profile_id)
            );

            INSERT OR IGNORE INTO session_provider_bindings (
                session_id, provider_profile_id, provider_kind, native_session_id,
                native_thread_id, native_resume_token, redacted_metadata_json,
                created_at_ms, updated_at_ms
            )
            SELECT
                session_id, provider_profile_id, provider_kind, native_session_id,
                native_thread_id, native_resume_token, redacted_metadata_json,
                created_at_ms, updated_at_ms
            FROM provider_bindings;
        ",
    },
    Migration {
        version: 21,
        name: "provider_profile_configured_models",
        sql: "
            ALTER TABLE provider_profiles
                ADD COLUMN configured_models_json TEXT NOT NULL DEFAULT '[]';
        ",
    },
    Migration {
        version: 22,
        name: "provider_session_config_state",
        sql: "
            ALTER TABLE provider_bindings
                ADD COLUMN session_config_state_json TEXT NULL;
            ALTER TABLE session_provider_bindings
                ADD COLUMN session_config_state_json TEXT NULL;
        ",
    },
    Migration {
        version: 23,
        name: "acp_runtime_hot_switch_core",
        sql: "
            ALTER TABLE agent_sessions ADD COLUMN current_agent_id TEXT NULL;
            ALTER TABLE agent_sessions ADD COLUMN current_binding_id TEXT NULL;
            ALTER TABLE agent_sessions ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE agent_sessions ADD COLUMN activation_generation INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE agent_sessions ADD COLUMN desired_runtime_selection_json TEXT NULL;
            ALTER TABLE agent_sessions ADD COLUMN effective_runtime_selection_json TEXT NULL;
            ALTER TABLE agent_sessions ADD COLUMN runtime_selection_status TEXT NULL;
            ALTER TABLE agent_sessions ADD COLUMN selection_revision INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE agent_sessions ADD COLUMN pending_switch_id TEXT NULL;

            CREATE TABLE IF NOT EXISTS session_runtime_bindings (
                binding_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                agent_id TEXT NOT NULL,
                transport_kind TEXT NOT NULL,
                adapter_id TEXT NOT NULL,
                adapter_version TEXT NOT NULL,
                adapter_compatibility_identity TEXT NOT NULL,
                provider_profile_id TEXT NOT NULL,
                profile_revision INTEGER NOT NULL DEFAULT 0,
                native_session_id TEXT NULL,
                native_state_home_id TEXT NOT NULL,
                provider_resume_identity TEXT NULL,
                process_spawn_fingerprint TEXT NOT NULL,
                session_runtime_config_state_json TEXT NOT NULL,
                capability_snapshot_json TEXT NULL,
                restore_compatibility_key_json TEXT NULL,
                last_context_sequence INTEGER NOT NULL DEFAULT 0,
                last_summary_sequence INTEGER NOT NULL DEFAULT 0,
                context_bridge_version INTEGER NOT NULL DEFAULT 0,
                activation_generation INTEGER NOT NULL DEFAULT 0,
                binding_state TEXT NOT NULL,
                created_by_switch_id TEXT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            -- Deliberately a plain (non-unique) index: the same
            -- session/agent/profile/compatibility identity may own multiple
            -- fresh bindings (plan section 20.2).
            CREATE INDEX IF NOT EXISTS idx_session_runtime_bindings_route
                ON session_runtime_bindings(
                    session_id, agent_id, provider_profile_id, adapter_compatibility_identity
                );
            CREATE INDEX IF NOT EXISTS idx_session_runtime_bindings_session
                ON session_runtime_bindings(session_id, binding_state);

            CREATE TABLE IF NOT EXISTS runtime_switches (
                switch_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                idempotency_key TEXT NOT NULL,
                source_revision INTEGER NOT NULL,
                source_binding_id TEXT NULL,
                desired_selection_revision INTEGER NOT NULL,
                target_binding_id TEXT NULL,
                target_agent_id TEXT NOT NULL,
                target_adapter_id TEXT NOT NULL,
                target_profile_id TEXT NOT NULL,
                requested_policy_json TEXT NULL,
                active_work_policy_json TEXT NULL,
                requested_session_config_json TEXT NULL,
                restore_compatibility_result_json TEXT NULL,
                status TEXT NOT NULL,
                error_code TEXT NULL,
                error_detail_redacted TEXT NULL,
                worker_lease_owner TEXT NULL,
                worker_lease_deadline_ms INTEGER NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                committed_at_ms INTEGER NULL,
                UNIQUE(session_id, idempotency_key)
            );
            CREATE INDEX IF NOT EXISTS idx_runtime_switches_session_status
                ON runtime_switches(session_id, status);

            CREATE TABLE IF NOT EXISTS runtime_switch_operations (
                operation_id TEXT PRIMARY KEY,
                switch_id TEXT NOT NULL REFERENCES runtime_switches(switch_id) ON DELETE CASCADE,
                sequence INTEGER NOT NULL,
                operation_kind TEXT NOT NULL,
                request_fingerprint TEXT NOT NULL,
                adapter_idempotency_token TEXT NULL,
                retry_semantics TEXT NOT NULL,
                status TEXT NOT NULL,
                native_result_reference TEXT NULL,
                error_detail_redacted TEXT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                UNIQUE(switch_id, sequence)
            );

            CREATE TABLE IF NOT EXISTS agent_message_submissions (
                submission_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                message_idempotency_key TEXT NOT NULL,
                desired_runtime_selection_json TEXT NOT NULL,
                required_switch_id TEXT NULL,
                message_payload_reference TEXT NOT NULL,
                user_message_timeline_item_id TEXT NULL,
                status TEXT NOT NULL,
                dispatch_operation_id TEXT NULL,
                provider_correlation_id TEXT NULL,
                error_code TEXT NULL,
                error_detail_redacted TEXT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                dispatched_at_ms INTEGER NULL,
                UNIQUE(session_id, message_idempotency_key)
            );
            CREATE INDEX IF NOT EXISTS idx_agent_message_submissions_session_status
                ON agent_message_submissions(session_id, status);
        ",
    },
    Migration {
        version: 24,
        name: "turn_execution_attribution",
        sql: "
            ALTER TABLE agent_timeline_items
                ADD COLUMN execution_attribution_json TEXT NULL;
        ",
    },
    Migration {
        version: 25,
        name: "runtime_selection_control_plane",
        sql: "
            ALTER TABLE agent_sessions
                ADD COLUMN runtime_selection_error_code TEXT NULL;

            CREATE TABLE IF NOT EXISTS runtime_switch_events (
                event_id TEXT PRIMARY KEY,
                switch_id TEXT NOT NULL REFERENCES runtime_switches(switch_id) ON DELETE CASCADE,
                session_id TEXT NOT NULL REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                event_kind TEXT NOT NULL,
                visibility TEXT NOT NULL,
                status TEXT NOT NULL,
                error_code TEXT NULL,
                created_at_ms INTEGER NOT NULL,
                UNIQUE(switch_id, event_kind, status)
            );
            CREATE INDEX IF NOT EXISTS idx_runtime_switch_events_session
                ON runtime_switch_events(session_id, created_at_ms, event_id);
        ",
    },
    Migration {
        version: 26,
        name: "durable_message_submission_payloads",
        sql: "
            CREATE TABLE IF NOT EXISTS agent_message_submission_payloads (
                payload_reference TEXT PRIMARY KEY,
                submission_id TEXT NOT NULL UNIQUE
                    REFERENCES agent_message_submissions(submission_id) ON DELETE CASCADE,
                session_id TEXT NOT NULL
                    REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                submission_sequence INTEGER NOT NULL,
                payload_json TEXT NOT NULL,
                result_first_sequence INTEGER NULL,
                result_last_sequence INTEGER NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                UNIQUE(session_id, submission_sequence)
            );
            CREATE INDEX IF NOT EXISTS idx_agent_message_submission_payloads_session_sequence
                ON agent_message_submission_payloads(session_id, submission_sequence);
        ",
    },
    Migration {
        version: 27,
        name: "incremental_context_bridge",
        sql: "
            CREATE TABLE IF NOT EXISTS runtime_context_bridges (
                switch_id TEXT PRIMARY KEY
                    REFERENCES runtime_switches(switch_id) ON DELETE CASCADE,
                session_id TEXT NOT NULL
                    REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                target_binding_id TEXT NOT NULL UNIQUE
                    REFERENCES session_runtime_bindings(binding_id) ON DELETE CASCADE,
                from_context_sequence INTEGER NOT NULL CHECK(from_context_sequence >= 0),
                from_summary_sequence INTEGER NOT NULL CHECK(from_summary_sequence >= 0),
                prepare_sequence INTEGER NOT NULL CHECK(prepare_sequence > from_context_sequence),
                summary_sequence INTEGER NOT NULL,
                bridge_version INTEGER NOT NULL CHECK(bridge_version > 0),
                content_fingerprint TEXT NOT NULL,
                applied_submission_id TEXT NULL,
                applied_context_sequence INTEGER NULL,
                created_at_ms INTEGER NOT NULL,
                applied_at_ms INTEGER NULL,
                CHECK(from_summary_sequence <= from_context_sequence),
                CHECK(summary_sequence >= from_summary_sequence),
                CHECK(summary_sequence <= prepare_sequence),
                CHECK(
                    (applied_at_ms IS NULL AND applied_context_sequence IS NULL)
                    OR
                    (applied_at_ms IS NOT NULL AND applied_context_sequence >= prepare_sequence)
                )
            );
            CREATE INDEX IF NOT EXISTS idx_runtime_context_bridges_pending
                ON runtime_context_bridges(target_binding_id, applied_at_ms);
        ",
    },
    Migration {
        version: 28,
        name: "acp_only_runtime_cutover",
        sql: "
            -- This release intentionally does not migrate Native online
            -- sessions. Cascades clear every session-scoped runtime/timeline
            -- row before the legacy authority columns and tables disappear.
            DELETE FROM agent_sessions;

            DROP TABLE session_provider_bindings;
            DROP TABLE provider_bindings;

            ALTER TABLE agent_sessions DROP COLUMN provider_kind;
            ALTER TABLE agent_sessions DROP COLUMN provider_profile_id;
        ",
    },
    Migration {
        version: 29,
        name: "remote_protocol_v2_pairing_offers",
        sql: "
            ALTER TABLE remote_pairing_codes ADD COLUMN offer_format_version INTEGER NULL;
            ALTER TABLE remote_pairing_codes ADD COLUMN protocol_min_major INTEGER NULL;
            ALTER TABLE remote_pairing_codes ADD COLUMN protocol_min_minor INTEGER NULL;
            ALTER TABLE remote_pairing_codes ADD COLUMN protocol_max_major INTEGER NULL;
            ALTER TABLE remote_pairing_codes ADD COLUMN protocol_max_minor INTEGER NULL;
            ALTER TABLE remote_pairing_codes ADD COLUMN server_id TEXT NULL;
            ALTER TABLE remote_pairing_codes ADD COLUMN server_identity_public_key TEXT NULL;
            ALTER TABLE remote_pairing_codes
                ADD COLUMN direct_candidates_json TEXT NOT NULL DEFAULT '[]';
            ALTER TABLE remote_pairing_codes ADD COLUMN relay_candidate_json TEXT NULL;
            ALTER TABLE remote_pairing_codes
                ADD COLUMN granted_permissions_json TEXT NOT NULL DEFAULT '[]';
            ALTER TABLE remote_pairing_codes ADD COLUMN canceled_at_ms INTEGER NULL;
            ALTER TABLE remote_pairing_codes ADD COLUMN claim_nonce_hash TEXT NULL;
            ALTER TABLE remote_pairing_codes ADD COLUMN device_ephemeral_public_key TEXT NULL;
            ALTER TABLE remote_devices ADD COLUMN grant_revision INTEGER NOT NULL DEFAULT 1;

            CREATE INDEX IF NOT EXISTS idx_remote_pairing_offers_state
                ON remote_pairing_codes(expires_at_ms, canceled_at_ms, claimed_at_ms);
        ",
    },
    Migration {
        version: 30,
        name: "provider_runtime_option_snapshots",
        sql: "
            CREATE TABLE IF NOT EXISTS provider_runtime_option_snapshots (
                provider_profile_id TEXT PRIMARY KEY
                    REFERENCES provider_profiles(provider_profile_id) ON DELETE CASCADE,
                agent_id TEXT NOT NULL,
                model_response_json TEXT NULL,
                session_config_json TEXT NULL,
                last_success_at_ms INTEGER NULL,
                last_attempt_at_ms INTEGER NOT NULL,
                last_error_code TEXT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_provider_runtime_option_snapshots_agent
                ON provider_runtime_option_snapshots(agent_id, last_attempt_at_ms);
        ",
    },
    Migration {
        version: 31,
        name: "agent_usage_statistics",
        sql: "
            CREATE TABLE IF NOT EXISTS agent_usage_checkpoints (
                usage_stream_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL
                    REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                binding_id TEXT NOT NULL,
                last_activation_generation INTEGER NOT NULL
                    CHECK(last_activation_generation >= 0),
                agent_id TEXT NOT NULL,
                provider_profile_id TEXT NOT NULL,
                last_model_id TEXT NOT NULL,
                reset_epoch INTEGER NOT NULL CHECK(reset_epoch >= 0),
                counter_origin TEXT NOT NULL,
                cumulative_input_tokens INTEGER NULL CHECK(cumulative_input_tokens >= 0),
                cumulative_output_tokens INTEGER NULL CHECK(cumulative_output_tokens >= 0),
                cumulative_thought_tokens INTEGER NULL CHECK(cumulative_thought_tokens >= 0),
                cumulative_cached_read_tokens INTEGER NULL
                    CHECK(cumulative_cached_read_tokens >= 0),
                cumulative_cached_write_tokens INTEGER NULL
                    CHECK(cumulative_cached_write_tokens >= 0),
                cumulative_total_tokens INTEGER NULL CHECK(cumulative_total_tokens >= 0),
                last_usage_execution_id TEXT NULL,
                last_observation_sequence INTEGER NOT NULL
                    CHECK(last_observation_sequence >= 0),
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                UNIQUE(session_id, binding_id)
            );
            CREATE INDEX IF NOT EXISTS idx_agent_usage_checkpoints_session
                ON agent_usage_checkpoints(session_id, updated_at_ms);

            CREATE TABLE IF NOT EXISTS agent_turn_usage_facts (
                usage_execution_id TEXT PRIMARY KEY,
                message_submission_id TEXT NULL,
                session_id TEXT NOT NULL
                    REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                project_id TEXT NOT NULL
                    REFERENCES projects(project_id) ON DELETE CASCADE,
                workspace_id TEXT NOT NULL
                    REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
                binding_id TEXT NOT NULL,
                activation_generation INTEGER NOT NULL CHECK(activation_generation >= 0),
                reset_epoch INTEGER NOT NULL CHECK(reset_epoch >= 0),
                agent_id TEXT NOT NULL,
                provider_profile_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                execution_status TEXT NOT NULL,
                input_delta INTEGER NULL CHECK(input_delta >= 0),
                output_delta INTEGER NULL CHECK(output_delta >= 0),
                thought_delta INTEGER NULL CHECK(thought_delta >= 0),
                cached_read_delta INTEGER NULL CHECK(cached_read_delta >= 0),
                cached_write_delta INTEGER NULL CHECK(cached_write_delta >= 0),
                total_delta INTEGER NULL CHECK(total_delta >= 0),
                cumulative_input_after INTEGER NULL CHECK(cumulative_input_after >= 0),
                cumulative_output_after INTEGER NULL CHECK(cumulative_output_after >= 0),
                cumulative_thought_after INTEGER NULL CHECK(cumulative_thought_after >= 0),
                cumulative_cached_read_after INTEGER NULL
                    CHECK(cumulative_cached_read_after >= 0),
                cumulative_cached_write_after INTEGER NULL
                    CHECK(cumulative_cached_write_after >= 0),
                cumulative_total_after INTEGER NULL CHECK(cumulative_total_after >= 0),
                context_window_used_tokens INTEGER NULL
                    CHECK(context_window_used_tokens >= 0),
                context_window_size_tokens INTEGER NULL
                    CHECK(context_window_size_tokens > 0),
                reported_fields INTEGER NOT NULL CHECK(reported_fields >= 0),
                coverage TEXT NOT NULL,
                last_source TEXT NULL,
                reset_reason TEXT NULL,
                dispatched_at_ms INTEGER NOT NULL,
                completed_at_ms INTEGER NULL,
                last_observed_at_ms INTEGER NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_agent_turn_usage_facts_dispatch
                ON agent_turn_usage_facts(dispatched_at_ms, usage_execution_id);
            CREATE INDEX IF NOT EXISTS idx_agent_turn_usage_facts_session
                ON agent_turn_usage_facts(session_id, dispatched_at_ms);
            CREATE INDEX IF NOT EXISTS idx_agent_turn_usage_facts_project
                ON agent_turn_usage_facts(project_id, dispatched_at_ms);
            CREATE INDEX IF NOT EXISTS idx_agent_turn_usage_facts_agent
                ON agent_turn_usage_facts(agent_id, dispatched_at_ms);
            CREATE INDEX IF NOT EXISTS idx_agent_turn_usage_facts_profile
                ON agent_turn_usage_facts(provider_profile_id, dispatched_at_ms);
            CREATE INDEX IF NOT EXISTS idx_agent_turn_usage_facts_model
                ON agent_turn_usage_facts(model_id, dispatched_at_ms);
        ",
    },
    Migration {
        version: 32,
        name: "agent_usage_zero_baseline_fence",
        sql: "
            ALTER TABLE session_runtime_bindings
                ADD COLUMN usage_zero_baseline_state TEXT NOT NULL DEFAULT 'unavailable'
                    CHECK(usage_zero_baseline_state IN ('available', 'claimed', 'unavailable'));
            ALTER TABLE session_runtime_bindings
                ADD COLUMN usage_zero_baseline_execution_id TEXT NULL;
            ALTER TABLE session_runtime_bindings
                ADD COLUMN usage_zero_baseline_activation_generation INTEGER NULL
                    CHECK(usage_zero_baseline_activation_generation >= 0);
        ",
    },
    Migration {
        version: 33,
        name: "managed_worktree_recovery_foundation",
        sql: "
            ALTER TABLE git_managed_worktrees ADD COLUMN repo_identity_key TEXT NULL;
            ALTER TABLE git_managed_worktrees ADD COLUMN worktree_identity_key TEXT NULL;
            ALTER TABLE git_managed_worktrees ADD COLUMN repository_identity_json TEXT NULL;
            ALTER TABLE git_managed_worktrees ADD COLUMN worktree_identity_json TEXT NULL;
            ALTER TABLE git_managed_worktrees ADD COLUMN canonical_worktree_path TEXT NULL;
            ALTER TABLE git_managed_worktrees ADD COLUMN origin_workspace_id TEXT NULL;
            ALTER TABLE git_managed_worktrees ADD COLUMN base_head TEXT NULL;
            ALTER TABLE git_managed_worktrees ADD COLUMN target_workspace_id TEXT NULL;
            ALTER TABLE git_managed_worktrees ADD COLUMN target_branch TEXT NULL;
            ALTER TABLE git_managed_worktrees
                ADD COLUMN reconciliation_state TEXT NOT NULL DEFAULT 'unverified';
            ALTER TABLE git_managed_worktrees ADD COLUMN diagnostic_json TEXT NULL;
            CREATE UNIQUE INDEX IF NOT EXISTS idx_git_managed_worktrees_identity
                ON git_managed_worktrees(worktree_identity_key)
                WHERE worktree_identity_key IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_git_managed_worktrees_repo_identity
                ON git_managed_worktrees(repo_identity_key, status, updated_at_ms);

            ALTER TABLE git_worktree_operations ADD COLUMN idempotency_key TEXT NULL;
            ALTER TABLE git_worktree_operations ADD COLUMN request_fingerprint TEXT NULL;
            ALTER TABLE git_worktree_operations
                ADD COLUMN checkpoint TEXT NOT NULL DEFAULT 'intent_recorded';
            ALTER TABLE git_worktree_operations ADD COLUMN detail_json TEXT NULL;
            ALTER TABLE git_worktree_operations ADD COLUMN lease_owner TEXT NULL;
            ALTER TABLE git_worktree_operations ADD COLUMN lease_expires_at_ms INTEGER NULL;
            ALTER TABLE git_worktree_operations
                ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0 CHECK(attempt >= 0);
            ALTER TABLE git_worktree_operations ADD COLUMN diagnostic_json TEXT NULL;
            CREATE UNIQUE INDEX IF NOT EXISTS idx_git_worktree_operations_idempotency
                ON git_worktree_operations(idempotency_key)
                WHERE idempotency_key IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_git_worktree_operations_reconcile
                ON git_worktree_operations(status, lease_expires_at_ms, updated_at_ms);
        ",
    },
    Migration {
        version: 34,
        name: "worktree_merge_lifecycle",
        sql: "
            CREATE TABLE IF NOT EXISTS git_worktree_readiness (
                worktree_id TEXT PRIMARY KEY
                    REFERENCES git_managed_worktrees(worktree_id) ON DELETE CASCADE,
                workspace_id TEXT NOT NULL
                    REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
                state TEXT NOT NULL,
                source_head TEXT NOT NULL,
                dirty_fingerprint TEXT NOT NULL,
                target_workspace_id TEXT NOT NULL,
                target_branch TEXT NOT NULL,
                checks_json TEXT NOT NULL DEFAULT '[]',
                revision TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_git_worktree_readiness_workspace
                ON git_worktree_readiness(workspace_id, updated_at_ms);
        ",
    },
    Migration {
        version: 35,
        name: "permission_response_options",
        sql: "
            ALTER TABLE permission_requests
                ADD COLUMN response_options_json TEXT NOT NULL DEFAULT '[]';
        ",
    },
    Migration {
        version: 36,
        name: "agent_elicitation_requests",
        sql: "
            CREATE TABLE IF NOT EXISTS elicitation_requests (
                request_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL
                    REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                status TEXT NOT NULL,
                request_json TEXT NOT NULL,
                resolution_json TEXT NULL,
                requested_at_ms INTEGER NOT NULL,
                resolved_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_elicitation_requests_session_status
                ON elicitation_requests(session_id, status, requested_at_ms);
        ",
    },
    Migration {
        version: 37,
        name: "agent_provider_projection_platform",
        sql: "
            CREATE TABLE IF NOT EXISTS model_provider_profiles (
                model_provider_profile_id TEXT PRIMARY KEY,
                legacy_provider_profile_id TEXT NULL UNIQUE,
                display_name TEXT NOT NULL,
                vendor_hint TEXT NULL,
                endpoints_json TEXT NOT NULL,
                proxy_policy_json TEXT NOT NULL,
                credentials_json TEXT NOT NULL,
                configured_models_json TEXT NOT NULL,
                default_model_id TEXT NULL,
                headers_json TEXT NOT NULL,
                status TEXT NOT NULL,
                revision INTEGER NOT NULL CHECK(revision > 0),
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                deleted_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_model_provider_profiles_active
                ON model_provider_profiles(deleted_at_ms, updated_at_ms);

            CREATE TABLE IF NOT EXISTS agent_runtime_profiles (
                agent_runtime_profile_id TEXT PRIMARY KEY,
                legacy_provider_profile_id TEXT NULL UNIQUE,
                agent_id TEXT NOT NULL,
                adapter_id TEXT NOT NULL,
                version_identity_json TEXT NOT NULL,
                command TEXT NOT NULL,
                args_json TEXT NOT NULL,
                safe_env_references_json TEXT NOT NULL,
                cwd_template TEXT NULL,
                process_strategy TEXT NOT NULL,
                runtime_home_strategy TEXT NOT NULL,
                host_capabilities_json TEXT NOT NULL,
                resource_policy_json TEXT NOT NULL,
                revision INTEGER NOT NULL CHECK(revision > 0),
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                deleted_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_agent_runtime_profiles_agent
                ON agent_runtime_profiles(agent_id, deleted_at_ms, updated_at_ms);

            CREATE TABLE IF NOT EXISTS agent_model_provider_bindings_v2 (
                agent_model_provider_binding_id TEXT PRIMARY KEY,
                legacy_provider_profile_id TEXT NULL UNIQUE,
                agent_id TEXT NOT NULL,
                agent_runtime_profile_id TEXT NOT NULL
                    REFERENCES agent_runtime_profiles(agent_runtime_profile_id),
                model_provider_profile_id TEXT NOT NULL
                    REFERENCES model_provider_profiles(model_provider_profile_id),
                projection_descriptor_id TEXT NOT NULL,
                projection_overrides_json TEXT NOT NULL,
                projection_fingerprint TEXT NULL,
                status TEXT NOT NULL,
                verification_json TEXT NOT NULL,
                revision INTEGER NOT NULL CHECK(revision > 0),
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                deleted_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_agent_model_provider_bindings_agent
                ON agent_model_provider_bindings_v2(agent_id, deleted_at_ms, updated_at_ms);
            CREATE INDEX IF NOT EXISTS idx_agent_model_provider_bindings_provider
                ON agent_model_provider_bindings_v2(
                    model_provider_profile_id, deleted_at_ms, updated_at_ms
                );

            CREATE TABLE IF NOT EXISTS agent_configured_model_bindings (
                agent_configured_model_binding_id TEXT PRIMARY KEY,
                agent_model_provider_binding_id TEXT NOT NULL
                    REFERENCES agent_model_provider_bindings_v2(
                        agent_model_provider_binding_id
                    ) ON DELETE CASCADE,
                provider_model_id TEXT NOT NULL,
                agent_model_id TEXT NOT NULL,
                wire_protocol_id TEXT NOT NULL,
                sdk_adapter_id TEXT NULL,
                deployment TEXT NULL,
                enabled INTEGER NOT NULL,
                process_scoped INTEGER NOT NULL DEFAULT 0,
                order_index INTEGER NOT NULL,
                UNIQUE(agent_model_provider_binding_id, agent_model_id)
            );
            CREATE INDEX IF NOT EXISTS idx_agent_configured_model_bindings_binding
                ON agent_configured_model_bindings(
                    agent_model_provider_binding_id, order_index
                );
        ",
    },
    Migration {
        version: 38,
        name: "agent_runtime_provider_probe_evidence",
        sql: "
            CREATE TABLE IF NOT EXISTS agent_runtime_provider_probes (
                probe_id TEXT PRIMARY KEY,
                agent_runtime_profile_id TEXT NOT NULL
                    REFERENCES agent_runtime_profiles(agent_runtime_profile_id),
                agent_model_provider_binding_id TEXT NULL
                    REFERENCES agent_model_provider_bindings_v2(agent_model_provider_binding_id),
                agent_id TEXT NOT NULL,
                adapter_id TEXT NOT NULL,
                descriptor_id TEXT NOT NULL,
                descriptor_version TEXT NOT NULL,
                status TEXT NOT NULL,
                stage TEXT NOT NULL,
                record_json TEXT NOT NULL,
                cancel_requested INTEGER NOT NULL DEFAULT 0,
                revision INTEGER NOT NULL CHECK(revision > 0),
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                finished_at_ms INTEGER NULL
            );
            CREATE INDEX IF NOT EXISTS idx_agent_runtime_provider_probes_runtime
                ON agent_runtime_provider_probes(agent_runtime_profile_id, updated_at_ms);
            CREATE INDEX IF NOT EXISTS idx_agent_runtime_provider_probes_status
                ON agent_runtime_provider_probes(status, updated_at_ms);
        ",
    },
    Migration {
        version: 39,
        name: "agent_runtime_option_snapshots",
        sql: "
            CREATE TABLE IF NOT EXISTS agent_runtime_option_snapshots (
                agent_id TEXT PRIMARY KEY,
                session_config_json TEXT NULL,
                last_success_at_ms INTEGER NULL,
                last_attempt_at_ms INTEGER NOT NULL,
                last_error_code TEXT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_agent_runtime_option_snapshots_attempt
                ON agent_runtime_option_snapshots(last_attempt_at_ms);
        ",
    },
    Migration {
        version: 40,
        name: "agent_auth_catalog_snapshots",
        sql: "
            CREATE TABLE IF NOT EXISTS agent_auth_catalog_snapshots (
                agent_id TEXT NOT NULL,
                provider_profile_id TEXT NOT NULL,
                catalog_json TEXT NOT NULL,
                refreshed_at_ms INTEGER NOT NULL,
                PRIMARY KEY(agent_id, provider_profile_id)
            );
            CREATE INDEX IF NOT EXISTS idx_agent_auth_catalog_snapshots_refreshed
                ON agent_auth_catalog_snapshots(refreshed_at_ms);
        ",
    },
    Migration {
        version: 41,
        name: "agent_managed_installations",
        sql: "
            CREATE TABLE IF NOT EXISTS agent_managed_installations (
                agent_id TEXT PRIMARY KEY,
                registry_agent_id TEXT NOT NULL,
                state_json TEXT NOT NULL,
                command_json TEXT,
                install_root TEXT,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_agent_managed_installations_updated
                ON agent_managed_installations(updated_at_ms);
        ",
    },
    Migration {
        version: 42,
        name: "provider_model_runtime_option_snapshots",
        sql: "
            CREATE TABLE IF NOT EXISTS provider_model_runtime_option_snapshots (
                provider_profile_id TEXT NOT NULL
                    REFERENCES provider_profiles(provider_profile_id) ON DELETE CASCADE,
                model_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                session_config_json TEXT NULL,
                last_success_at_ms INTEGER NULL,
                last_attempt_at_ms INTEGER NOT NULL,
                last_error_code TEXT NULL,
                PRIMARY KEY(provider_profile_id, model_id)
            );
            CREATE INDEX IF NOT EXISTS idx_provider_model_runtime_option_snapshots_agent
                ON provider_model_runtime_option_snapshots(agent_id, last_attempt_at_ms);
        ",
    },
    Migration {
        version: 43,
        name: "agent_model_provider_display_order",
        sql: "
            CREATE TABLE IF NOT EXISTS agent_model_provider_display_order (
                agent_id TEXT NOT NULL,
                provider_profile_id TEXT NOT NULL
                    REFERENCES provider_profiles(provider_profile_id) ON DELETE CASCADE,
                order_index INTEGER NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(agent_id, provider_profile_id)
            );
            CREATE INDEX IF NOT EXISTS idx_agent_model_provider_display_order
                ON agent_model_provider_display_order(agent_id, order_index, provider_profile_id);
        ",
    },
    Migration {
        version: 44,
        name: "runtime_switch_activation_completion",
        sql: "
            ALTER TABLE runtime_switches
                ADD COLUMN activation_completed_at_ms INTEGER NULL;

            -- Older releases returned a successful switch only after activation.
            -- Treat their committed rows as complete so upgrading does not eagerly
            -- restore every historical session. Any later provider work can still
            -- rematerialize the durable current binding on demand.
            UPDATE runtime_switches
            SET activation_completed_at_ms = COALESCE(committed_at_ms, updated_at_ms)
            WHERE status = 'committed';

            CREATE INDEX IF NOT EXISTS idx_runtime_switches_pending_activation
                ON runtime_switches(status, activation_completed_at_ms)
                WHERE activation_completed_at_ms IS NULL;
        ",
    },
    Migration {
        version: 45,
        name: "agent_auth_context_and_runtime_source",
        sql: "
            CREATE TABLE IF NOT EXISTS agent_auth_contexts (
                auth_context_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL UNIQUE,
                status TEXT NOT NULL,
                account_hint_redacted TEXT NULL,
                authenticated_via_method TEXT NULL,
                revision INTEGER NOT NULL CHECK(revision > 0),
                last_verified_at_ms INTEGER NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_agent_auth_contexts_status
                ON agent_auth_contexts(status, updated_at_ms);

            CREATE TABLE IF NOT EXISTS agent_authentication_operations (
                operation_id TEXT PRIMARY KEY,
                auth_context_id TEXT NOT NULL
                    REFERENCES agent_auth_contexts(auth_context_id) ON DELETE CASCADE,
                expected_context_revision INTEGER NOT NULL CHECK(expected_context_revision > 0),
                method_id TEXT NOT NULL,
                state TEXT NOT NULL,
                error_code TEXT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_authentication_operations_active
                ON agent_authentication_operations(auth_context_id)
                WHERE state IN (
                    'queued', 'discovering_methods', 'authenticating', 'awaiting_user',
                    'verifying', 'cancelling'
                );

            CREATE TABLE IF NOT EXISTS agent_auth_model_catalog_snapshots (
                auth_context_id TEXT NOT NULL
                    REFERENCES agent_auth_contexts(auth_context_id) ON DELETE CASCADE,
                auth_context_revision INTEGER NOT NULL CHECK(auth_context_revision > 0),
                runtime_fingerprint TEXT NOT NULL,
                discovery_source TEXT NOT NULL,
                status TEXT NOT NULL,
                catalog_json TEXT NOT NULL,
                last_success_at_ms INTEGER NULL,
                last_attempt_at_ms INTEGER NOT NULL,
                last_error_code TEXT NULL,
                PRIMARY KEY(auth_context_id, auth_context_revision, runtime_fingerprint)
            );
            CREATE INDEX IF NOT EXISTS idx_agent_auth_model_catalog_attempt
                ON agent_auth_model_catalog_snapshots(auth_context_id, last_attempt_at_ms);

            ALTER TABLE session_runtime_bindings ADD COLUMN auth_source_kind TEXT NULL;
            ALTER TABLE session_runtime_bindings ADD COLUMN auth_source_id TEXT NULL;
            ALTER TABLE session_runtime_bindings ADD COLUMN auth_source_revision INTEGER NULL;
            UPDATE session_runtime_bindings
            SET auth_source_kind = 'provider_profile',
                auth_source_id = provider_profile_id,
                auth_source_revision = profile_revision
            WHERE auth_source_kind IS NULL;

            ALTER TABLE runtime_switches ADD COLUMN target_auth_source_kind TEXT NULL;
            ALTER TABLE runtime_switches ADD COLUMN target_auth_source_id TEXT NULL;
            ALTER TABLE runtime_switches ADD COLUMN target_auth_source_revision INTEGER NULL;
            UPDATE runtime_switches
            SET target_auth_source_kind = 'provider_profile',
                target_auth_source_id = target_profile_id,
                target_auth_source_revision = 0
            WHERE target_auth_source_kind IS NULL;

            ALTER TABLE agent_usage_checkpoints ADD COLUMN auth_source_kind TEXT NULL;
            ALTER TABLE agent_usage_checkpoints ADD COLUMN auth_source_id TEXT NULL;
            ALTER TABLE agent_usage_checkpoints ADD COLUMN auth_source_revision INTEGER NULL;
            UPDATE agent_usage_checkpoints
            SET auth_source_kind = 'provider_profile',
                auth_source_id = provider_profile_id,
                auth_source_revision = 0
            WHERE auth_source_kind IS NULL;

            ALTER TABLE agent_turn_usage_facts ADD COLUMN auth_source_kind TEXT NULL;
            ALTER TABLE agent_turn_usage_facts ADD COLUMN auth_source_id TEXT NULL;
            ALTER TABLE agent_turn_usage_facts ADD COLUMN auth_source_revision INTEGER NULL;
            UPDATE agent_turn_usage_facts
            SET auth_source_kind = 'provider_profile',
                auth_source_id = provider_profile_id,
                auth_source_revision = 0
            WHERE auth_source_kind IS NULL;
        ",
    },
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseSmokeResult {
    pub database_path: PathBuf,
    pub schema_version: i64,
    pub applied_migrations: Vec<String>,
    pub marker: String,
}

pub struct WorkspaceRepository;
pub struct SessionRepository;
pub struct AgentConfigRepository;
pub struct AgentDiscoveryRepository;
pub struct ProviderProfileRepository;
pub struct ProviderSecretReferenceRepository;
pub struct ProviderDefaultProfileRepository;
pub struct AgentDefaultModelProviderProfileRepository;
pub struct AgentModelProviderFailoverRepository;
pub struct AgentModelProviderDisplayOrderRepository;
pub struct ProviderInjectionPreviewRepository;
pub struct ProviderNativeExportRepository;
pub struct ProviderCapabilityRepository;
pub struct ProviderRuntimeOptionSnapshotRepository;
pub struct ProviderModelRuntimeOptionSnapshotRepository;
pub struct AgentRuntimeOptionSnapshotRepository;
pub struct AgentAuthCatalogSnapshotRepository;
pub struct AgentManagedInstallationRepository;
pub struct ProviderHealthRepository;
pub struct ProviderUsageRepository;
pub struct ScheduledTaskRepository;
pub struct AutomationGraphRepository;
pub struct McpServerRepository;
pub struct SkillRepository;
pub struct PromptRepository;
pub struct HookRepository;
pub struct TimelineRepository;
pub struct PermissionRepository;
pub struct ElicitationRepository;
pub struct AdapterDiagnosticsRepository;
pub struct TerminalSessionRepository;
pub struct RecentFileRepository;
pub struct GitSnapshotRepository;
pub struct ManagedWorktreeRepository;
pub struct WorktreeReadinessRepository;
pub struct WorktreeOperationRepository;
pub struct RemoteDeviceRepository;
pub struct RemotePairingCodeRepository;
pub struct RemoteAuditRepository;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRuntimeOptionSnapshotRecord {
    pub provider_profile_id: ProviderProfileId,
    pub agent_id: AgentId,
    pub model_response: Option<AgentModelListResponse>,
    pub session_config: Option<AgentSessionConfigProbe>,
    pub last_success_at_ms: Option<i64>,
    pub last_attempt_at_ms: i64,
    pub last_error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelRuntimeOptionSnapshotRecord {
    pub provider_profile_id: ProviderProfileId,
    pub model_id: String,
    pub agent_id: AgentId,
    pub session_config: Option<AgentSessionConfigProbe>,
    pub last_success_at_ms: Option<i64>,
    pub last_attempt_at_ms: i64,
    pub last_error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRuntimeOptionSnapshotRecord {
    pub agent_id: AgentId,
    pub session_config: Option<AgentSessionConfigProbe>,
    pub last_success_at_ms: Option<i64>,
    pub last_attempt_at_ms: i64,
    pub last_error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAuthCatalogSnapshotRecord {
    pub agent_id: AgentId,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub catalog: AgentAuthCatalog,
    pub refreshed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentManagedInstallationRecord {
    pub agent_id: AgentId,
    pub registry_agent_id: String,
    pub state: AgentManagedInstallState,
    pub command: Option<AgentCommandConfig>,
    pub install_root: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectLookup {
    record: ProjectRecord,
    deleted_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceLookup {
    record: WorkspaceRecord,
    deleted_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineAppend {
    pub source: TimelineSource,
    pub payload: TimelinePayload,
    pub timestamp_ms: Option<i64>,
    pub correlation_id: Option<CorrelationId>,
    pub provider_correlation_id: Option<String>,
    pub redaction_state: TimelineRedactionState,
    pub execution_attribution: Option<TurnExecutionAttribution>,
}

macro_rules! provider_profile_params {
    ($profile:expr) => {
        params![
            $profile.id.as_str(),
            $profile.agent_id.as_str(),
            enum_to_db(&$profile.kind)?,
            $profile.display_name,
            enum_to_db(&$profile.status)?,
            $profile.account_alias,
            $profile.base_url,
            $profile.default_model,
            $profile.small_model,
            $profile.large_model,
            json_to_db(&$profile.configured_models)?,
            $profile.reasoning_effort,
            json_to_db(&$profile.sandbox_defaults)?,
            json_to_db(&$profile.network_defaults)?,
            json_to_db(&$profile.permission_defaults)?,
            json_to_db(&$profile.provider_options)?,
            $profile.created_at_ms,
            $profile.updated_at_ms,
            $profile.deleted_at_ms
        ]
    };
}

pub type ManagedWorktreeRecord = GitManagedWorktreeRecord;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeOperationClaimOutcome {
    Acquired(GitWorktreeOperationRecord),
    Completed(GitWorktreeOperationRecord),
    Busy(GitWorktreeOperationRecord),
    NeedsAttention(GitWorktreeOperationRecord),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeviceRecord {
    pub detail: RemoteDeviceDetail,
    pub auth_secret_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePairingCodeRecord {
    pub pairing: RemotePairingCode,
    pub code_hash: String,
}

impl WorkspaceRepository {
    pub fn ensure(
        conn: &Connection,
        workspace_root: impl AsRef<Path>,
        mode: WorkspaceMode,
    ) -> VibexResult<(ProjectRecord, WorkspaceRecord)> {
        let root_path = normalize_path(workspace_root.as_ref());
        let mode_text = enum_to_db(&mode)?;
        if let Some(workspace) =
            Self::find_active_workspace_by_root_and_mode(conn, &root_path, &mode_text)?
        {
            let project = Self::get_project(conn, &workspace.project_id)?.ok_or_else(|| {
                VibexError::storage(
                    "workspace_project_missing",
                    "workspace project was not found",
                )
                .with_diagnostic("workspaceId", workspace.id.as_str())
            })?;
            return Ok((project, workspace));
        }
        let now = unix_timestamp_ms();
        let project_name = workspace_root
            .as_ref()
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("Workspace")
            .to_string();

        let project = match Self::find_project_by_root(conn, &root_path)? {
            Some(mut project) => {
                if project.deleted_at_ms.is_some() {
                    conn.execute(
                        "
                        UPDATE projects
                        SET deleted_at_ms = NULL, updated_at_ms = ?2
                        WHERE project_id = ?1
                        ",
                        params![project.record.id.as_str(), now],
                    )
                    .map_err(storage_err(
                        "project_restore_failed",
                        "failed to restore project",
                    ))?;
                    project.record.updated_at_ms = now;
                }
                project.record
            }
            None => {
                let project = ProjectRecord {
                    id: ProjectId::new(),
                    name: project_name,
                    root_path: root_path.clone(),
                    created_at_ms: now,
                    updated_at_ms: now,
                };
                conn.execute(
                    "
                    INSERT INTO projects
                        (project_id, name, root_path, created_at_ms, updated_at_ms)
                    VALUES (?1, ?2, ?3, ?4, ?5)
                    ",
                    params![
                        project.id.as_str(),
                        project.name,
                        project.root_path,
                        project.created_at_ms,
                        project.updated_at_ms
                    ],
                )
                .map_err(storage_err(
                    "project_insert_failed",
                    "failed to insert project",
                ))?;
                project
            }
        };

        let workspace = match Self::find_workspace(conn, &project.id, &root_path, &mode_text)? {
            Some(mut workspace) => {
                if workspace.deleted_at_ms.is_some() {
                    conn.execute(
                        "
                        UPDATE workspaces
                        SET deleted_at_ms = NULL, updated_at_ms = ?2
                        WHERE workspace_id = ?1
                        ",
                        params![workspace.record.id.as_str(), now],
                    )
                    .map_err(storage_err(
                        "workspace_restore_failed",
                        "failed to restore workspace",
                    ))?;
                    workspace.record.updated_at_ms = now;
                }
                workspace.record
            }
            None => {
                let workspace = WorkspaceRecord {
                    id: WorkspaceId::new(),
                    project_id: project.id.clone(),
                    root_path,
                    mode,
                    created_at_ms: now,
                    updated_at_ms: now,
                };
                conn.execute(
                    "
                    INSERT INTO workspaces
                        (workspace_id, project_id, root_path, mode, created_at_ms, updated_at_ms)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                    ",
                    params![
                        workspace.id.as_str(),
                        workspace.project_id.as_str(),
                        workspace.root_path,
                        enum_to_db(&workspace.mode)?,
                        workspace.created_at_ms,
                        workspace.updated_at_ms
                    ],
                )
                .map_err(storage_err(
                    "workspace_insert_failed",
                    "failed to insert workspace",
                ))?;
                workspace
            }
        };

        Ok((project, workspace))
    }

    pub fn ensure_for_project(
        conn: &Connection,
        project_id: &ProjectId,
        workspace_root: impl AsRef<Path>,
        mode: WorkspaceMode,
    ) -> VibexResult<WorkspaceRecord> {
        let root_path = normalize_path(workspace_root.as_ref());
        let project = Self::get_project(conn, project_id)?.ok_or_else(|| {
            VibexError::validation("project_not_found", "project was not found")
                .with_diagnostic("projectId", project_id.as_str())
        })?;
        let mode_text = enum_to_db(&mode)?;
        let now = unix_timestamp_ms();

        match Self::find_workspace(conn, &project.id, &root_path, &mode_text)? {
            Some(mut workspace) => {
                if workspace.deleted_at_ms.is_some() {
                    conn.execute(
                        "
                        UPDATE workspaces
                        SET deleted_at_ms = NULL, updated_at_ms = ?2
                        WHERE workspace_id = ?1
                        ",
                        params![workspace.record.id.as_str(), now],
                    )
                    .map_err(storage_err(
                        "workspace_restore_failed",
                        "failed to restore workspace",
                    ))?;
                    workspace.record.updated_at_ms = now;
                }
                Ok(workspace.record)
            }
            None => {
                let workspace = WorkspaceRecord {
                    id: WorkspaceId::new(),
                    project_id: project.id,
                    root_path,
                    mode,
                    created_at_ms: now,
                    updated_at_ms: now,
                };
                conn.execute(
                    "
                    INSERT INTO workspaces
                        (workspace_id, project_id, root_path, mode, created_at_ms, updated_at_ms)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                    ",
                    params![
                        workspace.id.as_str(),
                        workspace.project_id.as_str(),
                        workspace.root_path,
                        enum_to_db(&workspace.mode)?,
                        workspace.created_at_ms,
                        workspace.updated_at_ms
                    ],
                )
                .map_err(storage_err(
                    "workspace_insert_failed",
                    "failed to insert workspace",
                ))?;
                Ok(workspace)
            }
        }
    }

    pub fn list(conn: &Connection) -> VibexResult<Vec<(ProjectRecord, WorkspaceRecord)>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT
                    p.project_id, p.name, p.root_path, p.created_at_ms, p.updated_at_ms,
                    w.workspace_id, w.project_id, w.root_path, w.mode,
                    w.created_at_ms, w.updated_at_ms
                FROM workspaces w
                JOIN projects p ON p.project_id = w.project_id
                WHERE p.deleted_at_ms IS NULL AND w.deleted_at_ms IS NULL
                ORDER BY w.updated_at_ms DESC
                ",
            )
            .map_err(storage_err(
                "workspace_list_failed",
                "failed to list workspaces",
            ))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    ProjectRecord {
                        id: parse_id_sql(row.get(0)?, ProjectId::parse)?,
                        name: row.get(1)?,
                        root_path: row.get(2)?,
                        created_at_ms: row.get(3)?,
                        updated_at_ms: row.get(4)?,
                    },
                    WorkspaceRecord {
                        id: parse_id_sql(row.get(5)?, WorkspaceId::parse)?,
                        project_id: parse_id_sql(row.get(6)?, ProjectId::parse)?,
                        root_path: row.get(7)?,
                        mode: enum_from_db_sql(row.get(8)?)?,
                        created_at_ms: row.get(9)?,
                        updated_at_ms: row.get(10)?,
                    },
                ))
            })
            .map_err(storage_err(
                "workspace_list_failed",
                "failed to list workspaces",
            ))?;

        let mut workspaces = Vec::new();
        for row in rows {
            workspaces.push(row.map_err(storage_err(
                "workspace_decode_failed",
                "failed to decode workspace row",
            ))?);
        }
        Ok(workspaces)
    }

    pub fn get(
        conn: &Connection,
        workspace_id: &WorkspaceId,
    ) -> VibexResult<Option<(ProjectRecord, WorkspaceRecord)>> {
        conn.query_row(
            "
            SELECT
                p.project_id, p.name, p.root_path, p.created_at_ms, p.updated_at_ms,
                w.workspace_id, w.project_id, w.root_path, w.mode,
                w.created_at_ms, w.updated_at_ms
            FROM workspaces w
            JOIN projects p ON p.project_id = w.project_id
            WHERE w.workspace_id = ?1
                AND w.deleted_at_ms IS NULL
                AND p.deleted_at_ms IS NULL
            ",
            params![workspace_id.as_str()],
            |row| {
                Ok((
                    ProjectRecord {
                        id: parse_id_sql(row.get(0)?, ProjectId::parse)?,
                        name: row.get(1)?,
                        root_path: row.get(2)?,
                        created_at_ms: row.get(3)?,
                        updated_at_ms: row.get(4)?,
                    },
                    WorkspaceRecord {
                        id: parse_id_sql(row.get(5)?, WorkspaceId::parse)?,
                        project_id: parse_id_sql(row.get(6)?, ProjectId::parse)?,
                        root_path: row.get(7)?,
                        mode: enum_from_db_sql(row.get(8)?)?,
                        created_at_ms: row.get(9)?,
                        updated_at_ms: row.get(10)?,
                    },
                ))
            },
        )
        .optional()
        .map_err(storage_err(
            "workspace_lookup_failed",
            "failed to lookup workspace",
        ))
    }

    fn find_project_by_root(
        conn: &Connection,
        root_path: &str,
    ) -> VibexResult<Option<ProjectLookup>> {
        conn.query_row(
            "
            SELECT project_id, name, root_path, created_at_ms, updated_at_ms, deleted_at_ms
            FROM projects
            WHERE root_path = ?1
            ",
            params![root_path],
            map_project_lookup,
        )
        .optional()
        .map_err(storage_err(
            "project_lookup_failed",
            "failed to lookup project",
        ))
    }

    pub fn get_project(
        conn: &Connection,
        project_id: &ProjectId,
    ) -> VibexResult<Option<ProjectRecord>> {
        conn.query_row(
            "
            SELECT project_id, name, root_path, created_at_ms, updated_at_ms
            FROM projects
            WHERE project_id = ?1 AND deleted_at_ms IS NULL
            ",
            params![project_id.as_str()],
            map_project,
        )
        .optional()
        .map_err(storage_err(
            "project_lookup_failed",
            "failed to lookup project",
        ))
    }

    fn find_workspace(
        conn: &Connection,
        project_id: &ProjectId,
        root_path: &str,
        mode: &str,
    ) -> VibexResult<Option<WorkspaceLookup>> {
        conn.query_row(
            "
            SELECT workspace_id, project_id, root_path, mode, created_at_ms, updated_at_ms,
                deleted_at_ms
            FROM workspaces
            WHERE project_id = ?1 AND root_path = ?2 AND mode = ?3
            ",
            params![project_id.as_str(), root_path, mode],
            map_workspace_lookup,
        )
        .optional()
        .map_err(storage_err(
            "workspace_lookup_failed",
            "failed to lookup workspace",
        ))
    }

    fn find_active_workspace_by_root_and_mode(
        conn: &Connection,
        root_path: &str,
        mode: &str,
    ) -> VibexResult<Option<WorkspaceRecord>> {
        conn.query_row(
            "
            SELECT
                w.workspace_id, w.project_id, w.root_path, w.mode,
                w.created_at_ms, w.updated_at_ms
            FROM workspaces w
            JOIN projects p ON p.project_id = w.project_id
            WHERE w.root_path = ?1
                AND w.mode = ?2
                AND w.deleted_at_ms IS NULL
                AND p.deleted_at_ms IS NULL
            ORDER BY
                EXISTS (
                    SELECT 1
                    FROM git_managed_worktrees managed
                    WHERE managed.workspace_id = w.workspace_id
                ) DESC,
                w.created_at_ms ASC,
                w.workspace_id ASC
            LIMIT 1
            ",
            params![root_path, mode],
            |row| {
                Ok(WorkspaceRecord {
                    id: parse_id_sql(row.get(0)?, WorkspaceId::parse)?,
                    project_id: parse_id_sql(row.get(1)?, ProjectId::parse)?,
                    root_path: row.get(2)?,
                    mode: enum_from_db_sql(row.get(3)?)?,
                    created_at_ms: row.get(4)?,
                    updated_at_ms: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(storage_err(
            "workspace_lookup_failed",
            "failed to lookup workspace",
        ))
    }

    pub fn delete_project(conn: &mut Connection, project_id: &ProjectId) -> VibexResult<()> {
        let now = unix_timestamp_ms();
        let tx = conn.transaction().map_err(storage_err(
            "project_delete_transaction_failed",
            "failed to start project delete transaction",
        ))?;
        let exists = tx
            .query_row(
                "SELECT 1 FROM projects WHERE project_id = ?1",
                params![project_id.as_str()],
                |_row| Ok(()),
            )
            .optional()
            .map_err(storage_err(
                "project_lookup_failed",
                "failed to lookup project",
            ))?
            .is_some();
        if !exists {
            return Err(
                VibexError::validation("project_not_found", "project was not found")
                    .with_diagnostic("projectId", project_id.as_str()),
            );
        }

        tx.execute(
            "
            UPDATE projects
            SET deleted_at_ms = COALESCE(deleted_at_ms, ?2), updated_at_ms = ?2
            WHERE project_id = ?1
            ",
            params![project_id.as_str(), now],
        )
        .map_err(storage_err(
            "project_delete_failed",
            "failed to delete project",
        ))?;
        tx.execute(
            "
            UPDATE workspaces
            SET deleted_at_ms = COALESCE(deleted_at_ms, ?2), updated_at_ms = ?2
            WHERE project_id = ?1
            ",
            params![project_id.as_str(), now],
        )
        .map_err(storage_err(
            "workspace_delete_failed",
            "failed to delete project workspaces",
        ))?;
        tx.execute(
            "
            UPDATE agent_sessions
            SET deleted_at_ms = COALESCE(deleted_at_ms, ?2), updated_at_ms = ?2
            WHERE project_id = ?1
            ",
            params![project_id.as_str(), now],
        )
        .map_err(storage_err(
            "project_session_delete_failed",
            "failed to delete project sessions",
        ))?;
        tx.commit().map_err(storage_err(
            "project_delete_commit_failed",
            "failed to commit project delete",
        ))?;
        Ok(())
    }
}

impl SessionRepository {
    pub fn insert(conn: &Connection, session: &AgentSession) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO agent_sessions (
                session_id, title, project_id, workspace_id, workspace_root,
                workspace_mode, state,
                permission_mode, ask_on_risk, bypass_all_permissions,
                created_at_ms, updated_at_ms, archived_at_ms, deleted_at_ms,
                current_agent_id
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            ",
            params![
                session.id.as_str(),
                session.title,
                session.project_id.as_str(),
                session.workspace_id.as_str(),
                session.workspace_root,
                enum_to_db(&session.workspace_mode)?,
                enum_to_db(&session.state)?,
                enum_to_db(&session.safety.permission_mode)?,
                session.safety.ask_on_risk,
                session.safety.bypass_all_permissions,
                session.created_at_ms,
                session.updated_at_ms,
                session.archived_at_ms,
                session.deleted_at_ms,
                session.agent_id.as_str()
            ],
        )
        .map_err(storage_err(
            "session_insert_failed",
            "failed to insert session",
        ))?;
        Ok(())
    }

    pub fn get(
        conn: &Connection,
        session_id: &VibexSessionId,
    ) -> VibexResult<Option<AgentSession>> {
        conn.query_row(
            "
                SELECT session_id, title, project_id, workspace_id, workspace_root,
                    workspace_mode, state,
                    permission_mode, ask_on_risk, bypass_all_permissions,
                    created_at_ms, updated_at_ms,
                    COALESCE(
                        (SELECT MAX(timestamp_ms)
                         FROM agent_timeline_items
                         WHERE agent_timeline_items.session_id = agent_sessions.session_id),
                        created_at_ms
                    ) AS last_message_at_ms,
                    archived_at_ms, deleted_at_ms, current_agent_id
                FROM agent_sessions
                WHERE session_id = ?1 AND deleted_at_ms IS NULL
                ",
            params![session_id.as_str()],
            map_agent_session,
        )
        .optional()
        .map_err(storage_err(
            "session_lookup_failed",
            "failed to lookup session",
        ))
    }

    pub fn list(conn: &Connection, include_archived: bool) -> VibexResult<Vec<AgentSession>> {
        let sql = if include_archived {
            "
            SELECT session_id, title, project_id, workspace_id, workspace_root,
                workspace_mode, state,
                permission_mode, ask_on_risk, bypass_all_permissions,
                created_at_ms, updated_at_ms,
                COALESCE(
                    (SELECT MAX(timestamp_ms)
                     FROM agent_timeline_items
                     WHERE agent_timeline_items.session_id = agent_sessions.session_id),
                    created_at_ms
                ) AS last_message_at_ms,
                archived_at_ms, deleted_at_ms, current_agent_id
            FROM agent_sessions
            WHERE deleted_at_ms IS NULL
            ORDER BY last_message_at_ms DESC, session_id DESC
            "
        } else {
            "
            SELECT session_id, title, project_id, workspace_id, workspace_root,
                workspace_mode, state,
                permission_mode, ask_on_risk, bypass_all_permissions,
                created_at_ms, updated_at_ms,
                COALESCE(
                    (SELECT MAX(timestamp_ms)
                     FROM agent_timeline_items
                     WHERE agent_timeline_items.session_id = agent_sessions.session_id),
                    created_at_ms
                ) AS last_message_at_ms,
                archived_at_ms, deleted_at_ms, current_agent_id
            FROM agent_sessions
            WHERE deleted_at_ms IS NULL AND archived_at_ms IS NULL
            ORDER BY last_message_at_ms DESC, session_id DESC
            "
        };
        let mut stmt = conn.prepare(sql).map_err(storage_err(
            "session_list_failed",
            "failed to list sessions",
        ))?;
        let rows = stmt.query_map([], map_agent_session).map_err(storage_err(
            "session_list_failed",
            "failed to list sessions",
        ))?;

        let mut sessions = Vec::new();
        for row in rows {
            let session = row.map_err(storage_err(
                "session_decode_failed",
                "failed to decode session row",
            ))?;
            sessions.push(session);
        }
        Ok(sessions)
    }

    pub fn update_state(
        conn: &Connection,
        session_id: &VibexSessionId,
        state: AgentSessionState,
    ) -> VibexResult<()> {
        conn.execute(
            "
            UPDATE agent_sessions
            SET state = ?2, updated_at_ms = ?3
            WHERE session_id = ?1 AND deleted_at_ms IS NULL
            ",
            params![
                session_id.as_str(),
                enum_to_db(&state)?,
                unix_timestamp_ms()
            ],
        )
        .map_err(storage_err(
            "session_state_update_failed",
            "failed to update session state",
        ))?;
        Ok(())
    }

    pub fn claim_running_turn(
        conn: &Connection,
        session_id: &VibexSessionId,
        expected_state: AgentSessionState,
    ) -> VibexResult<()> {
        let changed_rows = conn
            .execute(
                "
                UPDATE agent_sessions
                SET state = ?3, updated_at_ms = ?4
                WHERE session_id = ?1
                    AND state = ?2
                    AND deleted_at_ms IS NULL
                ",
                params![
                    session_id.as_str(),
                    enum_to_db(&expected_state)?,
                    enum_to_db(&AgentSessionState::Running)?,
                    unix_timestamp_ms()
                ],
            )
            .map_err(storage_err(
                "session_state_update_failed",
                "failed to update session state",
            ))?;

        if changed_rows == 0 {
            return Err(VibexError::conflict(
                "agent_turn_already_running",
                "Agent session already has a running turn",
            ));
        }

        Ok(())
    }

    pub fn update_title(
        conn: &Connection,
        session_id: &VibexSessionId,
        title: &str,
    ) -> VibexResult<AgentSession> {
        let trimmed_title = title.trim();
        if trimmed_title.is_empty() {
            return Err(VibexError::validation(
                "session_title_empty",
                "session title must not be empty",
            ));
        }

        let changed_rows = conn
            .execute(
                "
                UPDATE agent_sessions
                SET title = ?2
                WHERE session_id = ?1 AND deleted_at_ms IS NULL
                ",
                params![session_id.as_str(), trimmed_title],
            )
            .map_err(storage_err(
                "session_title_update_failed",
                "failed to update session title",
            ))?;

        if changed_rows == 0 {
            return Err(VibexError::validation(
                "session_not_found",
                "Agent session was not found",
            ));
        }

        Self::get(conn, session_id)?.ok_or_else(|| {
            VibexError::validation("session_not_found", "Agent session was not found")
        })
    }

    pub fn archive(conn: &Connection, session_id: &VibexSessionId) -> VibexResult<()> {
        let now = unix_timestamp_ms();
        conn.execute(
            "
            UPDATE agent_sessions
            SET state = ?2, archived_at_ms = COALESCE(archived_at_ms, ?3), updated_at_ms = ?3
            WHERE session_id = ?1 AND deleted_at_ms IS NULL
            ",
            params![
                session_id.as_str(),
                enum_to_db(&AgentSessionState::Archived)?,
                now
            ],
        )
        .map_err(storage_err(
            "session_archive_failed",
            "failed to archive session",
        ))?;
        Ok(())
    }

    pub fn archive_if_timeline_unchanged(
        conn: &Connection,
        session_id: &VibexSessionId,
        expected_end_sequence: i64,
    ) -> VibexResult<()> {
        let now = unix_timestamp_ms();
        let changed_rows = conn
            .execute(
                "
                UPDATE agent_sessions
                SET state = ?2, archived_at_ms = COALESCE(archived_at_ms, ?3), updated_at_ms = ?3
                WHERE session_id = ?1
                    AND deleted_at_ms IS NULL
                    AND state IN (?4, ?5)
                    AND ?6 = (
                        SELECT COALESCE(MAX(sequence), 0)
                        FROM agent_timeline_items
                        WHERE session_id = ?1
                    )
                ",
                params![
                    session_id.as_str(),
                    enum_to_db(&AgentSessionState::Archived)?,
                    now,
                    enum_to_db(&AgentSessionState::Idle)?,
                    enum_to_db(&AgentSessionState::Error)?,
                    expected_end_sequence,
                ],
            )
            .map_err(storage_err(
                "session_archive_failed",
                "failed to archive session",
            ))?;
        if changed_rows == 0 {
            return Err(VibexError::conflict(
                "session_archive_source_changed",
                "Agent session changed before it could be archived",
            ));
        }
        Ok(())
    }

    pub fn delete(conn: &Connection, session_id: &VibexSessionId) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM agent_sessions WHERE session_id = ?1",
            params![session_id.as_str()],
        )
        .map_err(storage_err(
            "session_delete_failed",
            "failed to delete session",
        ))?;
        Ok(())
    }
}

impl AgentConfigRepository {
    pub fn upsert(conn: &Connection, config: &AgentConfig) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO agent_configs (
                agent_id, runtime_kind, source_kind, label_override,
                description_override, enabled, order_index, command_json,
                env_json, params_json, created_at_ms, updated_at_ms,
                deleted_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ON CONFLICT(agent_id) DO UPDATE SET
                runtime_kind = excluded.runtime_kind,
                source_kind = excluded.source_kind,
                label_override = excluded.label_override,
                description_override = excluded.description_override,
                enabled = excluded.enabled,
                order_index = excluded.order_index,
                command_json = excluded.command_json,
                env_json = excluded.env_json,
                params_json = excluded.params_json,
                updated_at_ms = excluded.updated_at_ms,
                deleted_at_ms = excluded.deleted_at_ms
            ",
            params![
                config.agent_id.as_str(),
                enum_to_db(&config.runtime_kind)?,
                enum_to_db(&config.source_kind)?,
                config.label_override.as_deref(),
                config.description_override.as_deref(),
                config.enabled,
                config.order_index,
                config.command.as_ref().map(json_to_db).transpose()?,
                json_to_db(&config.env)?,
                json_to_db(&config.params)?,
                config.created_at_ms,
                config.updated_at_ms,
                config.deleted_at_ms
            ],
        )
        .map_err(storage_err(
            "agent_config_upsert_failed",
            "failed to upsert agent config",
        ))?;
        Ok(())
    }

    pub fn list(conn: &Connection) -> VibexResult<Vec<AgentConfig>> {
        // No agent-id whitelist here: callers match configs against the
        // current builtin definitions, so rows for removed agents are ignored
        // naturally. The runtime_kind filter stays to skip legacy rows (for
        // example runtime_kind 'mock') that no longer decode.
        let mut stmt = conn
            .prepare(
                "
                SELECT agent_id, runtime_kind, source_kind, label_override,
                    description_override, enabled, order_index, command_json,
                    env_json, params_json, created_at_ms, updated_at_ms,
                    deleted_at_ms
                FROM agent_configs
                WHERE runtime_kind IN ('claude_sdk', 'codex_sdk', 'acp')
                ORDER BY order_index ASC, agent_id ASC
                ",
            )
            .map_err(storage_err(
                "agent_config_list_failed",
                "failed to list agent configs",
            ))?;
        let rows = stmt.query_map([], map_agent_config).map_err(storage_err(
            "agent_config_list_failed",
            "failed to list agent configs",
        ))?;
        let mut configs = Vec::new();
        for row in rows {
            configs.push(row.map_err(storage_err(
                "agent_config_decode_failed",
                "failed to decode agent config",
            ))?);
        }
        Ok(configs)
    }

    pub fn get(conn: &Connection, agent_id: &AgentId) -> VibexResult<Option<AgentConfig>> {
        conn.query_row(
            "
            SELECT agent_id, runtime_kind, source_kind, label_override,
                description_override, enabled, order_index, command_json,
                env_json, params_json, created_at_ms, updated_at_ms,
                deleted_at_ms
            FROM agent_configs
            WHERE agent_id = ?1
            ",
            params![agent_id.as_str()],
            map_agent_config,
        )
        .optional()
        .map_err(storage_err(
            "agent_config_lookup_failed",
            "failed to lookup agent config",
        ))
    }
}

impl AgentDiscoveryRepository {
    pub fn insert(conn: &Connection, record: &AgentDiscoveryRecord) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO agent_discovery_records (
                discovery_record_id, agent_id, cwd_scope, install_status,
                config_status, runtime_status, binary_path, version,
                native_config_paths_json, models_json, modes_json,
                diagnostics_json, discovered_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ",
            params![
                record.discovery_record_id.as_str(),
                record.agent_id.as_str(),
                record.cwd_scope.as_str(),
                enum_to_db(&record.install_status)?,
                enum_to_db(&record.config_status)?,
                enum_to_db(&record.runtime_status)?,
                record.binary_path.as_deref(),
                record.version.as_deref(),
                json_to_db(&record.native_config_paths)?,
                json_to_db(&record.models)?,
                json_to_db(&record.modes)?,
                json_to_db(&record.diagnostics)?,
                record.discovered_at_ms
            ],
        )
        .map_err(storage_err(
            "agent_discovery_insert_failed",
            "failed to insert agent discovery record",
        ))?;
        Ok(())
    }

    pub fn latest_for_agent(
        conn: &Connection,
        agent_id: &AgentId,
        cwd_scope: &str,
    ) -> VibexResult<Option<AgentDiscoveryRecord>> {
        conn.query_row(
            "
            SELECT discovery_record_id, agent_id, cwd_scope, install_status,
                config_status, runtime_status, binary_path, version,
                native_config_paths_json, models_json, modes_json,
                diagnostics_json, discovered_at_ms
            FROM agent_discovery_records
            WHERE agent_id = ?1 AND cwd_scope = ?2
            ORDER BY discovered_at_ms DESC
            LIMIT 1
            ",
            params![agent_id.as_str(), cwd_scope],
            map_agent_discovery_record,
        )
        .optional()
        .map_err(storage_err(
            "agent_discovery_lookup_failed",
            "failed to lookup agent discovery record",
        ))
    }

    pub fn latest_by_agent(
        conn: &Connection,
        cwd_scope: &str,
    ) -> VibexResult<HashMap<AgentId, AgentDiscoveryRecord>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT discovery_record_id, agent_id, cwd_scope, install_status,
                    config_status, runtime_status, binary_path, version,
                    native_config_paths_json, models_json, modes_json,
                    diagnostics_json, discovered_at_ms
                FROM agent_discovery_records
                WHERE cwd_scope = ?1
                ORDER BY agent_id ASC, discovered_at_ms DESC
                ",
            )
            .map_err(storage_err(
                "agent_discovery_list_failed",
                "failed to list agent discovery records",
            ))?;
        let rows = stmt
            .query_map(params![cwd_scope], map_agent_discovery_record)
            .map_err(storage_err(
                "agent_discovery_list_failed",
                "failed to list agent discovery records",
            ))?;
        let mut records = HashMap::new();
        for row in rows {
            let record = row.map_err(storage_err(
                "agent_discovery_decode_failed",
                "failed to decode agent discovery record",
            ))?;
            records.entry(record.agent_id.clone()).or_insert(record);
        }
        Ok(records)
    }
}

impl ProviderProfileRepository {
    pub fn ensure_local_defaults(conn: &Connection) -> VibexResult<()> {
        for kind in [
            ProviderKind::Codex,
            ProviderKind::Claude,
            ProviderKind::Acp,
            ProviderKind::Codex,
        ] {
            let profile = ProviderProfile::local_default(kind);
            let exists = conn
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM provider_profiles WHERE provider_profile_id = ?1
                    )",
                    params![profile.id.as_str()],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(storage_err(
                    "provider_profile_seed_check_failed",
                    "failed to inspect the local default provider profile",
                ))?;
            if exists {
                continue;
            }
            conn.execute(
                "
                INSERT OR IGNORE INTO provider_profiles (
                    provider_profile_id, agent_id, provider_kind, display_name, status,
                    account_alias, base_url, default_model, small_model,
                    large_model, configured_models_json, reasoning_effort, sandbox_defaults_json,
                    network_defaults_json, permission_defaults_json,
                    provider_options_json, created_at_ms, updated_at_ms,
                    deleted_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                ",
                provider_profile_params!(profile),
            )
            .map_err(storage_err(
                "provider_profile_seed_failed",
                "failed to seed local default provider profile",
            ))?;
        }
        Ok(())
    }

    pub fn insert(conn: &Connection, profile: &ProviderProfile) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO provider_profiles (
                provider_profile_id, agent_id, provider_kind, display_name, status,
                account_alias, base_url, default_model, small_model, large_model,
                configured_models_json, reasoning_effort, sandbox_defaults_json, network_defaults_json,
                permission_defaults_json, provider_options_json, created_at_ms,
                updated_at_ms, deleted_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
            ",
            provider_profile_params!(profile),
        )
        .map_err(storage_err(
            "provider_profile_insert_failed",
            "failed to insert provider profile",
        ))?;
        for secret in &profile.secrets {
            ProviderSecretReferenceRepository::insert(conn, secret)?;
        }
        Ok(())
    }

    pub fn list(conn: &Connection) -> VibexResult<Vec<ProviderProfile>> {
        let configurable_agent_ids = vibex_core::model_provider_configurable_agent_ids()?;
        Self::list_internal(conn, Some(&configurable_agent_ids))
    }

    /// Lists every persisted ACP-compatible profile for runtime catalog
    /// construction. The Config Center uses [`Self::list`] so unsupported
    /// Agents stay out of the model-provider editor, but their Agent-owned
    /// runtime profiles must remain available to session startup.
    pub fn list_all(conn: &Connection) -> VibexResult<Vec<ProviderProfile>> {
        Self::list_internal(conn, None)
    }

    fn list_internal(
        conn: &Connection,
        configurable_agent_ids: Option<&BTreeSet<AgentId>>,
    ) -> VibexResult<Vec<ProviderProfile>> {
        Self::ensure_local_defaults(conn)?;
        let mut stmt = conn
            .prepare(
                "
                SELECT provider_profile_id, agent_id, provider_kind, display_name, status,
                    account_alias, base_url, default_model, small_model,
                    large_model, configured_models_json, reasoning_effort, sandbox_defaults_json,
                    network_defaults_json, permission_defaults_json,
                    provider_options_json, created_at_ms, updated_at_ms,
                    deleted_at_ms
                FROM provider_profiles
                WHERE deleted_at_ms IS NULL
                    AND provider_kind IN ('claude', 'codex', 'acp')
                ORDER BY provider_kind ASC, updated_at_ms DESC
                ",
            )
            .map_err(storage_err(
                "provider_profile_list_failed",
                "failed to list provider profiles",
            ))?;
        let rows = stmt
            .query_map([], map_provider_profile_without_secrets)
            .map_err(storage_err(
                "provider_profile_list_failed",
                "failed to list provider profiles",
            ))?;
        let mut profiles = Vec::new();
        for row in rows {
            let mut profile = row.map_err(storage_err(
                "provider_profile_decode_failed",
                "failed to decode provider profile",
            ))?;
            if configurable_agent_ids
                .is_some_and(|agent_ids| !agent_ids.contains(&profile.agent_id))
            {
                continue;
            }
            profile.secrets =
                ProviderSecretReferenceRepository::list_for_profile(conn, &profile.id)?;
            profiles.push(profile);
        }
        Ok(profiles)
    }

    pub fn list_by_agent(
        conn: &Connection,
        agent_id: &AgentId,
        include_disabled: bool,
    ) -> VibexResult<Vec<ProviderProfile>> {
        Self::ensure_local_defaults(conn)?;
        let disabled_status = enum_to_db(&ProviderProfileStatus::Disabled)?;
        let mut stmt = conn
            .prepare(
                "
                SELECT provider_profile_id, agent_id, provider_kind, display_name, status,
                    account_alias, base_url, default_model, small_model,
                    large_model, configured_models_json, reasoning_effort, sandbox_defaults_json,
                    network_defaults_json, permission_defaults_json,
                    provider_options_json, created_at_ms, updated_at_ms,
                    deleted_at_ms
                FROM provider_profiles
                WHERE agent_id = ?1
                    AND deleted_at_ms IS NULL
                    AND (?2 = 1 OR status != ?3)
                ORDER BY updated_at_ms DESC, provider_profile_id ASC
                ",
            )
            .map_err(storage_err(
                "provider_profile_agent_list_failed",
                "failed to list provider profiles for agent",
            ))?;
        let rows = stmt
            .query_map(
                params![
                    agent_id.as_str(),
                    if include_disabled { 1_i64 } else { 0_i64 },
                    disabled_status
                ],
                map_provider_profile_without_secrets,
            )
            .map_err(storage_err(
                "provider_profile_agent_list_failed",
                "failed to list provider profiles for agent",
            ))?;
        let mut profiles = Vec::new();
        for row in rows {
            let mut profile = row.map_err(storage_err(
                "provider_profile_agent_decode_failed",
                "failed to decode provider profile for agent",
            ))?;
            profile.secrets =
                ProviderSecretReferenceRepository::list_for_profile(conn, &profile.id)?;
            profiles.push(profile);
        }
        Ok(profiles)
    }

    pub fn first_enabled_for_agent(
        conn: &Connection,
        agent_id: &AgentId,
    ) -> VibexResult<Option<ProviderProfile>> {
        Self::ensure_local_defaults(conn)?;
        let enabled_status = enum_to_db(&ProviderProfileStatus::Enabled)?;
        let mut profile = conn
            .query_row(
                "
                SELECT provider_profile_id, agent_id, provider_kind, display_name, status,
                    account_alias, base_url, default_model, small_model,
                    large_model, configured_models_json, reasoning_effort, sandbox_defaults_json,
                    network_defaults_json, permission_defaults_json,
                    provider_options_json, created_at_ms, updated_at_ms,
                    deleted_at_ms
                FROM provider_profiles
                WHERE agent_id = ?1 AND status = ?2 AND deleted_at_ms IS NULL
                ORDER BY updated_at_ms DESC, provider_profile_id ASC
                LIMIT 1
                ",
                params![agent_id.as_str(), enabled_status],
                map_provider_profile_without_secrets,
            )
            .optional()
            .map_err(storage_err(
                "provider_profile_agent_default_lookup_failed",
                "failed to lookup first enabled provider profile for agent",
            ))?;
        if let Some(profile) = profile.as_mut() {
            profile.secrets =
                ProviderSecretReferenceRepository::list_for_profile(conn, &profile.id)?;
        }
        Ok(profile)
    }

    pub fn get(
        conn: &Connection,
        provider_profile_id: &ProviderProfileId,
    ) -> VibexResult<Option<ProviderProfile>> {
        Self::ensure_local_defaults(conn)?;
        let mut profile = conn
            .query_row(
                "
                SELECT provider_profile_id, agent_id, provider_kind, display_name, status,
                    account_alias, base_url, default_model, small_model,
                    large_model, configured_models_json, reasoning_effort, sandbox_defaults_json,
                    network_defaults_json, permission_defaults_json,
                    provider_options_json, created_at_ms, updated_at_ms,
                    deleted_at_ms
                FROM provider_profiles
                WHERE provider_profile_id = ?1 AND deleted_at_ms IS NULL
                ",
                params![provider_profile_id.as_str()],
                map_provider_profile_without_secrets,
            )
            .optional()
            .map_err(storage_err(
                "provider_profile_lookup_failed",
                "failed to lookup provider profile",
            ))?;
        if let Some(profile) = profile.as_mut() {
            profile.secrets =
                ProviderSecretReferenceRepository::list_for_profile(conn, &profile.id)?;
        }
        Ok(profile)
    }

    pub fn update(conn: &Connection, profile: &ProviderProfile) -> VibexResult<()> {
        conn.execute(
            "
            UPDATE provider_profiles
            SET agent_id = ?2,
                provider_kind = ?3,
                display_name = ?4,
                status = ?5,
                account_alias = ?6,
                base_url = ?7,
                default_model = ?8,
                small_model = ?9,
                large_model = ?10,
                configured_models_json = ?11,
                reasoning_effort = ?12,
                sandbox_defaults_json = ?13,
                network_defaults_json = ?14,
                permission_defaults_json = ?15,
                provider_options_json = ?16,
                updated_at_ms = ?17
            WHERE provider_profile_id = ?1 AND deleted_at_ms IS NULL
            ",
            params![
                profile.id.as_str(),
                profile.agent_id.as_str(),
                enum_to_db(&profile.kind)?,
                profile.display_name,
                enum_to_db(&profile.status)?,
                profile.account_alias,
                profile.base_url,
                profile.default_model,
                profile.small_model,
                profile.large_model,
                json_to_db(&profile.configured_models)?,
                profile.reasoning_effort,
                json_to_db(&profile.sandbox_defaults)?,
                json_to_db(&profile.network_defaults)?,
                json_to_db(&profile.permission_defaults)?,
                json_to_db(&profile.provider_options)?,
                profile.updated_at_ms
            ],
        )
        .map_err(storage_err(
            "provider_profile_update_failed",
            "failed to update provider profile",
        ))?;
        Ok(())
    }

    pub fn soft_delete(
        conn: &mut Connection,
        provider_profile_id: &ProviderProfileId,
    ) -> VibexResult<()> {
        let now = unix_timestamp_ms();
        let tx = conn.transaction().map_err(storage_err(
            "provider_profile_delete_transaction_failed",
            "failed to start provider profile deletion transaction",
        ))?;
        tx.execute(
            "
            UPDATE provider_profiles
            SET deleted_at_ms = COALESCE(deleted_at_ms, ?2), updated_at_ms = ?2
            WHERE provider_profile_id = ?1
            ",
            params![provider_profile_id.as_str(), now],
        )
        .map_err(storage_err(
            "provider_profile_delete_failed",
            "failed to delete provider profile",
        ))?;
        for (table, error_code, error_message) in [
            (
                "provider_default_profiles",
                "provider_profile_default_cleanup_failed",
                "failed to clear provider defaults for deleted profile",
            ),
            (
                "agent_default_model_provider_profiles",
                "provider_profile_agent_default_cleanup_failed",
                "failed to clear Agent defaults for deleted profile",
            ),
            (
                "agent_model_provider_failover",
                "provider_profile_failover_cleanup_failed",
                "failed to clear failover entries for deleted profile",
            ),
            (
                "agent_model_provider_display_order",
                "provider_profile_display_order_cleanup_failed",
                "failed to clear display order entries for deleted profile",
            ),
            (
                "provider_runtime_option_snapshots",
                "provider_profile_runtime_option_snapshot_cleanup_failed",
                "failed to clear runtime option snapshot for deleted profile",
            ),
            (
                "provider_model_runtime_option_snapshots",
                "provider_profile_model_runtime_option_snapshot_cleanup_failed",
                "failed to clear model runtime option snapshots for deleted profile",
            ),
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE provider_profile_id = ?1"),
                params![provider_profile_id.as_str()],
            )
            .map_err(storage_err(error_code, error_message))?;
        }
        tx.commit().map_err(storage_err(
            "provider_profile_delete_commit_failed",
            "failed to commit provider profile deletion",
        ))
    }

    pub fn from_create_request(request: ProviderProfileCreateRequest) -> ProviderProfile {
        let now = unix_timestamp_ms();
        let id = ProviderProfileId::new();
        let secrets = request
            .secret_references
            .into_iter()
            .map(|secret| ProviderSecretReference {
                id: RequestId::new(),
                provider_profile_id: id.clone(),
                secret_kind: secret.secret_kind,
                backend: secret.backend,
                setup_state: secret.setup_state,
                lookup_key: secret.lookup_key,
                display_label: secret.display_label,
                redacted_hint: secret.redacted_hint,
                created_at_ms: now,
                updated_at_ms: now,
            })
            .collect();
        ProviderProfile {
            id,
            agent_id: request
                .agent_id
                .unwrap_or_else(|| agent_id_for_provider_kind(request.kind)),
            kind: request.kind,
            display_name: request.display_name,
            status: ProviderProfileStatus::Enabled,
            account_alias: request.account_alias,
            base_url: request.base_url,
            default_model: request.default_model,
            small_model: request.small_model,
            large_model: request.large_model,
            configured_models: request.configured_models,
            reasoning_effort: request.reasoning_effort,
            sandbox_defaults: request
                .sandbox_defaults
                .unwrap_or_else(ProviderSandboxDefaults::workspace_write_ask_on_risk),
            network_defaults: request
                .network_defaults
                .unwrap_or_else(ProviderNetworkDefaults::local_default),
            permission_defaults: request
                .permission_defaults
                .unwrap_or_else(ProviderPermissionDefaults::ask_on_risk),
            provider_options: request
                .provider_options
                .unwrap_or_else(ProviderOptions::empty),
            secrets,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        }
    }
}

impl ProviderSecretReferenceRepository {
    pub fn insert(conn: &Connection, secret: &ProviderSecretReference) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO provider_secret_references (
                secret_ref_id, provider_profile_id, secret_kind, backend,
                setup_state, lookup_key, display_label, redacted_hint,
                created_at_ms, updated_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ",
            params![
                secret.id.as_str(),
                secret.provider_profile_id.as_str(),
                enum_to_db(&secret.secret_kind)?,
                enum_to_db(&secret.backend)?,
                enum_to_db(&secret.setup_state)?,
                secret.lookup_key,
                secret.display_label,
                secret.redacted_hint,
                secret.created_at_ms,
                secret.updated_at_ms
            ],
        )
        .map_err(storage_err(
            "provider_secret_reference_insert_failed",
            "failed to insert provider secret reference",
        ))?;
        Ok(())
    }

    pub fn list_for_profile(
        conn: &Connection,
        provider_profile_id: &ProviderProfileId,
    ) -> VibexResult<Vec<ProviderSecretReference>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT secret_ref_id, provider_profile_id, secret_kind, backend,
                    setup_state, lookup_key, display_label, redacted_hint,
                    created_at_ms, updated_at_ms
                FROM provider_secret_references
                WHERE provider_profile_id = ?1
                ORDER BY created_at_ms ASC
                ",
            )
            .map_err(storage_err(
                "provider_secret_reference_list_failed",
                "failed to list provider secret references",
            ))?;
        let rows = stmt
            .query_map(
                params![provider_profile_id.as_str()],
                map_provider_secret_reference,
            )
            .map_err(storage_err(
                "provider_secret_reference_list_failed",
                "failed to list provider secret references",
            ))?;
        let mut secrets = Vec::new();
        for row in rows {
            secrets.push(row.map_err(storage_err(
                "provider_secret_reference_decode_failed",
                "failed to decode provider secret reference",
            ))?);
        }
        Ok(secrets)
    }

    pub fn replace_for_profile(
        conn: &Connection,
        provider_profile_id: &ProviderProfileId,
        secrets: &[ProviderSecretReference],
    ) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM provider_secret_references WHERE provider_profile_id = ?1",
            params![provider_profile_id.as_str()],
        )
        .map_err(storage_err(
            "provider_secret_reference_replace_failed",
            "failed to replace provider secret references",
        ))?;
        for secret in secrets {
            Self::insert(conn, secret)?;
        }
        Ok(())
    }
}

impl ProviderDefaultProfileRepository {
    pub fn get(
        conn: &Connection,
        scope: ProviderProfileDefaultScope,
        provider_kind: ProviderKind,
    ) -> VibexResult<ProviderProfileDefaultSelection> {
        ProviderProfileRepository::ensure_local_defaults(conn)?;
        let scope_kind = enum_to_db(&scope.kind)?;
        let scope_id = scope.storage_key();
        let row = conn
            .query_row(
                "
                SELECT d.provider_profile_id, d.updated_at_ms
                FROM provider_default_profiles d
                JOIN provider_profiles p
                    ON p.provider_profile_id = d.provider_profile_id
                WHERE d.scope_kind = ?1 AND d.scope_id = ?2 AND d.provider_kind = ?3
                    AND p.deleted_at_ms IS NULL
                ",
                params![scope_kind, scope_id, enum_to_db(&provider_kind)?],
                |row| {
                    Ok((
                        parse_id_sql(row.get(0)?, ProviderProfileId::parse)?,
                        row.get::<_, i64>(1)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_err(
                "provider_default_lookup_failed",
                "failed to lookup provider default profile",
            ))?;
        Ok(ProviderProfileDefaultSelection {
            scope,
            provider_kind,
            provider_profile_id: row.as_ref().map(|(id, _)| id.clone()),
            updated_at_ms: row.map(|(_, updated_at_ms)| updated_at_ms),
        })
    }

    pub fn set(
        conn: &Connection,
        request: ProviderProfileSetDefaultRequest,
    ) -> VibexResult<ProviderProfileDefaultSelection> {
        ProviderProfileRepository::ensure_local_defaults(conn)?;
        if ProviderProfileRepository::get(conn, &request.provider_profile_id)?.is_none() {
            return Err(VibexError::validation(
                "provider_profile_not_found",
                "provider profile was not found",
            )
            .with_diagnostic("providerProfileId", request.provider_profile_id.as_str()));
        }
        let now = unix_timestamp_ms();
        let scope_kind = enum_to_db(&request.scope.kind)?;
        let scope_id = request.scope.storage_key();
        conn.execute(
            "
            INSERT INTO provider_default_profiles (
                scope_kind, scope_id, provider_kind, provider_profile_id,
                created_at_ms, updated_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            ON CONFLICT(scope_kind, scope_id, provider_kind) DO UPDATE SET
                provider_profile_id = excluded.provider_profile_id,
                updated_at_ms = excluded.updated_at_ms
            ",
            params![
                scope_kind,
                scope_id,
                enum_to_db(&request.provider_kind)?,
                request.provider_profile_id.as_str(),
                now
            ],
        )
        .map_err(storage_err(
            "provider_default_set_failed",
            "failed to set provider default profile",
        ))?;
        Ok(ProviderProfileDefaultSelection {
            scope: request.scope,
            provider_kind: request.provider_kind,
            provider_profile_id: Some(request.provider_profile_id),
            updated_at_ms: Some(now),
        })
    }
}

impl AgentDefaultModelProviderProfileRepository {
    pub fn get(
        conn: &Connection,
        scope: ProviderProfileDefaultScope,
        agent_id: AgentId,
    ) -> VibexResult<AgentModelProviderDefaultSelection> {
        ProviderProfileRepository::ensure_local_defaults(conn)?;
        let scope_kind = enum_to_db(&scope.kind)?;
        let scope_id = scope.storage_key();
        let row = conn
            .query_row(
                "
                SELECT d.provider_profile_id, d.updated_at_ms
                FROM agent_default_model_provider_profiles d
                JOIN provider_profiles p
                    ON p.provider_profile_id = d.provider_profile_id
                WHERE d.scope_kind = ?1 AND d.scope_id = ?2 AND d.agent_id = ?3
                    AND p.deleted_at_ms IS NULL
                ",
                params![scope_kind, scope_id, agent_id.as_str()],
                |row| {
                    Ok((
                        parse_id_sql(row.get(0)?, ProviderProfileId::parse)?,
                        row.get::<_, i64>(1)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_err(
                "agent_model_provider_default_lookup_failed",
                "failed to lookup agent model provider default profile",
            ))?;
        Ok(AgentModelProviderDefaultSelection {
            scope,
            agent_id,
            provider_profile_id: row.as_ref().map(|(id, _)| id.clone()),
            updated_at_ms: row.map(|(_, updated_at_ms)| updated_at_ms),
        })
    }

    pub fn set(
        conn: &Connection,
        scope: ProviderProfileDefaultScope,
        agent_id: AgentId,
        provider_profile_id: ProviderProfileId,
    ) -> VibexResult<AgentModelProviderDefaultSelection> {
        ProviderProfileRepository::ensure_local_defaults(conn)?;
        let now = unix_timestamp_ms();
        let scope_kind = enum_to_db(&scope.kind)?;
        let scope_id = scope.storage_key();
        conn.execute(
            "
            INSERT INTO agent_default_model_provider_profiles (
                scope_kind, scope_id, agent_id, provider_profile_id,
                created_at_ms, updated_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            ON CONFLICT(scope_kind, scope_id, agent_id) DO UPDATE SET
                provider_profile_id = excluded.provider_profile_id,
                updated_at_ms = excluded.updated_at_ms
            ",
            params![
                scope_kind,
                scope_id,
                agent_id.as_str(),
                provider_profile_id.as_str(),
                now
            ],
        )
        .map_err(storage_err(
            "agent_model_provider_default_set_failed",
            "failed to set agent model provider default profile",
        ))?;
        Ok(AgentModelProviderDefaultSelection {
            scope,
            agent_id,
            provider_profile_id: Some(provider_profile_id),
            updated_at_ms: Some(now),
        })
    }
}

impl AgentModelProviderFailoverRepository {
    pub fn list(
        conn: &Connection,
        agent_id: &AgentId,
    ) -> VibexResult<Vec<AgentModelProviderFailoverEntry>> {
        ProviderProfileRepository::ensure_local_defaults(conn)?;
        let mut stmt = conn
            .prepare(
                "
                SELECT f.agent_id, f.provider_profile_id, p.display_name, p.status,
                    f.order_index, f.enabled, f.updated_at_ms
                FROM agent_model_provider_failover f
                JOIN provider_profiles p
                    ON p.provider_profile_id = f.provider_profile_id
                WHERE f.agent_id = ?1 AND p.deleted_at_ms IS NULL
                ORDER BY f.order_index ASC, f.provider_profile_id ASC
                ",
            )
            .map_err(storage_err(
                "agent_model_provider_failover_list_failed",
                "failed to list agent model provider failover queue",
            ))?;
        let rows = stmt
            .query_map(params![agent_id.as_str()], |row| {
                Ok(AgentModelProviderFailoverEntry {
                    agent_id: parse_id_sql(row.get(0)?, AgentId::parse)?,
                    provider_profile_id: parse_id_sql(row.get(1)?, ProviderProfileId::parse)?,
                    display_name: row.get(2)?,
                    status: enum_from_db_sql(row.get(3)?)?,
                    order_index: row.get(4)?,
                    enabled: row.get::<_, i64>(5)? != 0,
                    updated_at_ms: row.get(6)?,
                })
            })
            .map_err(storage_err(
                "agent_model_provider_failover_list_failed",
                "failed to list agent model provider failover queue",
            ))?;
        collect_rows(
            rows,
            "agent_model_provider_failover_decode_failed",
            "failed to decode agent model provider failover queue",
        )
    }

    pub fn replace(
        conn: &mut Connection,
        agent_id: &AgentId,
        entries: &[AgentModelProviderFailoverEntry],
    ) -> VibexResult<Vec<AgentModelProviderFailoverEntry>> {
        ProviderProfileRepository::ensure_local_defaults(conn)?;
        let tx = conn.transaction().map_err(storage_err(
            "agent_model_provider_failover_transaction_failed",
            "failed to start agent model provider failover transaction",
        ))?;
        tx.execute(
            "DELETE FROM agent_model_provider_failover WHERE agent_id = ?1",
            params![agent_id.as_str()],
        )
        .map_err(storage_err(
            "agent_model_provider_failover_clear_failed",
            "failed to clear agent model provider failover queue",
        ))?;
        let now = unix_timestamp_ms();
        for entry in entries {
            tx.execute(
                "
                INSERT INTO agent_model_provider_failover (
                    agent_id, provider_profile_id, order_index, enabled,
                    created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                ",
                params![
                    agent_id.as_str(),
                    entry.provider_profile_id.as_str(),
                    entry.order_index,
                    if entry.enabled { 1_i64 } else { 0_i64 },
                    now
                ],
            )
            .map_err(storage_err(
                "agent_model_provider_failover_insert_failed",
                "failed to insert agent model provider failover entry",
            ))?;
        }
        tx.commit().map_err(storage_err(
            "agent_model_provider_failover_commit_failed",
            "failed to commit agent model provider failover transaction",
        ))?;
        Self::list(conn, agent_id)
    }
}

impl AgentModelProviderDisplayOrderRepository {
    pub fn list(
        conn: &Connection,
        agent_id: &AgentId,
    ) -> VibexResult<Vec<AgentModelProviderDisplayOrderEntry>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT agent_id, provider_profile_id, order_index, updated_at_ms
                FROM agent_model_provider_display_order
                WHERE agent_id = ?1
                ORDER BY order_index ASC, provider_profile_id ASC
                ",
            )
            .map_err(storage_err(
                "agent_model_provider_display_order_list_failed",
                "failed to list agent model provider display order",
            ))?;
        let rows = stmt
            .query_map(params![agent_id.as_str()], |row| {
                Ok(AgentModelProviderDisplayOrderEntry {
                    agent_id: parse_id_sql(row.get(0)?, AgentId::parse)?,
                    provider_profile_id: parse_id_sql(row.get(1)?, ProviderProfileId::parse)?,
                    order_index: row.get(2)?,
                    updated_at_ms: row.get(3)?,
                })
            })
            .map_err(storage_err(
                "agent_model_provider_display_order_list_failed",
                "failed to list agent model provider display order",
            ))?;
        collect_rows(
            rows,
            "agent_model_provider_display_order_decode_failed",
            "failed to decode agent model provider display order",
        )
    }

    pub fn replace(
        conn: &mut Connection,
        agent_id: &AgentId,
        entries: &[AgentModelProviderDisplayOrderEntry],
    ) -> VibexResult<Vec<AgentModelProviderDisplayOrderEntry>> {
        let tx = conn.transaction().map_err(storage_err(
            "agent_model_provider_display_order_transaction_failed",
            "failed to start agent model provider display order transaction",
        ))?;
        tx.execute(
            "DELETE FROM agent_model_provider_display_order WHERE agent_id = ?1",
            params![agent_id.as_str()],
        )
        .map_err(storage_err(
            "agent_model_provider_display_order_clear_failed",
            "failed to clear agent model provider display order",
        ))?;
        let now = unix_timestamp_ms();
        for entry in entries {
            tx.execute(
                "
                INSERT INTO agent_model_provider_display_order (
                    agent_id, provider_profile_id, order_index, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?4)
                ",
                params![
                    agent_id.as_str(),
                    entry.provider_profile_id.as_str(),
                    entry.order_index,
                    now
                ],
            )
            .map_err(storage_err(
                "agent_model_provider_display_order_insert_failed",
                "failed to insert agent model provider display order",
            ))?;
        }
        tx.commit().map_err(storage_err(
            "agent_model_provider_display_order_commit_failed",
            "failed to commit agent model provider display order",
        ))?;
        Self::list(conn, agent_id)
    }
}

impl ProviderInjectionPreviewRepository {
    pub fn insert(
        conn: &Connection,
        request: &ProviderInjectionPreviewRequest,
        preview: &ProviderInjectionPreview,
    ) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO provider_injection_previews (
                preview_id, provider_profile_id, request_json, preview_json, created_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            params![
                preview.preview_id.as_str(),
                preview.profile.id.as_str(),
                json_to_db(request)?,
                json_to_db(preview)?,
                preview.created_at_ms
            ],
        )
        .map_err(storage_err(
            "provider_injection_preview_insert_failed",
            "failed to insert provider injection preview",
        ))?;
        Ok(())
    }
}

impl ProviderNativeExportRepository {
    pub fn insert_preview(
        conn: &Connection,
        preview: &ProviderNativeExportPreview,
    ) -> VibexResult<()> {
        let now = preview.created_at_ms;
        conn.execute(
            "
            INSERT INTO provider_native_export_records (
                export_id, provider_profile_id, source, mode, status, preview_json,
                applied_at_ms, rolled_back_at_ms, created_at_ms, updated_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?7)
            ON CONFLICT(export_id) DO UPDATE SET
                status = excluded.status,
                preview_json = excluded.preview_json,
                updated_at_ms = excluded.updated_at_ms
            ",
            params![
                preview.export_id.as_str(),
                preview.provider_profile_id.as_str(),
                enum_to_db(&preview.source)?,
                enum_to_db(&preview.mode)?,
                "previewed",
                json_to_db(preview)?,
                now
            ],
        )
        .map_err(storage_err(
            "provider_native_export_record_insert_failed",
            "failed to insert provider native export record",
        ))?;

        for file in &preview.files {
            Self::upsert_file_plan(conn, &preview.export_id, file, now)?;
        }
        Ok(())
    }

    pub fn get_preview(
        conn: &Connection,
        export_id: &RequestId,
    ) -> VibexResult<Option<ProviderNativeExportPreview>> {
        let preview_json = conn
            .query_row(
                "
                SELECT preview_json
                FROM provider_native_export_records
                WHERE export_id = ?1
                ",
                params![export_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage_err(
                "provider_native_export_record_get_failed",
                "failed to get provider native export record",
            ))?;
        preview_json.map(json_from_db).transpose()
    }

    pub fn record_apply_result(
        conn: &Connection,
        result: &ProviderNativeExportApplyResult,
    ) -> VibexResult<()> {
        let status = enum_to_db(&result.status)?;
        conn.execute(
            "
            UPDATE provider_native_export_records
            SET status = ?2, applied_at_ms = ?3, updated_at_ms = ?3
            WHERE export_id = ?1
            ",
            params![result.export_id.as_str(), status, result.applied_at_ms],
        )
        .map_err(storage_err(
            "provider_native_export_apply_record_failed",
            "failed to record provider native export apply result",
        ))?;
        for file in &result.files {
            Self::upsert_file_plan(conn, &result.export_id, file, result.applied_at_ms)?;
        }
        Ok(())
    }

    pub fn record_rollback_result(
        conn: &Connection,
        result: &ProviderNativeExportRollbackResult,
    ) -> VibexResult<()> {
        let status = enum_to_db(&result.status)?;
        conn.execute(
            "
            UPDATE provider_native_export_records
            SET status = ?2, rolled_back_at_ms = ?3, updated_at_ms = ?3
            WHERE export_id = ?1
            ",
            params![result.export_id.as_str(), status, result.rolled_back_at_ms],
        )
        .map_err(storage_err(
            "provider_native_export_rollback_record_failed",
            "failed to record provider native export rollback result",
        ))?;
        for file in &result.files {
            Self::upsert_file_plan(conn, &result.export_id, file, result.rolled_back_at_ms)?;
        }
        Ok(())
    }

    pub fn list(
        conn: &Connection,
        request: ProviderNativeExportListRequest,
    ) -> VibexResult<Vec<ProviderNativeExportRecordSummary>> {
        let limit = request.limit.unwrap_or(20).clamp(1, 100);
        let mut sql = String::from(
            "
            SELECT
                r.export_id, r.provider_profile_id, r.source, r.mode, r.status,
                COUNT(f.operation_id) AS file_count,
                COALESCE(SUM(CASE WHEN f.status = 'blocked' THEN 1 ELSE 0 END), 0) AS blocked_count,
                r.applied_at_ms, r.rolled_back_at_ms, r.created_at_ms, r.updated_at_ms
            FROM provider_native_export_records r
            LEFT JOIN provider_native_export_file_operations f ON f.export_id = r.export_id
            ",
        );
        if request.provider_profile_id.is_some() {
            sql.push_str(" WHERE r.provider_profile_id = ?1 ");
        }
        sql.push_str(
            "
            GROUP BY r.export_id
            ORDER BY r.created_at_ms DESC
            LIMIT ",
        );
        sql.push_str(&limit.to_string());

        let mut stmt = conn.prepare(&sql).map_err(storage_err(
            "provider_native_export_record_list_failed",
            "failed to list provider native export records",
        ))?;

        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<_> {
            let file_count: i64 = row.get(5)?;
            let blocked_count: i64 = row.get(6)?;
            Ok(ProviderNativeExportRecordSummary {
                export_id: parse_id_sql(row.get(0)?, RequestId::parse)?,
                provider_profile_id: parse_id_sql(row.get(1)?, ProviderProfileId::parse)?,
                source: enum_from_db_sql(row.get(2)?)?,
                mode: enum_from_db_sql(row.get(3)?)?,
                status: row.get(4)?,
                file_count: u32::try_from(file_count).unwrap_or(u32::MAX),
                blocked_count: u32::try_from(blocked_count).unwrap_or(u32::MAX),
                applied_at_ms: row.get(7)?,
                rolled_back_at_ms: row.get(8)?,
                created_at_ms: row.get(9)?,
                updated_at_ms: row.get(10)?,
            })
        };

        let rows = if let Some(provider_profile_id) = request.provider_profile_id {
            stmt.query_map(params![provider_profile_id.as_str()], map_row)
                .map_err(storage_err(
                    "provider_native_export_record_list_failed",
                    "failed to list provider native export records",
                ))?
                .collect::<Result<Vec<_>, _>>()
        } else {
            stmt.query_map([], map_row)
                .map_err(storage_err(
                    "provider_native_export_record_list_failed",
                    "failed to list provider native export records",
                ))?
                .collect::<Result<Vec<_>, _>>()
        }
        .map_err(storage_err(
            "provider_native_export_record_list_failed",
            "failed to list provider native export records",
        ))?;
        Ok(rows)
    }

    fn upsert_file_plan(
        conn: &Connection,
        export_id: &RequestId,
        file: &ProviderNativeExportFilePlan,
        updated_at_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO provider_native_export_file_operations (
                operation_id, export_id, source, file_kind, operation_kind,
                target_path, backup_path, temp_path, marker, status, redacted_diff,
                diagnostics_json, target_size_before, target_size_after, backup_size,
                created_at_ms, updated_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, NULL, NULL, ?13, ?13)
            ON CONFLICT(operation_id) DO UPDATE SET
                operation_kind = excluded.operation_kind,
                backup_path = excluded.backup_path,
                temp_path = excluded.temp_path,
                marker = excluded.marker,
                status = excluded.status,
                redacted_diff = excluded.redacted_diff,
                diagnostics_json = excluded.diagnostics_json,
                updated_at_ms = excluded.updated_at_ms
            ",
            params![
                file.operation_id.as_str(),
                export_id.as_str(),
                enum_to_db(&file.source)?,
                enum_to_db(&file.file_kind)?,
                enum_to_db(&file.operation_kind)?,
                file.target_path,
                file.backup_path,
                file.temp_path,
                file.marker,
                enum_to_db(&file.status)?,
                file.redacted_diff,
                json_to_db(&file.diagnostics)?,
                updated_at_ms
            ],
        )
        .map_err(storage_err(
            "provider_native_export_file_operation_upsert_failed",
            "failed to upsert provider native export file operation",
        ))?;
        Ok(())
    }
}

impl ProviderCapabilityRepository {
    pub fn insert(conn: &Connection, result: &ProviderCapabilityProbeResult) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO provider_capability_probe_records (
                capability_record_id, provider_profile_id, provider_kind, status,
                summary, capabilities_json, source, checked_at_ms, expires_at_ms,
                diagnostics_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ",
            params![
                result.capability_record_id.as_str(),
                result.provider_profile_id.as_str(),
                enum_to_db(&result.provider_kind)?,
                enum_to_db(&result.status)?,
                result.summary,
                json_to_db(&result.capabilities)?,
                result.source,
                result.checked_at_ms,
                result.expires_at_ms,
                json_to_db(&result.diagnostics)?
            ],
        )
        .map_err(storage_err(
            "provider_capability_record_insert_failed",
            "failed to insert provider capability probe record",
        ))?;
        Ok(())
    }

    pub fn list_latest(conn: &Connection) -> VibexResult<Vec<ProviderCapabilityProbeResult>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT capability_record_id, provider_profile_id, provider_kind, status,
                    summary, capabilities_json, source, checked_at_ms, expires_at_ms,
                    diagnostics_json
                FROM provider_capability_probe_records current
                WHERE NOT EXISTS (
                    SELECT 1
                    FROM provider_capability_probe_records newer
                    WHERE newer.provider_profile_id = current.provider_profile_id
                        AND newer.checked_at_ms > current.checked_at_ms
                )
                ORDER BY provider_profile_id ASC, checked_at_ms DESC
                ",
            )
            .map_err(storage_err(
                "provider_capability_record_list_failed",
                "failed to list provider capability probe records",
            ))?;
        let rows = stmt
            .query_map([], map_provider_capability_probe_result)
            .map_err(storage_err(
                "provider_capability_record_list_failed",
                "failed to list provider capability probe records",
            ))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(storage_err(
                "provider_capability_record_decode_failed",
                "failed to decode provider capability probe record",
            ))?);
        }
        Ok(records)
    }
}

impl ProviderRuntimeOptionSnapshotRepository {
    pub fn upsert_success(
        conn: &Connection,
        record: &ProviderRuntimeOptionSnapshotRecord,
    ) -> VibexResult<()> {
        let model_response = record.model_response.as_ref().ok_or_else(|| {
            VibexError::validation(
                "runtime_option_snapshot_model_response_missing",
                "successful runtime option snapshot requires model evidence",
            )
        })?;
        let session_config = record.session_config.as_ref().ok_or_else(|| {
            VibexError::validation(
                "runtime_option_snapshot_session_config_missing",
                "successful runtime option snapshot requires session configuration evidence",
            )
        })?;
        let last_success_at_ms = record.last_success_at_ms.ok_or_else(|| {
            VibexError::validation(
                "runtime_option_snapshot_success_time_missing",
                "successful runtime option snapshot requires a success timestamp",
            )
        })?;
        conn.execute(
            "
            INSERT INTO provider_runtime_option_snapshots (
                provider_profile_id, agent_id, model_response_json, session_config_json,
                last_success_at_ms, last_attempt_at_ms, last_error_code
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)
            ON CONFLICT(provider_profile_id) DO UPDATE SET
                agent_id = excluded.agent_id,
                model_response_json = excluded.model_response_json,
                session_config_json = excluded.session_config_json,
                last_success_at_ms = excluded.last_success_at_ms,
                last_attempt_at_ms = excluded.last_attempt_at_ms,
                last_error_code = NULL
            ",
            params![
                record.provider_profile_id.as_str(),
                record.agent_id.as_str(),
                json_to_db(model_response)?,
                json_to_db(session_config)?,
                last_success_at_ms,
                record.last_attempt_at_ms,
            ],
        )
        .map_err(storage_err(
            "runtime_option_snapshot_upsert_failed",
            "failed to persist runtime option snapshot",
        ))?;
        Ok(())
    }

    pub fn record_failure(
        conn: &Connection,
        provider_profile_id: &ProviderProfileId,
        agent_id: &AgentId,
        attempted_at_ms: i64,
        error_code: &str,
    ) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO provider_runtime_option_snapshots (
                provider_profile_id, agent_id, model_response_json, session_config_json,
                last_success_at_ms, last_attempt_at_ms, last_error_code
            )
            VALUES (?1, ?2, NULL, NULL, NULL, ?3, ?4)
            ON CONFLICT(provider_profile_id) DO UPDATE SET
                agent_id = excluded.agent_id,
                last_attempt_at_ms = excluded.last_attempt_at_ms,
                last_error_code = excluded.last_error_code
            ",
            params![
                provider_profile_id.as_str(),
                agent_id.as_str(),
                attempted_at_ms,
                error_code,
            ],
        )
        .map_err(storage_err(
            "runtime_option_snapshot_failure_record_failed",
            "failed to record runtime option snapshot failure",
        ))?;
        Ok(())
    }

    pub fn delete(conn: &Connection, provider_profile_id: &ProviderProfileId) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM provider_runtime_option_snapshots WHERE provider_profile_id = ?1",
            params![provider_profile_id.as_str()],
        )
        .map_err(storage_err(
            "runtime_option_snapshot_delete_failed",
            "failed to delete runtime option snapshot",
        ))?;
        Ok(())
    }

    pub fn list(conn: &Connection) -> VibexResult<Vec<ProviderRuntimeOptionSnapshotRecord>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT provider_profile_id, agent_id, model_response_json, session_config_json,
                    last_success_at_ms, last_attempt_at_ms, last_error_code
                FROM provider_runtime_option_snapshots
                ORDER BY provider_profile_id ASC
                ",
            )
            .map_err(storage_err(
                "runtime_option_snapshot_list_failed",
                "failed to list runtime option snapshots",
            ))?;
        let rows = stmt
            .query_map([], map_provider_runtime_option_snapshot)
            .map_err(storage_err(
                "runtime_option_snapshot_list_failed",
                "failed to list runtime option snapshots",
            ))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(storage_err(
                "runtime_option_snapshot_decode_failed",
                "failed to decode runtime option snapshot",
            ))?);
        }
        Ok(records)
    }
}

impl ProviderModelRuntimeOptionSnapshotRepository {
    pub fn upsert_success(
        conn: &Connection,
        record: &ProviderModelRuntimeOptionSnapshotRecord,
    ) -> VibexResult<()> {
        let session_config = record.session_config.as_ref().ok_or_else(|| {
            VibexError::validation(
                "provider_model_runtime_option_snapshot_session_config_missing",
                "successful model runtime option snapshot requires session configuration evidence",
            )
        })?;
        let last_success_at_ms = record.last_success_at_ms.ok_or_else(|| {
            VibexError::validation(
                "provider_model_runtime_option_snapshot_success_time_missing",
                "successful model runtime option snapshot requires a success timestamp",
            )
        })?;
        conn.execute(
            "
            INSERT INTO provider_model_runtime_option_snapshots (
                provider_profile_id, model_id, agent_id, session_config_json,
                last_success_at_ms, last_attempt_at_ms, last_error_code
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)
            ON CONFLICT(provider_profile_id, model_id) DO UPDATE SET
                agent_id = excluded.agent_id,
                session_config_json = excluded.session_config_json,
                last_success_at_ms = excluded.last_success_at_ms,
                last_attempt_at_ms = excluded.last_attempt_at_ms,
                last_error_code = NULL
            ",
            params![
                record.provider_profile_id.as_str(),
                record.model_id,
                record.agent_id.as_str(),
                json_to_db(session_config)?,
                last_success_at_ms,
                record.last_attempt_at_ms,
            ],
        )
        .map_err(storage_err(
            "provider_model_runtime_option_snapshot_upsert_failed",
            "failed to persist model runtime option snapshot",
        ))?;
        Ok(())
    }

    pub fn record_failure(
        conn: &Connection,
        provider_profile_id: &ProviderProfileId,
        model_id: &str,
        agent_id: &AgentId,
        attempted_at_ms: i64,
        error_code: &str,
    ) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO provider_model_runtime_option_snapshots (
                provider_profile_id, model_id, agent_id, session_config_json,
                last_success_at_ms, last_attempt_at_ms, last_error_code
            )
            VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?5)
            ON CONFLICT(provider_profile_id, model_id) DO UPDATE SET
                agent_id = excluded.agent_id,
                last_attempt_at_ms = excluded.last_attempt_at_ms,
                last_error_code = excluded.last_error_code
            ",
            params![
                provider_profile_id.as_str(),
                model_id,
                agent_id.as_str(),
                attempted_at_ms,
                error_code,
            ],
        )
        .map_err(storage_err(
            "provider_model_runtime_option_snapshot_failure_record_failed",
            "failed to record model runtime option snapshot failure",
        ))?;
        Ok(())
    }

    pub fn delete_model(
        conn: &Connection,
        provider_profile_id: &ProviderProfileId,
        model_id: &str,
    ) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM provider_model_runtime_option_snapshots
             WHERE provider_profile_id = ?1 AND model_id = ?2",
            params![provider_profile_id.as_str(), model_id],
        )
        .map_err(storage_err(
            "provider_model_runtime_option_snapshot_delete_failed",
            "failed to delete model runtime option snapshot",
        ))?;
        Ok(())
    }

    pub fn delete_profile(
        conn: &Connection,
        provider_profile_id: &ProviderProfileId,
    ) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM provider_model_runtime_option_snapshots
             WHERE provider_profile_id = ?1",
            params![provider_profile_id.as_str()],
        )
        .map_err(storage_err(
            "provider_model_runtime_option_snapshot_delete_failed",
            "failed to delete model runtime option snapshots",
        ))?;
        Ok(())
    }

    pub fn list(conn: &Connection) -> VibexResult<Vec<ProviderModelRuntimeOptionSnapshotRecord>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT provider_profile_id, model_id, agent_id, session_config_json,
                    last_success_at_ms, last_attempt_at_ms, last_error_code
                FROM provider_model_runtime_option_snapshots
                ORDER BY provider_profile_id ASC, model_id ASC
                ",
            )
            .map_err(storage_err(
                "provider_model_runtime_option_snapshot_list_failed",
                "failed to list model runtime option snapshots",
            ))?;
        let rows = stmt
            .query_map([], map_provider_model_runtime_option_snapshot)
            .map_err(storage_err(
                "provider_model_runtime_option_snapshot_list_failed",
                "failed to list model runtime option snapshots",
            ))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(storage_err(
                "provider_model_runtime_option_snapshot_decode_failed",
                "failed to decode model runtime option snapshot",
            ))?);
        }
        Ok(records)
    }
}

impl AgentRuntimeOptionSnapshotRepository {
    pub fn upsert_success(
        conn: &Connection,
        record: &AgentRuntimeOptionSnapshotRecord,
    ) -> VibexResult<()> {
        let session_config = record.session_config.as_ref().ok_or_else(|| {
            VibexError::validation(
                "agent_runtime_option_snapshot_session_config_missing",
                "successful Agent runtime option snapshot requires session configuration evidence",
            )
        })?;
        let last_success_at_ms = record.last_success_at_ms.ok_or_else(|| {
            VibexError::validation(
                "agent_runtime_option_snapshot_success_time_missing",
                "successful Agent runtime option snapshot requires a success timestamp",
            )
        })?;
        conn.execute(
            "
            INSERT INTO agent_runtime_option_snapshots (
                agent_id, session_config_json, last_success_at_ms,
                last_attempt_at_ms, last_error_code
            )
            VALUES (?1, ?2, ?3, ?4, NULL)
            ON CONFLICT(agent_id) DO UPDATE SET
                session_config_json = excluded.session_config_json,
                last_success_at_ms = excluded.last_success_at_ms,
                last_attempt_at_ms = excluded.last_attempt_at_ms,
                last_error_code = NULL
            ",
            params![
                record.agent_id.as_str(),
                json_to_db(session_config)?,
                last_success_at_ms,
                record.last_attempt_at_ms,
            ],
        )
        .map_err(storage_err(
            "agent_runtime_option_snapshot_upsert_failed",
            "failed to persist Agent runtime option snapshot",
        ))?;
        Ok(())
    }

    pub fn record_failure(
        conn: &Connection,
        agent_id: &AgentId,
        attempted_at_ms: i64,
        error_code: &str,
    ) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO agent_runtime_option_snapshots (
                agent_id, session_config_json, last_success_at_ms,
                last_attempt_at_ms, last_error_code
            )
            VALUES (?1, NULL, NULL, ?2, ?3)
            ON CONFLICT(agent_id) DO UPDATE SET
                last_attempt_at_ms = excluded.last_attempt_at_ms,
                last_error_code = excluded.last_error_code
            ",
            params![agent_id.as_str(), attempted_at_ms, error_code],
        )
        .map_err(storage_err(
            "agent_runtime_option_snapshot_failure_record_failed",
            "failed to record Agent runtime option snapshot failure",
        ))?;
        Ok(())
    }

    pub fn delete(conn: &Connection, agent_id: &AgentId) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM agent_runtime_option_snapshots WHERE agent_id = ?1",
            params![agent_id.as_str()],
        )
        .map_err(storage_err(
            "agent_runtime_option_snapshot_delete_failed",
            "failed to delete Agent runtime option snapshot",
        ))?;
        Ok(())
    }

    pub fn list(conn: &Connection) -> VibexResult<Vec<AgentRuntimeOptionSnapshotRecord>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT agent_id, session_config_json, last_success_at_ms,
                    last_attempt_at_ms, last_error_code
                FROM agent_runtime_option_snapshots
                ORDER BY agent_id ASC
                ",
            )
            .map_err(storage_err(
                "agent_runtime_option_snapshot_list_failed",
                "failed to list Agent runtime option snapshots",
            ))?;
        let rows = stmt
            .query_map([], map_agent_runtime_option_snapshot)
            .map_err(storage_err(
                "agent_runtime_option_snapshot_list_failed",
                "failed to list Agent runtime option snapshots",
            ))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(storage_err(
                "agent_runtime_option_snapshot_decode_failed",
                "failed to decode Agent runtime option snapshot",
            ))?);
        }
        Ok(records)
    }
}

impl AgentAuthCatalogSnapshotRepository {
    pub fn get(
        conn: &Connection,
        agent_id: &AgentId,
        provider_profile_id: Option<&ProviderProfileId>,
    ) -> VibexResult<Option<AgentAuthCatalogSnapshotRecord>> {
        conn.query_row(
            "SELECT agent_id, provider_profile_id, catalog_json, refreshed_at_ms
             FROM agent_auth_catalog_snapshots
             WHERE agent_id = ?1 AND provider_profile_id = ?2",
            params![
                agent_id.as_str(),
                provider_profile_id.map_or("", ProviderProfileId::as_str)
            ],
            map_agent_auth_catalog_snapshot,
        )
        .optional()
        .map_err(|error| {
            VibexError::storage(
                "agent_auth_catalog_snapshot_get_failed",
                "failed to read Agent authentication cache",
            )
            .with_diagnostic("error", error.to_string())
        })
    }

    pub fn upsert(conn: &Connection, record: &AgentAuthCatalogSnapshotRecord) -> VibexResult<()> {
        let catalog_json = json_to_db(&record.catalog)?;
        conn.execute(
            "INSERT INTO agent_auth_catalog_snapshots
                (agent_id, provider_profile_id, catalog_json, refreshed_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(agent_id, provider_profile_id) DO UPDATE SET
                catalog_json = excluded.catalog_json,
                refreshed_at_ms = excluded.refreshed_at_ms",
            params![
                record.agent_id.as_str(),
                record
                    .provider_profile_id
                    .as_ref()
                    .map_or("", ProviderProfileId::as_str),
                catalog_json,
                record.refreshed_at_ms,
            ],
        )
        .map_err(|error| {
            VibexError::storage(
                "agent_auth_catalog_snapshot_upsert_failed",
                "failed to persist Agent authentication cache",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        Ok(())
    }

    pub fn delete_agent(conn: &Connection, agent_id: &AgentId) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM agent_auth_catalog_snapshots WHERE agent_id = ?1",
            params![agent_id.as_str()],
        )
        .map_err(|error| {
            VibexError::storage(
                "agent_auth_catalog_snapshot_delete_failed",
                "failed to delete Agent authentication cache",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        Ok(())
    }
}

impl AgentManagedInstallationRepository {
    pub fn get(
        conn: &Connection,
        agent_id: &AgentId,
    ) -> VibexResult<Option<AgentManagedInstallationRecord>> {
        conn.query_row(
            "SELECT agent_id, registry_agent_id, state_json, command_json,
                    install_root, updated_at_ms
             FROM agent_managed_installations
             WHERE agent_id = ?1",
            params![agent_id.as_str()],
            map_agent_managed_installation,
        )
        .optional()
        .map_err(storage_err(
            "agent_managed_installation_get_failed",
            "failed to read managed Agent installation",
        ))
    }

    pub fn list(conn: &Connection) -> VibexResult<Vec<AgentManagedInstallationRecord>> {
        let mut stmt = conn
            .prepare(
                "SELECT agent_id, registry_agent_id, state_json, command_json,
                        install_root, updated_at_ms
                 FROM agent_managed_installations
                 ORDER BY agent_id ASC",
            )
            .map_err(storage_err(
                "agent_managed_installation_list_failed",
                "failed to list managed Agent installations",
            ))?;
        let rows = stmt
            .query_map([], map_agent_managed_installation)
            .map_err(storage_err(
                "agent_managed_installation_list_failed",
                "failed to list managed Agent installations",
            ))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(storage_err(
                "agent_managed_installation_decode_failed",
                "failed to decode managed Agent installation",
            ))?);
        }
        Ok(records)
    }

    pub fn upsert(conn: &Connection, record: &AgentManagedInstallationRecord) -> VibexResult<()> {
        conn.execute(
            "INSERT INTO agent_managed_installations
                (agent_id, registry_agent_id, state_json, command_json,
                 install_root, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(agent_id) DO UPDATE SET
                registry_agent_id = excluded.registry_agent_id,
                state_json = excluded.state_json,
                command_json = excluded.command_json,
                install_root = excluded.install_root,
                updated_at_ms = excluded.updated_at_ms",
            params![
                record.agent_id.as_str(),
                record.registry_agent_id,
                json_to_db(&record.state)?,
                record.command.as_ref().map(json_to_db).transpose()?,
                record.install_root,
                record.updated_at_ms,
            ],
        )
        .map_err(storage_err(
            "agent_managed_installation_upsert_failed",
            "failed to persist managed Agent installation",
        ))?;
        Ok(())
    }

    pub fn delete(conn: &Connection, agent_id: &AgentId) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM agent_managed_installations WHERE agent_id = ?1",
            params![agent_id.as_str()],
        )
        .map_err(storage_err(
            "agent_managed_installation_delete_failed",
            "failed to delete managed Agent installation",
        ))?;
        Ok(())
    }
}

fn map_agent_managed_installation(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<AgentManagedInstallationRecord> {
    Ok(AgentManagedInstallationRecord {
        agent_id: parse_id_sql(row.get(0)?, AgentId::parse)?,
        registry_agent_id: row.get(1)?,
        state: json_from_db_sql(row.get(2)?)?,
        command: row
            .get::<_, Option<String>>(3)?
            .map(json_from_db)
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        install_root: row.get(4)?,
        updated_at_ms: row.get(5)?,
    })
}

fn map_agent_auth_catalog_snapshot(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<AgentAuthCatalogSnapshotRecord> {
    Ok(AgentAuthCatalogSnapshotRecord {
        agent_id: parse_id_sql(row.get(0)?, AgentId::parse)?,
        provider_profile_id: {
            let value: String = row.get(1)?;
            (!value.is_empty())
                .then_some(value)
                .map(ProviderProfileId::parse)
                .transpose()
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?
        },
        catalog: json_from_db_sql(row.get(2)?)?,
        refreshed_at_ms: row.get(3)?,
    })
}

impl ProviderHealthRepository {
    pub fn insert(conn: &Connection, result: &ProviderHealthProbeResult) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO provider_health_probe_records (
                health_record_id, provider_profile_id, provider_kind, probe_kind,
                status, summary, latency_ms, checked_at_ms, expires_at_ms, diagnostics_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ",
            params![
                result.health_record_id.as_str(),
                result.provider_profile_id.as_str(),
                enum_to_db(&result.provider_kind)?,
                enum_to_db(&result.probe_kind)?,
                enum_to_db(&result.status)?,
                result.summary,
                result.latency_ms.map(i64::from),
                result.checked_at_ms,
                result.expires_at_ms,
                json_to_db(&result.diagnostics)?
            ],
        )
        .map_err(storage_err(
            "provider_health_record_insert_failed",
            "failed to insert provider health record",
        ))?;
        Ok(())
    }

    pub fn list_latest(conn: &Connection) -> VibexResult<Vec<ProviderHealthProbeResult>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT health_record_id, provider_profile_id, provider_kind, probe_kind,
                    status, summary, latency_ms, checked_at_ms, expires_at_ms, diagnostics_json
                FROM provider_health_probe_records current
                WHERE NOT EXISTS (
                    SELECT 1
                    FROM provider_health_probe_records newer
                    WHERE newer.provider_profile_id = current.provider_profile_id
                        AND newer.probe_kind = current.probe_kind
                        AND newer.checked_at_ms > current.checked_at_ms
                )
                ORDER BY provider_profile_id ASC, probe_kind ASC
                ",
            )
            .map_err(storage_err(
                "provider_health_record_list_failed",
                "failed to list provider health records",
            ))?;
        let rows = stmt
            .query_map([], map_provider_health_probe_result)
            .map_err(storage_err(
                "provider_health_record_list_failed",
                "failed to list provider health records",
            ))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(storage_err(
                "provider_health_record_decode_failed",
                "failed to decode provider health record",
            ))?);
        }
        Ok(records)
    }
}

impl ProviderUsageRepository {
    pub fn insert(conn: &Connection, record: &ProviderUsageRecord) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO provider_usage_records (
                usage_record_id, provider_profile_id, provider_kind, source, unit,
                label, used, limit_value, remaining, window_label,
                window_started_at_ms, window_ends_at_ms, recorded_at_ms, metadata_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            ",
            params![
                record.usage_record_id.as_str(),
                record.provider_profile_id.as_str(),
                enum_to_db(&record.provider_kind)?,
                record.source,
                enum_to_db(&record.unit)?,
                record.label,
                record.used,
                record.limit_value,
                record.remaining,
                record.window.as_ref().map(|window| window.label.as_str()),
                record
                    .window
                    .as_ref()
                    .and_then(|window| window.started_at_ms),
                record.window.as_ref().and_then(|window| window.ends_at_ms),
                record.recorded_at_ms,
                json_to_db(&record.metadata)?
            ],
        )
        .map_err(storage_err(
            "provider_usage_record_insert_failed",
            "failed to insert provider usage record",
        ))?;
        Ok(())
    }

    pub fn list_latest(conn: &Connection) -> VibexResult<Vec<ProviderUsageRecord>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT usage_record_id, provider_profile_id, provider_kind, source, unit,
                    label, used, limit_value, remaining, window_label,
                    window_started_at_ms, window_ends_at_ms, recorded_at_ms, metadata_json
                FROM provider_usage_records current
                WHERE NOT EXISTS (
                    SELECT 1
                    FROM provider_usage_records newer
                    WHERE newer.provider_profile_id = current.provider_profile_id
                        AND newer.unit = current.unit
                        AND newer.label = current.label
                        AND newer.recorded_at_ms > current.recorded_at_ms
                )
                ORDER BY provider_profile_id ASC, unit ASC, label ASC
                ",
            )
            .map_err(storage_err(
                "provider_usage_record_list_failed",
                "failed to list provider usage records",
            ))?;
        let rows = stmt
            .query_map([], map_provider_usage_record)
            .map_err(storage_err(
                "provider_usage_record_list_failed",
                "failed to list provider usage records",
            ))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(storage_err(
                "provider_usage_record_decode_failed",
                "failed to decode provider usage record",
            ))?);
        }
        Ok(records)
    }
}

const SCHEDULED_TASK_LIST_DEFAULT_LIMIT: i64 = 100;
const SCHEDULED_TASK_LIST_MAX_LIMIT: i64 = 500;
const SCHEDULED_TASK_ERROR_CODE_MAX_CHARS: usize = 128;
const SCHEDULED_TASK_ERROR_MESSAGE_MAX_CHARS: usize = 1024;
const SCHEDULED_TASK_DIAGNOSTIC_KEY_MAX_CHARS: usize = 128;
const SCHEDULED_TASK_DIAGNOSTIC_VALUE_MAX_CHARS: usize = 2048;
const SCHEDULED_TASK_PERMISSION_REQUIRED_CODE: &str = "scheduler/permission_required";
const SCHEDULED_TASK_RECOVERED_STALE_RUN_CODE: &str = "scheduler/recovered_stale_run";

impl ScheduledTaskRepository {
    pub fn create(
        conn: &Connection,
        request: ScheduledTaskCreateRequest,
    ) -> VibexResult<ScheduledTask> {
        let now = unix_timestamp_ms();
        let task = ScheduledTask {
            id: ScheduledTaskId::new(),
            title: request.title,
            prompt: request.prompt,
            project_id: request.project_id,
            workspace_id: request.workspace_id,
            workspace_root: request.workspace_root,
            workspace_mode: request.workspace_mode,
            provider_kind: request.provider_kind,
            provider_profile_id: request.provider_profile_id,
            schedule: request.schedule,
            status: ScheduledTaskStatus::Active,
            safety: request
                .safety
                .unwrap_or_else(AgentSessionSafety::workspace_write_ask_on_risk),
            next_run_at_ms: request.next_run_at_ms,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        };

        insert_scheduled_task(conn, &task)?;
        Ok(task)
    }

    pub fn get(conn: &Connection, task_id: &ScheduledTaskId) -> VibexResult<Option<ScheduledTask>> {
        get_scheduled_task(conn, task_id, false)
    }

    pub fn list(
        conn: &Connection,
        request: ScheduledTaskListRequest,
    ) -> VibexResult<Vec<ScheduledTask>> {
        let status = request.status.as_ref().map(enum_to_db).transpose()?;
        let workspace_id = request.workspace_id.as_ref().map(WorkspaceId::as_str);
        let include_deleted = if request.include_deleted {
            1_i64
        } else {
            0_i64
        };
        let limit = bounded_limit(request.limit);

        let mut stmt = conn
            .prepare(
                "
                SELECT scheduled_task_id, title, prompt, project_id, workspace_id,
                    workspace_root, workspace_mode, provider_kind, provider_profile_id,
                    schedule_json, status, safety_json, next_run_at_ms, created_at_ms,
                    updated_at_ms, deleted_at_ms
                FROM scheduled_tasks
                WHERE (?1 IS NULL OR workspace_id = ?1)
                    AND (?2 IS NULL OR status = ?2)
                    AND (?3 = 1 OR deleted_at_ms IS NULL)
                ORDER BY updated_at_ms DESC, created_at_ms DESC
                LIMIT ?4
                ",
            )
            .map_err(storage_err(
                "scheduled_task_list_failed",
                "failed to list scheduled tasks",
            ))?;
        let rows = stmt
            .query_map(
                params![workspace_id, status, include_deleted, limit],
                map_scheduled_task,
            )
            .map_err(storage_err(
                "scheduled_task_list_failed",
                "failed to list scheduled tasks",
            ))?;
        collect_rows(
            rows,
            "scheduled_task_decode_failed",
            "failed to decode scheduled task row",
        )
    }

    pub fn list_due(
        conn: &Connection,
        now_ms: i64,
        limit: Option<u32>,
    ) -> VibexResult<Vec<ScheduledTask>> {
        let active = enum_to_db(&ScheduledTaskStatus::Active)?;
        let limit = bounded_limit(limit);
        let mut stmt = conn
            .prepare(
                "
                SELECT scheduled_task_id, title, prompt, project_id, workspace_id,
                    workspace_root, workspace_mode, provider_kind, provider_profile_id,
                    schedule_json, status, safety_json, next_run_at_ms, created_at_ms,
                    updated_at_ms, deleted_at_ms
                FROM scheduled_tasks
                WHERE deleted_at_ms IS NULL
                    AND status = ?1
                    AND next_run_at_ms IS NOT NULL
                    AND next_run_at_ms <= ?2
                ORDER BY next_run_at_ms ASC, created_at_ms ASC, scheduled_task_id ASC
                LIMIT ?3
                ",
            )
            .map_err(storage_err(
                "scheduled_task_due_list_failed",
                "failed to list due scheduled tasks",
            ))?;
        let rows = stmt
            .query_map(params![active, now_ms, limit], map_scheduled_task)
            .map_err(storage_err(
                "scheduled_task_due_list_failed",
                "failed to list due scheduled tasks",
            ))?;
        collect_rows(
            rows,
            "scheduled_task_due_decode_failed",
            "failed to decode due scheduled task row",
        )
    }

    pub fn claim_due(
        conn: &mut Connection,
        task_id: &ScheduledTaskId,
        now_ms: i64,
    ) -> VibexResult<Option<(ScheduledTask, ScheduledTaskRun)>> {
        let tx = conn.transaction().map_err(storage_err(
            "scheduled_task_claim_transaction_failed",
            "failed to start scheduled task claim transaction",
        ))?;
        let Some(mut task) = get_scheduled_task(&tx, task_id, false)? else {
            tx.commit().map_err(storage_err(
                "scheduled_task_claim_commit_failed",
                "failed to commit scheduled task claim transaction",
            ))?;
            return Ok(None);
        };
        let Some(due_at_ms) = task.next_run_at_ms else {
            tx.commit().map_err(storage_err(
                "scheduled_task_claim_commit_failed",
                "failed to commit scheduled task claim transaction",
            ))?;
            return Ok(None);
        };
        if task.status != ScheduledTaskStatus::Active || due_at_ms > now_ms {
            tx.commit().map_err(storage_err(
                "scheduled_task_claim_commit_failed",
                "failed to commit scheduled task claim transaction",
            ))?;
            return Ok(None);
        }

        let active = enum_to_db(&ScheduledTaskStatus::Active)?;
        let changed = tx
            .execute(
                "
                UPDATE scheduled_tasks
                SET next_run_at_ms = NULL, updated_at_ms = ?3
                WHERE scheduled_task_id = ?1
                    AND status = ?2
                    AND deleted_at_ms IS NULL
                    AND next_run_at_ms = ?4
                ",
                params![task_id.as_str(), active, now_ms, due_at_ms],
            )
            .map_err(storage_err(
                "scheduled_task_claim_failed",
                "failed to claim due scheduled task",
            ))?;
        if changed == 0 {
            tx.commit().map_err(storage_err(
                "scheduled_task_claim_commit_failed",
                "failed to commit scheduled task claim transaction",
            ))?;
            return Ok(None);
        }

        task.next_run_at_ms = None;
        task.updated_at_ms = now_ms;
        let run = ScheduledTaskRun {
            id: ScheduledTaskRunId::new(),
            task_id: task.id.clone(),
            status: ScheduledTaskRunStatus::Running,
            trigger: ScheduledTaskRunTrigger::Scheduler,
            session_id: None,
            due_at_ms,
            started_at_ms: Some(now_ms),
            ended_at_ms: None,
            attempt: 1,
            error_code: None,
            error_message: None,
            redacted_diagnostics: Vec::new(),
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        insert_scheduled_task_run(&tx, &run)?;
        tx.commit().map_err(storage_err(
            "scheduled_task_claim_commit_failed",
            "failed to commit scheduled task claim transaction",
        ))?;
        Ok(Some((task, run)))
    }

    pub fn update(
        conn: &Connection,
        request: ScheduledTaskUpdateRequest,
    ) -> VibexResult<ScheduledTask> {
        let mut task = require_scheduled_task(conn, &request.id, false)?;
        if let Some(title) = request.title {
            task.title = title;
        }
        if let Some(prompt) = request.prompt {
            task.prompt = prompt;
        }
        if let Some(project_id) = request.project_id {
            task.project_id = Some(project_id);
        } else if request.clear_project_id {
            task.project_id = None;
        }
        if let Some(workspace_id) = request.workspace_id {
            task.workspace_id = Some(workspace_id);
        } else if request.clear_workspace_id {
            task.workspace_id = None;
        }
        if let Some(workspace_root) = request.workspace_root {
            task.workspace_root = workspace_root;
        }
        if let Some(workspace_mode) = request.workspace_mode {
            task.workspace_mode = workspace_mode;
        }
        if let Some(provider_kind) = request.provider_kind {
            task.provider_kind = provider_kind;
        }
        if let Some(provider_profile_id) = request.provider_profile_id {
            task.provider_profile_id = Some(provider_profile_id);
        } else if request.clear_provider_profile_id {
            task.provider_profile_id = None;
        }
        if let Some(schedule) = request.schedule {
            task.schedule = schedule;
        }
        if let Some(safety) = request.safety {
            task.safety = safety;
        }
        if let Some(next_run_at_ms) = request.next_run_at_ms {
            task.next_run_at_ms = Some(next_run_at_ms);
        } else if request.clear_next_run_at_ms {
            task.next_run_at_ms = None;
        }
        task.updated_at_ms = unix_timestamp_ms();

        update_scheduled_task(conn, &task)?;
        Ok(task)
    }

    pub fn mark_task_after_run(
        conn: &Connection,
        task_id: &ScheduledTaskId,
        status: ScheduledTaskStatus,
        next_run_at_ms: Option<i64>,
        now_ms: i64,
    ) -> VibexResult<ScheduledTask> {
        let mut task = require_scheduled_task(conn, task_id, false)?;
        task.status = status;
        task.next_run_at_ms = next_run_at_ms;
        task.updated_at_ms = now_ms;
        update_scheduled_task(conn, &task)?;
        Ok(task)
    }

    pub fn pause(conn: &Connection, task_id: &ScheduledTaskId) -> VibexResult<ScheduledTask> {
        update_scheduled_task_status(conn, task_id, ScheduledTaskStatus::Paused)
    }

    pub fn resume(conn: &Connection, task_id: &ScheduledTaskId) -> VibexResult<ScheduledTask> {
        update_scheduled_task_status(conn, task_id, ScheduledTaskStatus::Active)
    }

    pub fn soft_delete(conn: &Connection, task_id: &ScheduledTaskId) -> VibexResult<ScheduledTask> {
        let mut task = require_scheduled_task(conn, task_id, false)?;
        let now = unix_timestamp_ms();
        task.status = ScheduledTaskStatus::Deleted;
        task.updated_at_ms = now;
        task.deleted_at_ms = Some(now);
        update_scheduled_task(conn, &task)?;
        Ok(task)
    }

    pub fn create_run(
        conn: &Connection,
        request: ScheduledTaskRunCreateRequest,
    ) -> VibexResult<ScheduledTaskRun> {
        let now = unix_timestamp_ms();
        let run = ScheduledTaskRun {
            id: ScheduledTaskRunId::new(),
            task_id: request.task_id,
            status: request.status,
            trigger: request.trigger,
            session_id: request.session_id,
            due_at_ms: request.due_at_ms,
            started_at_ms: request.started_at_ms,
            ended_at_ms: request.ended_at_ms,
            attempt: request.attempt,
            error_code: request
                .error_code
                .map(|value| truncate_chars(&value, SCHEDULED_TASK_ERROR_CODE_MAX_CHARS)),
            error_message: request
                .error_message
                .map(|value| truncate_chars(&value, SCHEDULED_TASK_ERROR_MESSAGE_MAX_CHARS)),
            redacted_diagnostics: bound_scheduled_task_diagnostics(request.redacted_diagnostics),
            created_at_ms: now,
            updated_at_ms: now,
        };

        insert_scheduled_task_run(conn, &run)?;
        Ok(run)
    }

    pub fn update_run(
        conn: &Connection,
        request: ScheduledTaskRunUpdateRequest,
    ) -> VibexResult<ScheduledTaskRun> {
        let mut run = require_scheduled_task_run(conn, &request.id)?;
        if let Some(status) = request.status {
            run.status = status;
        }
        if let Some(session_id) = request.session_id {
            run.session_id = Some(session_id);
        } else if request.clear_session_id {
            run.session_id = None;
        }
        if let Some(started_at_ms) = request.started_at_ms {
            run.started_at_ms = Some(started_at_ms);
        } else if request.clear_started_at_ms {
            run.started_at_ms = None;
        }
        if let Some(ended_at_ms) = request.ended_at_ms {
            run.ended_at_ms = Some(ended_at_ms);
        } else if request.clear_ended_at_ms {
            run.ended_at_ms = None;
        }
        if let Some(attempt) = request.attempt {
            run.attempt = attempt;
        }
        if let Some(error_code) = request.error_code {
            run.error_code = Some(truncate_chars(
                &error_code,
                SCHEDULED_TASK_ERROR_CODE_MAX_CHARS,
            ));
        } else if request.clear_error_code {
            run.error_code = None;
        }
        if let Some(error_message) = request.error_message {
            run.error_message = Some(truncate_chars(
                &error_message,
                SCHEDULED_TASK_ERROR_MESSAGE_MAX_CHARS,
            ));
        } else if request.clear_error_message {
            run.error_message = None;
        }
        if let Some(diagnostics) = request.redacted_diagnostics {
            run.redacted_diagnostics = bound_scheduled_task_diagnostics(diagnostics);
        }
        run.updated_at_ms = unix_timestamp_ms();

        update_scheduled_task_run(conn, &run)?;
        Ok(run)
    }

    pub fn list_runs(
        conn: &Connection,
        request: ScheduledTaskRunListRequest,
    ) -> VibexResult<Vec<ScheduledTaskRun>> {
        let task_id = request.task_id.as_ref().map(ScheduledTaskId::as_str);
        let session_id = request.session_id.as_ref().map(VibexSessionId::as_str);
        let status = request.status.as_ref().map(enum_to_db).transpose()?;
        let limit = bounded_limit(request.limit);

        let mut stmt = conn
            .prepare(
                "
                SELECT scheduled_task_run_id, scheduled_task_id, status, trigger,
                    session_id, due_at_ms, started_at_ms, ended_at_ms, attempt,
                    error_code, error_message, redacted_diagnostics_json,
                    created_at_ms, updated_at_ms
                FROM scheduled_task_runs
                WHERE (?1 IS NULL OR scheduled_task_id = ?1)
                    AND (?2 IS NULL OR session_id = ?2)
                    AND (?3 IS NULL OR status = ?3)
                ORDER BY created_at_ms DESC, scheduled_task_run_id DESC
                LIMIT ?4
                ",
            )
            .map_err(storage_err(
                "scheduled_task_run_list_failed",
                "failed to list scheduled task runs",
            ))?;
        let rows = stmt
            .query_map(
                params![task_id, session_id, status, limit],
                map_scheduled_task_run,
            )
            .map_err(storage_err(
                "scheduled_task_run_list_failed",
                "failed to list scheduled task runs",
            ))?;
        collect_rows(
            rows,
            "scheduled_task_run_decode_failed",
            "failed to decode scheduled task run row",
        )
    }

    pub fn list_attention(
        conn: &Connection,
        request: ScheduledTaskAttentionListRequest,
    ) -> VibexResult<Vec<ScheduledTaskAttentionSummary>> {
        let workspace_id = request.workspace_id.as_ref().map(WorkspaceId::as_str);
        let limit = bounded_limit(request.limit);
        let failed = enum_to_db(&ScheduledTaskRunStatus::Failed)?;
        let skipped = enum_to_db(&ScheduledTaskRunStatus::Skipped)?;
        let canceled = enum_to_db(&ScheduledTaskRunStatus::Canceled)?;

        let mut stmt = conn
            .prepare(
                "
                SELECT t.scheduled_task_id, t.title, t.workspace_id, t.workspace_root,
                    t.provider_kind, t.provider_profile_id,
                    r.scheduled_task_run_id, r.status, r.trigger, r.session_id,
                    r.error_code, r.error_message, r.created_at_ms
                FROM scheduled_task_runs r
                JOIN scheduled_tasks t ON t.scheduled_task_id = r.scheduled_task_id
                WHERE (?1 IS NULL OR t.workspace_id = ?1)
                    AND (
                        r.status IN (?2, ?3, ?4)
                        OR r.error_code IN (?5, ?6)
                    )
                ORDER BY r.created_at_ms DESC, r.scheduled_task_run_id DESC
                LIMIT ?7
                ",
            )
            .map_err(storage_err(
                "scheduled_task_attention_list_failed",
                "failed to list scheduled task attention summaries",
            ))?;
        let rows = stmt
            .query_map(
                params![
                    workspace_id,
                    failed,
                    skipped,
                    canceled,
                    SCHEDULED_TASK_PERMISSION_REQUIRED_CODE,
                    SCHEDULED_TASK_RECOVERED_STALE_RUN_CODE,
                    limit
                ],
                map_scheduled_task_attention_summary,
            )
            .map_err(storage_err(
                "scheduled_task_attention_list_failed",
                "failed to list scheduled task attention summaries",
            ))?;
        collect_rows(
            rows,
            "scheduled_task_attention_decode_failed",
            "failed to decode scheduled task attention summary",
        )
    }

    pub fn list_audit(
        conn: &Connection,
        request: ScheduledTaskAuditListRequest,
    ) -> VibexResult<Vec<ScheduledTaskAuditRecord>> {
        let workspace_id = request.workspace_id.as_ref().map(WorkspaceId::as_str);
        let status = request.status.as_ref().map(enum_to_db).transpose()?;
        let limit = bounded_limit(request.limit);

        let mut stmt = conn
            .prepare(
                "
                SELECT t.scheduled_task_id, t.title, t.workspace_id, t.workspace_root,
                    t.provider_kind, t.provider_profile_id,
                    r.scheduled_task_run_id, r.status, r.trigger, r.session_id,
                    r.error_code, r.error_message, r.redacted_diagnostics_json,
                    r.created_at_ms
                FROM scheduled_task_runs r
                JOIN scheduled_tasks t ON t.scheduled_task_id = r.scheduled_task_id
                WHERE (?1 IS NULL OR t.workspace_id = ?1)
                    AND (?2 IS NULL OR r.status = ?2)
                ORDER BY r.created_at_ms DESC, r.scheduled_task_run_id DESC
                LIMIT ?3
                ",
            )
            .map_err(storage_err(
                "scheduled_task_audit_list_failed",
                "failed to list scheduled task audit records",
            ))?;
        let rows = stmt
            .query_map(
                params![workspace_id, status, limit],
                map_scheduled_task_audit_record,
            )
            .map_err(storage_err(
                "scheduled_task_audit_list_failed",
                "failed to list scheduled task audit records",
            ))?;
        collect_rows(
            rows,
            "scheduled_task_audit_decode_failed",
            "failed to decode scheduled task audit record",
        )
    }

    pub fn list_stale_running_runs(
        conn: &Connection,
        before_ms: i64,
        limit: Option<u32>,
    ) -> VibexResult<Vec<ScheduledTaskRun>> {
        let running = enum_to_db(&ScheduledTaskRunStatus::Running)?;
        let limit = bounded_limit(limit);
        let mut stmt = conn
            .prepare(
                "
                SELECT scheduled_task_run_id, scheduled_task_id, status, trigger,
                    session_id, due_at_ms, started_at_ms, ended_at_ms, attempt,
                    error_code, error_message, redacted_diagnostics_json,
                    created_at_ms, updated_at_ms
                FROM scheduled_task_runs
                WHERE status = ?1
                    AND COALESCE(started_at_ms, created_at_ms) <= ?2
                ORDER BY COALESCE(started_at_ms, created_at_ms) ASC,
                    scheduled_task_run_id ASC
                LIMIT ?3
                ",
            )
            .map_err(storage_err(
                "scheduled_task_stale_run_list_failed",
                "failed to list stale scheduled task runs",
            ))?;
        let rows = stmt
            .query_map(params![running, before_ms, limit], map_scheduled_task_run)
            .map_err(storage_err(
                "scheduled_task_stale_run_list_failed",
                "failed to list stale scheduled task runs",
            ))?;
        collect_rows(
            rows,
            "scheduled_task_stale_run_decode_failed",
            "failed to decode stale scheduled task run row",
        )
    }
}

impl AutomationGraphRepository {
    pub fn create(
        conn: &mut Connection,
        request: AutomationGraphCreateRequest,
    ) -> VibexResult<AutomationGraph> {
        let now = unix_timestamp_ms();
        let graph_id = AutomationGraphId::new();
        let nodes = automation_nodes_from_requests(&graph_id, request.nodes, now);
        let edges = automation_edges_from_requests(&graph_id, request.edges, now);
        validate_automation_edges(&nodes, &edges)?;

        let graph = AutomationGraph {
            id: graph_id,
            title: request.title,
            description: request.description,
            project_id: request.project_id,
            workspace_id: request.workspace_id,
            workspace_root: request.workspace_root,
            workspace_mode: request.workspace_mode,
            provider_kind: request.provider_kind,
            provider_profile_id: request.provider_profile_id,
            trigger: request.trigger,
            status: AutomationGraphStatus::Active,
            version: 1,
            nodes,
            edges,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        };

        let tx = conn.transaction().map_err(storage_err(
            "automation_graph_create_transaction_failed",
            "failed to start automation graph create transaction",
        ))?;
        insert_automation_graph(&tx, &graph)?;
        for node in &graph.nodes {
            insert_automation_node(&tx, node)?;
        }
        for edge in &graph.edges {
            insert_automation_edge(&tx, edge)?;
        }
        tx.commit().map_err(storage_err(
            "automation_graph_create_commit_failed",
            "failed to commit automation graph create transaction",
        ))?;
        Ok(graph)
    }

    pub fn get(
        conn: &Connection,
        graph_id: &AutomationGraphId,
    ) -> VibexResult<Option<AutomationGraph>> {
        get_automation_graph(conn, graph_id, false)
    }

    pub fn list(
        conn: &Connection,
        request: AutomationGraphListRequest,
    ) -> VibexResult<Vec<AutomationGraph>> {
        let workspace_id = request.workspace_id.as_ref().map(WorkspaceId::as_str);
        let status = request.status.as_ref().map(enum_to_db).transpose()?;
        let include_deleted = if request.include_deleted {
            1_i64
        } else {
            0_i64
        };
        let limit = bounded_limit(request.limit);

        let mut stmt = conn
            .prepare(
                "
                SELECT automation_graph_id, title, description, project_id, workspace_id,
                    workspace_root, workspace_mode, provider_kind, provider_profile_id,
                    trigger_json, status, version, created_at_ms, updated_at_ms, deleted_at_ms
                FROM automation_graphs
                WHERE (?1 IS NULL OR workspace_id = ?1)
                    AND (?2 IS NULL OR status = ?2)
                    AND (?3 = 1 OR deleted_at_ms IS NULL)
                ORDER BY updated_at_ms DESC, created_at_ms DESC
                LIMIT ?4
                ",
            )
            .map_err(storage_err(
                "automation_graph_list_failed",
                "failed to list automation graphs",
            ))?;
        let rows = stmt
            .query_map(
                params![workspace_id, status, include_deleted, limit],
                map_automation_graph,
            )
            .map_err(storage_err(
                "automation_graph_list_failed",
                "failed to list automation graphs",
            ))?;
        let mut graphs = collect_rows(
            rows,
            "automation_graph_decode_failed",
            "failed to decode automation graph row",
        )?;
        for graph in &mut graphs {
            graph.nodes = list_automation_nodes(conn, &graph.id)?;
            graph.edges = list_automation_edges(conn, &graph.id)?;
        }
        Ok(graphs)
    }

    pub fn update(
        conn: &Connection,
        request: AutomationGraphUpdateRequest,
    ) -> VibexResult<AutomationGraph> {
        let mut graph = require_automation_graph(conn, &request.id, false)?;
        if let Some(title) = request.title {
            graph.title = title;
        }
        if let Some(description) = request.description {
            graph.description = Some(description);
        } else if request.clear_description {
            graph.description = None;
        }
        if let Some(project_id) = request.project_id {
            graph.project_id = Some(project_id);
        } else if request.clear_project_id {
            graph.project_id = None;
        }
        if let Some(workspace_id) = request.workspace_id {
            graph.workspace_id = Some(workspace_id);
        } else if request.clear_workspace_id {
            graph.workspace_id = None;
        }
        if let Some(workspace_root) = request.workspace_root {
            graph.workspace_root = workspace_root;
        }
        if let Some(workspace_mode) = request.workspace_mode {
            graph.workspace_mode = workspace_mode;
        }
        if let Some(provider_kind) = request.provider_kind {
            graph.provider_kind = Some(provider_kind);
        } else if request.clear_provider_kind {
            graph.provider_kind = None;
        }
        if let Some(provider_profile_id) = request.provider_profile_id {
            graph.provider_profile_id = Some(provider_profile_id);
        } else if request.clear_provider_profile_id {
            graph.provider_profile_id = None;
        }
        if let Some(trigger) = request.trigger {
            graph.trigger = trigger;
        }
        if let Some(status) = request.status {
            graph.status = status;
        }
        graph.version = graph.version.saturating_add(1);
        graph.updated_at_ms = unix_timestamp_ms();
        if graph.status != AutomationGraphStatus::Deleted {
            graph.deleted_at_ms = None;
        }

        update_automation_graph(conn, &graph)?;
        get_automation_graph(conn, &graph.id, true)?.ok_or_else(|| {
            VibexError::storage(
                "automation_graph_not_found",
                "automation graph was not found",
            )
            .with_diagnostic("automationGraphId", graph.id.as_str())
        })
    }

    pub fn soft_delete(
        conn: &Connection,
        graph_id: &AutomationGraphId,
    ) -> VibexResult<AutomationGraph> {
        let mut graph = require_automation_graph(conn, graph_id, false)?;
        let now = unix_timestamp_ms();
        graph.status = AutomationGraphStatus::Deleted;
        graph.version = graph.version.saturating_add(1);
        graph.updated_at_ms = now;
        graph.deleted_at_ms = Some(now);
        update_automation_graph(conn, &graph)?;
        get_automation_graph(conn, graph_id, true)?.ok_or_else(|| {
            VibexError::storage(
                "automation_graph_not_found",
                "automation graph was not found",
            )
            .with_diagnostic("automationGraphId", graph_id.as_str())
        })
    }

    pub fn replace_definition(
        conn: &mut Connection,
        graph_id: &AutomationGraphId,
        nodes: Vec<AutomationNodeCreateRequest>,
        edges: Vec<AutomationEdgeCreateRequest>,
        expected_version: Option<u32>,
    ) -> VibexResult<AutomationGraph> {
        let now = unix_timestamp_ms();
        let replacement_nodes = automation_nodes_from_requests(graph_id, nodes, now);
        let replacement_edges = automation_edges_from_requests(graph_id, edges, now);
        validate_automation_edges(&replacement_nodes, &replacement_edges)?;

        let tx = conn.transaction().map_err(storage_err(
            "automation_graph_definition_transaction_failed",
            "failed to start automation graph definition transaction",
        ))?;
        let mut graph = require_automation_graph(&tx, graph_id, false)?;
        if let Some(expected_version) = expected_version
            && graph.version != expected_version
        {
            return Err(VibexError::conflict(
                "automation_graph_version_conflict",
                "automation graph changed since this draft was loaded",
            )
            .with_diagnostic("expectedVersion", expected_version.to_string())
            .with_diagnostic("actualVersion", graph.version.to_string()));
        }
        tx.execute(
            "DELETE FROM automation_graph_edges WHERE automation_graph_id = ?1",
            params![graph_id.as_str()],
        )
        .map_err(storage_err(
            "automation_graph_edge_delete_failed",
            "failed to delete automation graph edges",
        ))?;
        tx.execute(
            "DELETE FROM automation_graph_nodes WHERE automation_graph_id = ?1",
            params![graph_id.as_str()],
        )
        .map_err(storage_err(
            "automation_graph_node_delete_failed",
            "failed to delete automation graph nodes",
        ))?;
        for node in &replacement_nodes {
            insert_automation_node(&tx, node)?;
        }
        for edge in &replacement_edges {
            insert_automation_edge(&tx, edge)?;
        }
        graph.nodes = replacement_nodes;
        graph.edges = replacement_edges;
        graph.version = graph.version.saturating_add(1);
        graph.updated_at_ms = now;
        update_automation_graph(&tx, &graph)?;
        tx.commit().map_err(storage_err(
            "automation_graph_definition_commit_failed",
            "failed to commit automation graph definition transaction",
        ))?;

        get_automation_graph(conn, graph_id, false)?.ok_or_else(|| {
            VibexError::storage(
                "automation_graph_not_found",
                "automation graph was not found",
            )
            .with_diagnostic("automationGraphId", graph_id.as_str())
        })
    }

    pub fn create_run(
        conn: &Connection,
        request: AutomationRunCreateRequest,
    ) -> VibexResult<AutomationRun> {
        let now = unix_timestamp_ms();
        let run = AutomationRun {
            id: AutomationRunId::new(),
            graph_id: request.graph_id,
            status: request.status,
            trigger: request.trigger,
            scheduled_task_id: request.scheduled_task_id,
            session_id: request.session_id,
            started_at_ms: request.started_at_ms,
            ended_at_ms: request.ended_at_ms,
            error_code: request
                .error_code
                .map(|value| truncate_chars(&value, SCHEDULED_TASK_ERROR_CODE_MAX_CHARS)),
            error_message: request
                .error_message
                .map(|value| truncate_chars(&value, SCHEDULED_TASK_ERROR_MESSAGE_MAX_CHARS)),
            redacted_diagnostics: bound_scheduled_task_diagnostics(request.redacted_diagnostics),
            created_at_ms: now,
            updated_at_ms: now,
        };
        insert_automation_run(conn, &run)?;
        Ok(run)
    }

    pub fn get_run(
        conn: &Connection,
        run_id: &AutomationRunId,
    ) -> VibexResult<Option<AutomationRun>> {
        get_automation_run(conn, run_id)
    }

    pub fn update_run(
        conn: &Connection,
        request: AutomationRunUpdateRequest,
    ) -> VibexResult<AutomationRun> {
        let mut run = require_automation_run(conn, &request.id)?;
        if let Some(status) = request.status {
            run.status = status;
        }
        if let Some(scheduled_task_id) = request.scheduled_task_id {
            run.scheduled_task_id = Some(scheduled_task_id);
        } else if request.clear_scheduled_task_id {
            run.scheduled_task_id = None;
        }
        if let Some(session_id) = request.session_id {
            run.session_id = Some(session_id);
        } else if request.clear_session_id {
            run.session_id = None;
        }
        if let Some(started_at_ms) = request.started_at_ms {
            run.started_at_ms = Some(started_at_ms);
        } else if request.clear_started_at_ms {
            run.started_at_ms = None;
        }
        if let Some(ended_at_ms) = request.ended_at_ms {
            run.ended_at_ms = Some(ended_at_ms);
        } else if request.clear_ended_at_ms {
            run.ended_at_ms = None;
        }
        if let Some(error_code) = request.error_code {
            run.error_code = Some(truncate_chars(
                &error_code,
                SCHEDULED_TASK_ERROR_CODE_MAX_CHARS,
            ));
        } else if request.clear_error_code {
            run.error_code = None;
        }
        if let Some(error_message) = request.error_message {
            run.error_message = Some(truncate_chars(
                &error_message,
                SCHEDULED_TASK_ERROR_MESSAGE_MAX_CHARS,
            ));
        } else if request.clear_error_message {
            run.error_message = None;
        }
        if let Some(diagnostics) = request.redacted_diagnostics {
            run.redacted_diagnostics = bound_scheduled_task_diagnostics(diagnostics);
        }
        run.updated_at_ms = unix_timestamp_ms();
        update_automation_run(conn, &run)?;
        Ok(run)
    }

    pub fn list_runs(
        conn: &Connection,
        request: AutomationRunListRequest,
    ) -> VibexResult<Vec<AutomationRun>> {
        let graph_id = request.graph_id.as_ref().map(AutomationGraphId::as_str);
        let status = request.status.as_ref().map(enum_to_db).transpose()?;
        let limit = bounded_limit(request.limit);
        let mut stmt = conn
            .prepare(
                "
                SELECT automation_run_id, automation_graph_id, status, trigger,
                    scheduled_task_id, session_id, started_at_ms, ended_at_ms,
                    error_code, error_message, redacted_diagnostics_json,
                    created_at_ms, updated_at_ms
                FROM automation_graph_runs
                WHERE (?1 IS NULL OR automation_graph_id = ?1)
                    AND (?2 IS NULL OR status = ?2)
                ORDER BY created_at_ms DESC, automation_run_id DESC
                LIMIT ?3
                ",
            )
            .map_err(storage_err(
                "automation_run_list_failed",
                "failed to list automation graph runs",
            ))?;
        let rows = stmt
            .query_map(params![graph_id, status, limit], map_automation_run)
            .map_err(storage_err(
                "automation_run_list_failed",
                "failed to list automation graph runs",
            ))?;
        collect_rows(
            rows,
            "automation_run_decode_failed",
            "failed to decode automation graph run row",
        )
    }

    pub fn create_run_step(
        conn: &Connection,
        request: AutomationRunStepCreateRequest,
    ) -> VibexResult<AutomationRunStep> {
        let now = unix_timestamp_ms();
        let step = AutomationRunStep {
            id: AutomationRunStepId::new(),
            run_id: request.run_id,
            node_id: request.node_id,
            status: request.status,
            session_id: request.session_id,
            permission_request_id: request.permission_request_id,
            started_at_ms: request.started_at_ms,
            ended_at_ms: request.ended_at_ms,
            error_code: request
                .error_code
                .map(|value| truncate_chars(&value, SCHEDULED_TASK_ERROR_CODE_MAX_CHARS)),
            error_message: request
                .error_message
                .map(|value| truncate_chars(&value, SCHEDULED_TASK_ERROR_MESSAGE_MAX_CHARS)),
            redacted_diagnostics: bound_scheduled_task_diagnostics(request.redacted_diagnostics),
            created_at_ms: now,
            updated_at_ms: now,
        };
        insert_automation_run_step(conn, &step)?;
        Ok(step)
    }

    pub fn get_run_step(
        conn: &Connection,
        step_id: &AutomationRunStepId,
    ) -> VibexResult<Option<AutomationRunStep>> {
        get_automation_run_step(conn, step_id)
    }

    pub fn update_run_step(
        conn: &Connection,
        request: AutomationRunStepUpdateRequest,
    ) -> VibexResult<AutomationRunStep> {
        let mut step = require_automation_run_step(conn, &request.id)?;
        if let Some(status) = request.status {
            step.status = status;
        }
        if let Some(session_id) = request.session_id {
            step.session_id = Some(session_id);
        } else if request.clear_session_id {
            step.session_id = None;
        }
        if let Some(permission_request_id) = request.permission_request_id {
            step.permission_request_id = Some(permission_request_id);
        } else if request.clear_permission_request_id {
            step.permission_request_id = None;
        }
        if let Some(started_at_ms) = request.started_at_ms {
            step.started_at_ms = Some(started_at_ms);
        } else if request.clear_started_at_ms {
            step.started_at_ms = None;
        }
        if let Some(ended_at_ms) = request.ended_at_ms {
            step.ended_at_ms = Some(ended_at_ms);
        } else if request.clear_ended_at_ms {
            step.ended_at_ms = None;
        }
        if let Some(error_code) = request.error_code {
            step.error_code = Some(truncate_chars(
                &error_code,
                SCHEDULED_TASK_ERROR_CODE_MAX_CHARS,
            ));
        } else if request.clear_error_code {
            step.error_code = None;
        }
        if let Some(error_message) = request.error_message {
            step.error_message = Some(truncate_chars(
                &error_message,
                SCHEDULED_TASK_ERROR_MESSAGE_MAX_CHARS,
            ));
        } else if request.clear_error_message {
            step.error_message = None;
        }
        if let Some(diagnostics) = request.redacted_diagnostics {
            step.redacted_diagnostics = bound_scheduled_task_diagnostics(diagnostics);
        }
        step.updated_at_ms = unix_timestamp_ms();
        update_automation_run_step(conn, &step)?;
        Ok(step)
    }

    pub fn list_run_steps(
        conn: &Connection,
        request: AutomationRunStepListRequest,
    ) -> VibexResult<Vec<AutomationRunStep>> {
        let run_id = request.run_id.as_ref().map(AutomationRunId::as_str);
        let node_id = request.node_id.as_ref().map(AutomationNodeId::as_str);
        let status = request.status.as_ref().map(enum_to_db).transpose()?;
        let limit = bounded_limit(request.limit);
        let mut stmt = conn
            .prepare(
                "
                SELECT automation_run_step_id, automation_run_id, automation_node_id,
                    status, session_id, permission_request_id, started_at_ms, ended_at_ms,
                    error_code, error_message, redacted_diagnostics_json,
                    created_at_ms, updated_at_ms
                FROM automation_graph_run_steps
                WHERE (?1 IS NULL OR automation_run_id = ?1)
                    AND (?2 IS NULL OR automation_node_id = ?2)
                    AND (?3 IS NULL OR status = ?3)
                ORDER BY created_at_ms DESC, automation_run_step_id DESC
                LIMIT ?4
                ",
            )
            .map_err(storage_err(
                "automation_run_step_list_failed",
                "failed to list automation graph run steps",
            ))?;
        let rows = stmt
            .query_map(
                params![run_id, node_id, status, limit],
                map_automation_run_step,
            )
            .map_err(storage_err(
                "automation_run_step_list_failed",
                "failed to list automation graph run steps",
            ))?;
        collect_rows(
            rows,
            "automation_run_step_decode_failed",
            "failed to decode automation graph run step row",
        )
    }
}

impl McpServerRepository {
    pub fn from_create_request(request: McpServerCreateRequest) -> McpServer {
        let now = unix_timestamp_ms();
        let id = McpServerId::new();
        let secret_references = request
            .secret_references
            .into_iter()
            .map(|secret| McpServerSecretReference {
                id: RequestId::new(),
                mcp_server_id: id.clone(),
                secret_kind: secret.secret_kind,
                backend: secret.backend,
                setup_state: secret.setup_state,
                lookup_key: secret.lookup_key,
                display_label: secret.display_label,
                redacted_hint: secret.redacted_hint,
                target: secret.target,
                created_at_ms: now,
                updated_at_ms: now,
            })
            .collect();
        let provider_matrix = request
            .provider_matrix
            .into_iter()
            .map(|entry| McpServerProviderMatrix {
                provider_kind: entry.provider_kind,
                enabled: entry.enabled,
                updated_at_ms: now,
            })
            .collect();
        McpServer {
            id,
            display_name: request.display_name,
            transport_kind: request.transport_kind,
            status: request.status,
            scope_kind: request.scope_kind,
            project_id: request.project_id,
            workspace_id: request.workspace_id,
            command: request.command,
            args: request.args,
            url: request.url,
            description: request.description,
            tags: request.tags,
            secret_references,
            provider_matrix,
            agent_matrix: Vec::new(),
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        }
    }

    pub fn insert(conn: &Connection, server: &McpServer) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO mcp_servers (
                mcp_server_id, display_name, transport_kind, status, scope_kind,
                project_id, workspace_id, command, args_json, url, description,
                tags_json, created_at_ms, updated_at_ms, deleted_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            ",
            params![
                server.id.as_str(),
                server.display_name,
                enum_to_db(&server.transport_kind)?,
                enum_to_db(&server.status)?,
                enum_to_db(&server.scope_kind)?,
                server.project_id.as_ref().map(ProjectId::as_str),
                server.workspace_id.as_ref().map(WorkspaceId::as_str),
                server.command,
                json_to_db(&server.args)?,
                server.url,
                server.description,
                json_to_db(&server.tags)?,
                server.created_at_ms,
                server.updated_at_ms,
                server.deleted_at_ms
            ],
        )
        .map_err(storage_err(
            "mcp_server_insert_failed",
            "failed to insert MCP server",
        ))?;
        Self::replace_secret_references(conn, &server.id, &server.secret_references)?;
        Self::replace_provider_matrix(conn, &server.id, &server.provider_matrix)?;
        Self::replace_agent_matrix(conn, &server.id, &server.agent_matrix)?;
        Ok(())
    }

    pub fn list(conn: &Connection) -> VibexResult<Vec<McpServer>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT mcp_server_id, display_name, transport_kind, status, scope_kind,
                    project_id, workspace_id, command, args_json, url, description,
                    tags_json, created_at_ms, updated_at_ms, deleted_at_ms
                FROM mcp_servers
                WHERE deleted_at_ms IS NULL
                ORDER BY updated_at_ms DESC, display_name ASC
                ",
            )
            .map_err(storage_err(
                "mcp_server_list_failed",
                "failed to list MCP servers",
            ))?;
        let rows = stmt
            .query_map([], map_mcp_server_without_children)
            .map_err(storage_err(
                "mcp_server_list_failed",
                "failed to list MCP servers",
            ))?;
        let mut servers = Vec::new();
        for row in rows {
            let mut server = row.map_err(storage_err(
                "mcp_server_decode_failed",
                "failed to decode MCP server",
            ))?;
            Self::hydrate(conn, &mut server)?;
            servers.push(server);
        }
        Ok(servers)
    }

    pub fn list_enabled_for_provider(
        conn: &Connection,
        provider_kind: ProviderKind,
    ) -> VibexResult<Vec<McpServer>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT s.mcp_server_id, s.display_name, s.transport_kind, s.status, s.scope_kind,
                    s.project_id, s.workspace_id, s.command, s.args_json, s.url, s.description,
                    s.tags_json, s.created_at_ms, s.updated_at_ms, s.deleted_at_ms
                FROM mcp_servers s
                INNER JOIN mcp_server_provider_matrix m
                    ON m.mcp_server_id = s.mcp_server_id
                WHERE s.deleted_at_ms IS NULL
                    AND s.status = ?1
                    AND m.provider_kind = ?2
                    AND m.enabled = 1
                ORDER BY s.display_name ASC
                ",
            )
            .map_err(storage_err(
                "mcp_server_list_failed",
                "failed to list enabled MCP servers",
            ))?;
        let rows = stmt
            .query_map(
                params![
                    enum_to_db(&McpServerStatus::Enabled)?,
                    enum_to_db(&provider_kind)?
                ],
                map_mcp_server_without_children,
            )
            .map_err(storage_err(
                "mcp_server_list_failed",
                "failed to list enabled MCP servers",
            ))?;
        let mut servers = Vec::new();
        for row in rows {
            let mut server = row.map_err(storage_err(
                "mcp_server_decode_failed",
                "failed to decode MCP server",
            ))?;
            Self::hydrate(conn, &mut server)?;
            servers.push(server);
        }
        Ok(servers)
    }

    pub fn list_enabled_for_agent(
        conn: &Connection,
        agent_id: &AgentId,
        legacy_provider_kind: ProviderKind,
    ) -> VibexResult<Vec<McpServer>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT DISTINCT s.mcp_server_id, s.display_name, s.transport_kind, s.status, s.scope_kind,
                    s.project_id, s.workspace_id, s.command, s.args_json, s.url, s.description,
                    s.tags_json, s.created_at_ms, s.updated_at_ms, s.deleted_at_ms
                FROM mcp_servers s
                LEFT JOIN mcp_server_agent_matrix am
                    ON am.mcp_server_id = s.mcp_server_id
                LEFT JOIN mcp_server_provider_matrix pm
                    ON pm.mcp_server_id = s.mcp_server_id
                WHERE s.deleted_at_ms IS NULL
                    AND s.status = ?1
                    AND (
                        (am.agent_id = ?2 AND am.enabled = 1)
                        OR (
                            am.mcp_server_id IS NULL
                            AND pm.provider_kind = ?3
                            AND pm.enabled = 1
                        )
                    )
                ORDER BY s.display_name ASC
                ",
            )
            .map_err(storage_err(
                "mcp_server_list_failed",
                "failed to list MCP servers enabled for agent",
            ))?;
        let rows = stmt
            .query_map(
                params![
                    enum_to_db(&McpServerStatus::Enabled)?,
                    agent_id.as_str(),
                    enum_to_db(&legacy_provider_kind)?
                ],
                map_mcp_server_without_children,
            )
            .map_err(storage_err(
                "mcp_server_list_failed",
                "failed to list MCP servers enabled for agent",
            ))?;
        let mut servers = Vec::new();
        for row in rows {
            let mut server = row.map_err(storage_err(
                "mcp_server_decode_failed",
                "failed to decode MCP server",
            ))?;
            Self::hydrate(conn, &mut server)?;
            servers.push(server);
        }
        Ok(servers)
    }

    pub fn get(conn: &Connection, mcp_server_id: &McpServerId) -> VibexResult<Option<McpServer>> {
        let mut server = conn
            .query_row(
                "
                SELECT mcp_server_id, display_name, transport_kind, status, scope_kind,
                    project_id, workspace_id, command, args_json, url, description,
                    tags_json, created_at_ms, updated_at_ms, deleted_at_ms
                FROM mcp_servers
                WHERE mcp_server_id = ?1 AND deleted_at_ms IS NULL
                ",
                params![mcp_server_id.as_str()],
                map_mcp_server_without_children,
            )
            .optional()
            .map_err(storage_err(
                "mcp_server_lookup_failed",
                "failed to lookup MCP server",
            ))?;
        if let Some(server) = server.as_mut() {
            Self::hydrate(conn, server)?;
        }
        Ok(server)
    }

    pub fn update(conn: &Connection, server: &McpServer) -> VibexResult<()> {
        conn.execute(
            "
            UPDATE mcp_servers
            SET display_name = ?2,
                transport_kind = ?3,
                status = ?4,
                scope_kind = ?5,
                project_id = ?6,
                workspace_id = ?7,
                command = ?8,
                args_json = ?9,
                url = ?10,
                description = ?11,
                tags_json = ?12,
                updated_at_ms = ?13
            WHERE mcp_server_id = ?1 AND deleted_at_ms IS NULL
            ",
            params![
                server.id.as_str(),
                server.display_name,
                enum_to_db(&server.transport_kind)?,
                enum_to_db(&server.status)?,
                enum_to_db(&server.scope_kind)?,
                server.project_id.as_ref().map(ProjectId::as_str),
                server.workspace_id.as_ref().map(WorkspaceId::as_str),
                server.command,
                json_to_db(&server.args)?,
                server.url,
                server.description,
                json_to_db(&server.tags)?,
                server.updated_at_ms
            ],
        )
        .map_err(storage_err(
            "mcp_server_update_failed",
            "failed to update MCP server",
        ))?;
        Self::replace_secret_references(conn, &server.id, &server.secret_references)?;
        Ok(())
    }

    pub fn soft_delete(conn: &Connection, mcp_server_id: &McpServerId) -> VibexResult<()> {
        let now = unix_timestamp_ms();
        conn.execute(
            "
            UPDATE mcp_servers
            SET deleted_at_ms = COALESCE(deleted_at_ms, ?2), updated_at_ms = ?2
            WHERE mcp_server_id = ?1
            ",
            params![mcp_server_id.as_str(), now],
        )
        .map_err(storage_err(
            "mcp_server_delete_failed",
            "failed to delete MCP server",
        ))?;
        Ok(())
    }

    pub fn replace_provider_matrix(
        conn: &Connection,
        mcp_server_id: &McpServerId,
        matrix: &[McpServerProviderMatrix],
    ) -> VibexResult<()> {
        let now = unix_timestamp_ms();
        conn.execute(
            "DELETE FROM mcp_server_provider_matrix WHERE mcp_server_id = ?1",
            params![mcp_server_id.as_str()],
        )
        .map_err(storage_err(
            "mcp_server_matrix_update_failed",
            "failed to replace MCP Provider matrix",
        ))?;
        for entry in matrix {
            conn.execute(
                "
                INSERT OR REPLACE INTO mcp_server_provider_matrix (
                    mcp_server_id, provider_kind, enabled, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5)
                ",
                params![
                    mcp_server_id.as_str(),
                    enum_to_db(&entry.provider_kind)?,
                    if entry.enabled { 1 } else { 0 },
                    now,
                    entry.updated_at_ms
                ],
            )
            .map_err(storage_err(
                "mcp_server_matrix_update_failed",
                "failed to insert MCP Provider matrix entry",
            ))?;
        }
        Ok(())
    }

    pub fn replace_agent_matrix(
        conn: &Connection,
        mcp_server_id: &McpServerId,
        matrix: &[McpServerAgentMatrix],
    ) -> VibexResult<()> {
        let now = unix_timestamp_ms();
        conn.execute(
            "DELETE FROM mcp_server_agent_matrix WHERE mcp_server_id = ?1",
            params![mcp_server_id.as_str()],
        )
        .map_err(storage_err(
            "mcp_server_agent_matrix_update_failed",
            "failed to replace MCP Agent matrix",
        ))?;
        for entry in matrix {
            conn.execute(
                "
                INSERT INTO mcp_server_agent_matrix (
                    mcp_server_id, agent_id, enabled, source_kind, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ",
                params![
                    mcp_server_id.as_str(),
                    entry.agent_id.as_str(),
                    if entry.enabled { 1 } else { 0 },
                    enum_to_db(&entry.source_kind)?,
                    now,
                    entry.updated_at_ms
                ],
            )
            .map_err(storage_err(
                "mcp_server_agent_matrix_update_failed",
                "failed to insert MCP Agent matrix entry",
            ))?;
        }
        Ok(())
    }

    fn replace_secret_references(
        conn: &Connection,
        mcp_server_id: &McpServerId,
        secrets: &[McpServerSecretReference],
    ) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM mcp_server_secret_references WHERE mcp_server_id = ?1",
            params![mcp_server_id.as_str()],
        )
        .map_err(storage_err(
            "mcp_server_secret_update_failed",
            "failed to replace MCP secret references",
        ))?;
        for secret in secrets {
            conn.execute(
                "
                INSERT INTO mcp_server_secret_references (
                    secret_ref_id, mcp_server_id, secret_kind, backend, setup_state,
                    lookup_key, display_label, redacted_hint, target, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ",
                params![
                    secret.id.as_str(),
                    mcp_server_id.as_str(),
                    enum_to_db(&secret.secret_kind)?,
                    enum_to_db(&secret.backend)?,
                    enum_to_db(&secret.setup_state)?,
                    secret.lookup_key,
                    secret.display_label,
                    secret.redacted_hint,
                    enum_to_db(&secret.target)?,
                    secret.created_at_ms,
                    secret.updated_at_ms
                ],
            )
            .map_err(storage_err(
                "mcp_server_secret_insert_failed",
                "failed to insert MCP secret reference",
            ))?;
        }
        Ok(())
    }

    fn hydrate(conn: &Connection, server: &mut McpServer) -> VibexResult<()> {
        server.secret_references = Self::list_secret_references(conn, &server.id)?;
        server.provider_matrix = Self::list_provider_matrix(conn, &server.id)?;
        server.agent_matrix = Self::list_agent_matrix(conn, &server.id)?;
        Ok(())
    }

    fn list_secret_references(
        conn: &Connection,
        mcp_server_id: &McpServerId,
    ) -> VibexResult<Vec<McpServerSecretReference>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT secret_ref_id, mcp_server_id, secret_kind, backend, setup_state,
                    lookup_key, display_label, redacted_hint, target, created_at_ms, updated_at_ms
                FROM mcp_server_secret_references
                WHERE mcp_server_id = ?1
                ORDER BY created_at_ms ASC
                ",
            )
            .map_err(storage_err(
                "mcp_server_secret_list_failed",
                "failed to list MCP secret references",
            ))?;
        let rows = stmt
            .query_map(params![mcp_server_id.as_str()], map_mcp_secret_reference)
            .map_err(storage_err(
                "mcp_server_secret_list_failed",
                "failed to list MCP secret references",
            ))?;
        let mut secrets = Vec::new();
        for row in rows {
            secrets.push(row.map_err(storage_err(
                "mcp_server_secret_decode_failed",
                "failed to decode MCP secret reference",
            ))?);
        }
        Ok(secrets)
    }

    fn list_provider_matrix(
        conn: &Connection,
        mcp_server_id: &McpServerId,
    ) -> VibexResult<Vec<McpServerProviderMatrix>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT provider_kind, enabled, updated_at_ms
                FROM mcp_server_provider_matrix
                WHERE mcp_server_id = ?1
                ORDER BY provider_kind ASC
                ",
            )
            .map_err(storage_err(
                "mcp_server_matrix_list_failed",
                "failed to list MCP Provider matrix",
            ))?;
        let rows = stmt
            .query_map(params![mcp_server_id.as_str()], map_mcp_provider_matrix)
            .map_err(storage_err(
                "mcp_server_matrix_list_failed",
                "failed to list MCP Provider matrix",
            ))?;
        let mut matrix = Vec::new();
        for row in rows {
            matrix.push(row.map_err(storage_err(
                "mcp_server_matrix_decode_failed",
                "failed to decode MCP Provider matrix",
            ))?);
        }
        Ok(matrix)
    }

    pub fn list_agent_matrix(
        conn: &Connection,
        mcp_server_id: &McpServerId,
    ) -> VibexResult<Vec<McpServerAgentMatrix>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT agent_id, enabled, source_kind, updated_at_ms
                FROM mcp_server_agent_matrix
                WHERE mcp_server_id = ?1
                ORDER BY agent_id ASC
                ",
            )
            .map_err(storage_err(
                "mcp_server_agent_matrix_list_failed",
                "failed to list MCP Agent matrix",
            ))?;
        let rows = stmt
            .query_map(params![mcp_server_id.as_str()], map_mcp_agent_matrix)
            .map_err(storage_err(
                "mcp_server_agent_matrix_list_failed",
                "failed to list MCP Agent matrix",
            ))?;
        let mut matrix = Vec::new();
        for row in rows {
            matrix.push(row.map_err(storage_err(
                "mcp_server_agent_matrix_decode_failed",
                "failed to decode MCP Agent matrix",
            ))?);
        }
        Ok(matrix)
    }
}

impl SkillRepository {
    pub fn from_create_request(request: SkillCreateRequest) -> Skill {
        let now = unix_timestamp_ms();
        Skill {
            id: SkillId::new(),
            display_name: request.display_name,
            source_kind: request.source_kind,
            status: request.status,
            scope_kind: request.scope_kind,
            project_id: request.project_id,
            workspace_id: request.workspace_id,
            source_uri: request.source_uri,
            description: request.description,
            tags: request.tags,
            content_preview: request.content_preview,
            provider_matrix: request
                .provider_matrix
                .into_iter()
                .map(|entry| SkillProviderMatrix {
                    provider_kind: entry.provider_kind,
                    enabled: entry.enabled,
                    updated_at_ms: now,
                })
                .collect(),
            agent_matrix: Vec::new(),
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        }
    }

    pub fn insert(conn: &Connection, skill: &Skill) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO skills (
                skill_id, display_name, source_kind, status, scope_kind,
                project_id, workspace_id, source_uri, description, tags_json,
                content_preview, created_at_ms, updated_at_ms, deleted_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            ",
            params![
                skill.id.as_str(),
                skill.display_name,
                enum_to_db(&skill.source_kind)?,
                enum_to_db(&skill.status)?,
                enum_to_db(&skill.scope_kind)?,
                skill.project_id.as_ref().map(ProjectId::as_str),
                skill.workspace_id.as_ref().map(WorkspaceId::as_str),
                skill.source_uri,
                skill.description,
                json_to_db(&skill.tags)?,
                skill.content_preview,
                skill.created_at_ms,
                skill.updated_at_ms,
                skill.deleted_at_ms
            ],
        )
        .map_err(storage_err("skill_insert_failed", "failed to insert Skill"))?;
        Self::replace_provider_matrix(conn, &skill.id, &skill.provider_matrix)?;
        Self::replace_agent_matrix(conn, &skill.id, &skill.agent_matrix)?;
        Ok(())
    }

    pub fn list(conn: &Connection) -> VibexResult<Vec<Skill>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT skill_id, display_name, source_kind, status, scope_kind,
                    project_id, workspace_id, source_uri, description, tags_json,
                    content_preview, created_at_ms, updated_at_ms, deleted_at_ms
                FROM skills
                WHERE deleted_at_ms IS NULL
                ORDER BY updated_at_ms DESC, display_name ASC
                ",
            )
            .map_err(storage_err("skill_list_failed", "failed to list Skills"))?;
        let rows = stmt
            .query_map([], map_skill_without_children)
            .map_err(storage_err("skill_list_failed", "failed to list Skills"))?;
        let mut skills = Vec::new();
        for row in rows {
            let mut skill = row.map_err(storage_err(
                "skill_decode_failed",
                "failed to decode Skill row",
            ))?;
            Self::hydrate(conn, &mut skill)?;
            skills.push(skill);
        }
        Ok(skills)
    }

    pub fn list_enabled_for_provider(
        conn: &Connection,
        provider_kind: ProviderKind,
    ) -> VibexResult<Vec<Skill>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT s.skill_id, s.display_name, s.source_kind, s.status, s.scope_kind,
                    s.project_id, s.workspace_id, s.source_uri, s.description, s.tags_json,
                    s.content_preview, s.created_at_ms, s.updated_at_ms, s.deleted_at_ms
                FROM skills s
                INNER JOIN skill_provider_matrix m ON m.skill_id = s.skill_id
                WHERE s.deleted_at_ms IS NULL
                    AND s.status = ?1
                    AND m.provider_kind = ?2
                    AND m.enabled = 1
                ORDER BY s.display_name ASC
                ",
            )
            .map_err(storage_err(
                "skill_list_failed",
                "failed to list enabled Skills",
            ))?;
        let rows = stmt
            .query_map(
                params![
                    enum_to_db(&SkillStatus::Enabled)?,
                    enum_to_db(&provider_kind)?
                ],
                map_skill_without_children,
            )
            .map_err(storage_err(
                "skill_list_failed",
                "failed to list enabled Skills",
            ))?;
        let mut skills = Vec::new();
        for row in rows {
            let mut skill = row.map_err(storage_err(
                "skill_decode_failed",
                "failed to decode Skill row",
            ))?;
            Self::hydrate(conn, &mut skill)?;
            skills.push(skill);
        }
        Ok(skills)
    }

    pub fn list_enabled_for_agent(
        conn: &Connection,
        agent_id: &AgentId,
        legacy_provider_kind: ProviderKind,
    ) -> VibexResult<Vec<Skill>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT DISTINCT s.skill_id, s.display_name, s.source_kind, s.status, s.scope_kind,
                    s.project_id, s.workspace_id, s.source_uri, s.description, s.tags_json,
                    s.content_preview, s.created_at_ms, s.updated_at_ms, s.deleted_at_ms
                FROM skills s
                LEFT JOIN skill_agent_matrix am ON am.skill_id = s.skill_id
                LEFT JOIN skill_provider_matrix pm ON pm.skill_id = s.skill_id
                WHERE s.deleted_at_ms IS NULL
                    AND s.status = ?1
                    AND (
                        (am.agent_id = ?2 AND am.enabled = 1)
                        OR (
                            am.skill_id IS NULL
                            AND pm.provider_kind = ?3
                            AND pm.enabled = 1
                        )
                    )
                ORDER BY s.display_name ASC
                ",
            )
            .map_err(storage_err(
                "skill_list_failed",
                "failed to list Skills enabled for agent",
            ))?;
        let rows = stmt
            .query_map(
                params![
                    enum_to_db(&SkillStatus::Enabled)?,
                    agent_id.as_str(),
                    enum_to_db(&legacy_provider_kind)?
                ],
                map_skill_without_children,
            )
            .map_err(storage_err(
                "skill_list_failed",
                "failed to list Skills enabled for agent",
            ))?;
        let mut skills = Vec::new();
        for row in rows {
            let mut skill = row.map_err(storage_err(
                "skill_decode_failed",
                "failed to decode Skill row",
            ))?;
            Self::hydrate(conn, &mut skill)?;
            skills.push(skill);
        }
        Ok(skills)
    }

    pub fn get(conn: &Connection, skill_id: &SkillId) -> VibexResult<Option<Skill>> {
        let mut skill = conn
            .query_row(
                "
                SELECT skill_id, display_name, source_kind, status, scope_kind,
                    project_id, workspace_id, source_uri, description, tags_json,
                    content_preview, created_at_ms, updated_at_ms, deleted_at_ms
                FROM skills
                WHERE skill_id = ?1 AND deleted_at_ms IS NULL
                ",
                params![skill_id.as_str()],
                map_skill_without_children,
            )
            .optional()
            .map_err(storage_err("skill_lookup_failed", "failed to lookup Skill"))?;
        if let Some(skill) = skill.as_mut() {
            Self::hydrate(conn, skill)?;
        }
        Ok(skill)
    }

    pub fn update(conn: &Connection, skill: &Skill) -> VibexResult<()> {
        conn.execute(
            "
            UPDATE skills
            SET display_name = ?2,
                source_kind = ?3,
                status = ?4,
                scope_kind = ?5,
                project_id = ?6,
                workspace_id = ?7,
                source_uri = ?8,
                description = ?9,
                tags_json = ?10,
                content_preview = ?11,
                updated_at_ms = ?12
            WHERE skill_id = ?1 AND deleted_at_ms IS NULL
            ",
            params![
                skill.id.as_str(),
                skill.display_name,
                enum_to_db(&skill.source_kind)?,
                enum_to_db(&skill.status)?,
                enum_to_db(&skill.scope_kind)?,
                skill.project_id.as_ref().map(ProjectId::as_str),
                skill.workspace_id.as_ref().map(WorkspaceId::as_str),
                skill.source_uri,
                skill.description,
                json_to_db(&skill.tags)?,
                skill.content_preview,
                skill.updated_at_ms
            ],
        )
        .map_err(storage_err("skill_update_failed", "failed to update Skill"))?;
        Ok(())
    }

    pub fn soft_delete(conn: &Connection, skill_id: &SkillId) -> VibexResult<()> {
        let now = unix_timestamp_ms();
        conn.execute(
            "
            UPDATE skills
            SET deleted_at_ms = COALESCE(deleted_at_ms, ?2), updated_at_ms = ?2
            WHERE skill_id = ?1
            ",
            params![skill_id.as_str(), now],
        )
        .map_err(storage_err("skill_delete_failed", "failed to delete Skill"))?;
        Ok(())
    }

    pub fn replace_provider_matrix(
        conn: &Connection,
        skill_id: &SkillId,
        matrix: &[SkillProviderMatrix],
    ) -> VibexResult<()> {
        let now = unix_timestamp_ms();
        conn.execute(
            "DELETE FROM skill_provider_matrix WHERE skill_id = ?1",
            params![skill_id.as_str()],
        )
        .map_err(storage_err(
            "skill_matrix_update_failed",
            "failed to replace Skill Provider matrix",
        ))?;
        for entry in matrix {
            conn.execute(
                "
                INSERT INTO skill_provider_matrix (
                    skill_id, provider_kind, enabled, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5)
                ",
                params![
                    skill_id.as_str(),
                    enum_to_db(&entry.provider_kind)?,
                    if entry.enabled { 1 } else { 0 },
                    now,
                    entry.updated_at_ms
                ],
            )
            .map_err(storage_err(
                "skill_matrix_update_failed",
                "failed to insert Skill Provider matrix entry",
            ))?;
        }
        Ok(())
    }

    pub fn replace_agent_matrix(
        conn: &Connection,
        skill_id: &SkillId,
        matrix: &[SkillAgentMatrix],
    ) -> VibexResult<()> {
        let now = unix_timestamp_ms();
        conn.execute(
            "DELETE FROM skill_agent_matrix WHERE skill_id = ?1",
            params![skill_id.as_str()],
        )
        .map_err(storage_err(
            "skill_agent_matrix_update_failed",
            "failed to replace Skill Agent matrix",
        ))?;
        for entry in matrix {
            conn.execute(
                "
                INSERT INTO skill_agent_matrix (
                    skill_id, agent_id, enabled, source_kind, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ",
                params![
                    skill_id.as_str(),
                    entry.agent_id.as_str(),
                    if entry.enabled { 1 } else { 0 },
                    enum_to_db(&entry.source_kind)?,
                    now,
                    entry.updated_at_ms
                ],
            )
            .map_err(storage_err(
                "skill_agent_matrix_update_failed",
                "failed to insert Skill Agent matrix entry",
            ))?;
        }
        Ok(())
    }

    fn hydrate(conn: &Connection, skill: &mut Skill) -> VibexResult<()> {
        skill.provider_matrix = Self::list_provider_matrix(conn, &skill.id)?;
        skill.agent_matrix = Self::list_agent_matrix(conn, &skill.id)?;
        Ok(())
    }

    fn list_provider_matrix(
        conn: &Connection,
        skill_id: &SkillId,
    ) -> VibexResult<Vec<SkillProviderMatrix>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT provider_kind, enabled, updated_at_ms
                FROM skill_provider_matrix
                WHERE skill_id = ?1
                ORDER BY provider_kind ASC
                ",
            )
            .map_err(storage_err(
                "skill_matrix_list_failed",
                "failed to list Skill Provider matrix",
            ))?;
        let rows = stmt
            .query_map(params![skill_id.as_str()], map_skill_provider_matrix)
            .map_err(storage_err(
                "skill_matrix_list_failed",
                "failed to list Skill Provider matrix",
            ))?;
        let mut matrix = Vec::new();
        for row in rows {
            matrix.push(row.map_err(storage_err(
                "skill_matrix_decode_failed",
                "failed to decode Skill Provider matrix",
            ))?);
        }
        Ok(matrix)
    }

    pub fn list_agent_matrix(
        conn: &Connection,
        skill_id: &SkillId,
    ) -> VibexResult<Vec<SkillAgentMatrix>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT agent_id, enabled, source_kind, updated_at_ms
                FROM skill_agent_matrix
                WHERE skill_id = ?1
                ORDER BY agent_id ASC
                ",
            )
            .map_err(storage_err(
                "skill_agent_matrix_list_failed",
                "failed to list Skill Agent matrix",
            ))?;
        let rows = stmt
            .query_map(params![skill_id.as_str()], map_skill_agent_matrix)
            .map_err(storage_err(
                "skill_agent_matrix_list_failed",
                "failed to list Skill Agent matrix",
            ))?;
        let mut matrix = Vec::new();
        for row in rows {
            matrix.push(row.map_err(storage_err(
                "skill_agent_matrix_decode_failed",
                "failed to decode Skill Agent matrix",
            ))?);
        }
        Ok(matrix)
    }
}

impl PromptRepository {
    pub fn from_create_request(request: PromptCreateRequest) -> Prompt {
        let now = unix_timestamp_ms();
        Prompt {
            id: PromptId::new(),
            display_name: request.display_name,
            kind: request.kind,
            status: request.status,
            scope_kind: request.scope_kind,
            project_id: request.project_id,
            workspace_id: request.workspace_id,
            body: request.body,
            description: request.description,
            tags: request.tags,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        }
    }

    pub fn insert(conn: &Connection, prompt: &Prompt) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO prompts (
                prompt_id, display_name, kind, status, scope_kind, project_id,
                workspace_id, body, description, tags_json, created_at_ms,
                updated_at_ms, deleted_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ",
            params![
                prompt.id.as_str(),
                prompt.display_name,
                enum_to_db(&prompt.kind)?,
                enum_to_db(&prompt.status)?,
                enum_to_db(&prompt.scope_kind)?,
                prompt.project_id.as_ref().map(ProjectId::as_str),
                prompt.workspace_id.as_ref().map(WorkspaceId::as_str),
                prompt.body,
                prompt.description,
                json_to_db(&prompt.tags)?,
                prompt.created_at_ms,
                prompt.updated_at_ms,
                prompt.deleted_at_ms
            ],
        )
        .map_err(storage_err(
            "prompt_insert_failed",
            "failed to insert Prompt",
        ))?;
        Ok(())
    }

    pub fn list(conn: &Connection) -> VibexResult<Vec<Prompt>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT prompt_id, display_name, kind, status, scope_kind,
                    project_id, workspace_id, body, description, tags_json,
                    created_at_ms, updated_at_ms, deleted_at_ms
                FROM prompts
                WHERE deleted_at_ms IS NULL
                ORDER BY updated_at_ms DESC, display_name ASC
                ",
            )
            .map_err(storage_err("prompt_list_failed", "failed to list Prompts"))?;
        let rows = stmt
            .query_map([], map_prompt)
            .map_err(storage_err("prompt_list_failed", "failed to list Prompts"))?;
        collect_rows(rows, "prompt_decode_failed", "failed to decode Prompt row")
    }

    pub fn list_enabled(conn: &Connection) -> VibexResult<Vec<Prompt>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT prompt_id, display_name, kind, status, scope_kind,
                    project_id, workspace_id, body, description, tags_json,
                    created_at_ms, updated_at_ms, deleted_at_ms
                FROM prompts
                WHERE deleted_at_ms IS NULL AND status = ?1
                ORDER BY display_name ASC
                ",
            )
            .map_err(storage_err(
                "prompt_list_failed",
                "failed to list enabled Prompts",
            ))?;
        let rows = stmt
            .query_map(params![enum_to_db(&PromptStatus::Enabled)?], map_prompt)
            .map_err(storage_err(
                "prompt_list_failed",
                "failed to list enabled Prompts",
            ))?;
        collect_rows(rows, "prompt_decode_failed", "failed to decode Prompt row")
    }

    pub fn get(conn: &Connection, prompt_id: &PromptId) -> VibexResult<Option<Prompt>> {
        conn.query_row(
            "
            SELECT prompt_id, display_name, kind, status, scope_kind,
                project_id, workspace_id, body, description, tags_json,
                created_at_ms, updated_at_ms, deleted_at_ms
            FROM prompts
            WHERE prompt_id = ?1 AND deleted_at_ms IS NULL
            ",
            params![prompt_id.as_str()],
            map_prompt,
        )
        .optional()
        .map_err(storage_err(
            "prompt_lookup_failed",
            "failed to lookup Prompt",
        ))
    }

    pub fn update(conn: &Connection, prompt: &Prompt) -> VibexResult<()> {
        conn.execute(
            "
            UPDATE prompts
            SET display_name = ?2,
                kind = ?3,
                status = ?4,
                scope_kind = ?5,
                project_id = ?6,
                workspace_id = ?7,
                body = ?8,
                description = ?9,
                tags_json = ?10,
                updated_at_ms = ?11
            WHERE prompt_id = ?1 AND deleted_at_ms IS NULL
            ",
            params![
                prompt.id.as_str(),
                prompt.display_name,
                enum_to_db(&prompt.kind)?,
                enum_to_db(&prompt.status)?,
                enum_to_db(&prompt.scope_kind)?,
                prompt.project_id.as_ref().map(ProjectId::as_str),
                prompt.workspace_id.as_ref().map(WorkspaceId::as_str),
                prompt.body,
                prompt.description,
                json_to_db(&prompt.tags)?,
                prompt.updated_at_ms
            ],
        )
        .map_err(storage_err(
            "prompt_update_failed",
            "failed to update Prompt",
        ))?;
        Ok(())
    }

    pub fn soft_delete(conn: &Connection, prompt_id: &PromptId) -> VibexResult<()> {
        let now = unix_timestamp_ms();
        conn.execute(
            "
            UPDATE prompts
            SET deleted_at_ms = COALESCE(deleted_at_ms, ?2), updated_at_ms = ?2
            WHERE prompt_id = ?1
            ",
            params![prompt_id.as_str(), now],
        )
        .map_err(storage_err(
            "prompt_delete_failed",
            "failed to delete Prompt",
        ))?;
        Ok(())
    }
}

impl HookRepository {
    pub fn from_create_request(request: HookCreateRequest) -> Hook {
        let now = unix_timestamp_ms();
        Hook {
            id: HookId::new(),
            display_name: request.display_name,
            provider_kind: request.provider_kind,
            event_kind: request.event_kind,
            status: request.status,
            install_state: HookInstallState::NotInstalled,
            command_preview: request.command_preview,
            managed_marker: request
                .managed_marker
                .unwrap_or_else(|| "VIBEX-MANAGED-HOOK".to_string()),
            description: request.description,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        }
    }

    pub fn insert(conn: &Connection, hook: &Hook) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO hooks (
                hook_id, display_name, provider_kind, event_kind, status,
                install_state, command_preview, managed_marker, description,
                created_at_ms, updated_at_ms, deleted_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ",
            params![
                hook.id.as_str(),
                hook.display_name,
                enum_to_db(&hook.provider_kind)?,
                enum_to_db(&hook.event_kind)?,
                enum_to_db(&hook.status)?,
                enum_to_db(&hook.install_state)?,
                hook.command_preview,
                hook.managed_marker,
                hook.description,
                hook.created_at_ms,
                hook.updated_at_ms,
                hook.deleted_at_ms
            ],
        )
        .map_err(storage_err("hook_insert_failed", "failed to insert Hook"))?;
        Ok(())
    }

    pub fn list(conn: &Connection) -> VibexResult<Vec<Hook>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT hook_id, display_name, provider_kind, event_kind, status,
                    install_state, command_preview, managed_marker, description,
                    created_at_ms, updated_at_ms, deleted_at_ms
                FROM hooks
                WHERE deleted_at_ms IS NULL
                ORDER BY updated_at_ms DESC, display_name ASC
                ",
            )
            .map_err(storage_err("hook_list_failed", "failed to list Hooks"))?;
        let rows = stmt
            .query_map([], map_hook)
            .map_err(storage_err("hook_list_failed", "failed to list Hooks"))?;
        collect_rows(rows, "hook_decode_failed", "failed to decode Hook row")
    }

    pub fn get(conn: &Connection, hook_id: &HookId) -> VibexResult<Option<Hook>> {
        conn.query_row(
            "
            SELECT hook_id, display_name, provider_kind, event_kind, status,
                install_state, command_preview, managed_marker, description,
                created_at_ms, updated_at_ms, deleted_at_ms
            FROM hooks
            WHERE hook_id = ?1 AND deleted_at_ms IS NULL
            ",
            params![hook_id.as_str()],
            map_hook,
        )
        .optional()
        .map_err(storage_err("hook_lookup_failed", "failed to lookup Hook"))
    }

    pub fn update(conn: &Connection, hook: &Hook) -> VibexResult<()> {
        conn.execute(
            "
            UPDATE hooks
            SET display_name = ?2,
                provider_kind = ?3,
                event_kind = ?4,
                status = ?5,
                install_state = ?6,
                command_preview = ?7,
                managed_marker = ?8,
                description = ?9,
                updated_at_ms = ?10
            WHERE hook_id = ?1 AND deleted_at_ms IS NULL
            ",
            params![
                hook.id.as_str(),
                hook.display_name,
                enum_to_db(&hook.provider_kind)?,
                enum_to_db(&hook.event_kind)?,
                enum_to_db(&hook.status)?,
                enum_to_db(&hook.install_state)?,
                hook.command_preview,
                hook.managed_marker,
                hook.description,
                hook.updated_at_ms
            ],
        )
        .map_err(storage_err("hook_update_failed", "failed to update Hook"))?;
        Ok(())
    }

    pub fn soft_delete(conn: &Connection, hook_id: &HookId) -> VibexResult<()> {
        let now = unix_timestamp_ms();
        conn.execute(
            "
            UPDATE hooks
            SET deleted_at_ms = COALESCE(deleted_at_ms, ?2), updated_at_ms = ?2
            WHERE hook_id = ?1
            ",
            params![hook_id.as_str(), now],
        )
        .map_err(storage_err("hook_delete_failed", "failed to delete Hook"))?;
        Ok(())
    }

    pub fn insert_install_preview(
        conn: &Connection,
        preview: &HookInstallPreview,
    ) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO hook_install_previews (
                preview_id, hook_id, target_path, marker, redacted_preview, created_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
            params![
                preview.preview_id.as_str(),
                preview.hook_id.as_str(),
                preview.target_path,
                preview.marker,
                preview.redacted_preview,
                preview.created_at_ms
            ],
        )
        .map_err(storage_err(
            "hook_install_preview_failed",
            "failed to persist Hook install preview",
        ))?;
        Ok(())
    }
}

impl TimelineRepository {
    pub fn latest_sequence(conn: &Connection, session_id: &VibexSessionId) -> VibexResult<i64> {
        conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0)
             FROM agent_timeline_items WHERE session_id = ?1",
            params![session_id.as_str()],
            |row| row.get(0),
        )
        .map_err(storage_err(
            "timeline_latest_sequence_failed",
            "failed to load the latest timeline sequence",
        ))
    }

    pub fn insert_session_and_append_many(
        conn: &mut Connection,
        session: &AgentSession,
        items: &[TimelineAppend],
    ) -> VibexResult<Vec<TimelineItem>> {
        let tx = conn.transaction().map_err(storage_err(
            "session_import_transaction_failed",
            "failed to start session import transaction",
        ))?;
        SessionRepository::insert(&tx, session)?;

        let mut appended = Vec::with_capacity(items.len());
        for item in items {
            appended.push(append_timeline_in_transaction(
                &tx,
                &session.id,
                item.source,
                item.payload.clone(),
                item.timestamp_ms,
                item.correlation_id.as_ref(),
                item.provider_correlation_id.as_deref(),
                item.redaction_state,
                item.execution_attribution.as_ref(),
            )?);
        }

        tx.commit().map_err(storage_err(
            "session_import_transaction_commit_failed",
            "failed to commit session import transaction",
        ))?;
        Ok(appended)
    }

    pub fn append(
        conn: &mut Connection,
        session_id: &VibexSessionId,
        source: TimelineSource,
        payload: TimelinePayload,
        correlation_id: Option<&vibex_core::CorrelationId>,
        provider_correlation_id: Option<&str>,
        redaction_state: TimelineRedactionState,
    ) -> VibexResult<TimelineItem> {
        Self::append_with_attribution(
            conn,
            session_id,
            source,
            payload,
            correlation_id,
            provider_correlation_id,
            redaction_state,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn append_with_attribution(
        conn: &mut Connection,
        session_id: &VibexSessionId,
        source: TimelineSource,
        payload: TimelinePayload,
        correlation_id: Option<&vibex_core::CorrelationId>,
        provider_correlation_id: Option<&str>,
        redaction_state: TimelineRedactionState,
        execution_attribution: Option<&TurnExecutionAttribution>,
    ) -> VibexResult<TimelineItem> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_err(
                "timeline_transaction_failed",
                "failed to start timeline transaction",
            ))?;
        let item = append_timeline_in_transaction(
            &tx,
            session_id,
            source,
            payload,
            None,
            correlation_id,
            provider_correlation_id,
            redaction_state,
            execution_attribution,
        )?;
        tx.commit().map_err(storage_err(
            "timeline_transaction_commit_failed",
            "failed to commit timeline transaction",
        ))?;
        Ok(item)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_by_provider_correlation(
        conn: &mut Connection,
        session_id: &VibexSessionId,
        source: TimelineSource,
        payload: TimelinePayload,
        provider_correlation_id: &str,
        after_sequence: i64,
        redaction_state: TimelineRedactionState,
        execution_attribution: Option<&TurnExecutionAttribution>,
    ) -> VibexResult<TimelineItem> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_err(
                "timeline_transaction_failed",
                "failed to start timeline transaction",
            ))?;
        let kind = payload.kind();
        let existing = tx
            .query_row(
                "
                SELECT session_id, sequence, timeline_item_id, kind, source, timestamp_ms,
                    correlation_id, provider_correlation_id, payload_json, redaction_state,
                    execution_attribution_json
                FROM agent_timeline_items
                WHERE session_id = ?1
                    AND provider_correlation_id = ?2
                    AND kind = ?3
                    AND source = ?4
                    AND sequence > ?5
                ORDER BY sequence DESC
                LIMIT 1
                ",
                params![
                    session_id.as_str(),
                    provider_correlation_id,
                    enum_to_db(&kind)?,
                    enum_to_db(&source)?,
                    after_sequence
                ],
                |row| {
                    let item = map_timeline_item(row)?;
                    let execution_attribution = row
                        .get::<_, Option<String>>(10)?
                        .map(json_from_db_sql)
                        .transpose()?;
                    Ok((item, execution_attribution))
                },
            )
            .optional()
            .map_err(storage_err(
                "timeline_provider_correlation_lookup_failed",
                "failed to lookup timeline item by provider correlation id",
            ))?;

        let item = if let Some((mut item, stored_attribution)) = existing {
            if stored_attribution.as_ref() != execution_attribution {
                return Err(VibexError::conflict(
                    "turn_execution_attribution_conflict",
                    "provider event attribution does not match the existing timeline item",
                ));
            }
            item.timestamp_ms = unix_timestamp_ms();
            item.payload = payload;
            item.redaction_state = redaction_state;
            tx.execute(
                "
                UPDATE agent_timeline_items
                SET timestamp_ms = ?3,
                    payload_json = ?4,
                    redaction_state = ?5
                WHERE session_id = ?1 AND sequence = ?2
                ",
                params![
                    session_id.as_str(),
                    item.sequence,
                    item.timestamp_ms,
                    json_to_db(&item.payload)?,
                    enum_to_db(&item.redaction_state)?
                ],
            )
            .map_err(storage_err(
                "timeline_provider_correlation_update_failed",
                "failed to update timeline item by provider correlation id",
            ))?;
            item
        } else {
            append_timeline_in_transaction(
                &tx,
                session_id,
                source,
                payload,
                None,
                None,
                Some(provider_correlation_id),
                redaction_state,
                execution_attribution,
            )?
        };

        tx.commit().map_err(storage_err(
            "timeline_transaction_commit_failed",
            "failed to commit timeline transaction",
        ))?;
        Ok(item)
    }

    pub fn fetch_after(
        conn: &Connection,
        session_id: &VibexSessionId,
        after_sequence: Option<i64>,
        limit: u32,
    ) -> VibexResult<TimelinePage> {
        let limit = limit.clamp(1, 500) as i64;
        let overfetch = limit + 1;
        let mut items = if let Some(after_sequence) = after_sequence {
            let mut stmt = conn
                .prepare(
                    "
                    SELECT session_id, sequence, timeline_item_id, kind, source, timestamp_ms,
                        correlation_id, provider_correlation_id, payload_json, redaction_state,
                        execution_attribution_json
                    FROM agent_timeline_items
                    WHERE session_id = ?1 AND sequence > ?2
                    ORDER BY sequence ASC
                    LIMIT ?3
                    ",
                )
                .map_err(storage_err(
                    "timeline_fetch_failed",
                    "failed to prepare timeline fetch",
                ))?;
            let rows = stmt
                .query_map(
                    params![session_id.as_str(), after_sequence, overfetch],
                    map_timeline_item,
                )
                .map_err(storage_err(
                    "timeline_fetch_failed",
                    "failed to query timeline items",
                ))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(storage_err(
                    "timeline_decode_failed",
                    "failed to decode timeline row",
                ))?);
            }
            out
        } else {
            let mut stmt = conn
                .prepare(
                    "
                    SELECT session_id, sequence, timeline_item_id, kind, source, timestamp_ms,
                        correlation_id, provider_correlation_id, payload_json, redaction_state,
                        execution_attribution_json
                    FROM agent_timeline_items
                    WHERE session_id = ?1
                    ORDER BY sequence DESC
                    LIMIT ?2
                    ",
                )
                .map_err(storage_err(
                    "timeline_fetch_failed",
                    "failed to prepare timeline fetch",
                ))?;
            let rows = stmt
                .query_map(params![session_id.as_str(), overfetch], map_timeline_item)
                .map_err(storage_err(
                    "timeline_fetch_failed",
                    "failed to query timeline items",
                ))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(storage_err(
                    "timeline_decode_failed",
                    "failed to decode timeline row",
                ))?);
            }
            out.reverse();
            out
        };

        let has_newer = items.len() as i64 > limit;
        if has_newer {
            if after_sequence.is_some() {
                items.truncate(limit as usize);
            } else {
                items.remove(0);
            }
        }

        let start_sequence = items.first().map(|item| item.sequence);
        let end_sequence = items.last().map(|item| item.sequence);
        let has_older = if let Some(start_sequence) = start_sequence {
            conn.query_row(
                "
                SELECT EXISTS(
                    SELECT 1 FROM agent_timeline_items
                    WHERE session_id = ?1 AND sequence < ?2
                )
                ",
                params![session_id.as_str(), start_sequence],
                |row| row.get::<_, bool>(0),
            )
            .map_err(storage_err(
                "timeline_older_probe_failed",
                "failed to inspect older timeline items",
            ))?
        } else {
            false
        };

        let has_newer = if let Some(end_sequence) = end_sequence {
            conn.query_row(
                "
                SELECT EXISTS(
                    SELECT 1 FROM agent_timeline_items
                    WHERE session_id = ?1 AND sequence > ?2
                )
                ",
                params![session_id.as_str(), end_sequence],
                |row| row.get::<_, bool>(0),
            )
            .map_err(storage_err(
                "timeline_newer_probe_failed",
                "failed to inspect newer timeline items",
            ))?
        } else {
            false
        };

        Ok(TimelinePage {
            session_id: session_id.clone(),
            items,
            start_sequence,
            end_sequence,
            has_older,
            has_newer,
        })
    }

    pub fn fetch_range(
        conn: &Connection,
        session_id: &VibexSessionId,
        first_sequence: i64,
        last_sequence: i64,
    ) -> VibexResult<Vec<TimelineItem>> {
        if first_sequence <= 0 || last_sequence < first_sequence {
            return Err(VibexError::validation(
                "timeline_range_invalid",
                "timeline sequence range is invalid",
            ));
        }
        let mut stmt = conn
            .prepare(
                "SELECT session_id, sequence, timeline_item_id, kind, source, timestamp_ms,
                        correlation_id, provider_correlation_id, payload_json, redaction_state,
                        execution_attribution_json
                 FROM agent_timeline_items
                 WHERE session_id = ?1 AND sequence BETWEEN ?2 AND ?3
                 ORDER BY sequence ASC",
            )
            .map_err(storage_err(
                "timeline_range_fetch_failed",
                "failed to prepare timeline range fetch",
            ))?;
        let rows = stmt
            .query_map(
                params![session_id.as_str(), first_sequence, last_sequence],
                map_timeline_item,
            )
            .map_err(storage_err(
                "timeline_range_fetch_failed",
                "failed to query timeline range",
            ))?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row.map_err(storage_err(
                "timeline_decode_failed",
                "failed to decode timeline row",
            ))?);
        }
        Ok(items)
    }
}

impl PermissionRepository {
    pub fn insert_request(conn: &Connection, request: &PermissionRequest) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO permission_requests (
                request_id, session_id, project_id, workspace_id, provider_request_id,
                risk_category, title, details_json, allowed_responses_json, status,
                requested_at_ms, expires_at_ms, response_options_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ",
            params![
                request.id.as_str(),
                request.session_id.as_str(),
                request.project_id.as_ref().map(ProjectId::as_str),
                request.workspace_id.as_ref().map(WorkspaceId::as_str),
                request.provider_request_id,
                enum_to_db(&request.risk_category)?,
                request.title,
                json_to_db(&request.details)?,
                json_to_db(&request.allowed_responses)?,
                enum_to_db(&request.status)?,
                request.requested_at_ms,
                request.expires_at_ms,
                json_to_db(&request.response_options)?,
            ],
        )
        .map_err(storage_err(
            "permission_request_insert_failed",
            "failed to insert permission request",
        ))?;
        Ok(())
    }

    pub fn get_request(
        conn: &Connection,
        request_id: &RequestId,
    ) -> VibexResult<Option<PermissionRequest>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT request_id, session_id, project_id, workspace_id, provider_request_id,
                    risk_category, title, details_json, allowed_responses_json, status,
                    requested_at_ms, expires_at_ms, response_options_json
                FROM permission_requests
                WHERE request_id = ?1
                ",
            )
            .map_err(storage_err(
                "permission_request_get_failed",
                "failed to load permission request",
            ))?;
        stmt.query_row(params![request_id.as_str()], map_permission_request)
            .optional()
            .map_err(storage_err(
                "permission_request_decode_failed",
                "failed to decode permission request",
            ))
    }

    pub fn resolve(conn: &Connection, resolution: &PermissionResolution) -> VibexResult<()> {
        let status = match resolution.response {
            PermissionResponseKind::Approve | PermissionResponseKind::AlwaysAllowForSession => {
                PermissionRequestStatus::Approved
            }
            PermissionResponseKind::Deny => PermissionRequestStatus::Denied,
        };
        conn.execute(
            "
            UPDATE permission_requests
            SET status = ?3, resolution_json = ?4, resolved_at_ms = ?5
            WHERE request_id = ?1 AND session_id = ?2
            ",
            params![
                resolution.request_id.as_str(),
                resolution.session_id.as_str(),
                enum_to_db(&status)?,
                json_to_db(resolution)?,
                resolution.resolved_at_ms
            ],
        )
        .map_err(storage_err(
            "permission_request_resolve_failed",
            "failed to resolve permission request",
        ))?;
        Ok(())
    }

    pub fn pending_for_session(
        conn: &Connection,
        session_id: &VibexSessionId,
    ) -> VibexResult<Vec<PermissionRequest>> {
        let pending = enum_to_db(&PermissionRequestStatus::Pending)?;
        let mut stmt = conn
            .prepare(
                "
                SELECT request_id, session_id, project_id, workspace_id, provider_request_id,
                    risk_category, title, details_json, allowed_responses_json, status,
                    requested_at_ms, expires_at_ms, response_options_json
                FROM permission_requests
                WHERE session_id = ?1 AND status = ?2
                ORDER BY requested_at_ms ASC
                ",
            )
            .map_err(storage_err(
                "permission_request_list_failed",
                "failed to list permission requests",
            ))?;
        let rows = stmt
            .query_map(
                params![session_id.as_str(), pending],
                map_permission_request,
            )
            .map_err(storage_err(
                "permission_request_list_failed",
                "failed to list permission requests",
            ))?;

        let mut requests = Vec::new();
        for row in rows {
            requests.push(row.map_err(storage_err(
                "permission_request_decode_failed",
                "failed to decode permission request",
            ))?);
        }
        Ok(requests)
    }
}

impl ElicitationRepository {
    pub fn insert_request(conn: &Connection, request: &ElicitationRequest) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO elicitation_requests (
                request_id, session_id, status, request_json, requested_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            params![
                request.id.as_str(),
                request.session_id.as_str(),
                enum_to_db(&request.status)?,
                json_to_db(request)?,
                request.requested_at_ms,
            ],
        )
        .map_err(storage_err(
            "elicitation_request_insert_failed",
            "failed to insert elicitation request",
        ))?;
        Ok(())
    }

    pub fn get_request(
        conn: &Connection,
        request_id: &RequestId,
    ) -> VibexResult<Option<ElicitationRequest>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT request_json, status
                FROM elicitation_requests
                WHERE request_id = ?1
                ",
            )
            .map_err(storage_err(
                "elicitation_request_get_failed",
                "failed to load elicitation request",
            ))?;
        stmt.query_row(params![request_id.as_str()], map_elicitation_request)
            .optional()
            .map_err(storage_err(
                "elicitation_request_decode_failed",
                "failed to decode elicitation request",
            ))
    }

    pub fn resolve(conn: &Connection, resolution: &ElicitationResolution) -> VibexResult<()> {
        let status = match resolution.action {
            ElicitationResolutionAction::Accept => ElicitationRequestStatus::Accepted,
            ElicitationResolutionAction::Decline => ElicitationRequestStatus::Declined,
            ElicitationResolutionAction::Cancel => ElicitationRequestStatus::Cancelled,
        };
        let updated = conn
            .execute(
                "
            UPDATE elicitation_requests
            SET status = ?3, resolution_json = ?4, resolved_at_ms = ?5
            WHERE request_id = ?1 AND session_id = ?2 AND status = ?6
            ",
                params![
                    resolution.request_id.as_str(),
                    resolution.session_id.as_str(),
                    enum_to_db(&status)?,
                    json_to_db(resolution)?,
                    resolution.resolved_at_ms,
                    enum_to_db(&ElicitationRequestStatus::Pending)?,
                ],
            )
            .map_err(storage_err(
                "elicitation_request_resolve_failed",
                "failed to resolve elicitation request",
            ))?;
        if updated != 1 {
            return Err(VibexError::conflict(
                "elicitation_request_not_pending",
                "elicitation request is no longer pending",
            )
            .with_diagnostic("requestId", resolution.request_id.as_str()));
        }
        Ok(())
    }

    pub fn pending_for_session(
        conn: &Connection,
        session_id: &VibexSessionId,
    ) -> VibexResult<Vec<ElicitationRequest>> {
        let pending = enum_to_db(&ElicitationRequestStatus::Pending)?;
        let mut stmt = conn
            .prepare(
                "
                SELECT request_json, status
                FROM elicitation_requests
                WHERE session_id = ?1 AND status = ?2
                ORDER BY requested_at_ms ASC
                ",
            )
            .map_err(storage_err(
                "elicitation_request_list_failed",
                "failed to list elicitation requests",
            ))?;
        let rows = stmt
            .query_map(
                params![session_id.as_str(), pending],
                map_elicitation_request,
            )
            .map_err(storage_err(
                "elicitation_request_list_failed",
                "failed to list elicitation requests",
            ))?;
        let mut requests = Vec::new();
        for row in rows {
            requests.push(row.map_err(storage_err(
                "elicitation_request_decode_failed",
                "failed to decode elicitation request",
            ))?);
        }
        Ok(requests)
    }
}

impl AdapterDiagnosticsRepository {
    pub fn insert(conn: &Connection, diagnostic: &AdapterDiagnostic) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO adapter_diagnostics (
                session_id, provider_kind, level, code, message,
                redacted_details_json, timestamp_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            params![
                diagnostic.session_id.as_ref().map(VibexSessionId::as_str),
                enum_to_db(&diagnostic.provider_kind)?,
                enum_to_db(&diagnostic.level)?,
                diagnostic.code,
                diagnostic.message,
                json_to_db(&diagnostic.redacted_details)?,
                diagnostic.timestamp_ms
            ],
        )
        .map_err(storage_err(
            "adapter_diagnostic_insert_failed",
            "failed to insert adapter diagnostic",
        ))?;
        Ok(())
    }
}

impl TerminalSessionRepository {
    pub fn upsert(conn: &Connection, session: &TerminalSession) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO terminal_sessions (
                terminal_id, workspace_id, title, shell, cwd, rows, cols, status,
                created_at_ms, updated_at_ms, closed_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(terminal_id) DO UPDATE SET
                workspace_id = excluded.workspace_id,
                title = excluded.title,
                shell = excluded.shell,
                cwd = excluded.cwd,
                rows = excluded.rows,
                cols = excluded.cols,
                status = excluded.status,
                updated_at_ms = excluded.updated_at_ms,
                closed_at_ms = excluded.closed_at_ms
            ",
            params![
                session.id.as_str(),
                session.workspace_id.as_str(),
                session.title,
                session.shell,
                session.cwd,
                session.rows,
                session.cols,
                enum_to_db(&session.status)?,
                session.created_at_ms,
                session.updated_at_ms,
                session.closed_at_ms
            ],
        )
        .map_err(storage_err(
            "terminal_session_upsert_failed",
            "failed to upsert terminal session",
        ))?;
        Ok(())
    }

    pub fn list(
        conn: &Connection,
        workspace_id: &WorkspaceId,
    ) -> VibexResult<Vec<TerminalSession>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT terminal_id, workspace_id, title, shell, cwd, rows, cols, status,
                    created_at_ms, updated_at_ms, closed_at_ms
                FROM terminal_sessions
                WHERE workspace_id = ?1
                ORDER BY created_at_ms ASC
                ",
            )
            .map_err(storage_err(
                "terminal_session_list_failed",
                "failed to list terminal sessions",
            ))?;
        let rows = stmt
            .query_map(params![workspace_id.as_str()], map_terminal_session)
            .map_err(storage_err(
                "terminal_session_list_failed",
                "failed to list terminal sessions",
            ))?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row.map_err(storage_err(
                "terminal_session_decode_failed",
                "failed to decode terminal session",
            ))?);
        }
        Ok(sessions)
    }

    pub fn get(
        conn: &Connection,
        terminal_id: &TerminalId,
    ) -> VibexResult<Option<TerminalSession>> {
        conn.query_row(
            "
            SELECT terminal_id, workspace_id, title, shell, cwd, rows, cols, status,
                created_at_ms, updated_at_ms, closed_at_ms
            FROM terminal_sessions
            WHERE terminal_id = ?1
            ",
            params![terminal_id.as_str()],
            map_terminal_session,
        )
        .optional()
        .map_err(storage_err(
            "terminal_session_lookup_failed",
            "failed to lookup terminal session",
        ))
    }
}

impl RecentFileRepository {
    pub fn touch(conn: &Connection, workspace_id: &WorkspaceId, path: &str) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO workbench_recent_files (workspace_id, path, last_opened_at_ms)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(workspace_id, path) DO UPDATE SET
                last_opened_at_ms = excluded.last_opened_at_ms
            ",
            params![workspace_id.as_str(), path, unix_timestamp_ms()],
        )
        .map_err(storage_err(
            "recent_file_touch_failed",
            "failed to update recent file",
        ))?;
        Ok(())
    }

    pub fn list(
        conn: &Connection,
        workspace_id: &WorkspaceId,
        limit: u32,
    ) -> VibexResult<Vec<String>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT path
                FROM workbench_recent_files
                WHERE workspace_id = ?1
                ORDER BY last_opened_at_ms DESC
                LIMIT ?2
                ",
            )
            .map_err(storage_err(
                "recent_file_list_failed",
                "failed to list recent files",
            ))?;
        let rows = stmt
            .query_map(params![workspace_id.as_str(), limit.clamp(1, 100)], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err(
                "recent_file_list_failed",
                "failed to list recent files",
            ))?;

        let mut paths = Vec::new();
        for row in rows {
            paths.push(row.map_err(storage_err(
                "recent_file_decode_failed",
                "failed to decode recent file",
            ))?);
        }
        Ok(paths)
    }
}

impl GitSnapshotRepository {
    pub fn upsert(
        conn: &Connection,
        workspace_id: &WorkspaceId,
        branch: Option<&str>,
        short_commit: Option<&str>,
        dirty: bool,
        changed_files: u32,
        captured_at_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO git_snapshots (
                workspace_id, branch, short_commit, dirty, changed_files, captured_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(workspace_id) DO UPDATE SET
                branch = excluded.branch,
                short_commit = excluded.short_commit,
                dirty = excluded.dirty,
                changed_files = excluded.changed_files,
                captured_at_ms = excluded.captured_at_ms
            ",
            params![
                workspace_id.as_str(),
                branch,
                short_commit,
                dirty,
                changed_files,
                captured_at_ms
            ],
        )
        .map_err(storage_err(
            "git_snapshot_upsert_failed",
            "failed to upsert Git snapshot",
        ))?;
        Ok(())
    }
}

impl ManagedWorktreeRepository {
    pub fn insert(conn: &Connection, record: &ManagedWorktreeRecord) -> VibexResult<()> {
        if let Some(identity) = record.worktree_path_identity.as_ref()
            && let Some(existing) = Self::get_by_identity_key(conn, &identity.comparison_key)?
            && existing.worktree_id != record.worktree_id
        {
            return Err(VibexError::conflict(
                "managed_worktree_identity_conflict",
                "canonical worktree identity is already managed",
            ));
        }
        let repository_identity_json = record
            .repository_identity
            .as_ref()
            .map(json_to_db)
            .transpose()?;
        let worktree_identity_json = record
            .worktree_path_identity
            .as_ref()
            .map(json_to_db)
            .transpose()?;
        let diagnostic_json = record.diagnostic.as_ref().map(json_to_db).transpose()?;
        conn.execute(
            "
            INSERT INTO git_managed_worktrees (
                worktree_id, project_id, workspace_id, repo_root, worktree_path,
                repo_identity_key, worktree_identity_key, repository_identity_json,
                worktree_identity_json, canonical_worktree_path, branch, origin_workspace_id,
                base_ref, base_head, target_workspace_id, target_branch, head, status,
                reconciliation_state, diagnostic_json, created_at_ms, updated_at_ms, closed_at_ms
            )
            VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23
            )
            ",
            params![
                record.worktree_id.as_str(),
                record.project_id.as_str(),
                record.workspace_id.as_ref().map(WorkspaceId::as_str),
                record.repo_root,
                record.worktree_path,
                record
                    .repository_identity
                    .as_ref()
                    .map(|identity| identity.comparison_key.as_str()),
                record
                    .worktree_path_identity
                    .as_ref()
                    .map(|identity| identity.comparison_key.as_str()),
                repository_identity_json,
                worktree_identity_json,
                record.worktree_path_identity.as_ref().map(|identity| {
                    identity
                        .canonical_path
                        .as_deref()
                        .unwrap_or(&identity.normalized_path)
                }),
                record.branch,
                record.origin_workspace_id.as_ref().map(WorkspaceId::as_str),
                record.base_ref,
                record.base_head,
                record.target_workspace_id.as_ref().map(WorkspaceId::as_str),
                record.target_branch,
                record.head,
                enum_to_db(&record.status)?,
                enum_to_db(&record.reconciliation_state)?,
                diagnostic_json,
                record.created_at_ms,
                record.updated_at_ms,
                record.closed_at_ms
            ],
        )
        .map_err(storage_err(
            "managed_worktree_insert_failed",
            "failed to insert managed worktree",
        ))?;
        Ok(())
    }

    pub fn upsert(conn: &Connection, record: &ManagedWorktreeRecord) -> VibexResult<()> {
        if let Some(identity) = record.worktree_path_identity.as_ref()
            && let Some(existing) = Self::get_by_identity_key(conn, &identity.comparison_key)?
            && existing.worktree_id != record.worktree_id
        {
            return Err(VibexError::conflict(
                "managed_worktree_identity_conflict",
                "canonical worktree identity is already managed",
            ));
        }
        let repository_identity_json = record
            .repository_identity
            .as_ref()
            .map(json_to_db)
            .transpose()?;
        let worktree_identity_json = record
            .worktree_path_identity
            .as_ref()
            .map(json_to_db)
            .transpose()?;
        let diagnostic_json = record.diagnostic.as_ref().map(json_to_db).transpose()?;
        conn.execute(
            "
            INSERT INTO git_managed_worktrees (
                worktree_id, project_id, workspace_id, repo_root, worktree_path,
                repo_identity_key, worktree_identity_key, repository_identity_json,
                worktree_identity_json, canonical_worktree_path, branch, origin_workspace_id,
                base_ref, base_head, target_workspace_id, target_branch, head, status,
                reconciliation_state, diagnostic_json, created_at_ms, updated_at_ms, closed_at_ms
            )
            VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23
            )
            ON CONFLICT(worktree_path) DO UPDATE SET
                workspace_id = excluded.workspace_id,
                repo_identity_key = excluded.repo_identity_key,
                worktree_identity_key = excluded.worktree_identity_key,
                repository_identity_json = excluded.repository_identity_json,
                worktree_identity_json = excluded.worktree_identity_json,
                canonical_worktree_path = excluded.canonical_worktree_path,
                branch = excluded.branch,
                origin_workspace_id = excluded.origin_workspace_id,
                base_ref = excluded.base_ref,
                base_head = excluded.base_head,
                target_workspace_id = excluded.target_workspace_id,
                target_branch = excluded.target_branch,
                head = excluded.head,
                status = excluded.status,
                reconciliation_state = excluded.reconciliation_state,
                diagnostic_json = excluded.diagnostic_json,
                updated_at_ms = excluded.updated_at_ms,
                closed_at_ms = excluded.closed_at_ms
            ",
            params![
                record.worktree_id.as_str(),
                record.project_id.as_str(),
                record.workspace_id.as_ref().map(WorkspaceId::as_str),
                record.repo_root,
                record.worktree_path,
                record
                    .repository_identity
                    .as_ref()
                    .map(|identity| identity.comparison_key.as_str()),
                record
                    .worktree_path_identity
                    .as_ref()
                    .map(|identity| identity.comparison_key.as_str()),
                repository_identity_json,
                worktree_identity_json,
                record.worktree_path_identity.as_ref().map(|identity| {
                    identity
                        .canonical_path
                        .as_deref()
                        .unwrap_or(&identity.normalized_path)
                }),
                record.branch,
                record.origin_workspace_id.as_ref().map(WorkspaceId::as_str),
                record.base_ref,
                record.base_head,
                record.target_workspace_id.as_ref().map(WorkspaceId::as_str),
                record.target_branch,
                record.head,
                enum_to_db(&record.status)?,
                enum_to_db(&record.reconciliation_state)?,
                diagnostic_json,
                record.created_at_ms,
                record.updated_at_ms,
                record.closed_at_ms
            ],
        )
        .map_err(storage_err(
            "managed_worktree_upsert_failed",
            "failed to upsert managed worktree",
        ))?;
        Ok(())
    }

    pub fn attach_workspace(
        conn: &Connection,
        worktree_path: &str,
        workspace_id: &WorkspaceId,
    ) -> VibexResult<()> {
        conn.execute(
            "
            UPDATE git_managed_worktrees
            SET workspace_id = ?2, updated_at_ms = ?3
            WHERE worktree_path = ?1
            ",
            params![worktree_path, workspace_id.as_str(), unix_timestamp_ms()],
        )
        .map_err(storage_err(
            "managed_worktree_attach_failed",
            "failed to attach worktree workspace",
        ))?;
        Ok(())
    }

    pub fn update_status(
        conn: &Connection,
        worktree_path: &str,
        status: GitManagedWorktreeStatus,
        head: Option<&str>,
        closed_at_ms: Option<i64>,
    ) -> VibexResult<()> {
        conn.execute(
            "
            UPDATE git_managed_worktrees
            SET status = ?2, head = COALESCE(?3, head), updated_at_ms = ?4, closed_at_ms = ?5
            WHERE worktree_path = ?1
            ",
            params![
                worktree_path,
                enum_to_db(&status)?,
                head,
                unix_timestamp_ms(),
                closed_at_ms
            ],
        )
        .map_err(storage_err(
            "managed_worktree_status_update_failed",
            "failed to update managed worktree status",
        ))?;
        Ok(())
    }

    pub fn update_reconciliation(
        conn: &Connection,
        worktree_id: &RequestId,
        state: GitWorktreeReconciliationState,
        diagnostic: Option<&GitWorktreeDiagnostic>,
    ) -> VibexResult<()> {
        let diagnostic_json = diagnostic.map(json_to_db).transpose()?;
        let changed = conn
            .execute(
                "
                UPDATE git_managed_worktrees
                SET reconciliation_state = ?2, diagnostic_json = ?3, updated_at_ms = ?4
                WHERE worktree_id = ?1
                ",
                params![
                    worktree_id.as_str(),
                    enum_to_db(&state)?,
                    diagnostic_json,
                    unix_timestamp_ms()
                ],
            )
            .map_err(storage_err(
                "managed_worktree_reconciliation_update_failed",
                "failed to update managed worktree reconciliation state",
            ))?;
        if changed == 0 {
            return Err(VibexError::storage(
                "managed_worktree_not_found",
                "managed worktree was not found",
            ));
        }
        Ok(())
    }

    pub fn get_by_path(
        conn: &Connection,
        worktree_path: &str,
    ) -> VibexResult<Option<ManagedWorktreeRecord>> {
        conn.query_row(
            "
            SELECT worktree_id, project_id, workspace_id, repo_root, worktree_path,
                repository_identity_json, worktree_identity_json, branch, origin_workspace_id,
                base_ref, base_head, target_workspace_id, target_branch, head, status,
                reconciliation_state, diagnostic_json, created_at_ms, updated_at_ms, closed_at_ms
            FROM git_managed_worktrees
            WHERE worktree_path = ?1
            ",
            params![worktree_path],
            map_managed_worktree,
        )
        .optional()
        .map_err(storage_err(
            "managed_worktree_lookup_failed",
            "failed to lookup managed worktree",
        ))
    }

    pub fn get_by_identity_key(
        conn: &Connection,
        identity_key: &str,
    ) -> VibexResult<Option<ManagedWorktreeRecord>> {
        conn.query_row(
            "
            SELECT worktree_id, project_id, workspace_id, repo_root, worktree_path,
                repository_identity_json, worktree_identity_json, branch, origin_workspace_id,
                base_ref, base_head, target_workspace_id, target_branch, head, status,
                reconciliation_state, diagnostic_json, created_at_ms, updated_at_ms, closed_at_ms
            FROM git_managed_worktrees
            WHERE worktree_identity_key = ?1
            ",
            params![identity_key],
            map_managed_worktree,
        )
        .optional()
        .map_err(storage_err(
            "managed_worktree_identity_lookup_failed",
            "failed to lookup managed worktree identity",
        ))
    }

    pub fn get_by_id(
        conn: &Connection,
        worktree_id: &RequestId,
    ) -> VibexResult<Option<ManagedWorktreeRecord>> {
        conn.query_row(
            "
            SELECT worktree_id, project_id, workspace_id, repo_root, worktree_path,
                repository_identity_json, worktree_identity_json, branch, origin_workspace_id,
                base_ref, base_head, target_workspace_id, target_branch, head, status,
                reconciliation_state, diagnostic_json, created_at_ms, updated_at_ms, closed_at_ms
            FROM git_managed_worktrees
            WHERE worktree_id = ?1
            ",
            params![worktree_id.as_str()],
            map_managed_worktree,
        )
        .optional()
        .map_err(storage_err(
            "managed_worktree_lookup_failed",
            "failed to lookup managed worktree",
        ))
    }

    pub fn list_for_project(
        conn: &Connection,
        project_id: &ProjectId,
    ) -> VibexResult<Vec<ManagedWorktreeRecord>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT worktree_id, project_id, workspace_id, repo_root, worktree_path,
                    repository_identity_json, worktree_identity_json, branch, origin_workspace_id,
                    base_ref, base_head, target_workspace_id, target_branch, head, status,
                    reconciliation_state, diagnostic_json, created_at_ms, updated_at_ms, closed_at_ms
                FROM git_managed_worktrees
                WHERE project_id = ?1
                ORDER BY updated_at_ms DESC
                ",
            )
            .map_err(storage_err(
                "managed_worktree_list_failed",
                "failed to list managed worktrees",
            ))?;
        let rows = stmt
            .query_map(params![project_id.as_str()], map_managed_worktree)
            .map_err(storage_err(
                "managed_worktree_list_failed",
                "failed to list managed worktrees",
            ))?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(storage_err(
                "managed_worktree_decode_failed",
                "failed to decode managed worktree",
            ))?);
        }
        Ok(records)
    }

    pub fn list_all(conn: &Connection) -> VibexResult<Vec<ManagedWorktreeRecord>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT worktree_id, project_id, workspace_id, repo_root, worktree_path,
                    repository_identity_json, worktree_identity_json, branch, origin_workspace_id,
                    base_ref, base_head, target_workspace_id, target_branch, head, status,
                    reconciliation_state, diagnostic_json, created_at_ms, updated_at_ms, closed_at_ms
                FROM git_managed_worktrees
                ORDER BY updated_at_ms DESC
                ",
            )
            .map_err(storage_err(
                "managed_worktree_list_failed",
                "failed to list managed worktrees",
            ))?;
        let rows = stmt
            .query_map([], map_managed_worktree)
            .map_err(storage_err(
                "managed_worktree_list_failed",
                "failed to list managed worktrees",
            ))?;
        collect_rows(
            rows,
            "managed_worktree_decode_failed",
            "failed to decode managed worktree",
        )
    }
}

impl WorktreeReadinessRepository {
    pub fn upsert(conn: &Connection, record: &GitWorktreeReadinessRecord) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO git_worktree_readiness (
                worktree_id, workspace_id, state, source_head, dirty_fingerprint,
                target_workspace_id, target_branch, checks_json, revision, updated_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(worktree_id) DO UPDATE SET
                workspace_id = excluded.workspace_id,
                state = excluded.state,
                source_head = excluded.source_head,
                dirty_fingerprint = excluded.dirty_fingerprint,
                target_workspace_id = excluded.target_workspace_id,
                target_branch = excluded.target_branch,
                checks_json = excluded.checks_json,
                revision = excluded.revision,
                updated_at_ms = excluded.updated_at_ms
            ",
            params![
                record.worktree_id.as_str(),
                record.workspace_id.as_str(),
                enum_to_db(&record.state)?,
                record.source_head,
                record.dirty_fingerprint,
                record.target_workspace_id.as_str(),
                record.target_branch,
                json_to_db(&record.checks)?,
                record.revision,
                record.updated_at_ms,
            ],
        )
        .map_err(storage_err(
            "worktree_readiness_upsert_failed",
            "failed to persist worktree readiness",
        ))?;
        Ok(())
    }

    pub fn get_by_worktree_id(
        conn: &Connection,
        worktree_id: &RequestId,
    ) -> VibexResult<Option<GitWorktreeReadinessRecord>> {
        conn.query_row(
            "
            SELECT worktree_id, workspace_id, state, source_head, dirty_fingerprint,
                target_workspace_id, target_branch, checks_json, revision, updated_at_ms
            FROM git_worktree_readiness
            WHERE worktree_id = ?1
            ",
            params![worktree_id.as_str()],
            map_worktree_readiness,
        )
        .optional()
        .map_err(storage_err(
            "worktree_readiness_lookup_failed",
            "failed to load worktree readiness",
        ))
    }

    pub fn get_by_workspace_id(
        conn: &Connection,
        workspace_id: &WorkspaceId,
    ) -> VibexResult<Option<GitWorktreeReadinessRecord>> {
        conn.query_row(
            "
            SELECT worktree_id, workspace_id, state, source_head, dirty_fingerprint,
                target_workspace_id, target_branch, checks_json, revision, updated_at_ms
            FROM git_worktree_readiness
            WHERE workspace_id = ?1
            ",
            params![workspace_id.as_str()],
            map_worktree_readiness,
        )
        .optional()
        .map_err(storage_err(
            "worktree_readiness_lookup_failed",
            "failed to load worktree readiness",
        ))
    }

    pub fn list_for_project(
        conn: &Connection,
        project_id: &ProjectId,
    ) -> VibexResult<Vec<GitWorktreeReadinessRecord>> {
        let mut statement = conn
            .prepare(
                "
                SELECT readiness.worktree_id, readiness.workspace_id, readiness.state,
                    readiness.source_head, readiness.dirty_fingerprint,
                    readiness.target_workspace_id, readiness.target_branch,
                    readiness.checks_json, readiness.revision, readiness.updated_at_ms
                FROM git_worktree_readiness readiness
                INNER JOIN git_managed_worktrees managed
                    ON managed.worktree_id = readiness.worktree_id
                WHERE managed.project_id = ?1
                ORDER BY readiness.updated_at_ms DESC
                ",
            )
            .map_err(storage_err(
                "worktree_readiness_list_failed",
                "failed to list worktree readiness",
            ))?;
        let rows = statement
            .query_map(params![project_id.as_str()], map_worktree_readiness)
            .map_err(storage_err(
                "worktree_readiness_list_failed",
                "failed to list worktree readiness",
            ))?;
        collect_rows(
            rows,
            "worktree_readiness_decode_failed",
            "failed to decode worktree readiness",
        )
    }
}

impl WorktreeOperationRepository {
    pub fn insert(conn: &Connection, record: &GitWorktreeOperationRecord) -> VibexResult<()> {
        let detail_json = json_to_db(&record.detail)?;
        let diagnostic_json = record
            .detail
            .diagnostic
            .as_ref()
            .map(json_to_db)
            .transpose()?;
        conn.execute(
            "
            INSERT INTO git_worktree_operations (
                operation_id, project_id, source_workspace_id, target_workspace_id,
                operation, status, worktree_path, branch, base_ref, head_before,
                head_after, error, created_at_ms, updated_at_ms, idempotency_key,
                request_fingerprint, checkpoint, detail_json, lease_owner,
                lease_expires_at_ms, attempt, diagnostic_json
            )
            VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22
            )
            ",
            params![
                record.operation_id.as_str(),
                record.project_id.as_str(),
                record.source_workspace_id.as_ref().map(WorkspaceId::as_str),
                record.target_workspace_id.as_ref().map(WorkspaceId::as_str),
                enum_to_db(&record.operation)?,
                enum_to_db(&record.status)?,
                record.worktree_path,
                record.branch,
                record.base_ref,
                record.head_before,
                record.head_after,
                record.error,
                record.created_at_ms,
                record.updated_at_ms,
                record.detail.idempotency_key,
                record.detail.request_fingerprint,
                enum_to_db(&record.detail.checkpoint)?,
                detail_json,
                record.detail.lease_owner,
                record.detail.lease_expires_at_ms,
                i64::from(record.detail.attempt),
                diagnostic_json
            ],
        )
        .map_err(storage_err(
            "worktree_operation_insert_failed",
            "failed to insert worktree operation",
        ))?;
        Ok(())
    }

    pub fn reserve(
        conn: &mut Connection,
        record: &GitWorktreeOperationRecord,
    ) -> VibexResult<GitWorktreeOperationRecord> {
        let idempotency_key = validate_worktree_operation_token(
            record.detail.idempotency_key.as_deref(),
            "worktree_operation_idempotency_key_invalid",
            "worktree operation idempotency key must be non-empty and bounded",
        )?;
        let request_fingerprint = validate_worktree_operation_token(
            record.detail.request_fingerprint.as_deref(),
            "worktree_operation_fingerprint_invalid",
            "worktree operation request fingerprint must be non-empty and bounded",
        )?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_err(
                "worktree_operation_reserve_transaction_failed",
                "failed to reserve worktree operation",
            ))?;
        if let Some(existing) = Self::get_by_idempotency_key(&transaction, idempotency_key)? {
            if existing.detail.request_fingerprint.as_deref() != Some(request_fingerprint)
                || existing.project_id != record.project_id
                || existing.operation != record.operation
            {
                return Err(VibexError::conflict(
                    "worktree_operation_idempotency_conflict",
                    "worktree operation idempotency key belongs to another request",
                ));
            }
            transaction.commit().map_err(storage_err(
                "worktree_operation_reserve_commit_failed",
                "failed to finish worktree operation reservation",
            ))?;
            return Ok(existing);
        }
        Self::insert(&transaction, record)?;
        transaction.commit().map_err(storage_err(
            "worktree_operation_reserve_commit_failed",
            "failed to finish worktree operation reservation",
        ))?;
        Ok(record.clone())
    }

    pub fn try_claim(
        conn: &Connection,
        operation_id: &RequestId,
        lease_owner: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> VibexResult<WorktreeOperationClaimOutcome> {
        validate_worktree_operation_token(
            Some(lease_owner),
            "worktree_operation_lease_owner_invalid",
            "worktree operation lease owner must be non-empty and bounded",
        )?;
        if !(1..=86_400_000).contains(&lease_duration_ms) {
            return Err(VibexError::validation(
                "worktree_operation_lease_duration_invalid",
                "worktree operation lease duration must be positive and at most 24 hours",
            ));
        }
        let existing = Self::get(conn, operation_id)?.ok_or_else(|| {
            VibexError::storage(
                "worktree_operation_not_found",
                "worktree operation was not found",
            )
        })?;
        match existing.status {
            GitWorktreeOperationStatus::Completed => {
                return Ok(WorktreeOperationClaimOutcome::Completed(existing));
            }
            GitWorktreeOperationStatus::NeedsAttention
            | GitWorktreeOperationStatus::NeedsResolution
            | GitWorktreeOperationStatus::Aborted
            | GitWorktreeOperationStatus::Unknown => {
                return Ok(WorktreeOperationClaimOutcome::NeedsAttention(existing));
            }
            GitWorktreeOperationStatus::Pending
            | GitWorktreeOperationStatus::Queued
            | GitWorktreeOperationStatus::Running
            | GitWorktreeOperationStatus::Aborting
            | GitWorktreeOperationStatus::Failed
            | GitWorktreeOperationStatus::Recoverable => {}
        }

        let lease_expires_at_ms = now_ms.saturating_add(lease_duration_ms);
        let mut detail = existing.detail.clone();
        detail.attempt = detail.attempt.saturating_add(1);
        detail.lease_owner = Some(lease_owner.to_string());
        detail.lease_expires_at_ms = Some(lease_expires_at_ms);
        let detail_json = json_to_db(&detail)?;
        let pending = enum_to_db(&GitWorktreeOperationStatus::Pending)?;
        let queued = enum_to_db(&GitWorktreeOperationStatus::Queued)?;
        let running = enum_to_db(&GitWorktreeOperationStatus::Running)?;
        let aborting = enum_to_db(&GitWorktreeOperationStatus::Aborting)?;
        let failed = enum_to_db(&GitWorktreeOperationStatus::Failed)?;
        let recoverable = enum_to_db(&GitWorktreeOperationStatus::Recoverable)?;
        let changed = conn
            .execute(
                "
                UPDATE git_worktree_operations
                SET status = ?2, detail_json = ?3, lease_owner = ?4,
                    lease_expires_at_ms = ?5, attempt = ?6, updated_at_ms = ?7
                WHERE operation_id = ?1
                  AND (
                    status IN (?8, ?9, ?10, ?11, ?12)
                    OR (status = ?2 AND (lease_expires_at_ms IS NULL OR lease_expires_at_ms <= ?13))
                  )
                ",
                params![
                    operation_id.as_str(),
                    running,
                    detail_json,
                    lease_owner,
                    lease_expires_at_ms,
                    i64::from(detail.attempt),
                    now_ms,
                    pending,
                    queued,
                    aborting,
                    failed,
                    recoverable,
                    now_ms
                ],
            )
            .map_err(storage_err(
                "worktree_operation_claim_failed",
                "failed to claim worktree operation",
            ))?;
        let current = Self::get(conn, operation_id)?.ok_or_else(|| {
            VibexError::storage(
                "worktree_operation_missing_after_claim",
                "worktree operation was not found after claim",
            )
        })?;
        if changed == 1 {
            Ok(WorktreeOperationClaimOutcome::Acquired(current))
        } else {
            Ok(WorktreeOperationClaimOutcome::Busy(current))
        }
    }

    pub fn update_checkpoint(
        conn: &Connection,
        operation_id: &RequestId,
        checkpoint: GitWorktreeOperationCheckpoint,
        head_after: Option<&str>,
    ) -> VibexResult<GitWorktreeOperationRecord> {
        let mut record = Self::get(conn, operation_id)?.ok_or_else(|| {
            VibexError::storage(
                "worktree_operation_not_found",
                "worktree operation was not found",
            )
        })?;
        record.detail.checkpoint = checkpoint;
        if let Some(head_after) = head_after {
            record.head_after = Some(head_after.to_string());
        }
        let detail_json = json_to_db(&record.detail)?;
        conn.execute(
            "
            UPDATE git_worktree_operations
            SET checkpoint = ?2, detail_json = ?3,
                head_after = COALESCE(?4, head_after), updated_at_ms = ?5
            WHERE operation_id = ?1
            ",
            params![
                operation_id.as_str(),
                enum_to_db(&checkpoint)?,
                detail_json,
                head_after,
                unix_timestamp_ms()
            ],
        )
        .map_err(storage_err(
            "worktree_operation_checkpoint_update_failed",
            "failed to update worktree operation checkpoint",
        ))?;
        Self::get(conn, operation_id)?.ok_or_else(|| {
            VibexError::storage(
                "worktree_operation_missing_after_update",
                "worktree operation was not found after update",
            )
        })
    }

    pub fn save(
        conn: &Connection,
        record: &GitWorktreeOperationRecord,
    ) -> VibexResult<GitWorktreeOperationRecord> {
        let detail_json = json_to_db(&record.detail)?;
        let diagnostic_json = record
            .detail
            .diagnostic
            .as_ref()
            .map(json_to_db)
            .transpose()?;
        let changed = conn
            .execute(
                "
                UPDATE git_worktree_operations
                SET status = ?2, checkpoint = ?3, detail_json = ?4,
                    head_after = ?5, error = ?6, lease_owner = ?7,
                    lease_expires_at_ms = ?8, attempt = ?9,
                    diagnostic_json = ?10, updated_at_ms = ?11
                WHERE operation_id = ?1
                ",
                params![
                    record.operation_id.as_str(),
                    enum_to_db(&record.status)?,
                    enum_to_db(&record.detail.checkpoint)?,
                    detail_json,
                    record.head_after,
                    record.error,
                    record.detail.lease_owner,
                    record.detail.lease_expires_at_ms,
                    i64::from(record.detail.attempt),
                    diagnostic_json,
                    unix_timestamp_ms(),
                ],
            )
            .map_err(storage_err(
                "worktree_operation_save_failed",
                "failed to save worktree operation state",
            ))?;
        if changed == 0 {
            return Err(VibexError::storage(
                "worktree_operation_not_found",
                "worktree operation was not found",
            ));
        }
        Self::get(conn, &record.operation_id)?.ok_or_else(|| {
            VibexError::storage(
                "worktree_operation_missing_after_update",
                "worktree operation was not found after update",
            )
        })
    }

    pub fn mark_outcome(
        conn: &Connection,
        operation_id: &RequestId,
        status: GitWorktreeOperationStatus,
        checkpoint: GitWorktreeOperationCheckpoint,
        head_after: Option<&str>,
        diagnostic: Option<&GitWorktreeDiagnostic>,
    ) -> VibexResult<GitWorktreeOperationRecord> {
        let mut record = Self::get(conn, operation_id)?.ok_or_else(|| {
            VibexError::storage(
                "worktree_operation_not_found",
                "worktree operation was not found",
            )
        })?;
        record.status = status;
        record.detail.checkpoint = checkpoint;
        record.detail.lease_owner = None;
        record.detail.lease_expires_at_ms = None;
        record.detail.diagnostic = diagnostic.cloned();
        record.error = diagnostic.map(|value| value.summary.clone());
        if let Some(head_after) = head_after {
            record.head_after = Some(head_after.to_string());
        }
        let detail_json = json_to_db(&record.detail)?;
        let diagnostic_json = diagnostic.map(json_to_db).transpose()?;
        conn.execute(
            "
            UPDATE git_worktree_operations
            SET status = ?2, checkpoint = ?3, detail_json = ?4,
                head_after = COALESCE(?5, head_after), error = ?6,
                lease_owner = NULL, lease_expires_at_ms = NULL,
                diagnostic_json = ?7, updated_at_ms = ?8
            WHERE operation_id = ?1
            ",
            params![
                operation_id.as_str(),
                enum_to_db(&status)?,
                enum_to_db(&checkpoint)?,
                detail_json,
                head_after,
                record.error,
                diagnostic_json,
                unix_timestamp_ms()
            ],
        )
        .map_err(storage_err(
            "worktree_operation_outcome_update_failed",
            "failed to update worktree operation outcome",
        ))?;
        Self::get(conn, operation_id)?.ok_or_else(|| {
            VibexError::storage(
                "worktree_operation_missing_after_update",
                "worktree operation was not found after update",
            )
        })
    }

    pub fn update(
        conn: &Connection,
        operation_id: &RequestId,
        status: GitWorktreeOperationStatus,
        head_after: Option<&str>,
        error: Option<&str>,
    ) -> VibexResult<GitWorktreeOperationRecord> {
        let diagnostic = error.map(|summary| GitWorktreeDiagnostic {
            code: "worktree_operation_failed".to_string(),
            summary: summary.chars().take(512).collect(),
            severity: vibex_core::GitWorktreeDiagnosticSeverity::Error,
            retryable: status == GitWorktreeOperationStatus::Failed,
            recovery_action: None,
            operation_id: Some(operation_id.clone()),
            worktree_id: None,
            observed_at_ms: unix_timestamp_ms(),
        });
        let checkpoint = if status == GitWorktreeOperationStatus::Completed {
            GitWorktreeOperationCheckpoint::Completed
        } else {
            Self::get(conn, operation_id)?
                .map(|record| record.detail.checkpoint)
                .unwrap_or_default()
        };
        Self::mark_outcome(
            conn,
            operation_id,
            status,
            checkpoint,
            head_after,
            diagnostic.as_ref(),
        )
    }

    pub fn get(
        conn: &Connection,
        operation_id: &RequestId,
    ) -> VibexResult<Option<GitWorktreeOperationRecord>> {
        conn.query_row(
            "
            SELECT operation_id, project_id, source_workspace_id, target_workspace_id,
                operation, status, worktree_path, branch, base_ref, head_before,
                head_after, error, created_at_ms, updated_at_ms, idempotency_key,
                request_fingerprint, checkpoint, detail_json, lease_owner,
                lease_expires_at_ms, attempt, diagnostic_json
            FROM git_worktree_operations
            WHERE operation_id = ?1
            ",
            params![operation_id.as_str()],
            map_worktree_operation,
        )
        .optional()
        .map_err(storage_err(
            "worktree_operation_lookup_failed",
            "failed to lookup worktree operation",
        ))
    }

    pub fn get_by_idempotency_key(
        conn: &Connection,
        idempotency_key: &str,
    ) -> VibexResult<Option<GitWorktreeOperationRecord>> {
        conn.query_row(
            "
            SELECT operation_id, project_id, source_workspace_id, target_workspace_id,
                operation, status, worktree_path, branch, base_ref, head_before,
                head_after, error, created_at_ms, updated_at_ms, idempotency_key,
                request_fingerprint, checkpoint, detail_json, lease_owner,
                lease_expires_at_ms, attempt, diagnostic_json
            FROM git_worktree_operations
            WHERE idempotency_key = ?1
            ",
            params![idempotency_key],
            map_worktree_operation,
        )
        .optional()
        .map_err(storage_err(
            "worktree_operation_idempotency_lookup_failed",
            "failed to lookup worktree operation idempotency key",
        ))
    }

    pub fn list_for_project(
        conn: &Connection,
        project_id: &ProjectId,
    ) -> VibexResult<Vec<GitWorktreeOperationRecord>> {
        let mut statement = conn
            .prepare(
                "
                SELECT operation_id, project_id, source_workspace_id, target_workspace_id,
                    operation, status, worktree_path, branch, base_ref, head_before,
                    head_after, error, created_at_ms, updated_at_ms, idempotency_key,
                    request_fingerprint, checkpoint, detail_json, lease_owner,
                    lease_expires_at_ms, attempt, diagnostic_json
                FROM git_worktree_operations
                WHERE project_id = ?1
                ORDER BY updated_at_ms DESC
                ",
            )
            .map_err(storage_err(
                "worktree_operation_list_failed",
                "failed to list worktree operations",
            ))?;
        let rows = statement
            .query_map(params![project_id.as_str()], map_worktree_operation)
            .map_err(storage_err(
                "worktree_operation_list_failed",
                "failed to list worktree operations",
            ))?;
        collect_rows(
            rows,
            "worktree_operation_decode_failed",
            "failed to decode worktree operation",
        )
    }

    pub fn list_reconcilable(conn: &Connection) -> VibexResult<Vec<GitWorktreeOperationRecord>> {
        let completed = enum_to_db(&GitWorktreeOperationStatus::Completed)?;
        let failed = enum_to_db(&GitWorktreeOperationStatus::Failed)?;
        let aborted = enum_to_db(&GitWorktreeOperationStatus::Aborted)?;
        let mut statement = conn
            .prepare(
                "
                SELECT operation_id, project_id, source_workspace_id, target_workspace_id,
                    operation, status, worktree_path, branch, base_ref, head_before,
                    head_after, error, created_at_ms, updated_at_ms, idempotency_key,
                    request_fingerprint, checkpoint, detail_json, lease_owner,
                    lease_expires_at_ms, attempt, diagnostic_json
                FROM git_worktree_operations
                WHERE status NOT IN (?1, ?2, ?3)
                ORDER BY updated_at_ms ASC
                ",
            )
            .map_err(storage_err(
                "worktree_operation_reconcile_list_failed",
                "failed to list reconcilable worktree operations",
            ))?;
        let rows = statement
            .query_map(params![completed, failed, aborted], map_worktree_operation)
            .map_err(storage_err(
                "worktree_operation_reconcile_list_failed",
                "failed to list reconcilable worktree operations",
            ))?;
        collect_rows(
            rows,
            "worktree_operation_decode_failed",
            "failed to decode worktree operation",
        )
    }
}

impl RemoteDeviceRepository {
    pub fn upsert(conn: &Connection, record: &RemoteDeviceRecord) -> VibexResult<()> {
        let detail = &record.detail;
        conn.execute(
            "
            INSERT INTO remote_devices (
                device_id, display_name, public_key, auth_secret_hash, grant_revision,
                permission_level, status, paired_at_ms, last_seen_at_ms, revoked_at_ms,
                created_at_ms, updated_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(device_id) DO UPDATE SET
                display_name = excluded.display_name,
                public_key = excluded.public_key,
                auth_secret_hash = excluded.auth_secret_hash,
                grant_revision = excluded.grant_revision,
                permission_level = excluded.permission_level,
                status = excluded.status,
                paired_at_ms = excluded.paired_at_ms,
                last_seen_at_ms = excluded.last_seen_at_ms,
                revoked_at_ms = excluded.revoked_at_ms,
                updated_at_ms = excluded.updated_at_ms
            ",
            params![
                detail.device_id.as_str(),
                detail.display_name,
                detail.public_key,
                record.auth_secret_hash,
                detail.grant_revision,
                enum_to_db(&detail.permission_level)?,
                enum_to_db(&detail.status)?,
                detail.paired_at_ms,
                detail.last_seen_at_ms,
                detail.revoked_at_ms,
                detail.created_at_ms,
                detail.updated_at_ms
            ],
        )
        .map_err(storage_err(
            "remote_device_upsert_failed",
            "failed to upsert remote device",
        ))?;
        Ok(())
    }

    pub fn get(conn: &Connection, device_id: &DeviceId) -> VibexResult<Option<RemoteDeviceRecord>> {
        conn.query_row(
            "
            SELECT device_id, display_name, public_key, auth_secret_hash, grant_revision,
                permission_level, status, paired_at_ms, last_seen_at_ms,
                revoked_at_ms, created_at_ms, updated_at_ms
            FROM remote_devices
            WHERE device_id = ?1
            ",
            params![device_id.as_str()],
            map_remote_device_record,
        )
        .optional()
        .map_err(storage_err(
            "remote_device_lookup_failed",
            "failed to lookup remote device",
        ))
    }

    pub fn list(conn: &Connection) -> VibexResult<Vec<RemoteDeviceRecord>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT device_id, display_name, public_key, auth_secret_hash, grant_revision,
                    permission_level, status, paired_at_ms, last_seen_at_ms,
                    revoked_at_ms, created_at_ms, updated_at_ms
                FROM remote_devices
                ORDER BY updated_at_ms DESC
                ",
            )
            .map_err(storage_err(
                "remote_device_list_failed",
                "failed to list remote devices",
            ))?;
        let rows = stmt
            .query_map([], map_remote_device_record)
            .map_err(storage_err(
                "remote_device_list_failed",
                "failed to list remote devices",
            ))?;
        collect_rows(
            rows,
            "remote_device_decode_failed",
            "failed to decode remote device row",
        )
    }

    pub fn update_last_seen(conn: &Connection, device_id: &DeviceId, now: i64) -> VibexResult<()> {
        conn.execute(
            "
            UPDATE remote_devices
            SET last_seen_at_ms = ?2, updated_at_ms = ?2
            WHERE device_id = ?1
            ",
            params![device_id.as_str(), now],
        )
        .map_err(storage_err(
            "remote_device_last_seen_failed",
            "failed to update remote device last seen",
        ))?;
        Ok(())
    }

    pub fn revoke(
        conn: &Connection,
        device_id: &DeviceId,
        revoked_at_ms: i64,
    ) -> VibexResult<RemoteDeviceRecord> {
        conn.execute(
            "
            UPDATE remote_devices
            SET status = ?2, revoked_at_ms = ?3, updated_at_ms = ?3
            WHERE device_id = ?1
            ",
            params![
                device_id.as_str(),
                enum_to_db(&RemoteDeviceStatus::Revoked)?,
                revoked_at_ms
            ],
        )
        .map_err(storage_err(
            "remote_device_revoke_failed",
            "failed to revoke remote device",
        ))?;
        Self::get(conn, device_id)?.ok_or_else(|| {
            VibexError::storage(
                "remote_device_missing_after_revoke",
                "remote device was not found after revoke",
            )
        })
    }
}

impl RemotePairingCodeRepository {
    pub fn insert(conn: &Connection, record: &RemotePairingCodeRecord) -> VibexResult<()> {
        let pairing = &record.pairing;
        conn.execute(
            "
            INSERT INTO remote_pairing_codes (
                pairing_id, code_hash, permission_level, expires_at_ms,
                claimed_device_id, created_at_ms, claimed_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            params![
                pairing.pairing_id.as_str(),
                record.code_hash,
                enum_to_db(&pairing.permission_level)?,
                pairing.expires_at_ms,
                pairing.claimed_device_id.as_ref().map(DeviceId::as_str),
                pairing.created_at_ms,
                pairing.claimed_at_ms
            ],
        )
        .map_err(storage_err(
            "remote_pairing_insert_failed",
            "failed to insert remote pairing code",
        ))?;
        Ok(())
    }

    pub fn get_by_hash(
        conn: &Connection,
        code_hash: &str,
    ) -> VibexResult<Option<RemotePairingCodeRecord>> {
        conn.query_row(
            "
            SELECT pairing_id, code_hash, permission_level, expires_at_ms,
                claimed_device_id, created_at_ms, claimed_at_ms
            FROM remote_pairing_codes
            WHERE code_hash = ?1
            ",
            params![code_hash],
            map_remote_pairing_code_record,
        )
        .optional()
        .map_err(storage_err(
            "remote_pairing_lookup_failed",
            "failed to lookup remote pairing code",
        ))
    }

    pub fn mark_claimed(
        conn: &Connection,
        pairing_id: &RequestId,
        device_id: &DeviceId,
        claimed_at_ms: i64,
    ) -> VibexResult<RemotePairingCodeRecord> {
        let changed = conn
            .execute(
                "
            UPDATE remote_pairing_codes
            SET claimed_device_id = ?2, claimed_at_ms = ?3
            WHERE pairing_id = ?1 AND claimed_at_ms IS NULL
            ",
                params![pairing_id.as_str(), device_id.as_str(), claimed_at_ms],
            )
            .map_err(storage_err(
                "remote_pairing_claim_failed",
                "failed to mark remote pairing code claimed",
            ))?;
        if changed != 1 {
            return Err(VibexError::conflict(
                "remote_pairing_claim_conflict",
                "remote pairing code was already claimed",
            ));
        }
        Self::get_by_id(conn, pairing_id)?.ok_or_else(|| {
            VibexError::storage(
                "remote_pairing_missing_after_claim",
                "remote pairing code was not found after claim",
            )
        })
    }

    pub fn get_by_id(
        conn: &Connection,
        pairing_id: &RequestId,
    ) -> VibexResult<Option<RemotePairingCodeRecord>> {
        conn.query_row(
            "
            SELECT pairing_id, code_hash, permission_level, expires_at_ms,
                claimed_device_id, created_at_ms, claimed_at_ms
            FROM remote_pairing_codes
            WHERE pairing_id = ?1
            ",
            params![pairing_id.as_str()],
            map_remote_pairing_code_record,
        )
        .optional()
        .map_err(storage_err(
            "remote_pairing_lookup_failed",
            "failed to lookup remote pairing code",
        ))
    }
}

impl RemoteAuditRepository {
    pub fn insert(conn: &Connection, record: &RemoteAuditRecord) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO remote_audit_logs (
                audit_id, device_id, action, target_kind, target_id, outcome,
                redacted_summary, request_id, correlation_id, created_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ",
            params![
                record.audit_id.as_str(),
                record.device_id.as_ref().map(DeviceId::as_str),
                enum_to_db(&record.action)?,
                enum_to_db(&record.target_kind)?,
                record.target_id,
                enum_to_db(&record.outcome)?,
                record.redacted_summary,
                record.request_id.as_ref().map(RequestId::as_str),
                record.correlation_id.as_ref().map(CorrelationId::as_str),
                record.created_at_ms
            ],
        )
        .map_err(storage_err(
            "remote_audit_insert_failed",
            "failed to insert remote audit record",
        ))?;
        Ok(())
    }

    pub fn list(
        conn: &Connection,
        request: &RemoteAuditListRequest,
    ) -> VibexResult<Vec<RemoteAuditRecord>> {
        let limit = request.limit.unwrap_or(100).clamp(1, 500);
        if let Some(device_id) = &request.device_id {
            let mut stmt = conn
                .prepare(
                    "
                    SELECT audit_id, device_id, action, target_kind, target_id, outcome,
                        redacted_summary, request_id, correlation_id, created_at_ms
                    FROM remote_audit_logs
                    WHERE device_id = ?1
                    ORDER BY created_at_ms DESC
                    LIMIT ?2
                    ",
                )
                .map_err(storage_err(
                    "remote_audit_list_failed",
                    "failed to list remote audit records",
                ))?;
            let rows = stmt
                .query_map(params![device_id.as_str(), limit], map_remote_audit_record)
                .map_err(storage_err(
                    "remote_audit_list_failed",
                    "failed to list remote audit records",
                ))?;
            return collect_rows(
                rows,
                "remote_audit_decode_failed",
                "failed to decode remote audit record",
            );
        }

        let mut stmt = conn
            .prepare(
                "
                SELECT audit_id, device_id, action, target_kind, target_id, outcome,
                    redacted_summary, request_id, correlation_id, created_at_ms
                FROM remote_audit_logs
                ORDER BY created_at_ms DESC
                LIMIT ?1
                ",
            )
            .map_err(storage_err(
                "remote_audit_list_failed",
                "failed to list remote audit records",
            ))?;
        let rows = stmt
            .query_map(params![limit], map_remote_audit_record)
            .map_err(storage_err(
                "remote_audit_list_failed",
                "failed to list remote audit records",
            ))?;
        collect_rows(
            rows,
            "remote_audit_decode_failed",
            "failed to decode remote audit record",
        )
    }
}

pub fn default_database_path() -> VibexResult<PathBuf> {
    if let Ok(value) = std::env::var("VIBEX_DB_PATH") {
        let path = PathBuf::from(value);
        ensure_safe_database_path(&path)?;
        return Ok(path);
    }

    let home = dirs::home_dir().ok_or_else(|| {
        VibexError::storage(
            "home_dir_unavailable",
            "could not resolve user home directory",
        )
    })?;
    Ok(home.join(".vibex").join("vibex.db"))
}

pub fn stage0_smoke_database_path() -> VibexResult<PathBuf> {
    if let Ok(value) = std::env::var("VIBEX_DB_PATH") {
        let path = PathBuf::from(value);
        ensure_safe_database_path(&path)?;
        return Ok(path);
    }

    Ok(PathBuf::from("target")
        .join("stage0")
        .join("vibex-smoke.db"))
}

pub fn open_database(path: &Path) -> VibexResult<Connection> {
    ensure_safe_database_path(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            VibexError::storage(
                "db_directory_create_failed",
                "failed to create database directory",
            )
            .with_diagnostic("path", parent.display().to_string())
            .with_diagnostic("error", err.to_string())
        })?;
    }

    let conn = Connection::open(path).map_err(|err| {
        VibexError::storage("db_open_failed", "failed to open SQLite database")
            .with_diagnostic("path", path.display().to_string())
            .with_diagnostic("error", err.to_string())
    })?;
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT).map_err(storage_err(
        "db_pragma_failed",
        "failed to configure SQLite busy timeout",
    ))?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(storage_err(
            "db_pragma_failed",
            "failed to enable SQLite WAL mode",
        ))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(storage_err(
            "db_pragma_failed",
            "failed to enable SQLite foreign keys",
        ))?;
    Ok(conn)
}

pub fn apply_migrations(conn: &mut Connection) -> VibexResult<Vec<String>> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at_ms INTEGER NOT NULL
        );
        ",
    )
    .map_err(storage_err(
        "migration_table_failed",
        "failed to initialize migrations table",
    ))?;

    let mut applied = Vec::new();
    for migration in MIGRATIONS {
        let already_applied = migration_applied(conn, migration.version)?;
        if already_applied {
            continue;
        }

        let tx = conn.transaction().map_err(storage_err(
            "migration_transaction_failed",
            "failed to start migration transaction",
        ))?;
        tx.execute_batch(migration.sql).map_err(storage_err(
            "migration_apply_failed",
            "failed to apply migration",
        ))?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![migration.version, migration.name, unix_timestamp_ms()],
        )
        .map_err(storage_err(
            "migration_record_failed",
            "failed to record migration",
        ))?;
        tx.commit().map_err(storage_err(
            "migration_commit_failed",
            "failed to commit migration",
        ))?;
        applied.push(format!("{}:{}", migration.version, migration.name));
    }

    apply_runtime_auth_source_table_rebuild(conn, &mut applied)?;
    apply_usage_model_id_nullable_table_rebuild(conn, &mut applied)?;
    apply_usage_counter_scope_column(conn, &mut applied)?;
    apply_message_submission_runtime_policy(conn, &mut applied)?;

    // Seed compatibility Profiles before the v37 backfill while no caller
    // transaction is active. Repository reads may run inside a transaction and
    // must not start the backfill's own transaction.
    ProviderProfileRepository::ensure_local_defaults(conn)?;
    ProviderProjectionCompatibilityRepository::backfill_legacy_profiles(conn)?;
    Ok(applied)
}

fn apply_message_submission_runtime_policy(
    conn: &mut Connection,
    applied: &mut Vec<String>,
) -> VibexResult<()> {
    const VERSION: i64 = 49;
    const NAME: &str = "message_submission_runtime_policy";
    if migration_applied(conn, VERSION)? {
        return Ok(());
    }
    let tx = conn.transaction().map_err(storage_err(
        "migration_transaction_failed",
        "failed to start message submission runtime policy migration",
    ))?;
    tx.execute(
        "ALTER TABLE agent_message_submissions ADD COLUMN required_runtime_policy TEXT NOT NULL DEFAULT 'automatic'",
        [],
    )
    .map_err(storage_err(
        "migration_apply_failed",
        "failed to add message submission runtime policy",
    ))?;
    tx.execute(
        "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
        params![VERSION, NAME, unix_timestamp_ms()],
    )
    .map_err(storage_err(
        "migration_record_failed",
        "failed to record message submission runtime policy migration",
    ))?;
    tx.commit().map_err(storage_err(
        "migration_commit_failed",
        "failed to commit message submission runtime policy migration",
    ))?;
    applied.push(format!("{VERSION}:{NAME}"));
    Ok(())
}

fn apply_runtime_auth_source_table_rebuild(
    conn: &mut Connection,
    applied: &mut Vec<String>,
) -> VibexResult<()> {
    const VERSION: i64 = 46;
    const NAME: &str = "runtime_auth_source_nullable_legacy_columns";
    if migration_applied(conn, VERSION)? {
        return Ok(());
    }

    // SQLite cannot change a NOT NULL constraint in place. Foreign keys must
    // be disabled outside the rebuild transaction so child tables keep their
    // references while the two parent tables are replaced.
    conn.pragma_update(None, "foreign_keys", false)
        .map_err(storage_err(
            "migration_foreign_keys_disable_failed",
            "failed to disable foreign keys for runtime auth source migration",
        ))?;
    let migration_result = (|| -> VibexResult<()> {
        let tx = conn.transaction().map_err(storage_err(
            "migration_transaction_failed",
            "failed to start runtime auth source migration transaction",
        ))?;
        tx.execute_batch(
            "
            CREATE TABLE session_runtime_bindings_v46 (
                binding_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                agent_id TEXT NOT NULL,
                transport_kind TEXT NOT NULL,
                adapter_id TEXT NOT NULL,
                adapter_version TEXT NOT NULL,
                adapter_compatibility_identity TEXT NOT NULL,
                provider_profile_id TEXT NULL,
                profile_revision INTEGER NULL,
                auth_source_kind TEXT NOT NULL,
                auth_source_id TEXT NOT NULL,
                auth_source_revision INTEGER NOT NULL CHECK(auth_source_revision >= 0),
                native_session_id TEXT NULL,
                native_state_home_id TEXT NOT NULL,
                provider_resume_identity TEXT NULL,
                process_spawn_fingerprint TEXT NOT NULL,
                session_runtime_config_state_json TEXT NOT NULL,
                capability_snapshot_json TEXT NULL,
                restore_compatibility_key_json TEXT NULL,
                last_context_sequence INTEGER NOT NULL DEFAULT 0,
                last_summary_sequence INTEGER NOT NULL DEFAULT 0,
                context_bridge_version INTEGER NOT NULL DEFAULT 0,
                activation_generation INTEGER NOT NULL DEFAULT 0,
                binding_state TEXT NOT NULL,
                created_by_switch_id TEXT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                usage_zero_baseline_state TEXT NOT NULL DEFAULT 'unavailable'
                    CHECK(usage_zero_baseline_state IN ('available', 'claimed', 'unavailable')),
                usage_zero_baseline_execution_id TEXT NULL,
                usage_zero_baseline_activation_generation INTEGER NULL
                    CHECK(usage_zero_baseline_activation_generation >= 0),
                CHECK(
                    (auth_source_kind = 'provider_profile'
                        AND provider_profile_id = auth_source_id
                        AND profile_revision = auth_source_revision)
                    OR
                    (auth_source_kind = 'agent_account'
                        AND provider_profile_id IS NULL
                        AND profile_revision IS NULL)
                )
            );
            INSERT INTO session_runtime_bindings_v46 (
                binding_id, session_id, agent_id, transport_kind, adapter_id,
                adapter_version, adapter_compatibility_identity, provider_profile_id,
                profile_revision, auth_source_kind, auth_source_id, auth_source_revision,
                native_session_id, native_state_home_id, provider_resume_identity,
                process_spawn_fingerprint, session_runtime_config_state_json,
                capability_snapshot_json, restore_compatibility_key_json,
                last_context_sequence, last_summary_sequence, context_bridge_version,
                activation_generation, binding_state, created_by_switch_id, created_at_ms,
                updated_at_ms, usage_zero_baseline_state, usage_zero_baseline_execution_id,
                usage_zero_baseline_activation_generation
            )
            SELECT
                binding_id, session_id, agent_id, transport_kind, adapter_id,
                adapter_version, adapter_compatibility_identity, provider_profile_id,
                profile_revision, auth_source_kind, auth_source_id, auth_source_revision,
                native_session_id, native_state_home_id, provider_resume_identity,
                process_spawn_fingerprint, session_runtime_config_state_json,
                capability_snapshot_json, restore_compatibility_key_json,
                last_context_sequence, last_summary_sequence, context_bridge_version,
                activation_generation, binding_state, created_by_switch_id, created_at_ms,
                updated_at_ms, usage_zero_baseline_state, usage_zero_baseline_execution_id,
                usage_zero_baseline_activation_generation
            FROM session_runtime_bindings;
            DROP TABLE session_runtime_bindings;
            ALTER TABLE session_runtime_bindings_v46 RENAME TO session_runtime_bindings;
            CREATE INDEX idx_session_runtime_bindings_route
                ON session_runtime_bindings(
                    session_id, agent_id, auth_source_kind, auth_source_id,
                    adapter_compatibility_identity
                );
            CREATE INDEX idx_session_runtime_bindings_session
                ON session_runtime_bindings(session_id, binding_state);

            CREATE TABLE runtime_switches_v46 (
                switch_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                idempotency_key TEXT NOT NULL,
                source_revision INTEGER NOT NULL,
                source_binding_id TEXT NULL,
                desired_selection_revision INTEGER NOT NULL,
                target_binding_id TEXT NULL,
                target_agent_id TEXT NOT NULL,
                target_adapter_id TEXT NOT NULL,
                target_profile_id TEXT NULL,
                target_auth_source_kind TEXT NOT NULL,
                target_auth_source_id TEXT NOT NULL,
                target_auth_source_revision INTEGER NOT NULL
                    CHECK(target_auth_source_revision >= 0),
                requested_policy_json TEXT NULL,
                active_work_policy_json TEXT NULL,
                requested_session_config_json TEXT NULL,
                restore_compatibility_result_json TEXT NULL,
                status TEXT NOT NULL,
                error_code TEXT NULL,
                error_detail_redacted TEXT NULL,
                worker_lease_owner TEXT NULL,
                worker_lease_deadline_ms INTEGER NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                committed_at_ms INTEGER NULL,
                activation_completed_at_ms INTEGER NULL,
                UNIQUE(session_id, idempotency_key),
                CHECK(
                    (target_auth_source_kind = 'provider_profile'
                        AND target_profile_id = target_auth_source_id)
                    OR
                    (target_auth_source_kind = 'agent_account'
                        AND target_profile_id IS NULL)
                )
            );
            INSERT INTO runtime_switches_v46 (
                switch_id, session_id, idempotency_key, source_revision,
                source_binding_id, desired_selection_revision, target_binding_id,
                target_agent_id, target_adapter_id, target_profile_id,
                target_auth_source_kind, target_auth_source_id, target_auth_source_revision,
                requested_policy_json, active_work_policy_json,
                requested_session_config_json, restore_compatibility_result_json,
                status, error_code, error_detail_redacted, worker_lease_owner,
                worker_lease_deadline_ms, created_at_ms, updated_at_ms, committed_at_ms,
                activation_completed_at_ms
            )
            SELECT
                switch_id, session_id, idempotency_key, source_revision,
                source_binding_id, desired_selection_revision, target_binding_id,
                target_agent_id, target_adapter_id, target_profile_id,
                target_auth_source_kind, target_auth_source_id, target_auth_source_revision,
                requested_policy_json, active_work_policy_json,
                requested_session_config_json, restore_compatibility_result_json,
                status, error_code, error_detail_redacted, worker_lease_owner,
                worker_lease_deadline_ms, created_at_ms, updated_at_ms, committed_at_ms,
                activation_completed_at_ms
            FROM runtime_switches;
            DROP TABLE runtime_switches;
            ALTER TABLE runtime_switches_v46 RENAME TO runtime_switches;
            CREATE INDEX idx_runtime_switches_session_status
                ON runtime_switches(session_id, status);
            CREATE INDEX idx_runtime_switches_pending_activation
                ON runtime_switches(status, activation_completed_at_ms)
                WHERE activation_completed_at_ms IS NULL;

            CREATE TABLE agent_usage_checkpoints_v46 (
                usage_stream_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL
                    REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                binding_id TEXT NOT NULL,
                last_activation_generation INTEGER NOT NULL
                    CHECK(last_activation_generation >= 0),
                agent_id TEXT NOT NULL,
                provider_profile_id TEXT NULL,
                auth_source_kind TEXT NOT NULL,
                auth_source_id TEXT NOT NULL,
                auth_source_revision INTEGER NOT NULL CHECK(auth_source_revision >= 0),
                last_model_id TEXT NOT NULL,
                reset_epoch INTEGER NOT NULL CHECK(reset_epoch >= 0),
                counter_origin TEXT NOT NULL,
                cumulative_input_tokens INTEGER NULL CHECK(cumulative_input_tokens >= 0),
                cumulative_output_tokens INTEGER NULL CHECK(cumulative_output_tokens >= 0),
                cumulative_thought_tokens INTEGER NULL CHECK(cumulative_thought_tokens >= 0),
                cumulative_cached_read_tokens INTEGER NULL
                    CHECK(cumulative_cached_read_tokens >= 0),
                cumulative_cached_write_tokens INTEGER NULL
                    CHECK(cumulative_cached_write_tokens >= 0),
                cumulative_total_tokens INTEGER NULL CHECK(cumulative_total_tokens >= 0),
                last_usage_execution_id TEXT NULL,
                last_observation_sequence INTEGER NOT NULL
                    CHECK(last_observation_sequence >= 0),
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                UNIQUE(session_id, binding_id),
                CHECK(
                    (auth_source_kind = 'provider_profile'
                        AND provider_profile_id = auth_source_id)
                    OR
                    (auth_source_kind = 'agent_account'
                        AND provider_profile_id IS NULL)
                )
            );
            INSERT INTO agent_usage_checkpoints_v46 (
                usage_stream_id, session_id, binding_id, last_activation_generation,
                agent_id, provider_profile_id, auth_source_kind, auth_source_id,
                auth_source_revision, last_model_id, reset_epoch, counter_origin,
                cumulative_input_tokens, cumulative_output_tokens,
                cumulative_thought_tokens, cumulative_cached_read_tokens,
                cumulative_cached_write_tokens, cumulative_total_tokens,
                last_usage_execution_id, last_observation_sequence, created_at_ms, updated_at_ms
            )
            SELECT
                usage_stream_id, session_id, binding_id, last_activation_generation,
                agent_id, provider_profile_id, auth_source_kind, auth_source_id,
                auth_source_revision, last_model_id, reset_epoch, counter_origin,
                cumulative_input_tokens, cumulative_output_tokens,
                cumulative_thought_tokens, cumulative_cached_read_tokens,
                cumulative_cached_write_tokens, cumulative_total_tokens,
                last_usage_execution_id, last_observation_sequence, created_at_ms, updated_at_ms
            FROM agent_usage_checkpoints;
            DROP TABLE agent_usage_checkpoints;
            ALTER TABLE agent_usage_checkpoints_v46 RENAME TO agent_usage_checkpoints;
            CREATE INDEX idx_agent_usage_checkpoints_session
                ON agent_usage_checkpoints(session_id, updated_at_ms);

            CREATE TABLE agent_turn_usage_facts_v46 (
                usage_execution_id TEXT PRIMARY KEY,
                message_submission_id TEXT NULL,
                session_id TEXT NOT NULL
                    REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
                project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
                workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
                binding_id TEXT NOT NULL,
                activation_generation INTEGER NOT NULL CHECK(activation_generation >= 0),
                reset_epoch INTEGER NOT NULL CHECK(reset_epoch >= 0),
                agent_id TEXT NOT NULL,
                provider_profile_id TEXT NULL,
                auth_source_kind TEXT NOT NULL,
                auth_source_id TEXT NOT NULL,
                auth_source_revision INTEGER NOT NULL CHECK(auth_source_revision >= 0),
                model_id TEXT NOT NULL,
                execution_status TEXT NOT NULL,
                input_delta INTEGER NULL CHECK(input_delta >= 0),
                output_delta INTEGER NULL CHECK(output_delta >= 0),
                thought_delta INTEGER NULL CHECK(thought_delta >= 0),
                cached_read_delta INTEGER NULL CHECK(cached_read_delta >= 0),
                cached_write_delta INTEGER NULL CHECK(cached_write_delta >= 0),
                total_delta INTEGER NULL CHECK(total_delta >= 0),
                cumulative_input_after INTEGER NULL CHECK(cumulative_input_after >= 0),
                cumulative_output_after INTEGER NULL CHECK(cumulative_output_after >= 0),
                cumulative_thought_after INTEGER NULL CHECK(cumulative_thought_after >= 0),
                cumulative_cached_read_after INTEGER NULL CHECK(cumulative_cached_read_after >= 0),
                cumulative_cached_write_after INTEGER NULL CHECK(cumulative_cached_write_after >= 0),
                cumulative_total_after INTEGER NULL CHECK(cumulative_total_after >= 0),
                context_window_used_tokens INTEGER NULL CHECK(context_window_used_tokens >= 0),
                context_window_size_tokens INTEGER NULL CHECK(context_window_size_tokens > 0),
                reported_fields INTEGER NOT NULL CHECK(reported_fields >= 0),
                coverage TEXT NOT NULL,
                last_source TEXT NULL,
                reset_reason TEXT NULL,
                dispatched_at_ms INTEGER NOT NULL,
                completed_at_ms INTEGER NULL,
                last_observed_at_ms INTEGER NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                CHECK(
                    (auth_source_kind = 'provider_profile'
                        AND provider_profile_id = auth_source_id)
                    OR
                    (auth_source_kind = 'agent_account'
                        AND provider_profile_id IS NULL)
                )
            );
            INSERT INTO agent_turn_usage_facts_v46 (
                usage_execution_id, message_submission_id, session_id, project_id,
                workspace_id, binding_id, activation_generation, reset_epoch, agent_id,
                provider_profile_id, auth_source_kind, auth_source_id, auth_source_revision,
                model_id, execution_status, input_delta, output_delta, thought_delta,
                cached_read_delta, cached_write_delta, total_delta, cumulative_input_after,
                cumulative_output_after, cumulative_thought_after, cumulative_cached_read_after,
                cumulative_cached_write_after, cumulative_total_after, context_window_used_tokens,
                context_window_size_tokens, reported_fields, coverage, last_source, reset_reason,
                dispatched_at_ms, completed_at_ms, last_observed_at_ms, created_at_ms, updated_at_ms
            )
            SELECT
                usage_execution_id, message_submission_id, session_id, project_id,
                workspace_id, binding_id, activation_generation, reset_epoch, agent_id,
                provider_profile_id, auth_source_kind, auth_source_id, auth_source_revision,
                model_id, execution_status, input_delta, output_delta, thought_delta,
                cached_read_delta, cached_write_delta, total_delta, cumulative_input_after,
                cumulative_output_after, cumulative_thought_after, cumulative_cached_read_after,
                cumulative_cached_write_after, cumulative_total_after, context_window_used_tokens,
                context_window_size_tokens, reported_fields, coverage, last_source, reset_reason,
                dispatched_at_ms, completed_at_ms, last_observed_at_ms, created_at_ms, updated_at_ms
            FROM agent_turn_usage_facts;
            DROP TABLE agent_turn_usage_facts;
            ALTER TABLE agent_turn_usage_facts_v46 RENAME TO agent_turn_usage_facts;
            CREATE INDEX idx_agent_turn_usage_facts_dispatch
                ON agent_turn_usage_facts(dispatched_at_ms, usage_execution_id);
            CREATE INDEX idx_agent_turn_usage_facts_session
                ON agent_turn_usage_facts(session_id, dispatched_at_ms);
            CREATE INDEX idx_agent_turn_usage_facts_project
                ON agent_turn_usage_facts(project_id, dispatched_at_ms);
            CREATE INDEX idx_agent_turn_usage_facts_agent
                ON agent_turn_usage_facts(agent_id, dispatched_at_ms);
            CREATE INDEX idx_agent_turn_usage_facts_auth_source
                ON agent_turn_usage_facts(auth_source_kind, auth_source_id, dispatched_at_ms);
            CREATE INDEX idx_agent_turn_usage_facts_profile
                ON agent_turn_usage_facts(provider_profile_id, dispatched_at_ms)
                WHERE provider_profile_id IS NOT NULL;
            CREATE INDEX idx_agent_turn_usage_facts_model
                ON agent_turn_usage_facts(model_id, dispatched_at_ms);
            ",
        )
        .map_err(storage_err(
            "migration_apply_failed",
            "failed to rebuild runtime auth source tables",
        ))?;

        let foreign_key_violation = tx
            .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
            .optional()
            .map_err(storage_err(
                "migration_foreign_key_check_failed",
                "failed to validate runtime auth source migration foreign keys",
            ))?;
        if foreign_key_violation.is_some() {
            return Err(VibexError::storage(
                "migration_foreign_key_violation",
                "runtime auth source migration would violate a foreign key",
            ));
        }
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![VERSION, NAME, unix_timestamp_ms()],
        )
        .map_err(storage_err(
            "migration_record_failed",
            "failed to record runtime auth source migration",
        ))?;
        tx.commit().map_err(storage_err(
            "migration_commit_failed",
            "failed to commit runtime auth source migration",
        ))?;
        Ok(())
    })();
    let enable_result = conn
        .pragma_update(None, "foreign_keys", true)
        .map_err(storage_err(
            "migration_foreign_keys_enable_failed",
            "failed to restore foreign key enforcement after runtime auth source migration",
        ));
    migration_result?;
    enable_result?;
    applied.push(format!("{VERSION}:{NAME}"));
    Ok(())
}

fn apply_usage_model_id_nullable_table_rebuild(
    conn: &mut Connection,
    applied: &mut Vec<String>,
) -> VibexResult<()> {
    const VERSION: i64 = 47;
    const NAME: &str = "agent_default_usage_model_nullable";
    if migration_applied(conn, VERSION)? {
        return Ok(());
    }

    let tx = conn.transaction().map_err(storage_err(
        "migration_transaction_failed",
        "failed to start Agent usage model migration transaction",
    ))?;
    tx.execute_batch(
        "
        CREATE TABLE agent_usage_checkpoints_v47 (
            usage_stream_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL
                REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
            binding_id TEXT NOT NULL,
            last_activation_generation INTEGER NOT NULL
                CHECK(last_activation_generation >= 0),
            agent_id TEXT NOT NULL,
            provider_profile_id TEXT NULL,
            auth_source_kind TEXT NOT NULL,
            auth_source_id TEXT NOT NULL,
            auth_source_revision INTEGER NOT NULL CHECK(auth_source_revision >= 0),
            last_model_id TEXT NULL,
            reset_epoch INTEGER NOT NULL CHECK(reset_epoch >= 0),
            counter_origin TEXT NOT NULL,
            cumulative_input_tokens INTEGER NULL CHECK(cumulative_input_tokens >= 0),
            cumulative_output_tokens INTEGER NULL CHECK(cumulative_output_tokens >= 0),
            cumulative_thought_tokens INTEGER NULL CHECK(cumulative_thought_tokens >= 0),
            cumulative_cached_read_tokens INTEGER NULL
                CHECK(cumulative_cached_read_tokens >= 0),
            cumulative_cached_write_tokens INTEGER NULL
                CHECK(cumulative_cached_write_tokens >= 0),
            cumulative_total_tokens INTEGER NULL CHECK(cumulative_total_tokens >= 0),
            last_usage_execution_id TEXT NULL,
            last_observation_sequence INTEGER NOT NULL
                CHECK(last_observation_sequence >= 0),
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            UNIQUE(session_id, binding_id),
            CHECK(last_model_id IS NOT NULL OR auth_source_kind = 'agent_account'),
            CHECK(
                (auth_source_kind = 'provider_profile'
                    AND provider_profile_id = auth_source_id)
                OR
                (auth_source_kind = 'agent_account'
                    AND provider_profile_id IS NULL)
            )
        );
        INSERT INTO agent_usage_checkpoints_v47 (
            usage_stream_id, session_id, binding_id, last_activation_generation,
            agent_id, provider_profile_id, auth_source_kind, auth_source_id,
            auth_source_revision, last_model_id, reset_epoch, counter_origin,
            cumulative_input_tokens, cumulative_output_tokens,
            cumulative_thought_tokens, cumulative_cached_read_tokens,
            cumulative_cached_write_tokens, cumulative_total_tokens,
            last_usage_execution_id, last_observation_sequence, created_at_ms, updated_at_ms
        )
        SELECT
            usage_stream_id, session_id, binding_id, last_activation_generation,
            agent_id, provider_profile_id, auth_source_kind, auth_source_id,
            auth_source_revision,
            CASE
                WHEN auth_source_kind = 'agent_account' AND last_model_id = 'agent_default'
                    THEN NULL
                ELSE last_model_id
            END,
            reset_epoch, counter_origin,
            cumulative_input_tokens, cumulative_output_tokens,
            cumulative_thought_tokens, cumulative_cached_read_tokens,
            cumulative_cached_write_tokens, cumulative_total_tokens,
            last_usage_execution_id, last_observation_sequence, created_at_ms, updated_at_ms
        FROM agent_usage_checkpoints;
        DROP TABLE agent_usage_checkpoints;
        ALTER TABLE agent_usage_checkpoints_v47 RENAME TO agent_usage_checkpoints;
        CREATE INDEX idx_agent_usage_checkpoints_session
            ON agent_usage_checkpoints(session_id, updated_at_ms);

        CREATE TABLE agent_turn_usage_facts_v47 (
            usage_execution_id TEXT PRIMARY KEY,
            message_submission_id TEXT NULL,
            session_id TEXT NOT NULL
                REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
            project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
            workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
            binding_id TEXT NOT NULL,
            activation_generation INTEGER NOT NULL CHECK(activation_generation >= 0),
            reset_epoch INTEGER NOT NULL CHECK(reset_epoch >= 0),
            agent_id TEXT NOT NULL,
            provider_profile_id TEXT NULL,
            auth_source_kind TEXT NOT NULL,
            auth_source_id TEXT NOT NULL,
            auth_source_revision INTEGER NOT NULL CHECK(auth_source_revision >= 0),
            model_id TEXT NULL,
            execution_status TEXT NOT NULL,
            input_delta INTEGER NULL CHECK(input_delta >= 0),
            output_delta INTEGER NULL CHECK(output_delta >= 0),
            thought_delta INTEGER NULL CHECK(thought_delta >= 0),
            cached_read_delta INTEGER NULL CHECK(cached_read_delta >= 0),
            cached_write_delta INTEGER NULL CHECK(cached_write_delta >= 0),
            total_delta INTEGER NULL CHECK(total_delta >= 0),
            cumulative_input_after INTEGER NULL CHECK(cumulative_input_after >= 0),
            cumulative_output_after INTEGER NULL CHECK(cumulative_output_after >= 0),
            cumulative_thought_after INTEGER NULL CHECK(cumulative_thought_after >= 0),
            cumulative_cached_read_after INTEGER NULL CHECK(cumulative_cached_read_after >= 0),
            cumulative_cached_write_after INTEGER NULL CHECK(cumulative_cached_write_after >= 0),
            cumulative_total_after INTEGER NULL CHECK(cumulative_total_after >= 0),
            context_window_used_tokens INTEGER NULL CHECK(context_window_used_tokens >= 0),
            context_window_size_tokens INTEGER NULL CHECK(context_window_size_tokens > 0),
            reported_fields INTEGER NOT NULL CHECK(reported_fields >= 0),
            coverage TEXT NOT NULL,
            last_source TEXT NULL,
            reset_reason TEXT NULL,
            dispatched_at_ms INTEGER NOT NULL,
            completed_at_ms INTEGER NULL,
            last_observed_at_ms INTEGER NULL,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            CHECK(model_id IS NOT NULL OR auth_source_kind = 'agent_account'),
            CHECK(
                (auth_source_kind = 'provider_profile'
                    AND provider_profile_id = auth_source_id)
                OR
                (auth_source_kind = 'agent_account'
                    AND provider_profile_id IS NULL)
            )
        );
        INSERT INTO agent_turn_usage_facts_v47 (
            usage_execution_id, message_submission_id, session_id, project_id,
            workspace_id, binding_id, activation_generation, reset_epoch, agent_id,
            provider_profile_id, auth_source_kind, auth_source_id, auth_source_revision,
            model_id, execution_status, input_delta, output_delta, thought_delta,
            cached_read_delta, cached_write_delta, total_delta, cumulative_input_after,
            cumulative_output_after, cumulative_thought_after, cumulative_cached_read_after,
            cumulative_cached_write_after, cumulative_total_after, context_window_used_tokens,
            context_window_size_tokens, reported_fields, coverage, last_source, reset_reason,
            dispatched_at_ms, completed_at_ms, last_observed_at_ms, created_at_ms, updated_at_ms
        )
        SELECT
            usage_execution_id, message_submission_id, session_id, project_id,
            workspace_id, binding_id, activation_generation, reset_epoch, agent_id,
            provider_profile_id, auth_source_kind, auth_source_id, auth_source_revision,
            CASE
                WHEN auth_source_kind = 'agent_account' AND model_id = 'agent_default'
                    THEN NULL
                ELSE model_id
            END,
            execution_status, input_delta, output_delta, thought_delta,
            cached_read_delta, cached_write_delta, total_delta, cumulative_input_after,
            cumulative_output_after, cumulative_thought_after, cumulative_cached_read_after,
            cumulative_cached_write_after, cumulative_total_after, context_window_used_tokens,
            context_window_size_tokens, reported_fields, coverage, last_source, reset_reason,
            dispatched_at_ms, completed_at_ms, last_observed_at_ms, created_at_ms, updated_at_ms
        FROM agent_turn_usage_facts;
        DROP TABLE agent_turn_usage_facts;
        ALTER TABLE agent_turn_usage_facts_v47 RENAME TO agent_turn_usage_facts;
        CREATE INDEX idx_agent_turn_usage_facts_dispatch
            ON agent_turn_usage_facts(dispatched_at_ms, usage_execution_id);
        CREATE INDEX idx_agent_turn_usage_facts_session
            ON agent_turn_usage_facts(session_id, dispatched_at_ms);
        CREATE INDEX idx_agent_turn_usage_facts_project
            ON agent_turn_usage_facts(project_id, dispatched_at_ms);
        CREATE INDEX idx_agent_turn_usage_facts_agent
            ON agent_turn_usage_facts(agent_id, dispatched_at_ms);
        CREATE INDEX idx_agent_turn_usage_facts_auth_source
            ON agent_turn_usage_facts(auth_source_kind, auth_source_id, dispatched_at_ms);
        CREATE INDEX idx_agent_turn_usage_facts_profile
            ON agent_turn_usage_facts(provider_profile_id, dispatched_at_ms)
            WHERE provider_profile_id IS NOT NULL;
        CREATE INDEX idx_agent_turn_usage_facts_model
            ON agent_turn_usage_facts(model_id, dispatched_at_ms)
            WHERE model_id IS NOT NULL;
        ",
    )
    .map_err(storage_err(
        "migration_apply_failed",
        "failed to make Agent usage model attribution nullable",
    ))?;

    let foreign_key_violation = tx
        .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
        .optional()
        .map_err(storage_err(
            "migration_foreign_key_check_failed",
            "failed to validate Agent usage model migration foreign keys",
        ))?;
    if foreign_key_violation.is_some() {
        return Err(VibexError::storage(
            "migration_foreign_key_violation",
            "Agent usage model migration would violate a foreign key",
        ));
    }
    tx.execute(
        "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
        params![VERSION, NAME, unix_timestamp_ms()],
    )
    .map_err(storage_err(
        "migration_record_failed",
        "failed to record Agent usage model migration",
    ))?;
    tx.commit().map_err(storage_err(
        "migration_commit_failed",
        "failed to commit Agent usage model migration",
    ))?;
    applied.push(format!("{VERSION}:{NAME}"));
    Ok(())
}

/// Records which reporting contract each turn's usage was accounted under, and
/// how many API requests the turn actually made.
///
/// Runs after the v47 table rebuild because that rebuild recreates
/// `agent_turn_usage_facts` from a literal definition and would drop a column
/// added by an earlier migration. Existing rows keep `session` — the contract
/// they were actually computed under — so the read path can tell a legacy row
/// from one written with the contract known and repair it from the raw reading.
fn apply_usage_counter_scope_column(
    conn: &mut Connection,
    applied: &mut Vec<String>,
) -> VibexResult<()> {
    const VERSION: i64 = 48;
    const NAME: &str = "agent_usage_counter_scope";
    if migration_applied(conn, VERSION)? {
        return Ok(());
    }

    let tx = conn.transaction().map_err(storage_err(
        "migration_transaction_failed",
        "failed to start Agent usage counter scope migration transaction",
    ))?;
    tx.execute_batch(
        "
        ALTER TABLE agent_turn_usage_facts
            ADD COLUMN counter_scope TEXT NOT NULL DEFAULT 'session';
        ALTER TABLE agent_turn_usage_facts
            ADD COLUMN api_requests INTEGER NULL CHECK(api_requests >= 0);
        ",
    )
    .map_err(storage_err(
        "migration_apply_failed",
        "failed to add the Agent usage counter scope column",
    ))?;
    tx.execute(
        "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
        params![VERSION, NAME, unix_timestamp_ms()],
    )
    .map_err(storage_err(
        "migration_record_failed",
        "failed to record Agent usage counter scope migration",
    ))?;
    tx.commit().map_err(storage_err(
        "migration_commit_failed",
        "failed to commit Agent usage counter scope migration",
    ))?;
    applied.push(format!("{VERSION}:{NAME}"));
    Ok(())
}

pub fn current_schema_version(conn: &Connection) -> VibexResult<i64> {
    let version = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(storage_err(
            "schema_version_read_failed",
            "failed to read schema version",
        ))?;
    Ok(version)
}

pub fn run_smoke(path: &Path) -> VibexResult<DatabaseSmokeResult> {
    let mut conn = open_database(path)?;
    let applied_migrations = apply_migrations(&mut conn)?;
    let marker = format!("vibex-db-smoke-{}", unix_timestamp_ms());

    conn.execute(
        "
        INSERT INTO foundation_smoke (id, marker, last_seen_at_ms)
        VALUES (1, ?1, ?2)
        ON CONFLICT(id) DO UPDATE SET
            marker = excluded.marker,
            last_seen_at_ms = excluded.last_seen_at_ms
        ",
        params![marker, unix_timestamp_ms()],
    )
    .map_err(storage_err(
        "sentinel_write_failed",
        "failed to write Stage 0 sentinel row",
    ))?;

    let read_back: String = conn
        .query_row(
            "SELECT marker FROM foundation_smoke WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(storage_err(
            "sentinel_read_failed",
            "failed to read Stage 0 sentinel row",
        ))?;

    if read_back != marker {
        return Err(VibexError::storage(
            "sentinel_mismatch",
            "database smoke marker did not round-trip",
        ));
    }

    Ok(DatabaseSmokeResult {
        database_path: path.to_path_buf(),
        schema_version: current_schema_version(&conn)?,
        applied_migrations,
        marker,
    })
}

#[allow(clippy::too_many_arguments)]
fn append_timeline_in_transaction(
    tx: &Transaction<'_>,
    session_id: &VibexSessionId,
    source: TimelineSource,
    payload: TimelinePayload,
    timestamp_ms: Option<i64>,
    correlation_id: Option<&vibex_core::CorrelationId>,
    provider_correlation_id: Option<&str>,
    redaction_state: TimelineRedactionState,
    execution_attribution: Option<&TurnExecutionAttribution>,
) -> VibexResult<TimelineItem> {
    let sequence = tx
        .query_row(
            "
            SELECT COALESCE(MAX(sequence), 0) + 1
            FROM agent_timeline_items
            WHERE session_id = ?1
            ",
            params![session_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(storage_err(
            "timeline_sequence_failed",
            "failed to allocate timeline sequence",
        ))?;
    let timestamp_ms = timestamp_ms.unwrap_or_else(unix_timestamp_ms);
    let item = TimelineItem {
        id: TimelineItemId::new(),
        session_id: session_id.clone(),
        sequence,
        timestamp_ms,
        source,
        kind: payload.kind(),
        correlation_id: correlation_id.cloned(),
        provider_correlation_id: provider_correlation_id.map(ToOwned::to_owned),
        redaction_state,
        execution_attribution: execution_attribution.map(TurnExecutionAttribution::view),
        payload,
    };

    tx.execute(
        "
        INSERT INTO agent_timeline_items (
            session_id, sequence, timeline_item_id, kind, source, timestamp_ms,
            correlation_id, provider_correlation_id, payload_json, redaction_state,
            created_at_ms, execution_attribution_json
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ",
        params![
            item.session_id.as_str(),
            item.sequence,
            item.id.as_str(),
            enum_to_db(&item.kind)?,
            enum_to_db(&item.source)?,
            item.timestamp_ms,
            item.correlation_id.as_ref().map(|value| value.as_str()),
            item.provider_correlation_id,
            json_to_db(&item.payload)?,
            enum_to_db(&item.redaction_state)?,
            unix_timestamp_ms(),
            execution_attribution.map(json_to_db).transpose()?
        ],
    )
    .map_err(|err| {
        VibexError::storage("timeline_insert_failed", "failed to insert timeline item")
            .with_diagnostic("error", err.to_string())
            .with_diagnostic("sessionId", session_id.as_str())
            .with_diagnostic("sequence", sequence.to_string())
            .with_diagnostic("kind", format!("{:?}", item.kind))
            .with_diagnostic("source", format!("{:?}", item.source))
    })?;

    Ok(item)
}

fn migration_applied(conn: &Connection, version: i64) -> VibexResult<bool> {
    let value = conn
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = ?1",
            params![version],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(storage_err(
            "migration_lookup_failed",
            "failed to inspect migration state",
        ))?;
    Ok(value.is_some())
}

fn ensure_safe_database_path(path: &Path) -> VibexResult<()> {
    if path.as_os_str().is_empty() {
        return Err(VibexError::validation(
            "empty_database_path",
            "database path must not be empty",
        ));
    }

    if path.file_name().is_none() {
        return Err(VibexError::validation(
            "database_path_without_file",
            "database path must include a file name",
        ));
    }

    Ok(())
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn storage_err(
    code: &'static str,
    message: &'static str,
) -> impl Fn(rusqlite::Error) -> VibexError {
    move |err| VibexError::storage(code, message).with_diagnostic("error", err.to_string())
}

fn json_to_db<T: Serialize + ?Sized>(value: &T) -> VibexResult<String> {
    serde_json::to_string(value).map_err(|err| {
        VibexError::storage("json_encode_failed", "failed to encode JSON payload")
            .with_diagnostic("error", err.to_string())
    })
}

fn json_from_db<T: DeserializeOwned>(value: String) -> VibexResult<T> {
    serde_json::from_str(&value).map_err(|err| {
        VibexError::storage("json_decode_failed", "failed to decode JSON payload")
            .with_diagnostic("error", err.to_string())
    })
}

fn enum_to_db<T: Serialize>(value: &T) -> VibexResult<String> {
    match serde_json::to_value(value).map_err(|err| {
        VibexError::storage("enum_encode_failed", "failed to encode enum value")
            .with_diagnostic("error", err.to_string())
    })? {
        serde_json::Value::String(value) => Ok(value),
        other => Err(VibexError::storage(
            "enum_encode_failed",
            "expected enum to serialize as string",
        )
        .with_diagnostic("value", other.to_string())),
    }
}

fn enum_from_db<T: DeserializeOwned>(value: String) -> VibexResult<T> {
    serde_json::from_value(serde_json::Value::String(value)).map_err(|err| {
        VibexError::storage("enum_decode_failed", "failed to decode enum value")
            .with_diagnostic("error", err.to_string())
    })
}

fn parse_id<T>(value: String, parser: impl FnOnce(String) -> VibexResult<T>) -> VibexResult<T> {
    parser(value).map_err(|err| {
        VibexError::storage("id_decode_failed", "failed to decode stored id")
            .with_diagnostic("error", err.to_string())
    })
}

fn sql_decode<T>(value: VibexResult<T>) -> rusqlite::Result<T> {
    value.map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
    })
}

fn parse_id_sql<T>(
    value: String,
    parser: impl FnOnce(String) -> VibexResult<T>,
) -> rusqlite::Result<T> {
    sql_decode(parse_id(value, parser))
}

fn parse_optional_id_sql<T>(
    value: Option<String>,
    parser: impl Fn(String) -> VibexResult<T>,
) -> rusqlite::Result<Option<T>> {
    value.map(parser).transpose().map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
    })
}

fn enum_from_db_sql<T: DeserializeOwned>(value: String) -> rusqlite::Result<T> {
    sql_decode(enum_from_db(value))
}

fn optional_enum_from_db_sql<T: DeserializeOwned>(
    value: Option<String>,
) -> rusqlite::Result<Option<T>> {
    value.map(enum_from_db).transpose().map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
    })
}

fn json_from_db_sql<T: DeserializeOwned>(value: String) -> rusqlite::Result<T> {
    sql_decode(json_from_db(value))
}

fn optional_json_from_db_sql<T: DeserializeOwned>(
    value: Option<String>,
) -> rusqlite::Result<Option<T>> {
    value.map(json_from_db).transpose().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn validate_worktree_operation_token<'a>(
    value: Option<&'a str>,
    code: &'static str,
    message: &'static str,
) -> VibexResult<&'a str> {
    let value = value.ok_or_else(|| VibexError::validation(code, message))?;
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(VibexError::validation(code, message));
    }
    Ok(value)
}

fn collect_rows<T>(
    rows: impl IntoIterator<Item = rusqlite::Result<T>>,
    code: &'static str,
    message: &'static str,
) -> VibexResult<Vec<T>> {
    let mut output = Vec::new();
    for row in rows {
        output.push(row.map_err(storage_err(code, message))?);
    }
    Ok(output)
}

fn bounded_limit(limit: Option<u32>) -> i64 {
    limit
        .map(i64::from)
        .unwrap_or(SCHEDULED_TASK_LIST_DEFAULT_LIMIT)
        .clamp(1, SCHEDULED_TASK_LIST_MAX_LIMIT)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn bound_scheduled_task_diagnostics(
    diagnostics: Vec<RedactedDiagnostic>,
) -> Vec<RedactedDiagnostic> {
    diagnostics
        .into_iter()
        .map(|diagnostic| RedactedDiagnostic {
            key: truncate_chars(&diagnostic.key, SCHEDULED_TASK_DIAGNOSTIC_KEY_MAX_CHARS),
            value: truncate_chars(&diagnostic.value, SCHEDULED_TASK_DIAGNOSTIC_VALUE_MAX_CHARS),
        })
        .collect()
}

fn insert_scheduled_task(conn: &Connection, task: &ScheduledTask) -> VibexResult<()> {
    conn.execute(
        "
        INSERT INTO scheduled_tasks (
            scheduled_task_id, title, prompt, project_id, workspace_id, workspace_root,
            workspace_mode, provider_kind, provider_profile_id, schedule_json, status,
            safety_json, next_run_at_ms, created_at_ms, updated_at_ms, deleted_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
        ",
        params![
            task.id.as_str(),
            task.title,
            task.prompt,
            task.project_id.as_ref().map(ProjectId::as_str),
            task.workspace_id.as_ref().map(WorkspaceId::as_str),
            task.workspace_root,
            enum_to_db(&task.workspace_mode)?,
            enum_to_db(&task.provider_kind)?,
            task.provider_profile_id
                .as_ref()
                .map(ProviderProfileId::as_str),
            json_to_db(&task.schedule)?,
            enum_to_db(&task.status)?,
            json_to_db(&task.safety)?,
            task.next_run_at_ms,
            task.created_at_ms,
            task.updated_at_ms,
            task.deleted_at_ms
        ],
    )
    .map_err(storage_err(
        "scheduled_task_insert_failed",
        "failed to insert scheduled task",
    ))?;
    Ok(())
}

fn update_scheduled_task(conn: &Connection, task: &ScheduledTask) -> VibexResult<()> {
    conn.execute(
        "
        UPDATE scheduled_tasks
        SET title = ?2,
            prompt = ?3,
            project_id = ?4,
            workspace_id = ?5,
            workspace_root = ?6,
            workspace_mode = ?7,
            provider_kind = ?8,
            provider_profile_id = ?9,
            schedule_json = ?10,
            status = ?11,
            safety_json = ?12,
            next_run_at_ms = ?13,
            updated_at_ms = ?14,
            deleted_at_ms = ?15
        WHERE scheduled_task_id = ?1
        ",
        params![
            task.id.as_str(),
            task.title,
            task.prompt,
            task.project_id.as_ref().map(ProjectId::as_str),
            task.workspace_id.as_ref().map(WorkspaceId::as_str),
            task.workspace_root,
            enum_to_db(&task.workspace_mode)?,
            enum_to_db(&task.provider_kind)?,
            task.provider_profile_id
                .as_ref()
                .map(ProviderProfileId::as_str),
            json_to_db(&task.schedule)?,
            enum_to_db(&task.status)?,
            json_to_db(&task.safety)?,
            task.next_run_at_ms,
            task.updated_at_ms,
            task.deleted_at_ms
        ],
    )
    .map_err(storage_err(
        "scheduled_task_update_failed",
        "failed to update scheduled task",
    ))?;
    Ok(())
}

fn update_scheduled_task_status(
    conn: &Connection,
    task_id: &ScheduledTaskId,
    status: ScheduledTaskStatus,
) -> VibexResult<ScheduledTask> {
    let mut task = require_scheduled_task(conn, task_id, false)?;
    task.status = status;
    task.updated_at_ms = unix_timestamp_ms();
    update_scheduled_task(conn, &task)?;
    Ok(task)
}

fn get_scheduled_task(
    conn: &Connection,
    task_id: &ScheduledTaskId,
    include_deleted: bool,
) -> VibexResult<Option<ScheduledTask>> {
    conn.query_row(
        "
        SELECT scheduled_task_id, title, prompt, project_id, workspace_id,
            workspace_root, workspace_mode, provider_kind, provider_profile_id,
            schedule_json, status, safety_json, next_run_at_ms, created_at_ms,
            updated_at_ms, deleted_at_ms
        FROM scheduled_tasks
        WHERE scheduled_task_id = ?1
            AND (?2 = 1 OR deleted_at_ms IS NULL)
        ",
        params![
            task_id.as_str(),
            if include_deleted { 1_i64 } else { 0_i64 }
        ],
        map_scheduled_task,
    )
    .optional()
    .map_err(storage_err(
        "scheduled_task_lookup_failed",
        "failed to lookup scheduled task",
    ))
}

fn require_scheduled_task(
    conn: &Connection,
    task_id: &ScheduledTaskId,
    include_deleted: bool,
) -> VibexResult<ScheduledTask> {
    get_scheduled_task(conn, task_id, include_deleted)?.ok_or_else(|| {
        VibexError::storage("scheduled_task_not_found", "scheduled task was not found")
            .with_diagnostic("scheduledTaskId", task_id.as_str())
    })
}

fn insert_scheduled_task_run(conn: &Connection, run: &ScheduledTaskRun) -> VibexResult<()> {
    conn.execute(
        "
        INSERT INTO scheduled_task_runs (
            scheduled_task_run_id, scheduled_task_id, status, trigger, session_id,
            due_at_ms, started_at_ms, ended_at_ms, attempt, error_code, error_message,
            redacted_diagnostics_json, created_at_ms, updated_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
        ",
        params![
            run.id.as_str(),
            run.task_id.as_str(),
            enum_to_db(&run.status)?,
            enum_to_db(&run.trigger)?,
            run.session_id.as_ref().map(VibexSessionId::as_str),
            run.due_at_ms,
            run.started_at_ms,
            run.ended_at_ms,
            i64::from(run.attempt),
            run.error_code,
            run.error_message,
            json_to_db(&run.redacted_diagnostics)?,
            run.created_at_ms,
            run.updated_at_ms
        ],
    )
    .map_err(storage_err(
        "scheduled_task_run_insert_failed",
        "failed to insert scheduled task run",
    ))?;
    Ok(())
}

fn update_scheduled_task_run(conn: &Connection, run: &ScheduledTaskRun) -> VibexResult<()> {
    conn.execute(
        "
        UPDATE scheduled_task_runs
        SET status = ?2,
            session_id = ?3,
            started_at_ms = ?4,
            ended_at_ms = ?5,
            attempt = ?6,
            error_code = ?7,
            error_message = ?8,
            redacted_diagnostics_json = ?9,
            updated_at_ms = ?10
        WHERE scheduled_task_run_id = ?1
        ",
        params![
            run.id.as_str(),
            enum_to_db(&run.status)?,
            run.session_id.as_ref().map(VibexSessionId::as_str),
            run.started_at_ms,
            run.ended_at_ms,
            i64::from(run.attempt),
            run.error_code,
            run.error_message,
            json_to_db(&run.redacted_diagnostics)?,
            run.updated_at_ms
        ],
    )
    .map_err(storage_err(
        "scheduled_task_run_update_failed",
        "failed to update scheduled task run",
    ))?;
    Ok(())
}

fn get_scheduled_task_run(
    conn: &Connection,
    run_id: &ScheduledTaskRunId,
) -> VibexResult<Option<ScheduledTaskRun>> {
    conn.query_row(
        "
        SELECT scheduled_task_run_id, scheduled_task_id, status, trigger,
            session_id, due_at_ms, started_at_ms, ended_at_ms, attempt,
            error_code, error_message, redacted_diagnostics_json,
            created_at_ms, updated_at_ms
        FROM scheduled_task_runs
        WHERE scheduled_task_run_id = ?1
        ",
        params![run_id.as_str()],
        map_scheduled_task_run,
    )
    .optional()
    .map_err(storage_err(
        "scheduled_task_run_lookup_failed",
        "failed to lookup scheduled task run",
    ))
}

fn require_scheduled_task_run(
    conn: &Connection,
    run_id: &ScheduledTaskRunId,
) -> VibexResult<ScheduledTaskRun> {
    get_scheduled_task_run(conn, run_id)?.ok_or_else(|| {
        VibexError::storage(
            "scheduled_task_run_not_found",
            "scheduled task run was not found",
        )
        .with_diagnostic("scheduledTaskRunId", run_id.as_str())
    })
}

fn automation_nodes_from_requests(
    graph_id: &AutomationGraphId,
    requests: Vec<AutomationNodeCreateRequest>,
    now: i64,
) -> Vec<AutomationNode> {
    requests
        .into_iter()
        .map(|request| AutomationNode {
            id: request.id.unwrap_or_else(AutomationNodeId::new),
            graph_id: graph_id.clone(),
            kind: request.kind,
            title: request.title,
            config: request.config,
            position: request.position,
            created_at_ms: now,
            updated_at_ms: now,
        })
        .collect()
}

fn automation_edges_from_requests(
    graph_id: &AutomationGraphId,
    requests: Vec<AutomationEdgeCreateRequest>,
    now: i64,
) -> Vec<AutomationEdge> {
    requests
        .into_iter()
        .map(|request| AutomationEdge {
            id: AutomationEdgeId::new(),
            graph_id: graph_id.clone(),
            source_node_id: request.source_node_id,
            target_node_id: request.target_node_id,
            condition: request.condition,
            created_at_ms: now,
            updated_at_ms: now,
        })
        .collect()
}

fn validate_automation_edges(
    nodes: &[AutomationNode],
    edges: &[AutomationEdge],
) -> VibexResult<()> {
    let node_ids = nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    for edge in edges {
        if !node_ids.contains(&edge.source_node_id) {
            return Err(VibexError::validation(
                "automation_graph_edge_source_missing",
                "automation graph edge source node is not part of the graph definition",
            )
            .with_diagnostic("sourceNodeId", edge.source_node_id.as_str()));
        }
        if !node_ids.contains(&edge.target_node_id) {
            return Err(VibexError::validation(
                "automation_graph_edge_target_missing",
                "automation graph edge target node is not part of the graph definition",
            )
            .with_diagnostic("targetNodeId", edge.target_node_id.as_str()));
        }
    }
    Ok(())
}

fn insert_automation_graph(conn: &Connection, graph: &AutomationGraph) -> VibexResult<()> {
    conn.execute(
        "
        INSERT INTO automation_graphs (
            automation_graph_id, title, description, project_id, workspace_id,
            workspace_root, workspace_mode, provider_kind, provider_profile_id,
            trigger_json, status, version, created_at_ms, updated_at_ms, deleted_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
        ",
        params![
            graph.id.as_str(),
            graph.title,
            graph.description,
            graph.project_id.as_ref().map(ProjectId::as_str),
            graph.workspace_id.as_ref().map(WorkspaceId::as_str),
            graph.workspace_root,
            enum_to_db(&graph.workspace_mode)?,
            graph.provider_kind.as_ref().map(enum_to_db).transpose()?,
            graph
                .provider_profile_id
                .as_ref()
                .map(ProviderProfileId::as_str),
            json_to_db(&graph.trigger)?,
            enum_to_db(&graph.status)?,
            i64::from(graph.version),
            graph.created_at_ms,
            graph.updated_at_ms,
            graph.deleted_at_ms
        ],
    )
    .map_err(storage_err(
        "automation_graph_insert_failed",
        "failed to insert automation graph",
    ))?;
    Ok(())
}

fn update_automation_graph(conn: &Connection, graph: &AutomationGraph) -> VibexResult<()> {
    conn.execute(
        "
        UPDATE automation_graphs
        SET title = ?2,
            description = ?3,
            project_id = ?4,
            workspace_id = ?5,
            workspace_root = ?6,
            workspace_mode = ?7,
            provider_kind = ?8,
            provider_profile_id = ?9,
            trigger_json = ?10,
            status = ?11,
            version = ?12,
            updated_at_ms = ?13,
            deleted_at_ms = ?14
        WHERE automation_graph_id = ?1
        ",
        params![
            graph.id.as_str(),
            graph.title,
            graph.description,
            graph.project_id.as_ref().map(ProjectId::as_str),
            graph.workspace_id.as_ref().map(WorkspaceId::as_str),
            graph.workspace_root,
            enum_to_db(&graph.workspace_mode)?,
            graph.provider_kind.as_ref().map(enum_to_db).transpose()?,
            graph
                .provider_profile_id
                .as_ref()
                .map(ProviderProfileId::as_str),
            json_to_db(&graph.trigger)?,
            enum_to_db(&graph.status)?,
            i64::from(graph.version),
            graph.updated_at_ms,
            graph.deleted_at_ms
        ],
    )
    .map_err(storage_err(
        "automation_graph_update_failed",
        "failed to update automation graph",
    ))?;
    Ok(())
}

fn insert_automation_node(conn: &Connection, node: &AutomationNode) -> VibexResult<()> {
    conn.execute(
        "
        INSERT INTO automation_graph_nodes (
            automation_node_id, automation_graph_id, kind, title, config_json,
            position_json, created_at_ms, updated_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ",
        params![
            node.id.as_str(),
            node.graph_id.as_str(),
            enum_to_db(&node.kind)?,
            node.title,
            json_to_db(&node.config)?,
            node.position.as_ref().map(json_to_db).transpose()?,
            node.created_at_ms,
            node.updated_at_ms
        ],
    )
    .map_err(storage_err(
        "automation_node_insert_failed",
        "failed to insert automation graph node",
    ))?;
    Ok(())
}

fn insert_automation_edge(conn: &Connection, edge: &AutomationEdge) -> VibexResult<()> {
    conn.execute(
        "
        INSERT INTO automation_graph_edges (
            automation_edge_id, automation_graph_id, source_node_id, target_node_id,
            condition_json, created_at_ms, updated_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ",
        params![
            edge.id.as_str(),
            edge.graph_id.as_str(),
            edge.source_node_id.as_str(),
            edge.target_node_id.as_str(),
            json_to_db(&edge.condition)?,
            edge.created_at_ms,
            edge.updated_at_ms
        ],
    )
    .map_err(storage_err(
        "automation_edge_insert_failed",
        "failed to insert automation graph edge",
    ))?;
    Ok(())
}

fn get_automation_graph(
    conn: &Connection,
    graph_id: &AutomationGraphId,
    include_deleted: bool,
) -> VibexResult<Option<AutomationGraph>> {
    let mut graph = conn
        .query_row(
            "
            SELECT automation_graph_id, title, description, project_id, workspace_id,
                workspace_root, workspace_mode, provider_kind, provider_profile_id,
                trigger_json, status, version, created_at_ms, updated_at_ms, deleted_at_ms
            FROM automation_graphs
            WHERE automation_graph_id = ?1
                AND (?2 = 1 OR deleted_at_ms IS NULL)
            ",
            params![
                graph_id.as_str(),
                if include_deleted { 1_i64 } else { 0_i64 }
            ],
            map_automation_graph,
        )
        .optional()
        .map_err(storage_err(
            "automation_graph_lookup_failed",
            "failed to lookup automation graph",
        ))?;
    if let Some(graph) = &mut graph {
        graph.nodes = list_automation_nodes(conn, &graph.id)?;
        graph.edges = list_automation_edges(conn, &graph.id)?;
    }
    Ok(graph)
}

fn require_automation_graph(
    conn: &Connection,
    graph_id: &AutomationGraphId,
    include_deleted: bool,
) -> VibexResult<AutomationGraph> {
    get_automation_graph(conn, graph_id, include_deleted)?.ok_or_else(|| {
        VibexError::storage(
            "automation_graph_not_found",
            "automation graph was not found",
        )
        .with_diagnostic("automationGraphId", graph_id.as_str())
    })
}

fn list_automation_nodes(
    conn: &Connection,
    graph_id: &AutomationGraphId,
) -> VibexResult<Vec<AutomationNode>> {
    let mut stmt = conn
        .prepare(
            "
            SELECT automation_node_id, automation_graph_id, kind, title, config_json,
                position_json, created_at_ms, updated_at_ms
            FROM automation_graph_nodes
            WHERE automation_graph_id = ?1
            ORDER BY created_at_ms ASC, automation_node_id ASC
            ",
        )
        .map_err(storage_err(
            "automation_node_list_failed",
            "failed to list automation graph nodes",
        ))?;
    let rows = stmt
        .query_map(params![graph_id.as_str()], map_automation_node)
        .map_err(storage_err(
            "automation_node_list_failed",
            "failed to list automation graph nodes",
        ))?;
    collect_rows(
        rows,
        "automation_node_decode_failed",
        "failed to decode automation graph node row",
    )
}

fn list_automation_edges(
    conn: &Connection,
    graph_id: &AutomationGraphId,
) -> VibexResult<Vec<AutomationEdge>> {
    let mut stmt = conn
        .prepare(
            "
            SELECT automation_edge_id, automation_graph_id, source_node_id, target_node_id,
                condition_json, created_at_ms, updated_at_ms
            FROM automation_graph_edges
            WHERE automation_graph_id = ?1
            ORDER BY created_at_ms ASC, automation_edge_id ASC
            ",
        )
        .map_err(storage_err(
            "automation_edge_list_failed",
            "failed to list automation graph edges",
        ))?;
    let rows = stmt
        .query_map(params![graph_id.as_str()], map_automation_edge)
        .map_err(storage_err(
            "automation_edge_list_failed",
            "failed to list automation graph edges",
        ))?;
    collect_rows(
        rows,
        "automation_edge_decode_failed",
        "failed to decode automation graph edge row",
    )
}

fn insert_automation_run(conn: &Connection, run: &AutomationRun) -> VibexResult<()> {
    conn.execute(
        "
        INSERT INTO automation_graph_runs (
            automation_run_id, automation_graph_id, status, trigger, scheduled_task_id,
            session_id, started_at_ms, ended_at_ms, error_code, error_message,
            redacted_diagnostics_json, created_at_ms, updated_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        ",
        params![
            run.id.as_str(),
            run.graph_id.as_str(),
            enum_to_db(&run.status)?,
            enum_to_db(&run.trigger)?,
            run.scheduled_task_id.as_ref().map(ScheduledTaskId::as_str),
            run.session_id.as_ref().map(VibexSessionId::as_str),
            run.started_at_ms,
            run.ended_at_ms,
            run.error_code,
            run.error_message,
            json_to_db(&run.redacted_diagnostics)?,
            run.created_at_ms,
            run.updated_at_ms
        ],
    )
    .map_err(storage_err(
        "automation_run_insert_failed",
        "failed to insert automation graph run",
    ))?;
    Ok(())
}

fn update_automation_run(conn: &Connection, run: &AutomationRun) -> VibexResult<()> {
    conn.execute(
        "
        UPDATE automation_graph_runs
        SET status = ?2,
            scheduled_task_id = ?3,
            session_id = ?4,
            started_at_ms = ?5,
            ended_at_ms = ?6,
            error_code = ?7,
            error_message = ?8,
            redacted_diagnostics_json = ?9,
            updated_at_ms = ?10
        WHERE automation_run_id = ?1
        ",
        params![
            run.id.as_str(),
            enum_to_db(&run.status)?,
            run.scheduled_task_id.as_ref().map(ScheduledTaskId::as_str),
            run.session_id.as_ref().map(VibexSessionId::as_str),
            run.started_at_ms,
            run.ended_at_ms,
            run.error_code,
            run.error_message,
            json_to_db(&run.redacted_diagnostics)?,
            run.updated_at_ms
        ],
    )
    .map_err(storage_err(
        "automation_run_update_failed",
        "failed to update automation graph run",
    ))?;
    Ok(())
}

fn get_automation_run(
    conn: &Connection,
    run_id: &AutomationRunId,
) -> VibexResult<Option<AutomationRun>> {
    conn.query_row(
        "
        SELECT automation_run_id, automation_graph_id, status, trigger,
            scheduled_task_id, session_id, started_at_ms, ended_at_ms,
            error_code, error_message, redacted_diagnostics_json,
            created_at_ms, updated_at_ms
        FROM automation_graph_runs
        WHERE automation_run_id = ?1
        ",
        params![run_id.as_str()],
        map_automation_run,
    )
    .optional()
    .map_err(storage_err(
        "automation_run_lookup_failed",
        "failed to lookup automation graph run",
    ))
}

fn require_automation_run(
    conn: &Connection,
    run_id: &AutomationRunId,
) -> VibexResult<AutomationRun> {
    get_automation_run(conn, run_id)?.ok_or_else(|| {
        VibexError::storage(
            "automation_run_not_found",
            "automation graph run was not found",
        )
        .with_diagnostic("automationRunId", run_id.as_str())
    })
}

fn insert_automation_run_step(conn: &Connection, step: &AutomationRunStep) -> VibexResult<()> {
    conn.execute(
        "
        INSERT INTO automation_graph_run_steps (
            automation_run_step_id, automation_run_id, automation_node_id, status,
            session_id, permission_request_id, started_at_ms, ended_at_ms, error_code,
            error_message, redacted_diagnostics_json, created_at_ms, updated_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        ",
        params![
            step.id.as_str(),
            step.run_id.as_str(),
            step.node_id.as_str(),
            enum_to_db(&step.status)?,
            step.session_id.as_ref().map(VibexSessionId::as_str),
            step.permission_request_id.as_ref().map(RequestId::as_str),
            step.started_at_ms,
            step.ended_at_ms,
            step.error_code,
            step.error_message,
            json_to_db(&step.redacted_diagnostics)?,
            step.created_at_ms,
            step.updated_at_ms
        ],
    )
    .map_err(storage_err(
        "automation_run_step_insert_failed",
        "failed to insert automation graph run step",
    ))?;
    Ok(())
}

fn update_automation_run_step(conn: &Connection, step: &AutomationRunStep) -> VibexResult<()> {
    conn.execute(
        "
        UPDATE automation_graph_run_steps
        SET status = ?2,
            session_id = ?3,
            permission_request_id = ?4,
            started_at_ms = ?5,
            ended_at_ms = ?6,
            error_code = ?7,
            error_message = ?8,
            redacted_diagnostics_json = ?9,
            updated_at_ms = ?10
        WHERE automation_run_step_id = ?1
        ",
        params![
            step.id.as_str(),
            enum_to_db(&step.status)?,
            step.session_id.as_ref().map(VibexSessionId::as_str),
            step.permission_request_id.as_ref().map(RequestId::as_str),
            step.started_at_ms,
            step.ended_at_ms,
            step.error_code,
            step.error_message,
            json_to_db(&step.redacted_diagnostics)?,
            step.updated_at_ms
        ],
    )
    .map_err(storage_err(
        "automation_run_step_update_failed",
        "failed to update automation graph run step",
    ))?;
    Ok(())
}

fn get_automation_run_step(
    conn: &Connection,
    step_id: &AutomationRunStepId,
) -> VibexResult<Option<AutomationRunStep>> {
    conn.query_row(
        "
        SELECT automation_run_step_id, automation_run_id, automation_node_id,
            status, session_id, permission_request_id, started_at_ms, ended_at_ms,
            error_code, error_message, redacted_diagnostics_json,
            created_at_ms, updated_at_ms
        FROM automation_graph_run_steps
        WHERE automation_run_step_id = ?1
        ",
        params![step_id.as_str()],
        map_automation_run_step,
    )
    .optional()
    .map_err(storage_err(
        "automation_run_step_lookup_failed",
        "failed to lookup automation graph run step",
    ))
}

fn require_automation_run_step(
    conn: &Connection,
    step_id: &AutomationRunStepId,
) -> VibexResult<AutomationRunStep> {
    get_automation_run_step(conn, step_id)?.ok_or_else(|| {
        VibexError::storage(
            "automation_run_step_not_found",
            "automation graph run step was not found",
        )
        .with_diagnostic("automationRunStepId", step_id.as_str())
    })
}

fn u32_from_sql(value: i64) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Integer, Box::new(err))
    })
}

fn map_scheduled_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledTask> {
    Ok(ScheduledTask {
        id: parse_id_sql(row.get(0)?, ScheduledTaskId::parse)?,
        title: row.get(1)?,
        prompt: row.get(2)?,
        project_id: parse_optional_id_sql(row.get::<_, Option<String>>(3)?, ProjectId::parse)?,
        workspace_id: parse_optional_id_sql(row.get::<_, Option<String>>(4)?, WorkspaceId::parse)?,
        workspace_root: row.get(5)?,
        workspace_mode: enum_from_db_sql(row.get(6)?)?,
        provider_kind: enum_from_db_sql(row.get(7)?)?,
        provider_profile_id: parse_optional_id_sql(
            row.get::<_, Option<String>>(8)?,
            ProviderProfileId::parse,
        )?,
        schedule: json_from_db_sql(row.get(9)?)?,
        status: enum_from_db_sql(row.get(10)?)?,
        safety: json_from_db_sql(row.get(11)?)?,
        next_run_at_ms: row.get(12)?,
        created_at_ms: row.get(13)?,
        updated_at_ms: row.get(14)?,
        deleted_at_ms: row.get(15)?,
    })
}

fn map_scheduled_task_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledTaskRun> {
    Ok(ScheduledTaskRun {
        id: parse_id_sql(row.get(0)?, ScheduledTaskRunId::parse)?,
        task_id: parse_id_sql(row.get(1)?, ScheduledTaskId::parse)?,
        status: enum_from_db_sql(row.get(2)?)?,
        trigger: enum_from_db_sql(row.get(3)?)?,
        session_id: parse_optional_id_sql(row.get::<_, Option<String>>(4)?, VibexSessionId::parse)?,
        due_at_ms: row.get(5)?,
        started_at_ms: row.get(6)?,
        ended_at_ms: row.get(7)?,
        attempt: u32_from_sql(row.get(8)?)?,
        error_code: row.get(9)?,
        error_message: row.get(10)?,
        redacted_diagnostics: json_from_db_sql(row.get(11)?)?,
        created_at_ms: row.get(12)?,
        updated_at_ms: row.get(13)?,
    })
}

fn map_automation_graph(row: &rusqlite::Row<'_>) -> rusqlite::Result<AutomationGraph> {
    Ok(AutomationGraph {
        id: parse_id_sql(row.get(0)?, AutomationGraphId::parse)?,
        title: row.get(1)?,
        description: row.get(2)?,
        project_id: parse_optional_id_sql(row.get::<_, Option<String>>(3)?, ProjectId::parse)?,
        workspace_id: parse_optional_id_sql(row.get::<_, Option<String>>(4)?, WorkspaceId::parse)?,
        workspace_root: row.get(5)?,
        workspace_mode: enum_from_db_sql(row.get(6)?)?,
        provider_kind: optional_enum_from_db_sql(row.get::<_, Option<String>>(7)?)?,
        provider_profile_id: parse_optional_id_sql(
            row.get::<_, Option<String>>(8)?,
            ProviderProfileId::parse,
        )?,
        trigger: json_from_db_sql(row.get(9)?)?,
        status: enum_from_db_sql(row.get(10)?)?,
        version: u32_from_sql(row.get(11)?)?,
        nodes: Vec::new(),
        edges: Vec::new(),
        created_at_ms: row.get(12)?,
        updated_at_ms: row.get(13)?,
        deleted_at_ms: row.get(14)?,
    })
}

fn map_automation_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<AutomationNode> {
    Ok(AutomationNode {
        id: parse_id_sql(row.get(0)?, AutomationNodeId::parse)?,
        graph_id: parse_id_sql(row.get(1)?, AutomationGraphId::parse)?,
        kind: enum_from_db_sql(row.get(2)?)?,
        title: row.get(3)?,
        config: json_from_db_sql(row.get(4)?)?,
        position: row
            .get::<_, Option<String>>(5)?
            .map(json_from_db)
            .transpose()
            .map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?,
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
    })
}

fn map_automation_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<AutomationEdge> {
    Ok(AutomationEdge {
        id: parse_id_sql(row.get(0)?, AutomationEdgeId::parse)?,
        graph_id: parse_id_sql(row.get(1)?, AutomationGraphId::parse)?,
        source_node_id: parse_id_sql(row.get(2)?, AutomationNodeId::parse)?,
        target_node_id: parse_id_sql(row.get(3)?, AutomationNodeId::parse)?,
        condition: json_from_db_sql(row.get(4)?)?,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
    })
}

fn map_automation_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<AutomationRun> {
    Ok(AutomationRun {
        id: parse_id_sql(row.get(0)?, AutomationRunId::parse)?,
        graph_id: parse_id_sql(row.get(1)?, AutomationGraphId::parse)?,
        status: enum_from_db_sql(row.get(2)?)?,
        trigger: enum_from_db_sql(row.get(3)?)?,
        scheduled_task_id: parse_optional_id_sql(
            row.get::<_, Option<String>>(4)?,
            ScheduledTaskId::parse,
        )?,
        session_id: parse_optional_id_sql(row.get::<_, Option<String>>(5)?, VibexSessionId::parse)?,
        started_at_ms: row.get(6)?,
        ended_at_ms: row.get(7)?,
        error_code: row.get(8)?,
        error_message: row.get(9)?,
        redacted_diagnostics: json_from_db_sql(row.get(10)?)?,
        created_at_ms: row.get(11)?,
        updated_at_ms: row.get(12)?,
    })
}

fn map_automation_run_step(row: &rusqlite::Row<'_>) -> rusqlite::Result<AutomationRunStep> {
    Ok(AutomationRunStep {
        id: parse_id_sql(row.get(0)?, AutomationRunStepId::parse)?,
        run_id: parse_id_sql(row.get(1)?, AutomationRunId::parse)?,
        node_id: parse_id_sql(row.get(2)?, AutomationNodeId::parse)?,
        status: enum_from_db_sql(row.get(3)?)?,
        session_id: parse_optional_id_sql(row.get::<_, Option<String>>(4)?, VibexSessionId::parse)?,
        permission_request_id: parse_optional_id_sql(
            row.get::<_, Option<String>>(5)?,
            RequestId::parse,
        )?,
        started_at_ms: row.get(6)?,
        ended_at_ms: row.get(7)?,
        error_code: row.get(8)?,
        error_message: row.get(9)?,
        redacted_diagnostics: json_from_db_sql(row.get(10)?)?,
        created_at_ms: row.get(11)?,
        updated_at_ms: row.get(12)?,
    })
}

fn map_scheduled_task_attention_summary(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ScheduledTaskAttentionSummary> {
    let task_id = parse_id_sql(row.get(0)?, ScheduledTaskId::parse)?;
    let task_title = row.get(1)?;
    let workspace_id = parse_optional_id_sql(row.get::<_, Option<String>>(2)?, WorkspaceId::parse)?;
    let workspace_root = row.get(3)?;
    let provider_kind = enum_from_db_sql(row.get(4)?)?;
    let provider_profile_id =
        parse_optional_id_sql(row.get::<_, Option<String>>(5)?, ProviderProfileId::parse)?;
    let run_id = parse_id_sql(row.get(6)?, ScheduledTaskRunId::parse)?;
    let status = enum_from_db_sql(row.get(7)?)?;
    let trigger = enum_from_db_sql(row.get(8)?)?;
    let session_id =
        parse_optional_id_sql(row.get::<_, Option<String>>(9)?, VibexSessionId::parse)?;
    let error_code: Option<String> = row.get(10)?;
    let error_message: Option<String> = row.get(11)?;
    let created_at_ms = row.get(12)?;
    let attention_kind = scheduled_task_attention_kind(status, error_code.as_deref())
        .ok_or_else(|| rusqlite::Error::InvalidQuery)?;

    Ok(ScheduledTaskAttentionSummary {
        task_id,
        task_title,
        run_id,
        workspace_id,
        workspace_root,
        provider_kind,
        provider_profile_id,
        trigger,
        status,
        attention_kind,
        session_id,
        error_code,
        error_message,
        created_at_ms,
    })
}

fn map_scheduled_task_audit_record(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ScheduledTaskAuditRecord> {
    let task_id = parse_id_sql(row.get(0)?, ScheduledTaskId::parse)?;
    let task_title = row.get(1)?;
    let workspace_id = parse_optional_id_sql(row.get::<_, Option<String>>(2)?, WorkspaceId::parse)?;
    let workspace_root = row.get(3)?;
    let provider_kind = enum_from_db_sql(row.get(4)?)?;
    let provider_profile_id =
        parse_optional_id_sql(row.get::<_, Option<String>>(5)?, ProviderProfileId::parse)?;
    let run_id = parse_id_sql(row.get(6)?, ScheduledTaskRunId::parse)?;
    let status = enum_from_db_sql(row.get(7)?)?;
    let trigger = enum_from_db_sql(row.get(8)?)?;
    let session_id =
        parse_optional_id_sql(row.get::<_, Option<String>>(9)?, VibexSessionId::parse)?;
    let error_code: Option<String> = row.get(10)?;
    let error_message: Option<String> = row.get(11)?;
    let redacted_diagnostics = json_from_db_sql(row.get(12)?)?;
    let created_at_ms = row.get(13)?;

    Ok(ScheduledTaskAuditRecord {
        audit_id: format!("scheduled_audit:{}", run_id.as_str()),
        task_id,
        task_title,
        run_id,
        workspace_id,
        workspace_root,
        provider_kind,
        provider_profile_id,
        trigger,
        outcome: scheduled_task_audit_outcome(status, error_code.as_deref()),
        status,
        session_id,
        error_code,
        error_message,
        redacted_diagnostics,
        created_at_ms,
    })
}

fn scheduled_task_attention_kind(
    status: ScheduledTaskRunStatus,
    error_code: Option<&str>,
) -> Option<ScheduledTaskAttentionKind> {
    match error_code {
        Some(SCHEDULED_TASK_PERMISSION_REQUIRED_CODE) => {
            Some(ScheduledTaskAttentionKind::PermissionRequired)
        }
        Some(SCHEDULED_TASK_RECOVERED_STALE_RUN_CODE) => {
            Some(ScheduledTaskAttentionKind::RecoveredStaleRun)
        }
        _ => match status {
            ScheduledTaskRunStatus::Failed => Some(ScheduledTaskAttentionKind::Failed),
            ScheduledTaskRunStatus::Skipped => Some(ScheduledTaskAttentionKind::Skipped),
            ScheduledTaskRunStatus::Canceled => Some(ScheduledTaskAttentionKind::Canceled),
            ScheduledTaskRunStatus::Queued
            | ScheduledTaskRunStatus::Running
            | ScheduledTaskRunStatus::Succeeded => None,
        },
    }
}

fn scheduled_task_audit_outcome(
    status: ScheduledTaskRunStatus,
    error_code: Option<&str>,
) -> ScheduledTaskAuditOutcome {
    match error_code {
        Some(SCHEDULED_TASK_PERMISSION_REQUIRED_CODE) => {
            ScheduledTaskAuditOutcome::PermissionRequired
        }
        Some(SCHEDULED_TASK_RECOVERED_STALE_RUN_CODE) => {
            ScheduledTaskAuditOutcome::RecoveredStaleRun
        }
        _ => match status {
            ScheduledTaskRunStatus::Queued => ScheduledTaskAuditOutcome::Queued,
            ScheduledTaskRunStatus::Running => ScheduledTaskAuditOutcome::Running,
            ScheduledTaskRunStatus::Succeeded => ScheduledTaskAuditOutcome::Succeeded,
            ScheduledTaskRunStatus::Failed => ScheduledTaskAuditOutcome::Failed,
            ScheduledTaskRunStatus::Skipped => ScheduledTaskAuditOutcome::Skipped,
            ScheduledTaskRunStatus::Canceled => ScheduledTaskAuditOutcome::Canceled,
        },
    }
}

fn map_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectRecord> {
    Ok(ProjectRecord {
        id: parse_id_sql(row.get(0)?, ProjectId::parse)?,
        name: row.get(1)?,
        root_path: row.get(2)?,
        created_at_ms: row.get(3)?,
        updated_at_ms: row.get(4)?,
    })
}

fn map_project_lookup(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectLookup> {
    Ok(ProjectLookup {
        record: map_project(row)?,
        deleted_at_ms: row.get(5)?,
    })
}

fn map_workspace(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceRecord> {
    Ok(WorkspaceRecord {
        id: parse_id_sql(row.get(0)?, WorkspaceId::parse)?,
        project_id: parse_id_sql(row.get(1)?, ProjectId::parse)?,
        root_path: row.get(2)?,
        mode: enum_from_db_sql(row.get(3)?)?,
        created_at_ms: row.get(4)?,
        updated_at_ms: row.get(5)?,
    })
}

fn map_workspace_lookup(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceLookup> {
    Ok(WorkspaceLookup {
        record: map_workspace(row)?,
        deleted_at_ms: row.get(6)?,
    })
}

fn map_remote_device_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteDeviceRecord> {
    Ok(RemoteDeviceRecord {
        detail: RemoteDeviceDetail {
            device_id: parse_id_sql(row.get(0)?, DeviceId::parse)?,
            display_name: row.get(1)?,
            public_key: row.get(2)?,
            grant_revision: row.get(4)?,
            permission_level: enum_from_db_sql(row.get(5)?)?,
            status: enum_from_db_sql(row.get(6)?)?,
            paired_at_ms: row.get(7)?,
            last_seen_at_ms: row.get(8)?,
            revoked_at_ms: row.get(9)?,
            created_at_ms: row.get(10)?,
            updated_at_ms: row.get(11)?,
        },
        auth_secret_hash: row.get(3)?,
    })
}

fn map_remote_pairing_code_record(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<RemotePairingCodeRecord> {
    Ok(RemotePairingCodeRecord {
        pairing: RemotePairingCode {
            pairing_id: parse_id_sql(row.get(0)?, RequestId::parse)?,
            permission_level: enum_from_db_sql(row.get(2)?)?,
            expires_at_ms: row.get(3)?,
            claimed_device_id: parse_optional_id_sql(row.get(4)?, DeviceId::parse)?,
            created_at_ms: row.get(5)?,
            claimed_at_ms: row.get(6)?,
        },
        code_hash: row.get(1)?,
    })
}

fn map_remote_audit_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteAuditRecord> {
    Ok(RemoteAuditRecord {
        audit_id: parse_id_sql(row.get(0)?, RequestId::parse)?,
        device_id: parse_optional_id_sql(row.get(1)?, DeviceId::parse)?,
        action: enum_from_db_sql(row.get(2)?)?,
        target_kind: enum_from_db_sql(row.get(3)?)?,
        target_id: row.get(4)?,
        outcome: enum_from_db_sql(row.get(5)?)?,
        redacted_summary: row.get(6)?,
        request_id: parse_optional_id_sql(row.get(7)?, RequestId::parse)?,
        correlation_id: parse_optional_id_sql(row.get(8)?, CorrelationId::parse)?,
        created_at_ms: row.get(9)?,
    })
}

fn map_agent_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentSession> {
    Ok(AgentSession {
        id: parse_id_sql(row.get(0)?, VibexSessionId::parse)?,
        title: row.get(1)?,
        project_id: parse_id_sql(row.get(2)?, ProjectId::parse)?,
        workspace_id: parse_id_sql(row.get(3)?, WorkspaceId::parse)?,
        workspace_root: row.get(4)?,
        workspace_mode: enum_from_db_sql(row.get(5)?)?,
        state: enum_from_db_sql(row.get(6)?)?,
        safety: AgentSessionSafety {
            permission_mode: enum_from_db_sql(row.get(7)?)?,
            ask_on_risk: row.get(8)?,
            bypass_all_permissions: row.get(9)?,
        },
        created_at_ms: row.get(10)?,
        updated_at_ms: row.get(11)?,
        last_message_at_ms: row.get(12)?,
        archived_at_ms: row.get(13)?,
        deleted_at_ms: row.get(14)?,
        agent_id: parse_id_sql(row.get(15)?, AgentId::parse)?,
    })
}

fn map_agent_config(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentConfig> {
    let enabled: i64 = row.get(5)?;
    Ok(AgentConfig {
        agent_id: parse_id_sql(row.get(0)?, AgentId::parse)?,
        runtime_kind: enum_from_db_sql(row.get(1)?)?,
        source_kind: enum_from_db_sql(row.get(2)?)?,
        label_override: row.get(3)?,
        description_override: row.get(4)?,
        enabled: enabled != 0,
        order_index: row.get(6)?,
        command: row
            .get::<_, Option<String>>(7)?
            .map(json_from_db)
            .transpose()
            .map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?,
        env: json_from_db_sql(row.get(8)?)?,
        params: json_from_db_sql(row.get(9)?)?,
        created_at_ms: row.get(10)?,
        updated_at_ms: row.get(11)?,
        deleted_at_ms: row.get(12)?,
    })
}

fn map_agent_discovery_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentDiscoveryRecord> {
    Ok(AgentDiscoveryRecord {
        discovery_record_id: row.get(0)?,
        agent_id: parse_id_sql(row.get(1)?, AgentId::parse)?,
        cwd_scope: row.get(2)?,
        install_status: enum_from_db_sql(row.get(3)?)?,
        config_status: enum_from_db_sql(row.get(4)?)?,
        runtime_status: enum_from_db_sql(row.get(5)?)?,
        binary_path: row.get(6)?,
        version: row.get(7)?,
        native_config_paths: json_from_db_sql(row.get(8)?)?,
        models: json_from_db_sql(row.get(9)?)?,
        modes: json_from_db_sql(row.get(10)?)?,
        diagnostics: json_from_db_sql(row.get(11)?)?,
        discovered_at_ms: row.get(12)?,
    })
}

fn map_provider_profile_without_secrets(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ProviderProfile> {
    Ok(ProviderProfile {
        id: parse_id_sql(row.get(0)?, ProviderProfileId::parse)?,
        agent_id: parse_id_sql(row.get(1)?, AgentId::parse)?,
        kind: enum_from_db_sql(row.get(2)?)?,
        display_name: row.get(3)?,
        status: enum_from_db_sql(row.get(4)?)?,
        account_alias: row.get(5)?,
        base_url: row.get(6)?,
        default_model: row.get(7)?,
        small_model: row.get(8)?,
        large_model: row.get(9)?,
        configured_models: json_from_db_sql(row.get(10)?)?,
        reasoning_effort: row.get(11)?,
        sandbox_defaults: json_from_db_sql(row.get(12)?)?,
        network_defaults: json_from_db_sql(row.get(13)?)?,
        permission_defaults: json_from_db_sql(row.get(14)?)?,
        provider_options: json_from_db_sql(row.get(15)?)?,
        secrets: Vec::new(),
        created_at_ms: row.get(16)?,
        updated_at_ms: row.get(17)?,
        deleted_at_ms: row.get(18)?,
    })
}

fn map_provider_secret_reference(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ProviderSecretReference> {
    Ok(ProviderSecretReference {
        id: parse_id_sql(row.get(0)?, RequestId::parse)?,
        provider_profile_id: parse_id_sql(row.get(1)?, ProviderProfileId::parse)?,
        secret_kind: enum_from_db_sql(row.get(2)?)?,
        backend: enum_from_db_sql(row.get(3)?)?,
        setup_state: enum_from_db_sql(row.get(4)?)?,
        lookup_key: row.get(5)?,
        display_label: row.get(6)?,
        redacted_hint: row.get(7)?,
        created_at_ms: row.get(8)?,
        updated_at_ms: row.get(9)?,
    })
}

fn map_mcp_server_without_children(row: &rusqlite::Row<'_>) -> rusqlite::Result<McpServer> {
    Ok(McpServer {
        id: parse_id_sql(row.get(0)?, McpServerId::parse)?,
        display_name: row.get(1)?,
        transport_kind: enum_from_db_sql(row.get(2)?)?,
        status: enum_from_db_sql(row.get(3)?)?,
        scope_kind: enum_from_db_sql(row.get(4)?)?,
        project_id: parse_optional_id_sql(row.get::<_, Option<String>>(5)?, ProjectId::parse)?,
        workspace_id: parse_optional_id_sql(row.get::<_, Option<String>>(6)?, WorkspaceId::parse)?,
        command: row.get(7)?,
        args: json_from_db_sql(row.get(8)?)?,
        url: row.get(9)?,
        description: row.get(10)?,
        tags: json_from_db_sql(row.get(11)?)?,
        secret_references: Vec::new(),
        provider_matrix: Vec::new(),
        agent_matrix: Vec::new(),
        created_at_ms: row.get(12)?,
        updated_at_ms: row.get(13)?,
        deleted_at_ms: row.get(14)?,
    })
}

fn map_mcp_secret_reference(row: &rusqlite::Row<'_>) -> rusqlite::Result<McpServerSecretReference> {
    Ok(McpServerSecretReference {
        id: parse_id_sql(row.get(0)?, RequestId::parse)?,
        mcp_server_id: parse_id_sql(row.get(1)?, McpServerId::parse)?,
        secret_kind: enum_from_db_sql(row.get(2)?)?,
        backend: enum_from_db_sql(row.get(3)?)?,
        setup_state: enum_from_db_sql(row.get(4)?)?,
        lookup_key: row.get(5)?,
        display_label: row.get(6)?,
        redacted_hint: row.get(7)?,
        target: enum_from_db_sql(row.get(8)?)?,
        created_at_ms: row.get(9)?,
        updated_at_ms: row.get(10)?,
    })
}

fn map_mcp_provider_matrix(row: &rusqlite::Row<'_>) -> rusqlite::Result<McpServerProviderMatrix> {
    let enabled: i64 = row.get(1)?;
    Ok(McpServerProviderMatrix {
        provider_kind: enum_from_db_sql(row.get(0)?)?,
        enabled: enabled != 0,
        updated_at_ms: row.get(2)?,
    })
}

fn map_mcp_agent_matrix(row: &rusqlite::Row<'_>) -> rusqlite::Result<McpServerAgentMatrix> {
    let enabled: i64 = row.get(1)?;
    Ok(McpServerAgentMatrix {
        agent_id: parse_id_sql(row.get(0)?, AgentId::parse)?,
        enabled: enabled != 0,
        source_kind: enum_from_db_sql(row.get(2)?)?,
        updated_at_ms: row.get(3)?,
    })
}

fn map_skill_without_children(row: &rusqlite::Row<'_>) -> rusqlite::Result<Skill> {
    Ok(Skill {
        id: parse_id_sql(row.get(0)?, SkillId::parse)?,
        display_name: row.get(1)?,
        source_kind: enum_from_db_sql(row.get(2)?)?,
        status: enum_from_db_sql(row.get(3)?)?,
        scope_kind: enum_from_db_sql(row.get(4)?)?,
        project_id: parse_optional_id_sql(row.get::<_, Option<String>>(5)?, ProjectId::parse)?,
        workspace_id: parse_optional_id_sql(row.get::<_, Option<String>>(6)?, WorkspaceId::parse)?,
        source_uri: row.get(7)?,
        description: row.get(8)?,
        tags: json_from_db_sql(row.get(9)?)?,
        content_preview: row.get(10)?,
        provider_matrix: Vec::new(),
        agent_matrix: Vec::new(),
        created_at_ms: row.get(11)?,
        updated_at_ms: row.get(12)?,
        deleted_at_ms: row.get(13)?,
    })
}

fn map_skill_provider_matrix(row: &rusqlite::Row<'_>) -> rusqlite::Result<SkillProviderMatrix> {
    let enabled: i64 = row.get(1)?;
    Ok(SkillProviderMatrix {
        provider_kind: enum_from_db_sql(row.get(0)?)?,
        enabled: enabled != 0,
        updated_at_ms: row.get(2)?,
    })
}

fn map_skill_agent_matrix(row: &rusqlite::Row<'_>) -> rusqlite::Result<SkillAgentMatrix> {
    let enabled: i64 = row.get(1)?;
    Ok(SkillAgentMatrix {
        agent_id: parse_id_sql(row.get(0)?, AgentId::parse)?,
        enabled: enabled != 0,
        source_kind: enum_from_db_sql(row.get(2)?)?,
        updated_at_ms: row.get(3)?,
    })
}

fn map_prompt(row: &rusqlite::Row<'_>) -> rusqlite::Result<Prompt> {
    Ok(Prompt {
        id: parse_id_sql(row.get(0)?, PromptId::parse)?,
        display_name: row.get(1)?,
        kind: enum_from_db_sql(row.get(2)?)?,
        status: enum_from_db_sql(row.get(3)?)?,
        scope_kind: enum_from_db_sql(row.get(4)?)?,
        project_id: parse_optional_id_sql(row.get::<_, Option<String>>(5)?, ProjectId::parse)?,
        workspace_id: parse_optional_id_sql(row.get::<_, Option<String>>(6)?, WorkspaceId::parse)?,
        body: row.get(7)?,
        description: row.get(8)?,
        tags: json_from_db_sql(row.get(9)?)?,
        created_at_ms: row.get(10)?,
        updated_at_ms: row.get(11)?,
        deleted_at_ms: row.get(12)?,
    })
}

fn map_hook(row: &rusqlite::Row<'_>) -> rusqlite::Result<Hook> {
    Ok(Hook {
        id: parse_id_sql(row.get(0)?, HookId::parse)?,
        display_name: row.get(1)?,
        provider_kind: enum_from_db_sql(row.get(2)?)?,
        event_kind: enum_from_db_sql(row.get(3)?)?,
        status: enum_from_db_sql(row.get(4)?)?,
        install_state: enum_from_db_sql(row.get(5)?)?,
        command_preview: row.get(6)?,
        managed_marker: row.get(7)?,
        description: row.get(8)?,
        created_at_ms: row.get(9)?,
        updated_at_ms: row.get(10)?,
        deleted_at_ms: row.get(11)?,
    })
}

fn map_provider_health_probe_result(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ProviderHealthProbeResult> {
    let latency_ms: Option<i64> = row.get(6)?;
    Ok(ProviderHealthProbeResult {
        health_record_id: parse_id_sql(row.get(0)?, RequestId::parse)?,
        provider_profile_id: parse_id_sql(row.get(1)?, ProviderProfileId::parse)?,
        provider_kind: enum_from_db_sql(row.get(2)?)?,
        probe_kind: enum_from_db_sql(row.get(3)?)?,
        status: enum_from_db_sql(row.get(4)?)?,
        summary: row.get(5)?,
        latency_ms: latency_ms.and_then(|value| u32::try_from(value).ok()),
        checked_at_ms: row.get(7)?,
        expires_at_ms: row.get(8)?,
        diagnostics: json_from_db_sql(row.get(9)?)?,
    })
}

fn map_provider_capability_probe_result(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ProviderCapabilityProbeResult> {
    Ok(ProviderCapabilityProbeResult {
        capability_record_id: parse_id_sql(row.get(0)?, RequestId::parse)?,
        provider_profile_id: parse_id_sql(row.get(1)?, ProviderProfileId::parse)?,
        provider_kind: enum_from_db_sql(row.get(2)?)?,
        status: enum_from_db_sql(row.get(3)?)?,
        summary: row.get(4)?,
        capabilities: json_from_db_sql(row.get(5)?)?,
        source: row.get(6)?,
        checked_at_ms: row.get(7)?,
        expires_at_ms: row.get(8)?,
        diagnostics: json_from_db_sql(row.get(9)?)?,
    })
}

fn map_provider_runtime_option_snapshot(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ProviderRuntimeOptionSnapshotRecord> {
    let model_response = row
        .get::<_, Option<String>>(2)?
        .map(json_from_db_sql::<AgentModelListResponse>)
        .transpose()?;
    let session_config = row
        .get::<_, Option<String>>(3)?
        .map(json_from_db_sql::<AgentSessionConfigProbe>)
        .transpose()?;
    Ok(ProviderRuntimeOptionSnapshotRecord {
        provider_profile_id: parse_id_sql(row.get(0)?, ProviderProfileId::parse)?,
        agent_id: parse_id_sql(row.get(1)?, AgentId::parse)?,
        model_response,
        session_config,
        last_success_at_ms: row.get(4)?,
        last_attempt_at_ms: row.get(5)?,
        last_error_code: row.get(6)?,
    })
}

fn map_provider_model_runtime_option_snapshot(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ProviderModelRuntimeOptionSnapshotRecord> {
    let session_config = row
        .get::<_, Option<String>>(3)?
        .map(json_from_db_sql::<AgentSessionConfigProbe>)
        .transpose()?;
    Ok(ProviderModelRuntimeOptionSnapshotRecord {
        provider_profile_id: parse_id_sql(row.get(0)?, ProviderProfileId::parse)?,
        model_id: row.get(1)?,
        agent_id: parse_id_sql(row.get(2)?, AgentId::parse)?,
        session_config,
        last_success_at_ms: row.get(4)?,
        last_attempt_at_ms: row.get(5)?,
        last_error_code: row.get(6)?,
    })
}

fn map_agent_runtime_option_snapshot(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<AgentRuntimeOptionSnapshotRecord> {
    let session_config = row
        .get::<_, Option<String>>(1)?
        .map(json_from_db_sql::<AgentSessionConfigProbe>)
        .transpose()?;
    Ok(AgentRuntimeOptionSnapshotRecord {
        agent_id: parse_id_sql(row.get(0)?, AgentId::parse)?,
        session_config,
        last_success_at_ms: row.get(2)?,
        last_attempt_at_ms: row.get(3)?,
        last_error_code: row.get(4)?,
    })
}

fn map_provider_usage_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProviderUsageRecord> {
    let window_label: Option<String> = row.get(9)?;
    let window_started_at_ms: Option<i64> = row.get(10)?;
    let window_ends_at_ms: Option<i64> = row.get(11)?;
    let window = window_label.map(|label| ProviderUsageWindow {
        label,
        started_at_ms: window_started_at_ms,
        ends_at_ms: window_ends_at_ms,
    });
    Ok(ProviderUsageRecord {
        usage_record_id: parse_id_sql(row.get(0)?, RequestId::parse)?,
        provider_profile_id: parse_id_sql(row.get(1)?, ProviderProfileId::parse)?,
        provider_kind: enum_from_db_sql(row.get(2)?)?,
        source: row.get(3)?,
        unit: enum_from_db_sql(row.get(4)?)?,
        label: row.get(5)?,
        used: row.get(6)?,
        limit_value: row.get(7)?,
        remaining: row.get(8)?,
        window,
        recorded_at_ms: row.get(12)?,
        metadata: json_from_db_sql(row.get(13)?)?,
    })
}

fn map_timeline_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<TimelineItem> {
    let execution_attribution = row
        .get::<_, Option<String>>(10)?
        .map(json_from_db_sql::<TurnExecutionAttribution>)
        .transpose()?;
    Ok(TimelineItem {
        session_id: parse_id_sql(row.get(0)?, VibexSessionId::parse)?,
        sequence: row.get(1)?,
        id: parse_id_sql(row.get(2)?, TimelineItemId::parse)?,
        kind: enum_from_db_sql(row.get(3)?)?,
        source: enum_from_db_sql(row.get(4)?)?,
        timestamp_ms: row.get(5)?,
        correlation_id: parse_optional_id_sql(
            row.get::<_, Option<String>>(6)?,
            vibex_core::CorrelationId::parse,
        )?,
        provider_correlation_id: row.get(7)?,
        payload: json_from_db_sql(row.get(8)?)?,
        redaction_state: enum_from_db_sql(row.get(9)?)?,
        execution_attribution: execution_attribution
            .as_ref()
            .map(TurnExecutionAttribution::view),
    })
}

fn map_permission_request(row: &rusqlite::Row<'_>) -> rusqlite::Result<PermissionRequest> {
    Ok(PermissionRequest {
        id: parse_id_sql(row.get(0)?, vibex_core::RequestId::parse)?,
        session_id: parse_id_sql(row.get(1)?, VibexSessionId::parse)?,
        project_id: parse_optional_id_sql(row.get::<_, Option<String>>(2)?, ProjectId::parse)?,
        workspace_id: parse_optional_id_sql(row.get::<_, Option<String>>(3)?, WorkspaceId::parse)?,
        provider_request_id: row.get(4)?,
        risk_category: enum_from_db_sql(row.get(5)?)?,
        title: row.get(6)?,
        details: json_from_db_sql::<Vec<PermissionActionDetail>>(row.get(7)?)?,
        allowed_responses: json_from_db_sql::<Vec<PermissionResponseKind>>(row.get(8)?)?,
        response_options: json_from_db_sql::<Vec<PermissionResponseOption>>(row.get(12)?)?,
        status: enum_from_db_sql(row.get(9)?)?,
        requested_at_ms: row.get(10)?,
        expires_at_ms: row.get(11)?,
    })
}

fn map_elicitation_request(row: &rusqlite::Row<'_>) -> rusqlite::Result<ElicitationRequest> {
    let mut request = json_from_db_sql::<ElicitationRequest>(row.get(0)?)?;
    request.status = enum_from_db_sql(row.get(1)?)?;
    Ok(request)
}

fn map_terminal_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<TerminalSession> {
    Ok(TerminalSession {
        id: parse_id_sql(row.get(0)?, TerminalId::parse)?,
        workspace_id: parse_id_sql(row.get(1)?, WorkspaceId::parse)?,
        title: row.get(2)?,
        shell: row.get(3)?,
        cwd: row.get(4)?,
        rows: row.get(5)?,
        cols: row.get(6)?,
        status: enum_from_db_sql(row.get(7)?)?,
        created_at_ms: row.get(8)?,
        updated_at_ms: row.get(9)?,
        closed_at_ms: row.get(10)?,
    })
}

fn map_managed_worktree(row: &rusqlite::Row<'_>) -> rusqlite::Result<ManagedWorktreeRecord> {
    Ok(ManagedWorktreeRecord {
        worktree_id: parse_id_sql(row.get(0)?, RequestId::parse)?,
        project_id: parse_id_sql(row.get(1)?, ProjectId::parse)?,
        workspace_id: parse_optional_id_sql(row.get::<_, Option<String>>(2)?, WorkspaceId::parse)?,
        repo_root: row.get(3)?,
        worktree_path: row.get(4)?,
        repository_identity: optional_json_from_db_sql(row.get(5)?)?,
        worktree_path_identity: optional_json_from_db_sql(row.get(6)?)?,
        branch: row.get(7)?,
        origin_workspace_id: parse_optional_id_sql(
            row.get::<_, Option<String>>(8)?,
            WorkspaceId::parse,
        )?,
        base_ref: row.get(9)?,
        base_head: row.get(10)?,
        target_workspace_id: parse_optional_id_sql(
            row.get::<_, Option<String>>(11)?,
            WorkspaceId::parse,
        )?,
        target_branch: row.get(12)?,
        head: row.get(13)?,
        status: enum_from_db_sql(row.get(14)?)?,
        reconciliation_state: enum_from_db_sql(row.get(15)?)?,
        diagnostic: optional_json_from_db_sql(row.get(16)?)?,
        created_at_ms: row.get(17)?,
        updated_at_ms: row.get(18)?,
        closed_at_ms: row.get(19)?,
    })
}

fn map_worktree_readiness(row: &rusqlite::Row<'_>) -> rusqlite::Result<GitWorktreeReadinessRecord> {
    Ok(GitWorktreeReadinessRecord {
        worktree_id: parse_id_sql(row.get(0)?, RequestId::parse)?,
        workspace_id: parse_id_sql(row.get(1)?, WorkspaceId::parse)?,
        state: enum_from_db_sql(row.get(2)?)?,
        source_head: row.get(3)?,
        dirty_fingerprint: row.get(4)?,
        target_workspace_id: parse_id_sql(row.get(5)?, WorkspaceId::parse)?,
        target_branch: row.get(6)?,
        checks: json_from_db_sql(row.get(7)?)?,
        revision: row.get(8)?,
        updated_at_ms: row.get(9)?,
    })
}

fn map_worktree_operation(row: &rusqlite::Row<'_>) -> rusqlite::Result<GitWorktreeOperationRecord> {
    let mut detail =
        optional_json_from_db_sql::<GitWorktreeOperationDetail>(row.get(17)?)?.unwrap_or_default();
    detail.idempotency_key = row.get::<_, Option<String>>(14)?.or(detail.idempotency_key);
    detail.request_fingerprint = row
        .get::<_, Option<String>>(15)?
        .or(detail.request_fingerprint);
    detail.checkpoint = enum_from_db_sql(row.get(16)?)?;
    detail.lease_owner = row.get(18)?;
    detail.lease_expires_at_ms = row.get(19)?;
    let attempt = row.get::<_, i64>(20)?;
    detail.attempt = u32::try_from(attempt).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            20,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })?;
    detail.diagnostic = optional_json_from_db_sql(row.get(21)?)?.or(detail.diagnostic);
    Ok(GitWorktreeOperationRecord {
        operation_id: parse_id_sql(row.get(0)?, RequestId::parse)?,
        project_id: parse_id_sql(row.get(1)?, ProjectId::parse)?,
        source_workspace_id: parse_optional_id_sql(
            row.get::<_, Option<String>>(2)?,
            WorkspaceId::parse,
        )?,
        target_workspace_id: parse_optional_id_sql(
            row.get::<_, Option<String>>(3)?,
            WorkspaceId::parse,
        )?,
        operation: enum_from_db_sql(row.get(4)?)?,
        status: enum_from_db_sql(row.get(5)?)?,
        worktree_path: row.get(6)?,
        branch: row.get(7)?,
        base_ref: row.get(8)?,
        head_before: row.get(9)?,
        head_after: row.get(10)?,
        error: row.get(11)?,
        detail,
        created_at_ms: row.get(12)?,
        updated_at_ms: row.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use vibex_core::{
        AcpAdapterId, AdapterDiagnosticLevel, AgentCommandConfig, AgentConfigStatus, AgentId,
        AgentInstallStatus, AgentMessageDeltaPayload, AgentMessagePayload, AgentModelListSource,
        AgentModelProviderFailoverEntry, AgentReasoningEffort, AgentRuntimeKind,
        AgentRuntimeStatus, AgentSourceKind, AutomationAgentPromptConfig,
        AutomationApprovalGateConfig, AutomationEdgeCondition, AutomationEdgeConditionKind,
        AutomationGraphTrigger, AutomationNodeConfig, AutomationNodeKind, AutomationNodePosition,
        AutomationRunStatus, AutomationRunStepStatus, AutomationRunTrigger, BindingState,
        GitWorktreeOperationKind, HookCreateRequest, HookEventKind, HookInstallPreview, HookStatus,
        McpSecretTarget, McpServerCreateRequest, McpServerProviderMatrix, McpServerScopeKind,
        McpServerSecretReferenceCreateRequest, McpServerStatus, McpServerTransportKind,
        MessageSubmissionId, NativeStateHomeId, PermissionResponseKind, PermissionRiskCategory,
        PromptCreateRequest, PromptKind, PromptScopeKind, PromptStatus, ProviderBindingMetadata,
        ProviderCapabilities, ProviderCapabilityProbeResult, ProviderCapabilityProbeStatus,
        ProviderDefaultScopeKind, ProviderHealthProbeKind, ProviderHealthProbeResult,
        ProviderHealthStatus, ProviderKind, ProviderNativeConfigFileKind,
        ProviderNativeExportApplyResult, ProviderNativeExportApplyStatus,
        ProviderNativeExportFilePlan, ProviderNativeExportFileStatus, ProviderNativeExportMode,
        ProviderNativeExportOperationKind, ProviderNativeExportPreview, ProviderNativeExportSource,
        ProviderProfileSetDefaultRequest, ProviderSecretBackend, ProviderSecretKind,
        ProviderSecretReferenceCreateRequest, ProviderSecretSetupState, ProviderSessionConfigValue,
        ProviderUsageRecord, ProviderUsageUnit, ProviderUsageWindow, RuntimeBinding,
        RuntimeBindingId, RuntimeSwitchActiveWorkPolicy, RuntimeSwitchId, RuntimeSwitchPolicy,
        ScheduledTaskDailySchedule, ScheduledTaskIntervalSchedule, ScheduledTaskOneShotSchedule,
        ScheduledTaskRunStatus, ScheduledTaskRunTrigger, ScheduledTaskSchedule,
        SendAgentMessageRequest, SessionRuntimeConfigState, SessionRuntimeSelection,
        SkillCreateRequest, SkillProviderMatrix, SkillScopeKind, SkillSourceKind, SkillStatus,
        SystemNoticeLevel, SystemNoticePayload, TerminalStatus, TransportKind,
        TurnExecutionAttribution, UserMessagePayload,
    };
    use vibex_core::{
        RemoteAuditAction, RemoteAuditOutcome, RemoteAuditTargetKind, RemoteDevicePermissionLevel,
    };

    #[test]
    fn migration_smoke_round_trips_sentinel() {
        let temp = temp_db_path("smoke");

        let result = run_smoke(&temp).unwrap();
        assert_eq!(result.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(result.marker.starts_with("vibex-db-smoke-"));

        cleanup_db(temp);
    }

    #[test]
    fn schema_v47_converts_only_agent_default_usage_models_to_null() {
        let temp = temp_db_path("schema-v47-agent-default-usage");
        let mut conn = open_database(&temp).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at_ms INTEGER NOT NULL
            );",
        )
        .unwrap();
        for migration in MIGRATIONS {
            let tx = conn.transaction().unwrap();
            tx.execute_batch(migration.sql).unwrap();
            tx.execute(
                "INSERT INTO schema_migrations (version, name, applied_at_ms)
                 VALUES (?1, ?2, ?3)",
                params![migration.version, migration.name, unix_timestamp_ms()],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let mut applied = Vec::new();
        apply_runtime_auth_source_table_rebuild(&mut conn, &mut applied).unwrap();
        assert_eq!(applied, ["46:runtime_auth_source_nullable_legacy_columns"]);
        assert_eq!(current_schema_version(&conn).unwrap(), 46);

        let workspace_root = std::env::temp_dir().join(format!(
            "vibex-schema-v47-agent-default-usage-{}",
            RequestId::new().as_str()
        ));
        let (project, workspace) =
            WorkspaceRepository::ensure(&conn, &workspace_root, WorkspaceMode::CurrentCheckout)
                .unwrap();
        let now = unix_timestamp_ms();
        let agent_id = AgentId::parse("codex").unwrap();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: "Pre-v47 Agent default usage".to_string(),
            project_id: project.id,
            workspace_id: workspace.id,
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
            agent_id: agent_id.clone(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now,
            updated_at_ms: now,
            last_message_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&conn, &session).unwrap();
        let auth_context = AgentAuthContextRepository::ensure_default(&conn, &agent_id).unwrap();

        for (stream_id, binding_id, source_kind, source_id, profile_id, model_id) in [
            (
                "stream_v47_agent",
                "binding_v47_agent",
                "agent_account",
                auth_context.id.as_str(),
                None,
                "agent_default",
            ),
            (
                "stream_v47_provider",
                "binding_v47_provider",
                "provider_profile",
                "provider_v47",
                Some("provider_v47"),
                "provider-model",
            ),
        ] {
            conn.execute(
                "INSERT INTO agent_usage_checkpoints (
                    usage_stream_id, session_id, binding_id, last_activation_generation,
                    agent_id, provider_profile_id, auth_source_kind, auth_source_id,
                    auth_source_revision, last_model_id, reset_epoch, counter_origin,
                    last_observation_sequence, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, 1, ?8, 0, 'known_zero', 1, ?9, ?9)",
                params![
                    stream_id,
                    session.id.as_str(),
                    binding_id,
                    agent_id.as_str(),
                    profile_id,
                    source_kind,
                    source_id,
                    model_id,
                    now,
                ],
            )
            .unwrap();
        }

        for (execution_id, binding_id, source_kind, source_id, profile_id, model_id) in [
            (
                "execution_v47_agent",
                "binding_v47_agent",
                "agent_account",
                auth_context.id.as_str(),
                None,
                "agent_default",
            ),
            (
                "execution_v47_provider",
                "binding_v47_provider",
                "provider_profile",
                "provider_v47",
                Some("provider_v47"),
                "provider-model",
            ),
        ] {
            conn.execute(
                "INSERT INTO agent_turn_usage_facts (
                    usage_execution_id, session_id, project_id, workspace_id, binding_id,
                    activation_generation, reset_epoch, agent_id, provider_profile_id,
                    auth_source_kind, auth_source_id, auth_source_revision, model_id,
                    execution_status, total_delta, cumulative_total_after, reported_fields,
                    coverage, dispatched_at_ms, created_at_ms, updated_at_ms
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, 1, 0, ?6, ?7, ?8, ?9, 1, ?10,
                    'completed', 100, 100, 32, 'complete', ?11, ?11, ?11
                 )",
                params![
                    execution_id,
                    session.id.as_str(),
                    session.project_id.as_str(),
                    session.workspace_id.as_str(),
                    binding_id,
                    agent_id.as_str(),
                    profile_id,
                    source_kind,
                    source_id,
                    model_id,
                    now,
                ],
            )
            .unwrap();
        }

        assert_eq!(
            apply_migrations(&mut conn).unwrap(),
            [
                "47:agent_default_usage_model_nullable",
                "48:agent_usage_counter_scope",
                "49:message_submission_runtime_policy"
            ]
        );
        let agent_models: (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT c.last_model_id, f.model_id
                 FROM agent_usage_checkpoints c
                 JOIN agent_turn_usage_facts f ON f.binding_id = c.binding_id
                 WHERE c.usage_stream_id = 'stream_v47_agent'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(agent_models, (None, None));
        let provider_models: (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT c.last_model_id, f.model_id
                 FROM agent_usage_checkpoints c
                 JOIN agent_turn_usage_facts f ON f.binding_id = c.binding_id
                 WHERE c.usage_stream_id = 'stream_v47_provider'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            provider_models,
            (
                Some("provider-model".to_string()),
                Some("provider-model".to_string())
            )
        );

        assert!(
            conn.execute(
                "UPDATE agent_usage_checkpoints SET last_model_id = NULL
                 WHERE usage_stream_id = 'stream_v47_provider'",
                [],
            )
            .is_err()
        );
        assert!(
            conn.execute(
                "UPDATE agent_turn_usage_facts SET model_id = NULL
                 WHERE usage_execution_id = 'execution_v47_provider'",
                [],
            )
            .is_err()
        );

        drop(conn);
        cleanup_db(temp);
        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn schema_v44_backfills_completed_runtime_switch_activations() {
        let temp = temp_db_path("schema-v44-runtime-switch-activation");
        let mut conn = open_database(&temp).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at_ms INTEGER NOT NULL
            );",
        )
        .unwrap();
        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version <= 43)
        {
            let tx = conn.transaction().unwrap();
            tx.execute_batch(migration.sql).unwrap();
            tx.execute(
                "INSERT INTO schema_migrations (version, name, applied_at_ms)
                 VALUES (?1, ?2, ?3)",
                params![migration.version, migration.name, unix_timestamp_ms()],
            )
            .unwrap();
            tx.commit().unwrap();
        }

        let now = unix_timestamp_ms();
        conn.execute(
            "INSERT INTO projects (project_id, name, root_path, created_at_ms, updated_at_ms)
             VALUES ('project_v44', 'V44', '/tmp/vibex-v44', ?1, ?1)",
            params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workspaces (
                workspace_id, project_id, root_path, mode, created_at_ms, updated_at_ms
             ) VALUES (
                'workspace_v44', 'project_v44', '/tmp/vibex-v44',
                'current_checkout', ?1, ?1
             )",
            params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_sessions (
                session_id, title, project_id, workspace_id, workspace_root, workspace_mode,
                state, permission_mode, ask_on_risk, bypass_all_permissions,
                created_at_ms, updated_at_ms, current_agent_id, current_binding_id
             ) VALUES (
                'session_v44', 'V44 session', 'project_v44', 'workspace_v44',
                '/tmp/vibex-v44', 'current_checkout', 'idle', 'workspace_write', 1, 0,
                ?1, ?1, 'codex', 'binding_v44'
             )",
            params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO runtime_switches (
                switch_id, session_id, idempotency_key, source_revision,
                desired_selection_revision, target_binding_id, target_agent_id,
                target_adapter_id, target_profile_id, status,
                created_at_ms, updated_at_ms, committed_at_ms
             ) VALUES (
                'switch_v44', 'session_v44', 'v44-backfill', 0, 1, 'binding_v44',
                'codex', 'codex-acp', 'provider_v44', 'committed', ?1, ?1, ?1
             )",
            params![now],
        )
        .unwrap();

        assert_eq!(
            apply_migrations(&mut conn).unwrap(),
            vec![
                "44:runtime_switch_activation_completion",
                "45:agent_auth_context_and_runtime_source",
                "46:runtime_auth_source_nullable_legacy_columns",
                "47:agent_default_usage_model_nullable",
                "48:agent_usage_counter_scope",
                "49:message_submission_runtime_policy",
            ]
        );
        let activation_completed_at_ms: Option<i64> = conn
            .query_row(
                "SELECT activation_completed_at_ms
                 FROM runtime_switches WHERE switch_id = 'switch_v44'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(activation_completed_at_ms, Some(now));
        let pending_index: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_runtime_switches_pending_activation'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending_index, 1);

        drop(conn);
        cleanup_db(temp);
    }

    #[test]
    fn schema_v32_marks_existing_runtime_bindings_zero_baseline_unavailable() {
        let temp = temp_db_path("schema-v32-zero-baseline-upgrade");
        let mut conn = open_database(&temp).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at_ms INTEGER NOT NULL
            );",
        )
        .unwrap();
        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version <= 31)
        {
            let tx = conn.transaction().unwrap();
            tx.execute_batch(migration.sql).unwrap();
            tx.execute(
                "INSERT INTO schema_migrations (version, name, applied_at_ms)
                 VALUES (?1, ?2, ?3)",
                params![migration.version, migration.name, unix_timestamp_ms()],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(current_schema_version(&conn).unwrap(), 31);

        let workspace_root = std::env::temp_dir().join(format!(
            "vibex-schema-v32-upgrade-{}",
            RequestId::new().as_str()
        ));
        let (project, workspace) =
            WorkspaceRepository::ensure(&conn, &workspace_root, WorkspaceMode::CurrentCheckout)
                .unwrap();
        let now = unix_timestamp_ms();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: "Pre-v32 ACP session".to_string(),
            project_id: project.id,
            workspace_id: workspace.id,
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("opencode").unwrap(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now,
            updated_at_ms: now,
            last_message_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&conn, &session).unwrap();
        conn.execute(
            "INSERT INTO session_runtime_bindings (
                binding_id, session_id, agent_id, transport_kind, adapter_id, adapter_version,
                adapter_compatibility_identity, provider_profile_id, native_state_home_id,
                process_spawn_fingerprint, session_runtime_config_state_json, binding_state,
                activation_generation, created_at_ms, updated_at_ms
             ) VALUES (
                'binding_pre_v32', ?1, 'opencode', 'acp', 'opencode-acp', '1.0.0',
                'opencode-acp@1', 'provider_acp_local', 'home_pre_v32',
                'fingerprint_pre_v32', '{}', 'current', 4, ?2, ?2
             )",
            params![session.id.as_str(), now],
        )
        .unwrap();

        assert_eq!(
            apply_migrations(&mut conn).unwrap(),
            vec![
                "32:agent_usage_zero_baseline_fence",
                "33:managed_worktree_recovery_foundation",
                "34:worktree_merge_lifecycle",
                "35:permission_response_options",
                "36:agent_elicitation_requests",
                "37:agent_provider_projection_platform",
                "38:agent_runtime_provider_probe_evidence",
                "39:agent_runtime_option_snapshots",
                "40:agent_auth_catalog_snapshots",
                "41:agent_managed_installations",
                "42:provider_model_runtime_option_snapshots",
                "43:agent_model_provider_display_order",
                "44:runtime_switch_activation_completion",
                "45:agent_auth_context_and_runtime_source",
                "46:runtime_auth_source_nullable_legacy_columns",
                "47:agent_default_usage_model_nullable",
                "48:agent_usage_counter_scope",
                "49:message_submission_runtime_policy"
            ]
        );
        let stored: (String, Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT usage_zero_baseline_state, usage_zero_baseline_execution_id,
                        usage_zero_baseline_activation_generation
                 FROM session_runtime_bindings WHERE binding_id = 'binding_pre_v32'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(stored, ("unavailable".to_string(), None, None));

        drop(conn);
        cleanup_db(temp);
        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn schema_v28_clears_legacy_sessions_and_accepts_new_durable_acp_rows() {
        let temp = temp_db_path("schema-v28-cutover");
        let mut conn = open_database(&temp).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at_ms INTEGER NOT NULL
            );",
        )
        .unwrap();
        for migration in MIGRATIONS.iter().filter(|migration| migration.version < 28) {
            conn.execute_batch(migration.sql).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations (version, name, applied_at_ms)
                 VALUES (?1, ?2, ?3)",
                params![migration.version, migration.name, unix_timestamp_ms()],
            )
            .unwrap();
        }

        let now = unix_timestamp_ms();
        conn.execute(
            "INSERT INTO projects (project_id, name, root_path, created_at_ms, updated_at_ms)
             VALUES ('project_legacy', 'Legacy', '/tmp/vibex-legacy', ?1, ?1)",
            params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workspaces (
                workspace_id, project_id, root_path, mode, created_at_ms, updated_at_ms
             ) VALUES (
                'workspace_legacy', 'project_legacy', '/tmp/vibex-legacy',
                'current_checkout', ?1, ?1
             )",
            params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_sessions (
                session_id, title, project_id, workspace_id, workspace_root, workspace_mode,
                provider_kind, provider_profile_id, state, permission_mode, ask_on_risk,
                bypass_all_permissions, created_at_ms, updated_at_ms, current_agent_id
             ) VALUES (
                'session_legacy', 'Legacy session', 'project_legacy', 'workspace_legacy',
                '/tmp/vibex-legacy', 'current_checkout', 'claude', 'profile_legacy', 'idle',
                'workspace_write', 1, 0, ?1, ?1, 'claude'
             )",
            params![now],
        )
        .unwrap();
        for table in ["provider_bindings", "session_provider_bindings"] {
            conn.execute(
                &format!(
                    "INSERT INTO {table} (
                        session_id, provider_profile_id, provider_kind, native_session_id,
                        native_thread_id, native_resume_token, redacted_metadata_json,
                        created_at_ms, updated_at_ms, session_config_state_json
                     ) VALUES (
                        'session_legacy', 'profile_legacy', 'claude', 'native_legacy',
                        NULL, NULL, '[]', ?1, ?1, NULL
                     )"
                ),
                params![now],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO session_runtime_bindings (
                binding_id, session_id, agent_id, transport_kind, adapter_id, adapter_version,
                adapter_compatibility_identity, provider_profile_id, native_state_home_id,
                process_spawn_fingerprint, session_runtime_config_state_json, binding_state,
                created_at_ms, updated_at_ms
             ) VALUES (
                'binding_legacy', 'session_legacy', 'claude', 'acp', 'claude-agent-acp',
                '0.58.1', 'adapter=claude-agent-acp@0.58.1', 'profile_legacy',
                'home_legacy', 'fingerprint_legacy', '{}', 'current', ?1, ?1
             )",
            params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO runtime_switches (
                switch_id, session_id, idempotency_key, source_revision,
                desired_selection_revision, target_agent_id, target_adapter_id,
                target_profile_id, status, created_at_ms, updated_at_ms
             ) VALUES (
                'switch_legacy', 'session_legacy', 'switch:legacy', 0, 1, 'claude',
                'claude-agent-acp', 'profile_legacy', 'requested', ?1, ?1
             )",
            params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_message_submissions (
                submission_id, session_id, message_idempotency_key,
                desired_runtime_selection_json, message_payload_reference, status,
                created_at_ms, updated_at_ms
             ) VALUES (
                'submission_legacy', 'session_legacy', 'message:legacy', '{}',
                'payload_legacy', 'awaiting_runtime', ?1, ?1
             )",
            params![now],
        )
        .unwrap();

        assert_eq!(current_schema_version(&conn).unwrap(), 27);
        let applied = apply_migrations(&mut conn).unwrap();
        assert_eq!(
            applied,
            vec![
                "28:acp_only_runtime_cutover",
                "29:remote_protocol_v2_pairing_offers",
                "30:provider_runtime_option_snapshots",
                "31:agent_usage_statistics",
                "32:agent_usage_zero_baseline_fence",
                "33:managed_worktree_recovery_foundation",
                "34:worktree_merge_lifecycle",
                "35:permission_response_options",
                "36:agent_elicitation_requests",
                "37:agent_provider_projection_platform",
                "38:agent_runtime_provider_probe_evidence",
                "39:agent_runtime_option_snapshots",
                "40:agent_auth_catalog_snapshots",
                "41:agent_managed_installations",
                "42:provider_model_runtime_option_snapshots",
                "43:agent_model_provider_display_order",
                "44:runtime_switch_activation_completion",
                "45:agent_auth_context_and_runtime_source",
                "46:runtime_auth_source_nullable_legacy_columns",
                "47:agent_default_usage_model_nullable",
                "48:agent_usage_counter_scope",
                "49:message_submission_runtime_policy"
            ]
        );
        assert_eq!(
            current_schema_version(&conn).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        for table in ["agent_usage_checkpoints", "agent_turn_usage_facts"] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "v31 must create usage table {table}");
        }
        for index in [
            "idx_agent_usage_checkpoints_session",
            "idx_agent_turn_usage_facts_dispatch",
            "idx_agent_turn_usage_facts_session",
            "idx_agent_turn_usage_facts_project",
            "idx_agent_turn_usage_facts_agent",
            "idx_agent_turn_usage_facts_profile",
            "idx_agent_turn_usage_facts_model",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    params![index],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "v31 must create usage index {index}");
        }
        let display_order_table: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'agent_model_provider_display_order'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(display_order_table, 1);
        let display_order_index: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_agent_model_provider_display_order'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(display_order_index, 1);
        for table in [
            "agent_sessions",
            "session_runtime_bindings",
            "runtime_switches",
            "agent_message_submissions",
        ] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "v28 must clear legacy rows from {table}");
        }
        for table in ["provider_bindings", "session_provider_bindings"] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "v28 must drop {table}");
        }
        let columns = conn
            .prepare("SELECT name FROM pragma_table_info('agent_sessions')")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            !columns.iter().any(|column| {
                matches!(column.as_str(), "provider_kind" | "provider_profile_id")
            })
        );
        let binding_columns = conn
            .prepare("SELECT name FROM pragma_table_info('session_runtime_bindings')")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for column in [
            "usage_zero_baseline_state",
            "usage_zero_baseline_execution_id",
            "usage_zero_baseline_activation_generation",
        ] {
            assert!(
                binding_columns.iter().any(|candidate| candidate == column),
                "v32 must add runtime binding column {column}"
            );
        }

        let workspace_root = std::env::temp_dir().join(format!(
            "vibex-schema-v28-new-{}",
            RequestId::new().as_str()
        ));
        let (project, workspace) =
            WorkspaceRepository::ensure(&conn, &workspace_root, WorkspaceMode::CurrentCheckout)
                .unwrap();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: "ACP-only session".to_string(),
            project_id: project.id,
            workspace_id: workspace.id,
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("claude").unwrap(),
            state: AgentSessionState::Initializing,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now,
            updated_at_ms: now,
            last_message_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&conn, &session).unwrap();
        let selection = SessionRuntimeSelection {
            reasoning_effort: Some("high".to_string()),
            ..SessionRuntimeSelection::provider(
                session.agent_id.clone(),
                ProviderProfileId::parse("provider_acp_cutover").unwrap(),
                "claude-sonnet",
            )
        };
        let switch_id = RuntimeSwitchId::new();
        let target_binding_id = RuntimeBindingId::new();
        let switch = AgentSessionRuntimeRepository::enqueue_initial_runtime_switch(
            &mut conn,
            switch_id.clone(),
            &DesiredRuntimeSwitchEnqueueRequest {
                session_id: session.id.clone(),
                idempotency_key: format!("session-init:{}", session.id.as_str()),
                expected_revision: 0,
                expected_selection_revision: 0,
                target_binding_id: target_binding_id.clone(),
                target_adapter_id: AcpAdapterId::parse("claude-agent-acp").unwrap(),
                target_auth_source_revision: 1,
                desired: selection.clone(),
                requested_policy: RuntimeSwitchPolicy::Automatic,
                active_work_policy: RuntimeSwitchActiveWorkPolicy::default(),
                requested_session_config: serde_json::json!({
                    "effectiveSelection": selection,
                    "profileRevision": 1,
                }),
            },
        )
        .unwrap();
        assert_eq!(switch.switch_id, switch_id);

        RuntimeBindingRepository::insert(
            &conn,
            &RuntimeBinding {
                binding_id: target_binding_id.clone(),
                session_id: session.id.clone(),
                agent_id: session.agent_id.clone(),
                transport_kind: TransportKind::Acp,
                auth_source: selection.auth_source.clone(),
                auth_source_revision: 1,
                adapter_id: AcpAdapterId::parse("claude-agent-acp").unwrap(),
                adapter_version: "0.58.1".to_string(),
                adapter_compatibility_identity: "adapter=claude-agent-acp@0.58.1".to_string(),
                native_session_id: Some("native_cutover".to_string()),
                native_state_home_id: NativeStateHomeId::new(),
                provider_resume_identity: None,
                process_spawn_fingerprint: "fingerprint_cutover".to_string(),
                session_runtime_config_state: SessionRuntimeConfigState::default(),
                capability_snapshot: None,
                restore_compatibility_key: None,
                last_context_sequence: 0,
                last_summary_sequence: 0,
                context_bridge_version: 0,
                activation_generation: 0,
                binding_state: BindingState::Preparing,
                created_by_switch_id: Some(switch_id),
                created_at_ms: now,
                updated_at_ms: now,
            },
        )
        .unwrap();
        let zero_baseline: (String, Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT usage_zero_baseline_state, usage_zero_baseline_execution_id,
                        usage_zero_baseline_activation_generation
                 FROM session_runtime_bindings WHERE binding_id = ?1",
                params![target_binding_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(zero_baseline, ("available".to_string(), None, None));
        let submission = MessageSubmissionRepository::enqueue(
            &mut conn,
            MessageSubmissionId::new(),
            &SendAgentMessageRequest {
                session_id: session.id.clone(),
                message_idempotency_key: "message:cutover".to_string(),
                desired_runtime: selection,
                text: "cutover message".to_string(),
                attachments: Vec::new(),
                reasoning_effort: Some("high".to_string()),
                correlation_id: None,
            },
        )
        .unwrap();
        assert_eq!(submission.session_id, session.id);

        drop(conn);
        cleanup_db(temp);
    }

    fn automation_agent_node_request(
        id: AutomationNodeId,
        title: &str,
    ) -> AutomationNodeCreateRequest {
        AutomationNodeCreateRequest {
            id: Some(id),
            kind: AutomationNodeKind::AgentPrompt,
            title: title.to_string(),
            config: AutomationNodeConfig::AgentPrompt(AutomationAgentPromptConfig {
                prompt_template: format!("Run {title}"),
                provider_kind: Some(ProviderKind::Codex),
                provider_profile_id: None,
                safety: None,
                workspace_root: None,
                workspace_mode: Some(WorkspaceMode::CurrentCheckout),
            }),
            position: Some(AutomationNodePosition { x: 1, y: 2 }),
        }
    }

    fn automation_approval_node_request(
        id: AutomationNodeId,
        title: &str,
    ) -> AutomationNodeCreateRequest {
        AutomationNodeCreateRequest {
            id: Some(id),
            kind: AutomationNodeKind::ApprovalGate,
            title: title.to_string(),
            config: AutomationNodeConfig::ApprovalGate(AutomationApprovalGateConfig {
                title: title.to_string(),
                details: "Review bounded automation output".to_string(),
                risk_category: PermissionRiskCategory::Command,
                allowed_responses: vec![
                    PermissionResponseKind::Approve,
                    PermissionResponseKind::Deny,
                ],
            }),
            position: None,
        }
    }

    #[test]
    fn automation_graph_migration_creates_contract_tables() {
        let temp = temp_db_path("automation-graph-migration");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        assert_eq!(
            current_schema_version(&conn).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        for table in [
            "automation_graphs",
            "automation_graph_nodes",
            "automation_graph_edges",
            "automation_graph_runs",
            "automation_graph_run_steps",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing table {table}");
        }

        cleanup_db(temp);
    }

    #[test]
    fn automation_graph_lifecycle_and_definition_replacement_round_trip() {
        let temp = temp_db_path("automation-graph-lifecycle");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let (_project, workspace) = WorkspaceRepository::ensure(
            &conn,
            "/tmp/vibex-db-automation-graph",
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let first_node_id = AutomationNodeId::new();
        let second_node_id = AutomationNodeId::new();
        let graph = AutomationGraphRepository::create(
            &mut conn,
            AutomationGraphCreateRequest {
                title: "Local review graph".to_string(),
                description: Some("Provider-neutral graph contract".to_string()),
                project_id: None,
                workspace_id: Some(workspace.id.clone()),
                workspace_root: workspace.root_path.clone(),
                workspace_mode: workspace.mode,
                provider_kind: Some(ProviderKind::Codex),
                provider_profile_id: None,
                trigger: AutomationGraphTrigger::Manual,
                nodes: vec![
                    automation_agent_node_request(first_node_id.clone(), "Prompt"),
                    automation_approval_node_request(second_node_id.clone(), "Approve"),
                ],
                edges: vec![AutomationEdgeCreateRequest {
                    source_node_id: first_node_id.clone(),
                    target_node_id: second_node_id.clone(),
                    condition: AutomationEdgeCondition {
                        kind: AutomationEdgeConditionKind::OnSuccess,
                        expression: Some("safe_to_continue".to_string()),
                    },
                }],
            },
        )
        .unwrap();
        assert_eq!(graph.status, AutomationGraphStatus::Active);
        assert_eq!(graph.version, 1);
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);

        let loaded = AutomationGraphRepository::get(&conn, &graph.id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.trigger, AutomationGraphTrigger::Manual);
        assert!(loaded.nodes.iter().any(|node| node.id == first_node_id));
        assert_eq!(loaded.edges[0].source_node_id, first_node_id);

        let listed = AutomationGraphRepository::list(
            &conn,
            AutomationGraphListRequest {
                workspace_id: Some(workspace.id.clone()),
                status: Some(AutomationGraphStatus::Active),
                include_deleted: false,
                limit: Some(10),
            },
        )
        .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, graph.id);

        let updated = AutomationGraphRepository::update(
            &conn,
            AutomationGraphUpdateRequest {
                id: graph.id.clone(),
                title: Some("Updated local review graph".to_string()),
                description: None,
                clear_description: true,
                project_id: None,
                clear_project_id: false,
                workspace_id: None,
                clear_workspace_id: false,
                workspace_root: None,
                workspace_mode: None,
                provider_kind: None,
                clear_provider_kind: true,
                provider_profile_id: None,
                clear_provider_profile_id: false,
                trigger: None,
                status: None,
            },
        )
        .unwrap();
        assert_eq!(updated.title, "Updated local review graph");
        assert!(updated.description.is_none());
        assert!(updated.provider_kind.is_none());
        assert_eq!(updated.version, 2);

        let replacement_node_id = AutomationNodeId::new();
        let replaced = AutomationGraphRepository::replace_definition(
            &mut conn,
            &graph.id,
            vec![automation_agent_node_request(
                replacement_node_id.clone(),
                "Replacement",
            )],
            Vec::new(),
            Some(2),
        )
        .unwrap();
        assert_eq!(replaced.version, 3);
        assert_eq!(replaced.nodes.len(), 1);
        assert_eq!(replaced.nodes[0].id, replacement_node_id);
        assert!(replaced.edges.is_empty());

        let stale = AutomationGraphRepository::replace_definition(
            &mut conn,
            &graph.id,
            vec![automation_agent_node_request(
                AutomationNodeId::new(),
                "Stale",
            )],
            Vec::new(),
            Some(2),
        )
        .unwrap_err();
        assert_eq!(stale.code, "automation_graph_version_conflict");
        let current = AutomationGraphRepository::get(&conn, &graph.id)
            .unwrap()
            .unwrap();
        assert_eq!(current.version, 3);
        assert_eq!(current.nodes[0].id, replacement_node_id);

        let deleted = AutomationGraphRepository::soft_delete(&conn, &graph.id).unwrap();
        assert_eq!(deleted.status, AutomationGraphStatus::Deleted);
        assert!(deleted.deleted_at_ms.is_some());
        assert!(
            AutomationGraphRepository::get(&conn, &graph.id)
                .unwrap()
                .is_none()
        );

        let deleted_list = AutomationGraphRepository::list(
            &conn,
            AutomationGraphListRequest {
                workspace_id: Some(workspace.id),
                status: Some(AutomationGraphStatus::Deleted),
                include_deleted: true,
                limit: Some(10),
            },
        )
        .unwrap();
        assert_eq!(deleted_list.len(), 1);
        assert_eq!(deleted_list[0].id, graph.id);

        cleanup_db(temp);
    }

    #[test]
    fn automation_graph_run_and_step_history_bounds_diagnostics() {
        let temp = temp_db_path("automation-graph-runs");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let node_id = AutomationNodeId::new();
        let graph = AutomationGraphRepository::create(
            &mut conn,
            AutomationGraphCreateRequest {
                title: "Run graph".to_string(),
                description: None,
                project_id: None,
                workspace_id: None,
                workspace_root: "/tmp/vibex-db-automation-runs".to_string(),
                workspace_mode: WorkspaceMode::CurrentCheckout,
                provider_kind: Some(ProviderKind::Codex),
                provider_profile_id: None,
                trigger: AutomationGraphTrigger::Manual,
                nodes: vec![automation_agent_node_request(node_id.clone(), "Run step")],
                edges: Vec::new(),
            },
        )
        .unwrap();

        let run = AutomationGraphRepository::create_run(
            &conn,
            AutomationRunCreateRequest {
                graph_id: graph.id.clone(),
                status: AutomationRunStatus::Failed,
                trigger: AutomationRunTrigger::Manual,
                scheduled_task_id: None,
                session_id: None,
                started_at_ms: Some(1_800_000_000_100),
                ended_at_ms: Some(1_800_000_000_200),
                error_code: Some("e".repeat(SCHEDULED_TASK_ERROR_CODE_MAX_CHARS + 1)),
                error_message: Some("m".repeat(SCHEDULED_TASK_ERROR_MESSAGE_MAX_CHARS + 1)),
                redacted_diagnostics: vec![RedactedDiagnostic {
                    key: "k".repeat(SCHEDULED_TASK_DIAGNOSTIC_KEY_MAX_CHARS + 1),
                    value: "v".repeat(SCHEDULED_TASK_DIAGNOSTIC_VALUE_MAX_CHARS + 1),
                }],
            },
        )
        .unwrap();
        assert_eq!(
            run.error_code.as_ref().unwrap().chars().count(),
            SCHEDULED_TASK_ERROR_CODE_MAX_CHARS
        );
        assert_eq!(
            run.redacted_diagnostics[0].value.chars().count(),
            SCHEDULED_TASK_DIAGNOSTIC_VALUE_MAX_CHARS
        );

        let updated_run = AutomationGraphRepository::update_run(
            &conn,
            AutomationRunUpdateRequest {
                id: run.id.clone(),
                status: Some(AutomationRunStatus::Succeeded),
                scheduled_task_id: None,
                clear_scheduled_task_id: false,
                session_id: None,
                clear_session_id: false,
                started_at_ms: None,
                clear_started_at_ms: false,
                ended_at_ms: Some(1_800_000_000_300),
                clear_ended_at_ms: false,
                error_code: None,
                clear_error_code: true,
                error_message: None,
                clear_error_message: true,
                redacted_diagnostics: Some(Vec::new()),
            },
        )
        .unwrap();
        assert_eq!(updated_run.status, AutomationRunStatus::Succeeded);
        assert!(updated_run.error_code.is_none());

        let runs = AutomationGraphRepository::list_runs(
            &conn,
            AutomationRunListRequest {
                graph_id: Some(graph.id),
                status: Some(AutomationRunStatus::Succeeded),
                limit: Some(10),
            },
        )
        .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run.id);

        let permission_id = RequestId::new();
        let step = AutomationGraphRepository::create_run_step(
            &conn,
            AutomationRunStepCreateRequest {
                run_id: run.id.clone(),
                node_id: node_id.clone(),
                status: AutomationRunStepStatus::WaitingForApproval,
                session_id: None,
                permission_request_id: Some(permission_id.clone()),
                started_at_ms: Some(1_800_000_000_150),
                ended_at_ms: None,
                error_code: None,
                error_message: None,
                redacted_diagnostics: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(step.permission_request_id, Some(permission_id));

        let updated_step = AutomationGraphRepository::update_run_step(
            &conn,
            AutomationRunStepUpdateRequest {
                id: step.id.clone(),
                status: Some(AutomationRunStepStatus::Succeeded),
                session_id: None,
                clear_session_id: false,
                permission_request_id: None,
                clear_permission_request_id: true,
                started_at_ms: None,
                clear_started_at_ms: false,
                ended_at_ms: Some(1_800_000_000_250),
                clear_ended_at_ms: false,
                error_code: None,
                clear_error_code: false,
                error_message: None,
                clear_error_message: false,
                redacted_diagnostics: Some(vec![RedactedDiagnostic {
                    key: "state".to_string(),
                    value: "approved".to_string(),
                }]),
            },
        )
        .unwrap();
        assert_eq!(updated_step.status, AutomationRunStepStatus::Succeeded);
        assert!(updated_step.permission_request_id.is_none());

        let steps = AutomationGraphRepository::list_run_steps(
            &conn,
            AutomationRunStepListRequest {
                run_id: Some(run.id),
                node_id: Some(node_id),
                status: Some(AutomationRunStepStatus::Succeeded),
                limit: Some(10),
            },
        )
        .unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].id, step.id);
        assert_eq!(steps[0].redacted_diagnostics[0].key, "state");

        cleanup_db(temp);
    }

    #[test]
    fn workspace_ensure_reuses_a_worktree_registered_under_its_origin_project() {
        let temp = temp_db_path("workspace-ensure-registered-worktree");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let (project, checkout) = WorkspaceRepository::ensure(
            &conn,
            "/tmp/vibex-db-worktree-origin",
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let worktree = WorkspaceRepository::ensure_for_project(
            &conn,
            &project.id,
            "/tmp/vibex-db-worktree-linked",
            WorkspaceMode::VibexWorktree,
        )
        .unwrap();

        let (resolved_project, resolved_workspace) =
            WorkspaceRepository::ensure(&conn, &worktree.root_path, WorkspaceMode::VibexWorktree)
                .unwrap();

        assert_eq!(resolved_project.id, project.id);
        assert_eq!(resolved_workspace.id, worktree.id);
        assert_eq!(resolved_workspace.project_id, checkout.project_id);
        assert_eq!(WorkspaceRepository::list(&conn).unwrap().len(), 2);

        cleanup_db(temp);
    }

    #[test]
    fn workspace_delete_project_soft_deletes_records_without_touching_files() {
        let temp = temp_db_path("workspace-delete-project");
        let project_dir = std::env::temp_dir().join(format!(
            "vibex-project-delete-{}",
            RequestId::new().as_str()
        ));
        fs::create_dir_all(&project_dir).unwrap();
        let sentinel_path = project_dir.join("keep.txt");
        fs::write(&sentinel_path, "do not delete").unwrap();
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let (project, workspace) =
            WorkspaceRepository::ensure(&conn, &project_dir, WorkspaceMode::CurrentCheckout)
                .unwrap();
        let now = unix_timestamp_ms();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: "Project session".to_string(),
            project_id: project.id.clone(),
            workspace_id: workspace.id.clone(),
            workspace_root: workspace.root_path.clone(),
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("codex").unwrap(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now,
            updated_at_ms: now,
            last_message_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&conn, &session).unwrap();

        WorkspaceRepository::delete_project(&mut conn, &project.id).unwrap();

        assert!(sentinel_path.exists());
        assert!(WorkspaceRepository::list(&conn).unwrap().is_empty());
        assert!(
            WorkspaceRepository::get(&conn, &workspace.id)
                .unwrap()
                .is_none()
        );
        assert!(SessionRepository::list(&conn, false).unwrap().is_empty());

        let (_restored_project, restored_workspace) =
            WorkspaceRepository::ensure(&conn, &project_dir, WorkspaceMode::CurrentCheckout)
                .unwrap();
        assert_eq!(restored_workspace.id, workspace.id);
        assert_eq!(WorkspaceRepository::list(&conn).unwrap().len(), 1);

        cleanup_db(temp);
        fs::remove_dir_all(project_dir).unwrap();
    }

    #[test]
    fn scheduled_task_lifecycle_round_trip() {
        let temp = temp_db_path("scheduled-task-lifecycle");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let (project, workspace) = WorkspaceRepository::ensure(
            &conn,
            "/tmp/vibex-db-scheduled-task",
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();

        let task = ScheduledTaskRepository::create(
            &conn,
            ScheduledTaskCreateRequest {
                title: "Daily summary".to_string(),
                prompt: "Summarize the repository state".to_string(),
                project_id: Some(project.id.clone()),
                workspace_id: Some(workspace.id.clone()),
                workspace_root: workspace.root_path.clone(),
                workspace_mode: workspace.mode,
                provider_kind: ProviderKind::Codex,
                provider_profile_id: None,
                schedule: ScheduledTaskSchedule::Daily(ScheduledTaskDailySchedule {
                    local_time_minutes: 9 * 60,
                    timezone: "Asia/Shanghai".to_string(),
                    start_at_ms: 1_800_000_000_000,
                    end_at_ms: None,
                }),
                safety: None,
                next_run_at_ms: Some(1_800_003_600_000),
            },
        )
        .unwrap();
        assert_eq!(task.status, ScheduledTaskStatus::Active);

        let loaded = ScheduledTaskRepository::get(&conn, &task.id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.schedule, task.schedule);
        assert_eq!(
            loaded.safety,
            AgentSessionSafety::workspace_write_ask_on_risk()
        );

        let listed = ScheduledTaskRepository::list(
            &conn,
            ScheduledTaskListRequest {
                workspace_id: Some(workspace.id.clone()),
                status: Some(ScheduledTaskStatus::Active),
                include_deleted: false,
                limit: Some(10),
            },
        )
        .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, task.id);

        let updated = ScheduledTaskRepository::update(
            &conn,
            ScheduledTaskUpdateRequest {
                id: task.id.clone(),
                title: Some("Hourly summary".to_string()),
                prompt: Some("Summarize recent changes".to_string()),
                project_id: None,
                clear_project_id: false,
                workspace_id: None,
                clear_workspace_id: false,
                workspace_root: None,
                workspace_mode: None,
                provider_kind: None,
                provider_profile_id: None,
                clear_provider_profile_id: false,
                schedule: Some(ScheduledTaskSchedule::Interval(
                    ScheduledTaskIntervalSchedule {
                        every_seconds: 3600,
                        start_at_ms: 1_800_000_000_000,
                        end_at_ms: Some(1_800_086_400_000),
                    },
                )),
                safety: None,
                next_run_at_ms: None,
                clear_next_run_at_ms: true,
            },
        )
        .unwrap();
        assert_eq!(updated.title, "Hourly summary");
        assert_eq!(updated.next_run_at_ms, None);

        let paused = ScheduledTaskRepository::pause(&conn, &task.id).unwrap();
        assert_eq!(paused.status, ScheduledTaskStatus::Paused);
        let resumed = ScheduledTaskRepository::resume(&conn, &task.id).unwrap();
        assert_eq!(resumed.status, ScheduledTaskStatus::Active);

        let deleted = ScheduledTaskRepository::soft_delete(&conn, &task.id).unwrap();
        assert_eq!(deleted.status, ScheduledTaskStatus::Deleted);
        assert!(deleted.deleted_at_ms.is_some());
        assert!(
            ScheduledTaskRepository::get(&conn, &task.id)
                .unwrap()
                .is_none()
        );

        let deleted_list = ScheduledTaskRepository::list(
            &conn,
            ScheduledTaskListRequest {
                workspace_id: Some(workspace.id),
                status: Some(ScheduledTaskStatus::Deleted),
                include_deleted: true,
                limit: Some(10),
            },
        )
        .unwrap();
        assert_eq!(deleted_list.len(), 1);
        assert_eq!(deleted_list[0].id, task.id);

        cleanup_db(temp);
    }

    #[test]
    fn scheduled_task_run_history_bounds_diagnostics_and_updates() {
        let temp = temp_db_path("scheduled-task-runs");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let (_project, workspace) = WorkspaceRepository::ensure(
            &conn,
            "/tmp/vibex-db-scheduled-task-run",
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let task = ScheduledTaskRepository::create(
            &conn,
            ScheduledTaskCreateRequest {
                title: "One shot".to_string(),
                prompt: "Run once".to_string(),
                project_id: None,
                workspace_id: Some(workspace.id),
                workspace_root: workspace.root_path,
                workspace_mode: WorkspaceMode::CurrentCheckout,
                provider_kind: ProviderKind::Codex,
                provider_profile_id: None,
                schedule: ScheduledTaskSchedule::OneShot(ScheduledTaskOneShotSchedule {
                    run_at_ms: 1_800_000_000_000,
                }),
                safety: None,
                next_run_at_ms: Some(1_800_000_000_000),
            },
        )
        .unwrap();

        let run = ScheduledTaskRepository::create_run(
            &conn,
            ScheduledTaskRunCreateRequest {
                task_id: task.id.clone(),
                status: ScheduledTaskRunStatus::Failed,
                trigger: ScheduledTaskRunTrigger::Scheduler,
                session_id: None,
                due_at_ms: 1_800_000_000_000,
                started_at_ms: Some(1_800_000_000_100),
                ended_at_ms: Some(1_800_000_000_200),
                attempt: 1,
                error_code: Some("x".repeat(SCHEDULED_TASK_ERROR_CODE_MAX_CHARS + 10)),
                error_message: Some("y".repeat(SCHEDULED_TASK_ERROR_MESSAGE_MAX_CHARS + 10)),
                redacted_diagnostics: vec![RedactedDiagnostic {
                    key: "k".repeat(SCHEDULED_TASK_DIAGNOSTIC_KEY_MAX_CHARS + 10),
                    value: "v".repeat(SCHEDULED_TASK_DIAGNOSTIC_VALUE_MAX_CHARS + 10),
                }],
            },
        )
        .unwrap();
        assert_eq!(
            run.error_code.as_ref().unwrap().chars().count(),
            SCHEDULED_TASK_ERROR_CODE_MAX_CHARS
        );
        assert_eq!(
            run.error_message.as_ref().unwrap().chars().count(),
            SCHEDULED_TASK_ERROR_MESSAGE_MAX_CHARS
        );
        assert_eq!(
            run.redacted_diagnostics[0].key.chars().count(),
            SCHEDULED_TASK_DIAGNOSTIC_KEY_MAX_CHARS
        );
        assert_eq!(
            run.redacted_diagnostics[0].value.chars().count(),
            SCHEDULED_TASK_DIAGNOSTIC_VALUE_MAX_CHARS
        );

        let updated = ScheduledTaskRepository::update_run(
            &conn,
            ScheduledTaskRunUpdateRequest {
                id: run.id.clone(),
                status: Some(ScheduledTaskRunStatus::Succeeded),
                session_id: None,
                clear_session_id: false,
                started_at_ms: None,
                clear_started_at_ms: false,
                ended_at_ms: Some(1_800_000_000_300),
                clear_ended_at_ms: false,
                attempt: Some(2),
                error_code: None,
                clear_error_code: true,
                error_message: None,
                clear_error_message: true,
                redacted_diagnostics: Some(Vec::new()),
            },
        )
        .unwrap();
        assert_eq!(updated.status, ScheduledTaskRunStatus::Succeeded);
        assert_eq!(updated.attempt, 2);
        assert!(updated.error_code.is_none());
        assert!(updated.redacted_diagnostics.is_empty());

        let runs = ScheduledTaskRepository::list_runs(
            &conn,
            ScheduledTaskRunListRequest {
                task_id: Some(task.id),
                session_id: None,
                status: Some(ScheduledTaskRunStatus::Succeeded),
                limit: Some(10),
            },
        )
        .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run.id);

        cleanup_db(temp);
    }

    #[test]
    fn scheduled_task_attention_and_audit_are_bounded_projections() {
        let temp = temp_db_path("scheduled-task-audit");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let (_project, workspace) = WorkspaceRepository::ensure(
            &conn,
            "/tmp/vibex-db-scheduled-task-audit",
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let task = ScheduledTaskRepository::create(
            &conn,
            ScheduledTaskCreateRequest {
                title: "Permission gated run".to_string(),
                prompt: "Prompt text must stay out of audit projections".to_string(),
                project_id: None,
                workspace_id: Some(workspace.id.clone()),
                workspace_root: workspace.root_path.clone(),
                workspace_mode: WorkspaceMode::CurrentCheckout,
                provider_kind: ProviderKind::Codex,
                provider_profile_id: None,
                schedule: ScheduledTaskSchedule::OneShot(ScheduledTaskOneShotSchedule {
                    run_at_ms: 1_800_000_000_000,
                }),
                safety: None,
                next_run_at_ms: Some(1_800_000_000_000),
            },
        )
        .unwrap();
        let run = ScheduledTaskRepository::create_run(
            &conn,
            ScheduledTaskRunCreateRequest {
                task_id: task.id.clone(),
                status: ScheduledTaskRunStatus::Skipped,
                trigger: ScheduledTaskRunTrigger::Scheduler,
                session_id: None,
                due_at_ms: 1_800_000_000_000,
                started_at_ms: Some(1_800_000_000_100),
                ended_at_ms: Some(1_800_000_000_200),
                attempt: 1,
                error_code: Some(SCHEDULED_TASK_PERMISSION_REQUIRED_CODE.to_string()),
                error_message: Some("Open session to review the provider request.".to_string()),
                redacted_diagnostics: vec![RedactedDiagnostic {
                    key: "state".to_string(),
                    value: "needs_input".to_string(),
                }],
            },
        )
        .unwrap();
        let _success = ScheduledTaskRepository::create_run(
            &conn,
            ScheduledTaskRunCreateRequest {
                task_id: task.id.clone(),
                status: ScheduledTaskRunStatus::Succeeded,
                trigger: ScheduledTaskRunTrigger::Manual,
                session_id: None,
                due_at_ms: 1_800_000_001_000,
                started_at_ms: Some(1_800_000_001_100),
                ended_at_ms: Some(1_800_000_001_200),
                attempt: 1,
                error_code: None,
                error_message: None,
                redacted_diagnostics: Vec::new(),
            },
        )
        .unwrap();

        let attention = ScheduledTaskRepository::list_attention(
            &conn,
            ScheduledTaskAttentionListRequest {
                workspace_id: Some(workspace.id.clone()),
                limit: Some(10),
            },
        )
        .unwrap();
        assert_eq!(attention.len(), 1);
        assert_eq!(attention[0].run_id, run.id);
        assert_eq!(
            attention[0].attention_kind,
            ScheduledTaskAttentionKind::PermissionRequired
        );
        assert_eq!(attention[0].task_title, "Permission gated run");

        let audit = ScheduledTaskRepository::list_audit(
            &conn,
            ScheduledTaskAuditListRequest {
                workspace_id: Some(workspace.id),
                status: Some(ScheduledTaskRunStatus::Skipped),
                limit: Some(10),
            },
        )
        .unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(
            audit[0].audit_id,
            format!("scheduled_audit:{}", run.id.as_str())
        );
        assert_eq!(
            audit[0].outcome,
            ScheduledTaskAuditOutcome::PermissionRequired
        );
        assert_eq!(audit[0].redacted_diagnostics[0].key, "state");
        let audit_json = serde_json::to_string(&audit[0]).unwrap();
        assert!(!audit_json.contains("Prompt text must stay out"));

        cleanup_db(temp);
    }

    #[test]
    fn scheduled_task_due_claim_and_recovery_helpers_are_deterministic() {
        let temp = temp_db_path("scheduled-task-runtime");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let due_now = 1_800_000_000_000;
        let later = due_now + 60_000;
        let due_task = ScheduledTaskRepository::create(
            &conn,
            ScheduledTaskCreateRequest {
                title: "Due task".to_string(),
                prompt: "Run now".to_string(),
                project_id: None,
                workspace_id: None,
                workspace_root: "/tmp/vibex-db-scheduled-task-due".to_string(),
                workspace_mode: WorkspaceMode::CurrentCheckout,
                provider_kind: ProviderKind::Codex,
                provider_profile_id: None,
                schedule: ScheduledTaskSchedule::OneShot(ScheduledTaskOneShotSchedule {
                    run_at_ms: due_now,
                }),
                safety: None,
                next_run_at_ms: Some(due_now),
            },
        )
        .unwrap();
        let future_task = ScheduledTaskRepository::create(
            &conn,
            ScheduledTaskCreateRequest {
                title: "Future task".to_string(),
                prompt: "Run later".to_string(),
                project_id: None,
                workspace_id: None,
                workspace_root: "/tmp/vibex-db-scheduled-task-future".to_string(),
                workspace_mode: WorkspaceMode::CurrentCheckout,
                provider_kind: ProviderKind::Codex,
                provider_profile_id: None,
                schedule: ScheduledTaskSchedule::OneShot(ScheduledTaskOneShotSchedule {
                    run_at_ms: later,
                }),
                safety: None,
                next_run_at_ms: Some(later),
            },
        )
        .unwrap();

        let due = ScheduledTaskRepository::list_due(&conn, due_now, Some(10)).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, due_task.id);

        let (claimed_task, run) =
            ScheduledTaskRepository::claim_due(&mut conn, &due_task.id, due_now)
                .unwrap()
                .unwrap();
        assert_eq!(claimed_task.next_run_at_ms, None);
        assert_eq!(run.status, ScheduledTaskRunStatus::Running);
        assert_eq!(run.trigger, ScheduledTaskRunTrigger::Scheduler);
        assert_eq!(run.due_at_ms, due_now);
        assert_eq!(run.started_at_ms, Some(due_now));
        assert!(
            ScheduledTaskRepository::claim_due(&mut conn, &due_task.id, due_now)
                .unwrap()
                .is_none()
        );

        let due_after_claim = ScheduledTaskRepository::list_due(&conn, later, Some(10)).unwrap();
        assert_eq!(due_after_claim.len(), 1);
        assert_eq!(due_after_claim[0].id, future_task.id);

        let updated_task = ScheduledTaskRepository::mark_task_after_run(
            &conn,
            &due_task.id,
            ScheduledTaskStatus::Paused,
            None,
            due_now + 1,
        )
        .unwrap();
        assert_eq!(updated_task.status, ScheduledTaskStatus::Paused);
        assert_eq!(updated_task.next_run_at_ms, None);

        let stale =
            ScheduledTaskRepository::list_stale_running_runs(&conn, due_now + 5_000, Some(10))
                .unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].id, run.id);

        cleanup_db(temp);
    }

    #[test]
    fn managed_worktree_and_operation_round_trip() {
        let temp = temp_db_path("worktree");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let (project, main_workspace) = WorkspaceRepository::ensure(
            &conn,
            "/tmp/vibex-db-worktree-main",
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let worktree_workspace = WorkspaceRepository::ensure_for_project(
            &conn,
            &project.id,
            "/tmp/vibex-db-worktree-feature",
            WorkspaceMode::VibexWorktree,
        )
        .unwrap();
        assert_eq!(worktree_workspace.project_id, project.id);

        let now = unix_timestamp_ms();
        let record = ManagedWorktreeRecord {
            worktree_id: RequestId::new(),
            project_id: project.id.clone(),
            workspace_id: Some(worktree_workspace.id.clone()),
            repo_root: main_workspace.root_path.clone(),
            worktree_path: worktree_workspace.root_path.clone(),
            repository_identity: None,
            worktree_path_identity: None,
            branch: Some("feature/demo".to_string()),
            origin_workspace_id: Some(main_workspace.id.clone()),
            base_ref: Some("main".to_string()),
            base_head: Some("abc1234".to_string()),
            target_workspace_id: Some(main_workspace.id.clone()),
            target_branch: Some("main".to_string()),
            head: Some("abc1234".to_string()),
            status: GitManagedWorktreeStatus::Active,
            reconciliation_state: GitWorktreeReconciliationState::Consistent,
            diagnostic: None,
            created_at_ms: now,
            updated_at_ms: now,
            closed_at_ms: None,
        };
        ManagedWorktreeRepository::insert(&conn, &record).unwrap();
        let stored = ManagedWorktreeRepository::get_by_path(&conn, &record.worktree_path)
            .unwrap()
            .unwrap();
        assert_eq!(stored.branch, record.branch);
        assert_eq!(stored.status, GitManagedWorktreeStatus::Active);

        let readiness = GitWorktreeReadinessRecord {
            worktree_id: record.worktree_id.clone(),
            workspace_id: worktree_workspace.id.clone(),
            state: vibex_core::GitWorktreeReadinessState::ReadyToMerge,
            source_head: "abc1234".to_string(),
            dirty_fingerprint: "clean:abc1234".to_string(),
            target_workspace_id: main_workspace.id.clone(),
            target_branch: "main".to_string(),
            checks: vec![vibex_core::GitWorktreeCheckRecord {
                command: "cargo test -p demo".to_string(),
                outcome: vibex_core::GitWorktreeCheckOutcome::Passed,
                recorded_at_ms: now,
            }],
            revision: "ready-v1".to_string(),
            updated_at_ms: now,
        };
        WorktreeReadinessRepository::upsert(&conn, &readiness).unwrap();
        assert_eq!(
            WorktreeReadinessRepository::get_by_workspace_id(&conn, &worktree_workspace.id)
                .unwrap(),
            Some(readiness.clone())
        );
        assert_eq!(
            WorktreeReadinessRepository::list_for_project(&conn, &project.id).unwrap(),
            vec![readiness]
        );

        let operation = GitWorktreeOperationRecord {
            operation_id: RequestId::new(),
            project_id: project.id.clone(),
            source_workspace_id: Some(worktree_workspace.id.clone()),
            target_workspace_id: Some(main_workspace.id.clone()),
            operation: GitWorktreeOperationKind::MergeBack,
            status: GitWorktreeOperationStatus::Pending,
            worktree_path: Some(record.worktree_path.clone()),
            branch: record.branch.clone(),
            base_ref: record.base_ref.clone(),
            head_before: Some("abc1234".to_string()),
            head_after: None,
            error: None,
            detail: GitWorktreeOperationDetail {
                idempotency_key: Some("merge:worktree-demo".to_string()),
                request_fingerprint: Some("merge:worktree-demo:main".to_string()),
                origin_workspace_id: Some(main_workspace.id.clone()),
                base_head: record.base_head.clone(),
                target_branch: record.target_branch.clone(),
                expected_source_head: record.head.clone(),
                expected_target_head: Some("abc1234".to_string()),
                ..GitWorktreeOperationDetail::default()
            },
            created_at_ms: now,
            updated_at_ms: now,
        };
        WorktreeOperationRepository::insert(&conn, &operation).unwrap();
        let completed = WorktreeOperationRepository::update(
            &conn,
            &operation.operation_id,
            GitWorktreeOperationStatus::Completed,
            Some("def5678"),
            None,
        )
        .unwrap();
        assert_eq!(completed.status, GitWorktreeOperationStatus::Completed);
        assert_eq!(completed.head_after.as_deref(), Some("def5678"));

        ManagedWorktreeRepository::update_status(
            &conn,
            &record.worktree_path,
            GitManagedWorktreeStatus::Archived,
            Some("def5678"),
            Some(now + 1),
        )
        .unwrap();
        let archived = ManagedWorktreeRepository::get_by_id(&conn, &record.worktree_id)
            .unwrap()
            .unwrap();
        assert_eq!(archived.status, GitManagedWorktreeStatus::Archived);
        assert_eq!(archived.origin_workspace_id, record.origin_workspace_id);
        assert_eq!(archived.base_head, record.base_head);
        assert_eq!(archived.target_workspace_id, record.target_workspace_id);
        assert_eq!(archived.target_branch, record.target_branch);

        conn.execute(
            "DELETE FROM workspaces WHERE workspace_id = ?1",
            params![worktree_workspace.id.as_str()],
        )
        .unwrap();
        let detached = ManagedWorktreeRepository::get_by_id(&conn, &record.worktree_id)
            .unwrap()
            .unwrap();
        assert!(detached.workspace_id.is_none());
        assert_eq!(detached.origin_workspace_id, record.origin_workspace_id);
        assert_eq!(detached.target_workspace_id, record.target_workspace_id);
        let detached_operation = WorktreeOperationRepository::get(&conn, &operation.operation_id)
            .unwrap()
            .unwrap();
        assert!(detached_operation.source_workspace_id.is_none());
        assert_eq!(
            detached_operation.target_workspace_id,
            Some(main_workspace.id)
        );
        assert!(
            WorktreeReadinessRepository::get_by_worktree_id(&conn, &record.worktree_id)
                .unwrap()
                .is_none()
        );

        cleanup_db(temp);
    }

    #[test]
    fn worktree_v33_migrates_legacy_rows_additively() {
        let temp = temp_db_path("worktree-v33-legacy");
        let mut conn = open_database(&temp).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at_ms INTEGER NOT NULL
            );",
        )
        .unwrap();
        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version <= 32)
        {
            let transaction = conn.transaction().unwrap();
            transaction.execute_batch(migration.sql).unwrap();
            transaction
                .execute(
                    "INSERT INTO schema_migrations (version, name, applied_at_ms)
                     VALUES (?1, ?2, ?3)",
                    params![migration.version, migration.name, unix_timestamp_ms()],
                )
                .unwrap();
            transaction.commit().unwrap();
        }
        let (project, workspace) = WorkspaceRepository::ensure(
            &conn,
            "/tmp/vibex-db-worktree-v33-legacy",
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let worktree_id = RequestId::new();
        let operation_id = RequestId::new();
        let now = unix_timestamp_ms();
        conn.execute(
            "INSERT INTO git_managed_worktrees (
                worktree_id, project_id, workspace_id, repo_root, worktree_path,
                branch, base_ref, head, status, created_at_ms, updated_at_ms, closed_at_ms
             ) VALUES (?1, ?2, NULL, ?3, ?4, 'feature/legacy', 'main', 'abc',
                       'active', ?5, ?5, NULL)",
            params![
                worktree_id.as_str(),
                project.id.as_str(),
                workspace.root_path,
                "/tmp/vibex-db-worktree-v33-legacy-feature",
                now
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO git_worktree_operations (
                operation_id, project_id, source_workspace_id, target_workspace_id,
                operation, status, worktree_path, branch, base_ref, head_before,
                head_after, error, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, NULL, 'create', 'running', ?4,
                       'feature/legacy', 'main', 'abc', NULL, NULL, ?5, ?5)",
            params![
                operation_id.as_str(),
                project.id.as_str(),
                workspace.id.as_str(),
                "/tmp/vibex-db-worktree-v33-legacy-feature",
                now
            ],
        )
        .unwrap();

        assert_eq!(
            apply_migrations(&mut conn).unwrap(),
            vec![
                "33:managed_worktree_recovery_foundation",
                "34:worktree_merge_lifecycle",
                "35:permission_response_options",
                "36:agent_elicitation_requests",
                "37:agent_provider_projection_platform",
                "38:agent_runtime_provider_probe_evidence",
                "39:agent_runtime_option_snapshots",
                "40:agent_auth_catalog_snapshots",
                "41:agent_managed_installations",
                "42:provider_model_runtime_option_snapshots",
                "43:agent_model_provider_display_order",
                "44:runtime_switch_activation_completion",
                "45:agent_auth_context_and_runtime_source",
                "46:runtime_auth_source_nullable_legacy_columns",
                "47:agent_default_usage_model_nullable",
                "48:agent_usage_counter_scope",
                "49:message_submission_runtime_policy"
            ]
        );
        let managed = ManagedWorktreeRepository::get_by_id(&conn, &worktree_id)
            .unwrap()
            .unwrap();
        assert_eq!(managed.branch.as_deref(), Some("feature/legacy"));
        assert_eq!(
            managed.reconciliation_state,
            GitWorktreeReconciliationState::Unverified
        );
        assert!(managed.repository_identity.is_none());
        let operation = WorktreeOperationRepository::get(&conn, &operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation.status, GitWorktreeOperationStatus::Running);
        assert_eq!(
            operation.detail.checkpoint,
            GitWorktreeOperationCheckpoint::IntentRecorded
        );
        assert_eq!(operation.detail.attempt, 0);
        let readiness_table: String = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'git_worktree_readiness'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(readiness_table, "git_worktree_readiness");

        drop(conn);
        cleanup_db(temp);
    }

    #[test]
    fn worktree_operation_reserve_and_lease_claim_are_idempotent() {
        let temp = temp_db_path("worktree-operation-claim");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();
        let (project, workspace) = WorkspaceRepository::ensure(
            &conn,
            "/tmp/vibex-db-worktree-operation-claim",
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let now = unix_timestamp_ms();
        let operation = GitWorktreeOperationRecord {
            operation_id: RequestId::new(),
            project_id: project.id.clone(),
            source_workspace_id: Some(workspace.id.clone()),
            target_workspace_id: Some(workspace.id.clone()),
            operation: GitWorktreeOperationKind::Create,
            status: GitWorktreeOperationStatus::Pending,
            worktree_path: Some("/tmp/vibex-db-worktree-operation-claim-feature".to_string()),
            branch: Some("feature/claim".to_string()),
            base_ref: Some("main".to_string()),
            head_before: Some("abc".to_string()),
            head_after: None,
            error: None,
            detail: GitWorktreeOperationDetail {
                idempotency_key: Some("create:workspace:feature-claim".to_string()),
                request_fingerprint: Some("create:workspace:feature-claim:main".to_string()),
                origin_workspace_id: Some(workspace.id.clone()),
                ..GitWorktreeOperationDetail::default()
            },
            created_at_ms: now,
            updated_at_ms: now,
        };
        let first = WorktreeOperationRepository::reserve(&mut conn, &operation).unwrap();
        let mut retry = operation.clone();
        retry.operation_id = RequestId::new();
        let repeated = WorktreeOperationRepository::reserve(&mut conn, &retry).unwrap();
        assert_eq!(repeated.operation_id, first.operation_id);

        let mut conflict = retry;
        conflict.detail.request_fingerprint = Some("different-request".to_string());
        let error = WorktreeOperationRepository::reserve(&mut conn, &conflict).unwrap_err();
        assert_eq!(error.code, "worktree_operation_idempotency_conflict");

        let claimed = WorktreeOperationRepository::try_claim(
            &conn,
            &first.operation_id,
            "worker-a",
            now,
            1_000,
        )
        .unwrap();
        let WorktreeOperationClaimOutcome::Acquired(claimed) = claimed else {
            panic!("first worker did not acquire operation");
        };
        assert_eq!(claimed.detail.attempt, 1);
        assert!(matches!(
            WorktreeOperationRepository::try_claim(
                &conn,
                &first.operation_id,
                "worker-b",
                now + 1,
                1_000,
            )
            .unwrap(),
            WorktreeOperationClaimOutcome::Busy(_)
        ));
        let takeover = WorktreeOperationRepository::try_claim(
            &conn,
            &first.operation_id,
            "worker-b",
            now + 1_001,
            1_000,
        )
        .unwrap();
        let WorktreeOperationClaimOutcome::Acquired(takeover) = takeover else {
            panic!("expired lease was not taken over");
        };
        assert_eq!(takeover.detail.attempt, 2);
        WorktreeOperationRepository::mark_outcome(
            &conn,
            &first.operation_id,
            GitWorktreeOperationStatus::Completed,
            GitWorktreeOperationCheckpoint::Completed,
            Some("def"),
            None,
        )
        .unwrap();
        assert!(matches!(
            WorktreeOperationRepository::try_claim(
                &conn,
                &first.operation_id,
                "worker-c",
                now + 2_100,
                1_000,
            )
            .unwrap(),
            WorktreeOperationClaimOutcome::Completed(_)
        ));
        assert_eq!(
            WorktreeOperationRepository::list_for_project(&conn, &project.id)
                .unwrap()
                .len(),
            1
        );

        drop(conn);
        cleanup_db(temp);
    }

    #[test]
    fn provider_native_export_preview_apply_and_list_round_trip() {
        let temp = temp_db_path("native-export");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let profile =
            ProviderProfileRepository::from_create_request(ProviderProfileCreateRequest {
                agent_id: None,
                kind: ProviderKind::Codex,
                display_name: "Native export profile".to_string(),
                account_alias: None,
                base_url: Some("https://api.example.test/v1".to_string()),
                default_model: Some("gpt-test".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            });
        ProviderProfileRepository::insert(&conn, &profile).unwrap();

        let now = unix_timestamp_ms();
        let export_id = RequestId::new();
        let file = ProviderNativeExportFilePlan {
            operation_id: RequestId::new(),
            source: ProviderNativeExportSource::Codex,
            file_kind: ProviderNativeConfigFileKind::CodexConfigToml,
            operation_kind: ProviderNativeExportOperationKind::UpdateFile,
            target_path: "/tmp/vibex-native-export/config.toml".to_string(),
            backup_path: Some("/tmp/vibex-native-export/config.toml.bak".to_string()),
            temp_path: Some("/tmp/vibex-native-export/config.toml.tmp".to_string()),
            marker: Some("Vibex managed TOML block".to_string()),
            redacted_before: "model = \"old\"\n".to_string(),
            redacted_after: "model = \"new\"\n".to_string(),
            redacted_diff: "--- current\n+++ vibex\n-model = \"old\"\n+model = \"new\"\n"
                .to_string(),
            rollback_plan: "restore backup".to_string(),
            diagnostics: Vec::new(),
            status: ProviderNativeExportFileStatus::Ready,
        };
        let preview = ProviderNativeExportPreview {
            export_id: export_id.clone(),
            provider_profile_id: profile.id.clone(),
            source: ProviderNativeExportSource::Codex,
            mode: ProviderNativeExportMode::ProviderProfile,
            files: vec![file.clone()],
            diagnostics: vec![ProviderBindingMetadata {
                key: "secretPolicy".to_string(),
                value: "redacted references only".to_string(),
            }],
            created_at_ms: now,
        };
        ProviderNativeExportRepository::insert_preview(&conn, &preview).unwrap();

        let loaded = ProviderNativeExportRepository::get_preview(&conn, &export_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.export_id, export_id);
        assert_eq!(loaded.files[0].redacted_diff, file.redacted_diff);

        let apply = ProviderNativeExportApplyResult {
            export_id: export_id.clone(),
            status: ProviderNativeExportApplyStatus::Applied,
            files: vec![ProviderNativeExportFilePlan {
                status: ProviderNativeExportFileStatus::Applied,
                ..file
            }],
            diagnostics: Vec::new(),
            applied_at_ms: now + 1,
        };
        ProviderNativeExportRepository::record_apply_result(&conn, &apply).unwrap();

        let records = ProviderNativeExportRepository::list(
            &conn,
            ProviderNativeExportListRequest {
                provider_profile_id: Some(profile.id),
                limit: Some(10),
            },
        )
        .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, "applied");
        assert_eq!(records[0].file_count, 1);
        assert_eq!(records[0].blocked_count, 0);

        cleanup_db(temp);
    }

    #[test]
    fn provider_profile_secret_default_and_preview_round_trip() {
        let temp = temp_db_path("provider");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        ProviderProfileRepository::ensure_local_defaults(&conn).unwrap();
        let defaults = ProviderProfileRepository::list(&conn).unwrap();
        assert!(defaults.iter().any(|profile| {
            profile.id.as_str() == ProviderKind::Codex.local_default_profile_id()
        }));

        let create = ProviderProfileCreateRequest {
            agent_id: None,
            kind: ProviderKind::Claude,
            display_name: "Claude work".to_string(),
            account_alias: Some("work".to_string()),
            base_url: Some("https://api.anthropic.invalid".to_string()),
            default_model: Some("claude-sonnet".to_string()),
            small_model: None,
            large_model: None,
            configured_models: Vec::new(),
            reasoning_effort: None,
            sandbox_defaults: None,
            network_defaults: None,
            permission_defaults: None,
            provider_options: None,
            secret_references: vec![ProviderSecretReferenceCreateRequest {
                secret_kind: ProviderSecretKind::AuthToken,
                backend: ProviderSecretBackend::Placeholder,
                setup_state: ProviderSecretSetupState::Missing,
                lookup_key: "ANTHROPIC_API_KEY".to_string(),
                display_label: "Anthropic API key".to_string(),
                redacted_hint: "not configured".to_string(),
            }],
        };
        let profile = ProviderProfileRepository::from_create_request(create);
        ProviderProfileRepository::insert(&conn, &profile).unwrap();
        let loaded = ProviderProfileRepository::get(&conn, &profile.id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.secrets.len(), 1);
        assert_eq!(loaded.secrets[0].lookup_key, "ANTHROPIC_API_KEY");

        let scope = ProviderProfileDefaultScope {
            kind: ProviderDefaultScopeKind::Global,
            project_id: None,
            workspace_id: None,
        };
        ProviderDefaultProfileRepository::set(
            &conn,
            ProviderProfileSetDefaultRequest {
                scope: scope.clone(),
                provider_kind: ProviderKind::Claude,
                provider_profile_id: profile.id.clone(),
            },
        )
        .unwrap();
        let selection =
            ProviderDefaultProfileRepository::get(&conn, scope, ProviderKind::Claude).unwrap();
        assert_eq!(selection.provider_profile_id.as_ref(), Some(&profile.id));

        let preview = ProviderInjectionPreview {
            preview_id: RequestId::new(),
            profile: loaded.summary(),
            strategy_order: Vec::new(),
            endpoint: loaded.base_url.clone(),
            model: loaded.default_model.clone(),
            sdk_options: Vec::new(),
            cli_args: Vec::new(),
            env: Vec::new(),
            overlay_files: Vec::new(),
            mcp_servers: Vec::new(),
            skills: Vec::new(),
            sandbox_defaults: loaded.sandbox_defaults.clone(),
            network_defaults: loaded.network_defaults.clone(),
            permission_defaults: loaded.permission_defaults.clone(),
            created_at_ms: unix_timestamp_ms(),
        };
        ProviderInjectionPreviewRepository::insert(
            &conn,
            &ProviderInjectionPreviewRequest {
                provider_profile_id: loaded.id,
                project_id: None,
                workspace_id: None,
                session_id: None,
                persist: true,
            },
            &preview,
        )
        .unwrap();

        cleanup_db(temp);
    }

    #[test]
    fn provider_profile_list_uses_model_provider_capability_whitelist() {
        let temp = temp_db_path("provider-capability-whitelist");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let mut supported = ProviderProfile::local_default(ProviderKind::Acp);
        supported.id = ProviderProfileId::new();
        supported.agent_id = AgentId::parse("glm-acp-agent").unwrap();
        supported.display_name = "GLM custom provider".to_string();
        supported.status = ProviderProfileStatus::Enabled;
        ProviderProfileRepository::insert(&conn, &supported).unwrap();

        let mut unsupported = ProviderProfile::local_default(ProviderKind::Acp);
        unsupported.id = ProviderProfileId::new();
        unsupported.agent_id = AgentId::parse("cline").unwrap();
        unsupported.display_name = "Cline internal profile".to_string();
        unsupported.status = ProviderProfileStatus::Enabled;
        ProviderProfileRepository::insert(&conn, &unsupported).unwrap();

        let listed = ProviderProfileRepository::list(&conn).unwrap();
        assert!(listed.iter().any(|profile| profile.id == supported.id));
        assert!(!listed.iter().any(|profile| profile.id == unsupported.id));
        assert!(
            ProviderProfileRepository::list_all(&conn)
                .unwrap()
                .iter()
                .any(|profile| profile.id == unsupported.id),
            "runtime profile listing must retain unsupported Agent profiles"
        );
        assert!(
            ProviderProfileRepository::list_by_agent(&conn, &unsupported.agent_id, true)
                .unwrap()
                .iter()
                .any(|profile| profile.id == unsupported.id),
            "non-configurable Agents retain their internal runtime profiles"
        );

        cleanup_db(temp);
    }

    #[test]
    fn provider_profiles_are_agent_scoped_with_default_and_failover_queue() {
        let temp = temp_db_path("provider-agent-scope");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        ProviderProfileRepository::ensure_local_defaults(&conn).unwrap();
        let codex_profiles = ProviderProfileRepository::list_by_agent(
            &conn,
            &AgentId::parse("codex").unwrap(),
            true,
        )
        .unwrap();
        assert!(
            codex_profiles.iter().any(
                |profile| profile.id.as_str() == ProviderKind::Codex.local_default_profile_id()
            )
        );
        assert!(
            codex_profiles
                .iter()
                .all(|profile| profile.agent_id.as_str() == "codex")
        );

        let agent_id = AgentId::parse("opencode").unwrap();
        let profile =
            ProviderProfileRepository::from_create_request(ProviderProfileCreateRequest {
                agent_id: Some(agent_id.clone()),
                kind: ProviderKind::Acp,
                display_name: "OpenCode ACP scoped".to_string(),
                account_alias: None,
                base_url: None,
                default_model: Some("opencode-default".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            });
        ProviderProfileRepository::insert(&conn, &profile).unwrap();

        let acp_profiles =
            ProviderProfileRepository::list_by_agent(&conn, &agent_id, true).unwrap();
        assert!(
            acp_profiles
                .iter()
                .any(|candidate| candidate.id == profile.id)
        );

        let scope = ProviderProfileDefaultScope {
            kind: ProviderDefaultScopeKind::Global,
            project_id: None,
            workspace_id: None,
        };
        let default = AgentDefaultModelProviderProfileRepository::set(
            &conn,
            scope.clone(),
            agent_id.clone(),
            profile.id.clone(),
        )
        .unwrap();
        assert_eq!(default.provider_profile_id.as_ref(), Some(&profile.id));
        let loaded_default =
            AgentDefaultModelProviderProfileRepository::get(&conn, scope.clone(), agent_id.clone())
                .unwrap();
        assert_eq!(
            loaded_default.provider_profile_id.as_ref(),
            Some(&profile.id)
        );

        let queue = AgentModelProviderFailoverRepository::replace(
            &mut conn,
            &agent_id,
            &[AgentModelProviderFailoverEntry {
                agent_id: agent_id.clone(),
                provider_profile_id: profile.id.clone(),
                display_name: profile.display_name.clone(),
                status: profile.status,
                order_index: 0,
                enabled: true,
                updated_at_ms: unix_timestamp_ms(),
            }],
        )
        .unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].provider_profile_id, profile.id);
        assert!(queue[0].enabled);

        let display_order = AgentModelProviderDisplayOrderRepository::replace(
            &mut conn,
            &agent_id,
            &[AgentModelProviderDisplayOrderEntry {
                agent_id: agent_id.clone(),
                provider_profile_id: profile.id.clone(),
                order_index: 0,
                updated_at_ms: unix_timestamp_ms(),
            }],
        )
        .unwrap();
        assert_eq!(display_order.len(), 1);
        assert_eq!(display_order[0].provider_profile_id, profile.id);

        ProviderDefaultProfileRepository::set(
            &conn,
            ProviderProfileSetDefaultRequest {
                scope: scope.clone(),
                provider_kind: ProviderKind::Acp,
                provider_profile_id: profile.id.clone(),
            },
        )
        .unwrap();

        ProviderProfileRepository::soft_delete(&mut conn, &profile.id).unwrap();
        assert!(
            AgentDefaultModelProviderProfileRepository::get(&conn, scope.clone(), agent_id.clone())
                .unwrap()
                .provider_profile_id
                .is_none()
        );
        assert!(
            ProviderDefaultProfileRepository::get(&conn, scope.clone(), ProviderKind::Acp)
                .unwrap()
                .provider_profile_id
                .is_none()
        );
        assert!(
            AgentModelProviderFailoverRepository::list(&conn, &agent_id)
                .unwrap()
                .is_empty()
        );
        assert!(
            AgentModelProviderDisplayOrderRepository::list(&conn, &agent_id)
                .unwrap()
                .is_empty()
        );
        for table in [
            "provider_default_profiles",
            "agent_default_model_provider_profiles",
            "agent_model_provider_failover",
            "agent_model_provider_display_order",
        ] {
            let count: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE provider_profile_id = ?1"),
                    params![profile.id.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "{table} retained a deleted profile reference");
        }

        let scope_kind = enum_to_db(&scope.kind).unwrap();
        let scope_id = scope.storage_key();
        let now = unix_timestamp_ms();
        conn.execute(
            "
            INSERT INTO agent_default_model_provider_profiles (
                scope_kind, scope_id, agent_id, provider_profile_id,
                created_at_ms, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            ",
            params![
                scope_kind,
                scope_id,
                agent_id.as_str(),
                profile.id.as_str(),
                now
            ],
        )
        .unwrap();
        conn.execute(
            "
            INSERT INTO provider_default_profiles (
                scope_kind, scope_id, provider_kind, provider_profile_id,
                created_at_ms, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            ",
            params![
                enum_to_db(&scope.kind).unwrap(),
                scope.storage_key(),
                enum_to_db(&ProviderKind::Acp).unwrap(),
                profile.id.as_str(),
                now
            ],
        )
        .unwrap();
        assert!(
            AgentDefaultModelProviderProfileRepository::get(&conn, scope.clone(), agent_id)
                .unwrap()
                .provider_profile_id
                .is_none()
        );
        assert!(
            ProviderDefaultProfileRepository::get(&conn, scope, ProviderKind::Acp)
                .unwrap()
                .provider_profile_id
                .is_none()
        );

        cleanup_db(temp);
    }

    #[test]
    fn agent_config_and_discovery_round_trip() {
        let temp = temp_db_path("agent");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let agent_id = AgentId::parse("opencode").unwrap();
        let mut env = BTreeMap::new();
        env.insert("NO_BROWSER".to_string(), "1".to_string());
        let now = unix_timestamp_ms();
        let mut config = AgentConfig {
            agent_id: agent_id.clone(),
            runtime_kind: AgentRuntimeKind::Acp,
            source_kind: AgentSourceKind::Catalog,
            label_override: Some("OpenCode".to_string()),
            description_override: Some("ACP backed OpenCode".to_string()),
            enabled: true,
            order_index: 10,
            command: Some(AgentCommandConfig {
                command: "opencode".to_string(),
                args: vec!["serve".to_string()],
            }),
            env,
            params: serde_json::json!({ "preset": "opencode" }),
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        };
        AgentConfigRepository::upsert(&conn, &config).unwrap();

        config.enabled = false;
        config.updated_at_ms = now + 1;
        AgentConfigRepository::upsert(&conn, &config).unwrap();

        let loaded = AgentConfigRepository::get(&conn, &agent_id)
            .unwrap()
            .unwrap();
        assert!(!loaded.enabled);
        assert_eq!(loaded.command.unwrap().command, "opencode");
        assert_eq!(AgentConfigRepository::list(&conn).unwrap().len(), 1);

        AgentDiscoveryRepository::insert(
            &conn,
            &AgentDiscoveryRecord {
                discovery_record_id: "agent_discovery_old".to_string(),
                agent_id: agent_id.clone(),
                cwd_scope: "home".to_string(),
                install_status: AgentInstallStatus::Missing,
                config_status: AgentConfigStatus::NeedsConfiguration,
                runtime_status: AgentRuntimeStatus::Unavailable,
                binary_path: None,
                version: None,
                native_config_paths: Vec::new(),
                models: Vec::new(),
                modes: Vec::new(),
                diagnostics: Vec::new(),
                discovered_at_ms: now,
            },
        )
        .unwrap();
        AgentDiscoveryRepository::insert(
            &conn,
            &AgentDiscoveryRecord {
                discovery_record_id: "agent_discovery_new".to_string(),
                agent_id: agent_id.clone(),
                cwd_scope: "home".to_string(),
                install_status: AgentInstallStatus::Installed,
                config_status: AgentConfigStatus::Configured,
                runtime_status: AgentRuntimeStatus::Ready,
                binary_path: Some("/usr/bin/opencode".to_string()),
                version: Some("1.0.0".to_string()),
                native_config_paths: vec!["/home/user/.opencode.json".to_string()],
                models: vec!["anthropic/claude-sonnet".to_string()],
                modes: vec!["chat".to_string(), "plan".to_string()],
                diagnostics: Vec::new(),
                discovered_at_ms: now + 1,
            },
        )
        .unwrap();

        let latest = AgentDiscoveryRepository::latest_for_agent(&conn, &agent_id, "home")
            .unwrap()
            .unwrap();
        assert_eq!(latest.discovery_record_id, "agent_discovery_new");
        assert_eq!(latest.models, vec!["anthropic/claude-sonnet"]);

        let by_agent = AgentDiscoveryRepository::latest_by_agent(&conn, "home").unwrap();
        assert_eq!(
            by_agent.get(&agent_id).unwrap().discovery_record_id,
            "agent_discovery_new"
        );

        cleanup_db(temp);
    }

    #[test]
    fn mcp_server_secret_matrix_and_soft_delete_round_trip() {
        let temp = temp_db_path("mcp");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let request = McpServerCreateRequest {
            display_name: "Filesystem tools".to_string(),
            transport_kind: McpServerTransportKind::Stdio,
            status: McpServerStatus::Enabled,
            scope_kind: McpServerScopeKind::Workspace,
            project_id: None,
            workspace_id: None,
            command: Some("mcp-filesystem".to_string()),
            args: vec!["--root".to_string(), "/tmp/workspace".to_string()],
            url: None,
            description: Some("Local filesystem MCP server".to_string()),
            tags: vec!["local".to_string(), "filesystem".to_string()],
            secret_references: vec![McpServerSecretReferenceCreateRequest {
                secret_kind: ProviderSecretKind::Environment,
                backend: ProviderSecretBackend::Placeholder,
                setup_state: ProviderSecretSetupState::Missing,
                lookup_key: "MCP_FILESYSTEM_TOKEN".to_string(),
                display_label: "Filesystem token".to_string(),
                redacted_hint: "not configured".to_string(),
                target: McpSecretTarget::Environment,
            }],
            provider_matrix: vec![
                McpServerProviderMatrix {
                    provider_kind: ProviderKind::Codex,
                    enabled: true,
                    updated_at_ms: unix_timestamp_ms(),
                },
                McpServerProviderMatrix {
                    provider_kind: ProviderKind::Codex,
                    enabled: true,
                    updated_at_ms: unix_timestamp_ms(),
                },
                McpServerProviderMatrix {
                    provider_kind: ProviderKind::Claude,
                    enabled: false,
                    updated_at_ms: unix_timestamp_ms(),
                },
            ],
        };
        let mut server = McpServerRepository::from_create_request(request);
        let server_id = server.id.clone();
        server.agent_matrix = vec![
            McpServerAgentMatrix {
                agent_id: AgentId::parse("codex").unwrap(),
                enabled: true,
                source_kind: vibex_core::ResourceAgentMatrixSourceKind::Manual,
                updated_at_ms: unix_timestamp_ms(),
            },
            McpServerAgentMatrix {
                agent_id: AgentId::parse("opencode").unwrap(),
                enabled: false,
                source_kind: vibex_core::ResourceAgentMatrixSourceKind::Manual,
                updated_at_ms: unix_timestamp_ms(),
            },
        ];
        McpServerRepository::insert(&conn, &server).unwrap();

        let loaded = McpServerRepository::get(&conn, &server_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.display_name, "Filesystem tools");
        assert_eq!(loaded.secret_references.len(), 1);
        assert_eq!(loaded.secret_references[0].redacted_hint, "not configured");
        assert!(!format!("{loaded:?}").contains("secret-token"));
        assert!(
            loaded
                .provider_matrix
                .iter()
                .any(|entry| { entry.provider_kind == ProviderKind::Codex && entry.enabled })
        );
        assert!(
            loaded
                .agent_matrix
                .iter()
                .any(|entry| entry.agent_id.as_str() == "codex" && entry.enabled)
        );

        let mock_enabled =
            McpServerRepository::list_enabled_for_provider(&conn, ProviderKind::Codex).unwrap();
        assert_eq!(mock_enabled.len(), 1);
        assert_eq!(mock_enabled[0].id, server_id);
        let claude_enabled =
            McpServerRepository::list_enabled_for_provider(&conn, ProviderKind::Claude).unwrap();
        assert!(claude_enabled.is_empty());
        let codex_agent_enabled = McpServerRepository::list_enabled_for_agent(
            &conn,
            &AgentId::parse("codex").unwrap(),
            ProviderKind::Codex,
        )
        .unwrap();
        assert_eq!(codex_agent_enabled.len(), 1);
        let opencode_agent_enabled = McpServerRepository::list_enabled_for_agent(
            &conn,
            &AgentId::parse("opencode").unwrap(),
            ProviderKind::Acp,
        )
        .unwrap();
        assert!(opencode_agent_enabled.is_empty());

        McpServerRepository::replace_provider_matrix(
            &conn,
            &server_id,
            &[McpServerProviderMatrix {
                provider_kind: ProviderKind::Claude,
                enabled: true,
                updated_at_ms: unix_timestamp_ms(),
            }],
        )
        .unwrap();
        let claude_enabled =
            McpServerRepository::list_enabled_for_provider(&conn, ProviderKind::Claude).unwrap();
        assert_eq!(claude_enabled.len(), 1);
        McpServerRepository::replace_agent_matrix(
            &conn,
            &server_id,
            &[McpServerAgentMatrix {
                agent_id: AgentId::parse("opencode").unwrap(),
                enabled: true,
                source_kind: vibex_core::ResourceAgentMatrixSourceKind::Manual,
                updated_at_ms: unix_timestamp_ms(),
            }],
        )
        .unwrap();
        let opencode_agent_enabled = McpServerRepository::list_enabled_for_agent(
            &conn,
            &AgentId::parse("opencode").unwrap(),
            ProviderKind::Acp,
        )
        .unwrap();
        assert_eq!(opencode_agent_enabled.len(), 1);

        McpServerRepository::soft_delete(&conn, &server_id).unwrap();
        assert!(
            McpServerRepository::get(&conn, &server_id)
                .unwrap()
                .is_none()
        );

        cleanup_db(temp);
    }

    #[test]
    fn skill_matrix_and_soft_delete_round_trip() {
        let temp = temp_db_path("skill");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let mut skill = SkillRepository::from_create_request(SkillCreateRequest {
            display_name: "Rust workspace guide".to_string(),
            source_kind: SkillSourceKind::Manual,
            status: SkillStatus::Enabled,
            scope_kind: SkillScopeKind::Workspace,
            project_id: None,
            workspace_id: None,
            source_uri: None,
            description: Some("Use cargo package-scoped checks".to_string()),
            tags: vec!["rust".to_string(), "quality".to_string()],
            content_preview: Some("Run package-scoped cargo checks.".to_string()),
            provider_matrix: vec![
                SkillProviderMatrix {
                    provider_kind: ProviderKind::Codex,
                    enabled: true,
                    updated_at_ms: unix_timestamp_ms(),
                },
                SkillProviderMatrix {
                    provider_kind: ProviderKind::Claude,
                    enabled: false,
                    updated_at_ms: unix_timestamp_ms(),
                },
            ],
        });
        let skill_id = skill.id.clone();
        skill.agent_matrix = vec![SkillAgentMatrix {
            agent_id: AgentId::parse("codex").unwrap(),
            enabled: true,
            source_kind: vibex_core::ResourceAgentMatrixSourceKind::Manual,
            updated_at_ms: unix_timestamp_ms(),
        }];
        SkillRepository::insert(&conn, &skill).unwrap();

        let loaded = SkillRepository::get(&conn, &skill_id).unwrap().unwrap();
        assert_eq!(loaded.display_name, "Rust workspace guide");
        assert_eq!(loaded.provider_matrix.len(), 2);
        assert_eq!(loaded.agent_matrix.len(), 1);
        assert!(
            SkillRepository::list_enabled_for_provider(&conn, ProviderKind::Codex)
                .unwrap()
                .iter()
                .any(|item| item.id == skill_id)
        );
        assert!(
            SkillRepository::list_enabled_for_provider(&conn, ProviderKind::Claude)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            SkillRepository::list_enabled_for_agent(
                &conn,
                &AgentId::parse("codex").unwrap(),
                ProviderKind::Codex,
            )
            .unwrap()
            .len(),
            1
        );
        assert!(
            SkillRepository::list_enabled_for_agent(
                &conn,
                &AgentId::parse("claude").unwrap(),
                ProviderKind::Claude,
            )
            .unwrap()
            .is_empty()
        );

        SkillRepository::replace_provider_matrix(
            &conn,
            &skill_id,
            &[SkillProviderMatrix {
                provider_kind: ProviderKind::Claude,
                enabled: true,
                updated_at_ms: unix_timestamp_ms(),
            }],
        )
        .unwrap();
        assert_eq!(
            SkillRepository::list_enabled_for_provider(&conn, ProviderKind::Claude)
                .unwrap()
                .len(),
            1
        );
        SkillRepository::replace_agent_matrix(
            &conn,
            &skill_id,
            &[SkillAgentMatrix {
                agent_id: AgentId::parse("claude").unwrap(),
                enabled: true,
                source_kind: vibex_core::ResourceAgentMatrixSourceKind::Manual,
                updated_at_ms: unix_timestamp_ms(),
            }],
        )
        .unwrap();
        assert_eq!(
            SkillRepository::list_enabled_for_agent(
                &conn,
                &AgentId::parse("claude").unwrap(),
                ProviderKind::Claude,
            )
            .unwrap()
            .len(),
            1
        );

        SkillRepository::soft_delete(&conn, &skill_id).unwrap();
        assert!(SkillRepository::get(&conn, &skill_id).unwrap().is_none());

        cleanup_db(temp);
    }

    #[test]
    fn prompt_persistence_and_soft_delete_round_trip() {
        let temp = temp_db_path("prompt");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let mut prompt = PromptRepository::from_create_request(PromptCreateRequest {
            display_name: "Release notes".to_string(),
            kind: PromptKind::ReusablePrompt,
            status: PromptStatus::Enabled,
            scope_kind: PromptScopeKind::User,
            project_id: None,
            workspace_id: None,
            body: "Summarize merged changes with risks.".to_string(),
            description: Some("Reusable release summary prompt".to_string()),
            tags: vec!["release".to_string()],
        });
        let prompt_id = prompt.id.clone();
        PromptRepository::insert(&conn, &prompt).unwrap();

        let loaded = PromptRepository::get(&conn, &prompt_id).unwrap().unwrap();
        assert_eq!(loaded.body, "Summarize merged changes with risks.");
        assert_eq!(PromptRepository::list_enabled(&conn).unwrap().len(), 1);

        prompt.display_name = "Release digest".to_string();
        prompt.updated_at_ms = unix_timestamp_ms();
        PromptRepository::update(&conn, &prompt).unwrap();
        assert_eq!(
            PromptRepository::get(&conn, &prompt_id)
                .unwrap()
                .unwrap()
                .display_name,
            "Release digest"
        );

        PromptRepository::soft_delete(&conn, &prompt_id).unwrap();
        assert!(PromptRepository::get(&conn, &prompt_id).unwrap().is_none());

        cleanup_db(temp);
    }

    #[test]
    fn hook_preview_metadata_round_trip_without_native_write() {
        let temp = temp_db_path("hook");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let hook = HookRepository::from_create_request(HookCreateRequest {
            display_name: "Terminal activity audit".to_string(),
            provider_kind: ProviderKind::Claude,
            event_kind: HookEventKind::TerminalActivity,
            status: HookStatus::Draft,
            command_preview: Some("vibex hook terminal-activity".to_string()),
            managed_marker: Some("VIBEX-MANAGED-HOOK:test".to_string()),
            description: Some("Preview only".to_string()),
        });
        let hook_id = hook.id.clone();
        HookRepository::insert(&conn, &hook).unwrap();
        assert_eq!(
            HookRepository::get(&conn, &hook_id)
                .unwrap()
                .unwrap()
                .managed_marker,
            "VIBEX-MANAGED-HOOK:test"
        );

        let preview = HookInstallPreview {
            preview_id: RequestId::new(),
            hook_id: hook_id.clone(),
            target_path: "~/.claude/settings.json".to_string(),
            marker: "VIBEX-MANAGED-HOOK:test".to_string(),
            redacted_preview: "Preview only; future install removes only Vibex-owned marker blocks"
                .to_string(),
            created_at_ms: unix_timestamp_ms(),
        };
        HookRepository::insert_install_preview(&conn, &preview).unwrap();

        HookRepository::soft_delete(&conn, &hook_id).unwrap();
        assert!(HookRepository::get(&conn, &hook_id).unwrap().is_none());

        cleanup_db(temp);
    }

    #[test]
    fn runtime_option_snapshot_failure_preserves_last_success() {
        let temp = temp_db_path("runtime-option-snapshot");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let agent_id = AgentId::parse("opencode").unwrap();
        let create_profile = |display_name: &str| {
            ProviderProfileRepository::from_create_request(ProviderProfileCreateRequest {
                agent_id: Some(agent_id.clone()),
                kind: ProviderKind::Acp,
                display_name: display_name.to_string(),
                account_alias: None,
                base_url: None,
                default_model: None,
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
        };
        let successful_profile = create_profile("Cached ACP profile");
        let failed_profile = create_profile("Unavailable ACP profile");
        ProviderProfileRepository::insert(&conn, &successful_profile).unwrap();
        ProviderProfileRepository::insert(&conn, &failed_profile).unwrap();

        let success = ProviderRuntimeOptionSnapshotRecord {
            provider_profile_id: successful_profile.id.clone(),
            agent_id: agent_id.clone(),
            model_response: Some(AgentModelListResponse {
                agent_id: Some(agent_id.clone()),
                provider_kind: ProviderKind::Acp,
                provider_profile_id: Some(successful_profile.id.clone()),
                models: vec!["opencode/test-model".to_string()],
                reasoning_efforts: vec![AgentReasoningEffort {
                    value: "high".to_string(),
                    description: None,
                }],
                model_capabilities: Vec::new(),
                source: AgentModelListSource::Probed,
                diagnostics: Vec::new(),
            }),
            session_config: Some(AgentSessionConfigProbe {
                models: vec!["opencode/test-model".to_string()],
                modes: vec![ProviderSessionConfigValue {
                    value: "plan".to_string(),
                    label: Some("Plan".to_string()),
                }],
                reasoning_efforts: vec![AgentReasoningEffort {
                    value: "high".to_string(),
                    description: None,
                }],
                options: Vec::new(),
            }),
            last_success_at_ms: Some(100),
            last_attempt_at_ms: 100,
            last_error_code: None,
        };
        ProviderRuntimeOptionSnapshotRepository::upsert_success(&conn, &success).unwrap();
        ProviderRuntimeOptionSnapshotRepository::record_failure(
            &conn,
            &successful_profile.id,
            &agent_id,
            200,
            "agent_probe_failed",
        )
        .unwrap();
        ProviderRuntimeOptionSnapshotRepository::record_failure(
            &conn,
            &failed_profile.id,
            &agent_id,
            300,
            "agent_not_installed",
        )
        .unwrap();

        let snapshots = ProviderRuntimeOptionSnapshotRepository::list(&conn).unwrap();
        let preserved = snapshots
            .iter()
            .find(|snapshot| snapshot.provider_profile_id == successful_profile.id)
            .unwrap();
        assert_eq!(preserved.model_response, success.model_response);
        assert_eq!(preserved.session_config, success.session_config);
        assert_eq!(preserved.last_success_at_ms, Some(100));
        assert_eq!(preserved.last_attempt_at_ms, 200);
        assert_eq!(
            preserved.last_error_code.as_deref(),
            Some("agent_probe_failed")
        );

        let first_failure = snapshots
            .iter()
            .find(|snapshot| snapshot.provider_profile_id == failed_profile.id)
            .unwrap();
        assert!(first_failure.model_response.is_none());
        assert!(first_failure.session_config.is_none());
        assert_eq!(first_failure.last_success_at_ms, None);
        assert_eq!(first_failure.last_attempt_at_ms, 300);
        assert_eq!(
            first_failure.last_error_code.as_deref(),
            Some("agent_not_installed")
        );

        ProviderRuntimeOptionSnapshotRepository::delete(&conn, &failed_profile.id).unwrap();
        assert!(
            ProviderRuntimeOptionSnapshotRepository::list(&conn)
                .unwrap()
                .iter()
                .all(|snapshot| snapshot.provider_profile_id != failed_profile.id)
        );

        ProviderProfileRepository::soft_delete(&mut conn, &successful_profile.id).unwrap();
        assert!(
            ProviderRuntimeOptionSnapshotRepository::list(&conn)
                .unwrap()
                .iter()
                .all(|snapshot| snapshot.provider_profile_id != successful_profile.id)
        );

        cleanup_db(temp);
    }

    #[test]
    fn provider_model_runtime_option_snapshot_round_trips_by_model() {
        let temp = temp_db_path("provider-model-runtime-option-snapshot");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let agent_id = AgentId::parse("opencode").unwrap();
        let profile =
            ProviderProfileRepository::from_create_request(ProviderProfileCreateRequest {
                agent_id: Some(agent_id.clone()),
                kind: ProviderKind::Acp,
                display_name: "Model-scoped ACP profile".to_string(),
                account_alias: None,
                base_url: None,
                default_model: None,
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            });
        ProviderProfileRepository::insert(&conn, &profile).unwrap();
        let session_config = AgentSessionConfigProbe {
            models: vec!["gpt-5.6-sol".to_string()],
            modes: Vec::new(),
            reasoning_efforts: vec![AgentReasoningEffort {
                value: "on".to_string(),
                description: None,
            }],
            options: Vec::new(),
        };
        ProviderModelRuntimeOptionSnapshotRepository::upsert_success(
            &conn,
            &ProviderModelRuntimeOptionSnapshotRecord {
                provider_profile_id: profile.id.clone(),
                model_id: "gpt-5.6-sol".to_string(),
                agent_id: agent_id.clone(),
                session_config: Some(session_config.clone()),
                last_success_at_ms: Some(100),
                last_attempt_at_ms: 100,
                last_error_code: None,
            },
        )
        .unwrap();
        ProviderModelRuntimeOptionSnapshotRepository::record_failure(
            &conn,
            &profile.id,
            "gpt-5.6-sol",
            &agent_id,
            200,
            "model_probe_failed",
        )
        .unwrap();
        ProviderModelRuntimeOptionSnapshotRepository::record_failure(
            &conn,
            &profile.id,
            "glm-5.2",
            &agent_id,
            300,
            "model_unavailable",
        )
        .unwrap();

        let snapshots = ProviderModelRuntimeOptionSnapshotRepository::list(&conn).unwrap();
        let success = snapshots
            .iter()
            .find(|snapshot| snapshot.model_id == "gpt-5.6-sol")
            .unwrap();
        assert_eq!(success.session_config, Some(session_config));
        assert_eq!(success.last_success_at_ms, Some(100));
        assert_eq!(success.last_attempt_at_ms, 200);
        assert_eq!(
            success.last_error_code.as_deref(),
            Some("model_probe_failed")
        );
        let failure = snapshots
            .iter()
            .find(|snapshot| snapshot.model_id == "glm-5.2")
            .unwrap();
        assert!(failure.session_config.is_none());
        assert_eq!(failure.last_success_at_ms, None);
        assert_eq!(
            failure.last_error_code.as_deref(),
            Some("model_unavailable")
        );

        ProviderModelRuntimeOptionSnapshotRepository::delete_model(&conn, &profile.id, "glm-5.2")
            .unwrap();
        assert_eq!(
            ProviderModelRuntimeOptionSnapshotRepository::list(&conn)
                .unwrap()
                .len(),
            1
        );
        ProviderProfileRepository::soft_delete(&mut conn, &profile.id).unwrap();
        assert!(
            ProviderModelRuntimeOptionSnapshotRepository::list(&conn)
                .unwrap()
                .is_empty()
        );

        cleanup_db(temp);
    }

    #[test]
    fn agent_runtime_option_snapshot_round_trips_by_agent() {
        let temp = temp_db_path("agent-runtime-option-snapshot");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let agent_id = AgentId::parse("opencode").unwrap();
        let session_config = AgentSessionConfigProbe {
            models: Vec::new(),
            modes: vec![ProviderSessionConfigValue {
                value: "plan".to_string(),
                label: Some("Plan".to_string()),
            }],
            reasoning_efforts: vec![AgentReasoningEffort {
                value: "high".to_string(),
                description: None,
            }],
            options: Vec::new(),
        };
        AgentRuntimeOptionSnapshotRepository::upsert_success(
            &conn,
            &AgentRuntimeOptionSnapshotRecord {
                agent_id: agent_id.clone(),
                session_config: Some(session_config.clone()),
                last_success_at_ms: Some(100),
                last_attempt_at_ms: 100,
                last_error_code: None,
            },
        )
        .unwrap();
        AgentRuntimeOptionSnapshotRepository::record_failure(
            &conn,
            &agent_id,
            200,
            "agent_option_probe_failed",
        )
        .unwrap();

        let snapshots = AgentRuntimeOptionSnapshotRepository::list(&conn).unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].agent_id, agent_id);
        assert_eq!(snapshots[0].session_config, Some(session_config));
        assert_eq!(snapshots[0].last_success_at_ms, Some(100));
        assert_eq!(snapshots[0].last_attempt_at_ms, 200);
        assert_eq!(
            snapshots[0].last_error_code.as_deref(),
            Some("agent_option_probe_failed")
        );

        AgentRuntimeOptionSnapshotRepository::delete(&conn, &agent_id).unwrap();
        assert!(
            AgentRuntimeOptionSnapshotRepository::list(&conn)
                .unwrap()
                .is_empty()
        );
        cleanup_db(temp);
    }

    #[test]
    fn agent_auth_catalog_snapshot_round_trips_by_agent_and_profile() {
        let temp = temp_db_path("agent-auth-catalog-snapshot");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();
        let agent_id = AgentId::parse("opencode").unwrap();
        let profile_id = ProviderProfileId::new();
        let catalog = AgentAuthCatalog {
            agent_id: agent_id.clone(),
            methods: Vec::new(),
            supports_logout: true,
            status: vibex_core::AgentAuthStatus::Unknown,
            refreshed_at_ms: 100,
        };
        AgentAuthCatalogSnapshotRepository::upsert(
            &conn,
            &AgentAuthCatalogSnapshotRecord {
                agent_id: agent_id.clone(),
                provider_profile_id: Some(profile_id.clone()),
                catalog: catalog.clone(),
                refreshed_at_ms: 100,
            },
        )
        .unwrap();
        let cached = AgentAuthCatalogSnapshotRepository::get(&conn, &agent_id, Some(&profile_id))
            .unwrap()
            .unwrap();
        assert_eq!(cached.catalog, catalog);
        assert!(
            AgentAuthCatalogSnapshotRepository::get(&conn, &agent_id, None)
                .unwrap()
                .is_none()
        );
        AgentAuthCatalogSnapshotRepository::delete_agent(&conn, &agent_id).unwrap();
        assert!(
            AgentAuthCatalogSnapshotRepository::get(&conn, &agent_id, Some(&profile_id))
                .unwrap()
                .is_none()
        );
        cleanup_db(temp);
    }

    #[test]
    fn managed_agent_installation_round_trips_and_deletes() {
        let temp = temp_db_path("managed-agent-installation");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();
        let agent_id = AgentId::parse("gemini").unwrap();
        let state = AgentManagedInstallState {
            managed: true,
            status: vibex_core::AgentManagedInstallStatus::Installed,
            distribution_kind: Some(vibex_core::AgentManagedDistributionKind::Npm),
            installed_version: Some("0.54.0".to_string()),
            available_version: Some("0.54.0".to_string()),
            last_error_code: None,
            last_error_message: None,
            updated_at_ms: Some(100),
        };
        let record = AgentManagedInstallationRecord {
            agent_id: agent_id.clone(),
            registry_agent_id: "gemini".to_string(),
            state: state.clone(),
            command: Some(AgentCommandConfig {
                command: "/managed/node".to_string(),
                args: vec!["/managed/gemini.js".to_string(), "--acp".to_string()],
            }),
            install_root: Some("/managed/gemini/0.54.0".to_string()),
            updated_at_ms: 100,
        };
        AgentManagedInstallationRepository::upsert(&conn, &record).unwrap();
        assert_eq!(
            AgentManagedInstallationRepository::get(&conn, &agent_id)
                .unwrap()
                .unwrap(),
            record
        );
        assert_eq!(
            AgentManagedInstallationRepository::list(&conn)
                .unwrap()
                .len(),
            1
        );
        AgentManagedInstallationRepository::delete(&conn, &agent_id).unwrap();
        assert!(
            AgentManagedInstallationRepository::get(&conn, &agent_id)
                .unwrap()
                .is_none()
        );
        cleanup_db(temp);
    }

    #[test]
    fn provider_health_and_usage_records_round_trip() {
        let temp = temp_db_path("provider-health-usage");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();
        ProviderProfileRepository::ensure_local_defaults(&conn).unwrap();

        let profile_id =
            ProviderProfileId::parse(ProviderKind::Codex.local_default_profile_id().to_string())
                .unwrap();
        let now = unix_timestamp_ms();
        let health = ProviderHealthProbeResult {
            health_record_id: RequestId::new(),
            provider_profile_id: profile_id.clone(),
            provider_kind: ProviderKind::Codex,
            probe_kind: ProviderHealthProbeKind::AuthStatus,
            status: ProviderHealthStatus::Pass,
            summary: "Codex auth fixture is deterministic".to_string(),
            latency_ms: Some(1),
            checked_at_ms: now,
            expires_at_ms: Some(now + 60_000),
            diagnostics: vec![ProviderBindingMetadata {
                key: "mode".to_string(),
                value: "codex".to_string(),
            }],
        };
        ProviderHealthRepository::insert(&conn, &health).unwrap();
        let health_rows = ProviderHealthRepository::list_latest(&conn).unwrap();
        assert!(health_rows.iter().any(|row| {
            row.provider_profile_id == profile_id
                && row.probe_kind == ProviderHealthProbeKind::AuthStatus
                && row.status == ProviderHealthStatus::Pass
        }));

        let mut capabilities =
            ProviderCapabilities::conservative(ProviderKind::Codex, "test-capability");
        capabilities.tool_invocations = true;
        let capability = ProviderCapabilityProbeResult {
            capability_record_id: RequestId::new(),
            provider_profile_id: profile_id.clone(),
            provider_kind: ProviderKind::Codex,
            status: ProviderCapabilityProbeStatus::Pass,
            summary: "Codex capability projection is deterministic".to_string(),
            capabilities,
            source: "test".to_string(),
            checked_at_ms: now,
            expires_at_ms: Some(now + 60_000),
            diagnostics: vec![ProviderBindingMetadata {
                key: "redacted".to_string(),
                value: "true".to_string(),
            }],
        };
        ProviderCapabilityRepository::insert(&conn, &capability).unwrap();
        let capability_rows = ProviderCapabilityRepository::list_latest(&conn).unwrap();
        let stored_capability = capability_rows
            .iter()
            .find(|row| row.provider_profile_id == profile_id)
            .unwrap();
        assert_eq!(
            stored_capability.status,
            ProviderCapabilityProbeStatus::Pass
        );
        assert!(stored_capability.capabilities.tool_invocations);

        let usage = ProviderUsageRecord {
            usage_record_id: RequestId::new(),
            provider_profile_id: profile_id.clone(),
            provider_kind: ProviderKind::Codex,
            source: "test".to_string(),
            unit: ProviderUsageUnit::Percent,
            label: "Codex quota".to_string(),
            used: Some(40.0),
            limit_value: Some(100.0),
            remaining: Some(60.0),
            window: Some(ProviderUsageWindow {
                label: "monthly".to_string(),
                started_at_ms: Some(now - 1_000),
                ends_at_ms: Some(now + 1_000),
            }),
            recorded_at_ms: now,
            metadata: vec![ProviderBindingMetadata {
                key: "redacted".to_string(),
                value: "true".to_string(),
            }],
        };
        ProviderUsageRepository::insert(&conn, &usage).unwrap();
        let usage_rows = ProviderUsageRepository::list_latest(&conn).unwrap();
        let stored = usage_rows
            .iter()
            .find(|row| row.provider_profile_id == profile_id && row.label == "Codex quota")
            .unwrap();
        assert_eq!(stored.unit, ProviderUsageUnit::Percent);
        assert_eq!(
            stored.window.as_ref().map(|window| window.label.as_str()),
            Some("monthly")
        );

        cleanup_db(temp);
    }

    #[test]
    fn session_timeline_and_permission_round_trip() {
        let temp = temp_db_path("agent");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let (_project, workspace) = WorkspaceRepository::ensure(
            &conn,
            "/tmp/vibex-db-test",
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let now = unix_timestamp_ms();
        let profile_id =
            ProviderProfileId::parse(ProviderKind::Codex.local_default_profile_id().to_string())
                .unwrap();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: "Codex session".to_string(),
            project_id: workspace.project_id.clone(),
            workspace_id: workspace.id.clone(),
            workspace_root: workspace.root_path.clone(),
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("codex").unwrap(),
            state: AgentSessionState::Initializing,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now,
            updated_at_ms: now,
            last_message_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&conn, &session).unwrap();
        SessionRepository::update_state(&conn, &session.id, AgentSessionState::Idle).unwrap();

        let first = TimelineRepository::append(
            &mut conn,
            &session.id,
            TimelineSource::User,
            TimelinePayload::UserMessage(UserMessagePayload {
                text: "hello".to_string(),
                attachments: Vec::new(),
            }),
            None,
            None,
            TimelineRedactionState::None,
        )
        .unwrap();
        let attribution = TurnExecutionAttribution::new(
            AgentId::parse("codex").unwrap(),
            vibex_core::RuntimeAuthSource::provider_profile(profile_id.clone()),
            vibex_core::RuntimeModelSelection::explicit("gpt-5"),
            Some("gpt-5".to_string()),
            RuntimeBindingId::parse("binding_current").unwrap(),
            3,
            "Codex",
            "OpenAI work",
            "GPT-5",
        )
        .unwrap();
        let second = TimelineRepository::append_with_attribution(
            &mut conn,
            &session.id,
            TimelineSource::Agent,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "hi".to_string(),
                is_final: true,
            }),
            None,
            None,
            TimelineRedactionState::None,
            Some(&attribution),
        )
        .unwrap();
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(first.execution_attribution, None);
        assert_eq!(second.execution_attribution, Some(attribution.view()));

        let page = TimelineRepository::fetch_after(&conn, &session.id, Some(1), 10).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].sequence, 2);
        assert_eq!(
            page.items[0].execution_attribution,
            Some(attribution.view())
        );

        let coalesced = TimelineRepository::upsert_by_provider_correlation(
            &mut conn,
            &session.id,
            TimelineSource::Provider,
            TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                text_delta: "streaming".to_string(),
                chunk_index: 1,
                phase: None,
            }),
            "provider-turn-1",
            2,
            TimelineRedactionState::None,
            Some(&attribution),
        )
        .unwrap();
        assert_eq!(coalesced.execution_attribution, Some(attribution.view()));
        let stale_attribution = TurnExecutionAttribution::new(
            AgentId::parse("codex").unwrap(),
            vibex_core::RuntimeAuthSource::provider_profile(profile_id),
            vibex_core::RuntimeModelSelection::explicit("gpt-5"),
            Some("gpt-5".to_string()),
            RuntimeBindingId::parse("binding_stale").unwrap(),
            2,
            "Codex",
            "OpenAI work",
            "GPT-5",
        )
        .unwrap();
        let error = TimelineRepository::upsert_by_provider_correlation(
            &mut conn,
            &session.id,
            TimelineSource::Provider,
            TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                text_delta: "stale".to_string(),
                chunk_index: 2,
                phase: None,
            }),
            "provider-turn-1",
            2,
            TimelineRedactionState::None,
            Some(&stale_attribution),
        )
        .unwrap_err();
        assert_eq!(error.code, "turn_execution_attribution_conflict");

        let request = PermissionRequest {
            id: vibex_core::RequestId::new(),
            session_id: session.id.clone(),
            project_id: Some(session.project_id.clone()),
            workspace_id: Some(session.workspace_id.clone()),
            provider_request_id: Some("native-permission".to_string()),
            risk_category: PermissionRiskCategory::Command,
            title: "Run command".to_string(),
            details: vec![PermissionActionDetail {
                label: "command".to_string(),
                value: "echo ok".to_string(),
            }],
            allowed_responses: vec![
                PermissionResponseKind::Approve,
                PermissionResponseKind::Deny,
            ],
            response_options: vec![PermissionResponseOption {
                option_id: "allow-command-prefix".to_string(),
                label: "Allow commands starting with echo".to_string(),
                response: PermissionResponseKind::AlwaysAllowForSession,
            }],
            status: PermissionRequestStatus::Pending,
            requested_at_ms: unix_timestamp_ms(),
            expires_at_ms: None,
        };
        PermissionRepository::insert_request(&conn, &request).unwrap();
        let pending = PermissionRepository::pending_for_session(&conn, &session.id).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].response_options, request.response_options);
        PermissionRepository::resolve(
            &conn,
            &PermissionResolution {
                request_id: request.id.clone(),
                session_id: session.id.clone(),
                response: PermissionResponseKind::Approve,
                responder_device_id: None,
                provider_resolution_id: None,
                note: None,
                resolved_at_ms: unix_timestamp_ms(),
            },
        )
        .unwrap();
        assert!(
            PermissionRepository::pending_for_session(&conn, &session.id)
                .unwrap()
                .is_empty()
        );

        let elicitation = ElicitationRequest {
            id: RequestId::new(),
            session_id: session.id.clone(),
            provider_request_id: Some("native-elicitation".to_string()),
            tool_call_id: Some("tool-call-1".to_string()),
            message: "Choose a target".to_string(),
            title: Some("Target".to_string()),
            description: None,
            fields: Vec::new(),
            status: ElicitationRequestStatus::Pending,
            requested_at_ms: unix_timestamp_ms(),
        };
        ElicitationRepository::insert_request(&conn, &elicitation).unwrap();
        assert_eq!(
            ElicitationRepository::get_request(&conn, &elicitation.id).unwrap(),
            Some(elicitation.clone())
        );
        assert_eq!(
            ElicitationRepository::pending_for_session(&conn, &session.id).unwrap(),
            vec![elicitation.clone()]
        );
        let elicitation_resolution = ElicitationResolution {
            request_id: elicitation.id.clone(),
            session_id: session.id.clone(),
            action: ElicitationResolutionAction::Decline,
            answers: BTreeMap::new(),
            responder_device_id: None,
            resolved_at_ms: unix_timestamp_ms(),
        };
        ElicitationRepository::resolve(&conn, &elicitation_resolution).unwrap();
        assert_eq!(
            ElicitationRepository::get_request(&conn, &elicitation.id)
                .unwrap()
                .unwrap()
                .status,
            ElicitationRequestStatus::Declined
        );
        assert!(
            ElicitationRepository::pending_for_session(&conn, &session.id)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            ElicitationRepository::resolve(&conn, &elicitation_resolution)
                .unwrap_err()
                .code,
            "elicitation_request_not_pending"
        );

        AdapterDiagnosticsRepository::insert(
            &conn,
            &AdapterDiagnostic {
                session_id: Some(session.id.clone()),
                provider_kind: ProviderKind::Codex,
                level: AdapterDiagnosticLevel::Info,
                code: "test".to_string(),
                message: "diagnostic".to_string(),
                redacted_details: Vec::new(),
                timestamp_ms: unix_timestamp_ms(),
            },
        )
        .unwrap();

        let loaded = SessionRepository::get(&conn, &session.id).unwrap().unwrap();
        assert_eq!(loaded.state, AgentSessionState::Idle);
        assert_eq!(loaded.agent_id, AgentId::parse("codex").unwrap());

        let notice = TimelineRepository::append(
            &mut conn,
            &session.id,
            TimelineSource::System,
            TimelinePayload::SystemNotice(SystemNoticePayload {
                level: SystemNoticeLevel::Info,
                message: "done".to_string(),
            }),
            None,
            None,
            TimelineRedactionState::None,
        )
        .unwrap();
        assert_eq!(notice.sequence, 4);

        cleanup_db(temp);
    }

    #[test]
    fn timeline_batch_preserves_source_timestamp_and_archive_uses_end_sequence_cas() {
        let temp = temp_db_path("timeline-fork-copy");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();
        let (_project, workspace) = WorkspaceRepository::ensure(
            &conn,
            "/tmp/vibex-db-fork-copy-test",
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: "Fork copy".to_string(),
            project_id: workspace.project_id.clone(),
            workspace_id: workspace.id.clone(),
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("codex").unwrap(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 200,
            updated_at_ms: 200,
            last_message_at_ms: 200,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        let appended = TimelineRepository::insert_session_and_append_many(
            &mut conn,
            &session,
            &[TimelineAppend {
                source: TimelineSource::User,
                payload: TimelinePayload::UserMessage(UserMessagePayload {
                    text: "preserve me".to_string(),
                    attachments: Vec::new(),
                }),
                timestamp_ms: Some(123),
                correlation_id: None,
                provider_correlation_id: None,
                redaction_state: TimelineRedactionState::None,
                execution_attribution: None,
            }],
        )
        .unwrap();
        assert_eq!(appended[0].timestamp_ms, 123);
        assert_eq!(
            TimelineRepository::fetch_after(&conn, &session.id, Some(0), 10)
                .unwrap()
                .items[0]
                .timestamp_ms,
            123
        );

        let changed =
            SessionRepository::archive_if_timeline_unchanged(&conn, &session.id, 0).unwrap_err();
        assert_eq!(changed.code, "session_archive_source_changed");
        assert_eq!(
            SessionRepository::get(&conn, &session.id)
                .unwrap()
                .unwrap()
                .state,
            AgentSessionState::Idle
        );
        SessionRepository::archive_if_timeline_unchanged(&conn, &session.id, 1).unwrap();
        assert_eq!(
            SessionRepository::get(&conn, &session.id)
                .unwrap()
                .unwrap()
                .state,
            AgentSessionState::Archived
        );

        cleanup_db(temp);
    }

    #[test]
    fn session_last_message_time_ignores_state_updates() {
        let temp = temp_db_path("session-last-message-time");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();
        let (_project, workspace) = WorkspaceRepository::ensure(
            &conn,
            "/tmp/vibex-db-session-last-message-time-test",
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: "Last message time".to_string(),
            project_id: workspace.project_id.clone(),
            workspace_id: workspace.id.clone(),
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("codex").unwrap(),
            state: AgentSessionState::Initializing,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 100,
            updated_at_ms: 100,
            last_message_at_ms: 100,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        TimelineRepository::insert_session_and_append_many(
            &mut conn,
            &session,
            &[TimelineAppend {
                source: TimelineSource::User,
                payload: TimelinePayload::UserMessage(UserMessagePayload {
                    text: "hello".to_string(),
                    attachments: Vec::new(),
                }),
                timestamp_ms: Some(200),
                correlation_id: None,
                provider_correlation_id: None,
                redaction_state: TimelineRedactionState::None,
                execution_attribution: None,
            }],
        )
        .unwrap();
        SessionRepository::update_state(&conn, &session.id, AgentSessionState::Idle).unwrap();

        let loaded = SessionRepository::get(&conn, &session.id).unwrap().unwrap();
        assert_eq!(loaded.last_message_at_ms, 200);
        assert!(loaded.updated_at_ms > loaded.last_message_at_ms);

        cleanup_db(temp);
    }

    #[test]
    fn timeline_forward_pagination_preserves_complete_long_turn() {
        let temp = temp_db_path("timeline-long-turn");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let (_project, workspace) = WorkspaceRepository::ensure(
            &conn,
            "/tmp/vibex-db-long-turn-test",
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let now = unix_timestamp_ms();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: "Long turn".to_string(),
            project_id: workspace.project_id.clone(),
            workspace_id: workspace.id.clone(),
            workspace_root: workspace.root_path.clone(),
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("codex").unwrap(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now,
            updated_at_ms: now,
            last_message_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&conn, &session).unwrap();

        TimelineRepository::append(
            &mut conn,
            &session.id,
            TimelineSource::User,
            TimelinePayload::UserMessage(UserMessagePayload {
                text: "please do a complex task".to_string(),
                attachments: Vec::new(),
            }),
            None,
            None,
            TimelineRedactionState::None,
        )
        .unwrap();
        for index in 0..600 {
            TimelineRepository::append(
                &mut conn,
                &session.id,
                TimelineSource::Agent,
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: format!("chunk-{index};"),
                    chunk_index: index,
                    phase: None,
                }),
                None,
                None,
                TimelineRedactionState::None,
            )
            .unwrap();
        }
        TimelineRepository::append(
            &mut conn,
            &session.id,
            TimelineSource::Agent,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "complete final answer".to_string(),
                is_final: true,
            }),
            None,
            None,
            TimelineRedactionState::None,
        )
        .unwrap();

        let latest_window = TimelineRepository::fetch_after(&conn, &session.id, None, 150).unwrap();
        assert!(latest_window.has_older);
        assert_ne!(latest_window.start_sequence, Some(1));

        let mut cursor = 0;
        let mut all_items = Vec::new();
        loop {
            let page =
                TimelineRepository::fetch_after(&conn, &session.id, Some(cursor), 500).unwrap();
            if let Some(end_sequence) = page.end_sequence {
                cursor = end_sequence;
            }
            all_items.extend(page.items);
            if !page.has_newer {
                break;
            }
        }

        assert_eq!(all_items.len(), 602);
        assert!(matches!(
            &all_items[0].payload,
            TimelinePayload::UserMessage(message)
                if message.text == "please do a complex task"
        ));
        assert!(matches!(
            &all_items[601].payload,
            TimelinePayload::AgentMessage(message)
                if message.text == "complete final answer" && message.is_final
        ));

        cleanup_db(temp);
    }

    #[test]
    fn remote_device_pairing_and_audit_round_trip_without_plaintext_secret() {
        let temp = temp_db_path("remote");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let now = unix_timestamp_ms();
        let raw_pairing_code = "PAIR-123456";
        let pairing_hash = "hash:pairing-code";
        let auth_hash = "hash:auth-token";
        let pairing = RemotePairingCodeRecord {
            pairing: RemotePairingCode {
                pairing_id: RequestId::new(),
                permission_level: RemoteDevicePermissionLevel::ApproveOnly,
                expires_at_ms: now + 60_000,
                claimed_device_id: None,
                created_at_ms: now,
                claimed_at_ms: None,
            },
            code_hash: pairing_hash.to_string(),
        };
        RemotePairingCodeRepository::insert(&conn, &pairing).unwrap();

        let raw_db = fs::read_to_string(&temp).unwrap_or_default();
        assert!(!raw_db.contains(raw_pairing_code));
        assert_eq!(
            RemotePairingCodeRepository::get_by_hash(&conn, pairing_hash)
                .unwrap()
                .unwrap()
                .pairing
                .permission_level,
            RemoteDevicePermissionLevel::ApproveOnly
        );

        let device_id = DeviceId::new();
        let device = RemoteDeviceRecord {
            detail: RemoteDeviceDetail {
                device_id: device_id.clone(),
                display_name: "iPhone".to_string(),
                public_key: Some("pubkey-redacted".to_string()),
                grant_revision: 1,
                permission_level: RemoteDevicePermissionLevel::ApproveOnly,
                status: RemoteDeviceStatus::Active,
                paired_at_ms: Some(now),
                last_seen_at_ms: Some(now),
                revoked_at_ms: None,
                created_at_ms: now,
                updated_at_ms: now,
            },
            auth_secret_hash: auth_hash.to_string(),
        };
        RemoteDeviceRepository::upsert(&conn, &device).unwrap();
        RemotePairingCodeRepository::mark_claimed(
            &conn,
            &pairing.pairing.pairing_id,
            &device_id,
            now + 1,
        )
        .unwrap();

        let stored_device = RemoteDeviceRepository::get(&conn, &device_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored_device.detail.display_name, "iPhone");
        assert_eq!(stored_device.auth_secret_hash, auth_hash);
        let claimed = RemotePairingCodeRepository::get_by_hash(&conn, pairing_hash)
            .unwrap()
            .unwrap();
        assert_eq!(claimed.pairing.claimed_device_id, Some(device_id.clone()));

        RemoteDeviceRepository::update_last_seen(&conn, &device_id, now + 2).unwrap();
        let seen = RemoteDeviceRepository::get(&conn, &device_id)
            .unwrap()
            .unwrap();
        assert_eq!(seen.detail.last_seen_at_ms, Some(now + 2));

        let revoked = RemoteDeviceRepository::revoke(&conn, &device_id, now + 3).unwrap();
        assert_eq!(revoked.detail.status, RemoteDeviceStatus::Revoked);

        RemoteAuditRepository::insert(
            &conn,
            &RemoteAuditRecord {
                audit_id: RequestId::new(),
                device_id: Some(device_id.clone()),
                action: RemoteAuditAction::DeviceRevoked,
                target_kind: RemoteAuditTargetKind::Device,
                target_id: Some(device_id.to_string()),
                outcome: RemoteAuditOutcome::Revoked,
                redacted_summary: "Device revoked by local owner".to_string(),
                request_id: Some(RequestId::new()),
                correlation_id: None,
                created_at_ms: now + 4,
            },
        )
        .unwrap();
        let audits = RemoteAuditRepository::list(
            &conn,
            &RemoteAuditListRequest {
                device_id: Some(device_id),
                limit: Some(10),
            },
        )
        .unwrap();
        assert_eq!(audits.len(), 1);
        assert!(!audits[0].redacted_summary.contains(raw_pairing_code));

        cleanup_db(temp);
    }

    #[test]
    fn workbench_metadata_round_trips() {
        let temp = temp_db_path("workbench");
        let mut conn = open_database(&temp).unwrap();
        apply_migrations(&mut conn).unwrap();

        let (_project, workspace) = WorkspaceRepository::ensure(
            &conn,
            "/tmp/vibex-workbench-test",
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let now = unix_timestamp_ms();
        let terminal = TerminalSession {
            id: TerminalId::new(),
            workspace_id: workspace.id.clone(),
            title: "shell".to_string(),
            shell: "/bin/sh".to_string(),
            cwd: workspace.root_path.clone(),
            rows: 24,
            cols: 80,
            status: TerminalStatus::Running,
            created_at_ms: now,
            updated_at_ms: now,
            closed_at_ms: None,
        };
        TerminalSessionRepository::upsert(&conn, &terminal).unwrap();
        RecentFileRepository::touch(&conn, &workspace.id, "src/main.rs").unwrap();
        GitSnapshotRepository::upsert(
            &conn,
            &workspace.id,
            Some("main"),
            Some("abc1234"),
            true,
            2,
            now,
        )
        .unwrap();

        let terminals = TerminalSessionRepository::list(&conn, &workspace.id).unwrap();
        assert_eq!(terminals, vec![terminal]);
        let recent = RecentFileRepository::list(&conn, &workspace.id, 10).unwrap();
        assert_eq!(recent, vec!["src/main.rs".to_string()]);

        cleanup_db(temp);
    }

    fn temp_db_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vibex-db-{label}-{}.db",
            vibex_core::RequestId::new().as_str()
        ))
    }

    fn cleanup_db(path: PathBuf) {
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("db-wal"));
        let _ = fs::remove_file(path.with_extension("db-shm"));
    }
}

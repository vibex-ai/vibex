use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use vibex_agent::{RuntimeMetricSnapshot, RuntimeObservability};
use vibex_config_switch::ProviderConfigService;
use vibex_core::{
    AgentSession, AgentSessionSafety, AgentSessionState, DIAGNOSTIC_BUNDLE_SCHEMA_VERSION,
    DiagnosticBundle, DiagnosticBundleMetadata, DiagnosticBundleRedactionPolicy,
    DiagnosticBundleRequest, DiagnosticCount, DiagnosticDatabasePathKind, DiagnosticErrorSection,
    DiagnosticExcludedContent, DiagnosticProviderCapabilitySummary, DiagnosticProviderHealthProbe,
    DiagnosticProviderHealthSummary, DiagnosticProviderProfileRef, DiagnosticProviderSection,
    DiagnosticProviderUsageSummary, DiagnosticReleaseContext, DiagnosticRuntimeMetric,
    DiagnosticRuntimeSection, DiagnosticScheduledTaskAttentionRecord,
    DiagnosticScheduledTaskAuditRecord, DiagnosticScheduledTaskSection, DiagnosticSmokeCommandKind,
    DiagnosticSmokeCommandReference, DiagnosticSmokeSection, DiagnosticStorageSection,
    DiagnosticWorkbenchSection, FileSearchRequest, FileTreeRequest, FoundationStatusPayload,
    ProviderCapabilitySummary, ProviderHealthProbeResult, ProviderHealthSummary, ProviderKind,
    ProviderProfileSummary, ProviderUsageListRequest, ProviderUsageSummary, RedactedDiagnostic,
    ScheduledTaskAttentionListRequest, ScheduledTaskAttentionSummary,
    ScheduledTaskAuditListRequest, ScheduledTaskAuditRecord, ScheduledTaskCreateRequest,
    ScheduledTaskOneShotSchedule, ScheduledTaskRunCreateRequest, ScheduledTaskRunStatus,
    ScheduledTaskRunTrigger, ScheduledTaskSchedule, SystemNoticeLevel, SystemNoticePayload,
    TerminalCreateRequest, TerminalWriteRequest, TimelinePayload, TimelineRedactionState,
    TimelineSource, VibexError, VibexResult, VibexSessionId, WorkspaceMode, unix_timestamp_ms,
};
use vibex_db::{
    CURRENT_SCHEMA_VERSION, DbConnection, RecentFileRepository, ScheduledTaskRepository,
    SessionRepository, TimelineRepository, WorkspaceRepository, apply_migrations,
    current_schema_version, default_database_path, open_database,
};
use vibex_fs::WorkspaceFileService;
use vibex_terminal::TerminalManager;

mod e2e;
pub use e2e::{
    E2eRegressionCheck, E2eRegressionCheckStatus, E2eRegressionClassification,
    E2eRegressionHarnessResult, E2eRegressionOverallStatus, assert_e2e_regression_output_redacted,
    run_e2e_regression_harness,
};

pub const DEFAULT_DIAGNOSTIC_RECORD_LIMIT: u32 = 25;
pub const MAX_DIAGNOSTIC_RECORD_LIMIT: u32 = 100;

const DIAGNOSTIC_REDACTION_POLICY_VERSION: &str = "diagnostic_redaction.v1";

#[derive(Debug, Clone)]
pub struct DiagnosticBundleServiceConfig {
    pub db_path: PathBuf,
    pub app_version: String,
    pub core_contract_version: String,
    pub runtime_observability: Option<Arc<RuntimeObservability>>,
}

#[derive(Debug, Clone)]
pub struct DiagnosticBundleService {
    config: DiagnosticBundleServiceConfig,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticBundleSmokeResult {
    pub status: String,
    pub db_path: String,
    pub output_path: String,
    pub schema_version: String,
    pub provider_health_count: u32,
    pub scheduled_audit_count: u32,
    pub redaction_verified: bool,
}

impl DiagnosticBundleServiceConfig {
    pub fn new(db_path: PathBuf) -> Self {
        let foundation = FoundationStatusPayload::stage0();
        Self {
            db_path,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            core_contract_version: foundation.core_contract_version,
            runtime_observability: None,
        }
    }

    pub fn with_runtime_observability(
        mut self,
        runtime_observability: Arc<RuntimeObservability>,
    ) -> Self {
        self.runtime_observability = Some(runtime_observability);
        self
    }
}

impl DiagnosticBundleService {
    pub fn new(config: DiagnosticBundleServiceConfig) -> Self {
        Self { config }
    }

    pub fn export_bundle(&self, request: DiagnosticBundleRequest) -> VibexResult<DiagnosticBundle> {
        let limit = bounded_limit(request.record_limit);
        let include_smoke_references = request.include_smoke_references.unwrap_or(true);
        let mut conn = open_database(&self.config.db_path)?;
        apply_migrations(&mut conn)?;

        let provider_service = ProviderConfigService::new(self.config.db_path.clone());
        let health_summaries = provider_service.list_health_summaries()?;
        let capability_summaries = provider_service.list_capability_summaries()?;
        let usage_summaries = provider_service.list_usage_summaries(ProviderUsageListRequest {
            provider_profile_ids: None,
            include_empty: true,
        })?;

        let attention = ScheduledTaskRepository::list_attention(
            &conn,
            ScheduledTaskAttentionListRequest {
                workspace_id: None,
                limit: Some(limit),
            },
        )?;
        let audit = ScheduledTaskRepository::list_audit(
            &conn,
            ScheduledTaskAuditListRequest {
                workspace_id: None,
                status: None,
                limit: Some(limit),
            },
        )?;

        Ok(DiagnosticBundle {
            metadata: self.metadata(),
            redaction: redaction_policy(limit),
            storage: self.storage_section(&conn)?,
            providers: DiagnosticProviderSection {
                record_limit: limit,
                health_summary_count: saturating_len(&health_summaries),
                capability_summary_count: saturating_len(&capability_summaries),
                usage_summary_count: saturating_len(&usage_summaries),
                health_summaries: health_summaries
                    .iter()
                    .take(limit as usize)
                    .map(project_provider_health_summary)
                    .collect(),
                capability_summaries: capability_summaries
                    .iter()
                    .take(limit as usize)
                    .map(project_provider_capability_summary)
                    .collect(),
                usage_summaries: usage_summaries
                    .iter()
                    .take(limit as usize)
                    .map(project_provider_usage_summary)
                    .collect(),
            },
            scheduled_tasks: DiagnosticScheduledTaskSection {
                record_limit: limit,
                attention_count: saturating_len(&attention),
                audit_count: saturating_len(&audit),
                attention: attention
                    .iter()
                    .take(limit as usize)
                    .map(project_attention_record)
                    .collect(),
                audit: audit
                    .iter()
                    .take(limit as usize)
                    .map(project_audit_record)
                    .collect(),
            },
            workbench: self.workbench_section(&conn)?,
            runtime: project_runtime_metrics(
                self.config
                    .runtime_observability
                    .as_ref()
                    .map(|observability| observability.snapshot()),
                limit,
            ),
            smokes: DiagnosticSmokeSection {
                references: if include_smoke_references {
                    smoke_references()
                } else {
                    Vec::new()
                },
            },
            errors: DiagnosticErrorSection {
                scheduled_task_error_codes: scheduled_error_codes(&audit, limit),
            },
        })
    }

    fn metadata(&self) -> DiagnosticBundleMetadata {
        DiagnosticBundleMetadata {
            schema_version: DIAGNOSTIC_BUNDLE_SCHEMA_VERSION.to_string(),
            generated_at_ms: unix_timestamp_ms(),
            app_version: self.config.app_version.clone(),
            core_contract_version: self.config.core_contract_version.clone(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            target_family: target_family().to_string(),
            debug_assertions: cfg!(debug_assertions),
            release_context: Some(DiagnosticReleaseContext {
                gpui_revision: safe_release_text(option_env!("VIBEX_GPUI_REVISION")),
                gpui_component_revision: safe_release_text(option_env!(
                    "VIBEX_GPUI_COMPONENT_REVISION"
                )),
                renderer: safe_release_text(option_env!("VIBEX_RENDERER")),
                window_size: safe_release_text(option_env!("VIBEX_WINDOW_SIZE")),
                dpi_scale: safe_release_text(option_env!("VIBEX_DPI_SCALE")),
                web_backend: safe_release_text(option_env!("VIBEX_WEB_BACKEND"))
                    .or_else(|| Some("external_browser_only".to_string())),
                pdf_backend: safe_release_text(option_env!("VIBEX_PDF_BACKEND"))
                    .or_else(|| Some("pdfium_7881_supervised".to_string())),
                terminal_backend: safe_release_text(option_env!("VIBEX_TERMINAL_BACKEND"))
                    .or_else(|| Some("pty_alacritty_terminal".to_string())),
                ui_state_schema: Some("desktop-ui-state.v1".to_string()),
                last_clean_shutdown: option_env!("VIBEX_LAST_CLEAN_SHUTDOWN")
                    .and_then(parse_release_bool),
                cache_budgets: vec![
                    DiagnosticCount {
                        name: "pdf_page_cache_bytes".to_string(),
                        count: 24 * 1024 * 1024,
                    },
                    DiagnosticCount {
                        name: "terminal_raw_ring_bytes".to_string(),
                        count: 10 * 1024 * 1024,
                    },
                ],
                crash_metadata: vec![DiagnosticCount {
                    name: "bounded_crash_records".to_string(),
                    count: 0,
                }],
            }),
        }
    }

    // Release context is an allowlisted projection.  Build-time environment
    // values are treated as untrusted input and never copied verbatim into a
    // diagnostic bundle.
}

fn safe_release_text(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    let lowercase = value.to_ascii_lowercase();
    if value.is_empty()
        || value.len() > 128
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || [
            "secret", "token", "password", "private", "auth", "prompt", "header",
        ]
        .iter()
        .any(|marker| lowercase.contains(marker))
    {
        return None;
    }
    Some(value.to_string())
}

fn parse_release_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

impl DiagnosticBundleService {
    fn storage_section(&self, conn: &DbConnection) -> VibexResult<DiagnosticStorageSection> {
        let (database_path_kind, database_path_hint) = classify_database_path(&self.config.db_path);
        let current_schema_version = current_schema_version(conn)?;
        Ok(DiagnosticStorageSection {
            database_path_kind,
            database_path_hint,
            current_schema_version,
            expected_schema_version: CURRENT_SCHEMA_VERSION,
            applied_migration_count: current_schema_version.max(0) as u32,
            counts: vec![
                table_count(conn, "projects")?,
                table_count(conn, "workspaces")?,
                table_count(conn, "agent_sessions")?,
                table_count(conn, "provider_profiles")?,
                table_count(conn, "provider_health_probe_records")?,
                table_count(conn, "provider_capability_probe_records")?,
                table_count(conn, "provider_usage_records")?,
                table_count(conn, "scheduled_tasks")?,
                table_count(conn, "scheduled_task_runs")?,
                table_count(conn, "remote_audit_logs")?,
            ],
        })
    }

    fn workbench_section(&self, conn: &DbConnection) -> VibexResult<DiagnosticWorkbenchSection> {
        Ok(DiagnosticWorkbenchSection {
            counts: vec![
                table_count(conn, "workspaces")?,
                table_count(conn, "agent_sessions")?,
                table_count(conn, "terminal_sessions")?,
                table_count(conn, "git_snapshots")?,
                table_count(conn, "git_managed_worktrees")?,
            ],
        })
    }
}

pub fn run_diagnostic_bundle_smoke(
    db_path: &Path,
    output_path: &Path,
) -> VibexResult<DiagnosticBundleSmokeResult> {
    remove_database_family(db_path)?;
    seed_diagnostic_smoke_fixture(db_path)?;

    let service =
        DiagnosticBundleService::new(DiagnosticBundleServiceConfig::new(db_path.to_path_buf()));
    let bundle = service.export_bundle(DiagnosticBundleRequest {
        record_limit: Some(10),
        include_smoke_references: Some(true),
    })?;
    let json = serde_json::to_string_pretty(&bundle).map_err(|err| {
        VibexError::storage(
            "diagnostic_bundle_serialize_failed",
            "failed to serialize diagnostic bundle smoke output",
        )
        .with_diagnostic("error", err.to_string())
    })?;
    assert_no_sensitive_sentinels(&json)?;

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            VibexError::storage(
                "diagnostic_bundle_smoke_output_dir_failed",
                "failed to create diagnostic smoke output directory",
            )
            .with_diagnostic("error", err.to_string())
        })?;
    }
    std::fs::write(output_path, json).map_err(|err| {
        VibexError::storage(
            "diagnostic_bundle_smoke_output_write_failed",
            "failed to write diagnostic smoke output",
        )
        .with_diagnostic("error", err.to_string())
    })?;

    Ok(DiagnosticBundleSmokeResult {
        status: "ok".to_string(),
        db_path: "target/stage0/vibex-diagnostic-bundle-smoke.db".to_string(),
        output_path: output_path.display().to_string(),
        schema_version: bundle.metadata.schema_version,
        provider_health_count: bundle.providers.health_summary_count,
        scheduled_audit_count: bundle.scheduled_tasks.audit_count,
        redaction_verified: true,
    })
}

pub fn seed_diagnostic_smoke_fixture(db_path: &Path) -> VibexResult<()> {
    let mut conn = open_database(db_path)?;
    apply_migrations(&mut conn)?;
    let (_project, workspace) = WorkspaceRepository::ensure(
        &conn,
        "/tmp/vibex-diagnostic-smoke-DIAG_FILE_PATH_SENTINEL",
        WorkspaceMode::CurrentCheckout,
    )?;
    let task = ScheduledTaskRepository::create(
        &conn,
        ScheduledTaskCreateRequest {
            title: "DIAG_TASK_TITLE_SENTINEL".to_string(),
            prompt: "DIAG_PROMPT_SENTINEL".to_string(),
            project_id: None,
            workspace_id: Some(workspace.id.clone()),
            workspace_root: workspace.root_path,
            workspace_mode: WorkspaceMode::CurrentCheckout,
            provider_kind: ProviderKind::Codex,
            provider_profile_id: None,
            schedule: ScheduledTaskSchedule::OneShot(ScheduledTaskOneShotSchedule {
                run_at_ms: 1_900_000_000_000,
            }),
            safety: None,
            next_run_at_ms: Some(1_900_000_000_000),
        },
    )?;
    ScheduledTaskRepository::create_run(
        &conn,
        ScheduledTaskRunCreateRequest {
            task_id: task.id,
            status: ScheduledTaskRunStatus::Failed,
            trigger: ScheduledTaskRunTrigger::Scheduler,
            session_id: None,
            due_at_ms: 1_900_000_000_000,
            started_at_ms: Some(1_900_000_000_100),
            ended_at_ms: Some(1_900_000_000_200),
            attempt: 1,
            error_code: Some("diagnostic_fixture_failed".to_string()),
            error_message: Some("DIAG_TERMINAL_OUTPUT_SENTINEL".to_string()),
            redacted_diagnostics: vec![RedactedDiagnostic {
                key: "state".to_string(),
                value: "redacted".to_string(),
            }],
        },
    )?;
    Ok(())
}

pub fn assert_no_sensitive_sentinels(serialized_bundle: &str) -> VibexResult<()> {
    for sentinel in sensitive_sentinels() {
        if serialized_bundle.contains(sentinel) {
            return Err(VibexError::validation(
                "diagnostic_bundle_redaction_failed",
                "diagnostic bundle included a sensitive fixture sentinel",
            )
            .with_diagnostic("sentinel", sentinel));
        }
    }
    Ok(())
}

fn redaction_policy(limit: u32) -> DiagnosticBundleRedactionPolicy {
    DiagnosticBundleRedactionPolicy {
        default_safe: true,
        policy_version: DIAGNOSTIC_REDACTION_POLICY_VERSION.to_string(),
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
        max_section_records: limit,
    }
}

fn table_count(conn: &DbConnection, table_name: &str) -> VibexResult<DiagnosticCount> {
    let exists = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table_name],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|err| {
            VibexError::storage(
                "diagnostic_table_lookup_failed",
                "failed to inspect diagnostic table availability",
            )
            .with_diagnostic("table", table_name)
            .with_diagnostic("error", err.to_string())
        })?;
    if exists == 0 {
        return Ok(DiagnosticCount {
            name: table_name.to_string(),
            count: 0,
        });
    }

    let sql = format!("SELECT COUNT(*) FROM {table_name}");
    let count = conn
        .query_row(&sql, [], |row| row.get::<_, i64>(0))
        .map_err(|err| {
            VibexError::storage(
                "diagnostic_table_count_failed",
                "failed to read diagnostic table count",
            )
            .with_diagnostic("table", table_name)
            .with_diagnostic("error", err.to_string())
        })?;
    Ok(DiagnosticCount {
        name: table_name.to_string(),
        count: count.max(0).min(u32::MAX as i64) as u32,
    })
}

fn project_attention_record(
    record: &ScheduledTaskAttentionSummary,
) -> DiagnosticScheduledTaskAttentionRecord {
    DiagnosticScheduledTaskAttentionRecord {
        task_id: record.task_id.clone(),
        run_id: record.run_id.clone(),
        workspace_id: record.workspace_id.clone(),
        provider_kind: record.provider_kind,
        provider_profile_id_present: record.provider_profile_id.is_some(),
        trigger: record.trigger,
        status: record.status,
        attention_kind: record.attention_kind,
        session_id_present: record.session_id.is_some(),
        error_code: record.error_code.clone(),
        created_at_ms: record.created_at_ms,
    }
}

fn project_audit_record(record: &ScheduledTaskAuditRecord) -> DiagnosticScheduledTaskAuditRecord {
    DiagnosticScheduledTaskAuditRecord {
        audit_id: record.audit_id.clone(),
        task_id: record.task_id.clone(),
        run_id: record.run_id.clone(),
        workspace_id: record.workspace_id.clone(),
        provider_kind: record.provider_kind,
        provider_profile_id_present: record.provider_profile_id.is_some(),
        trigger: record.trigger,
        outcome: record.outcome,
        status: record.status,
        session_id: record.session_id.clone(),
        error_code: record.error_code.clone(),
        redacted_diagnostics: record.redacted_diagnostics.clone(),
        created_at_ms: record.created_at_ms,
    }
}

fn project_provider_profile_ref(profile: &ProviderProfileSummary) -> DiagnosticProviderProfileRef {
    DiagnosticProviderProfileRef {
        id: profile.id.clone(),
        kind: profile.kind,
        status: profile.status,
        secret_setup_state: profile.secret_setup_state,
        updated_at_ms: profile.updated_at_ms,
    }
}

fn project_provider_health_summary(
    summary: &ProviderHealthSummary,
) -> DiagnosticProviderHealthSummary {
    DiagnosticProviderHealthSummary {
        profile: project_provider_profile_ref(&summary.profile),
        overall_status: summary.overall_status,
        last_checked_at_ms: summary.last_checked_at_ms,
        expires_at_ms: summary.expires_at_ms,
        probe_results: summary
            .probe_results
            .iter()
            .map(project_provider_health_probe)
            .collect(),
    }
}

fn project_provider_health_probe(
    result: &ProviderHealthProbeResult,
) -> DiagnosticProviderHealthProbe {
    DiagnosticProviderHealthProbe {
        provider_kind: result.provider_kind,
        probe_kind: result.probe_kind,
        status: result.status,
        summary: result.summary.clone(),
        latency_ms: result.latency_ms,
        checked_at_ms: result.checked_at_ms,
        expires_at_ms: result.expires_at_ms,
        diagnostics: result.diagnostics.clone(),
    }
}

fn project_provider_capability_summary(
    summary: &ProviderCapabilitySummary,
) -> DiagnosticProviderCapabilitySummary {
    DiagnosticProviderCapabilitySummary {
        profile: project_provider_profile_ref(&summary.profile),
        status: summary.status,
        effective_capabilities: summary.effective_capabilities.clone(),
        capability_source: summary.capability_source.clone(),
        fresh: summary.fresh,
        last_checked_at_ms: summary.last_checked_at_ms,
        expires_at_ms: summary.expires_at_ms,
        diagnostics: summary.diagnostics.clone(),
    }
}

fn project_provider_usage_summary(
    summary: &ProviderUsageSummary,
) -> DiagnosticProviderUsageSummary {
    DiagnosticProviderUsageSummary {
        profile: project_provider_profile_ref(&summary.profile),
        balances: summary.balances.clone(),
        latest_recorded_at_ms: summary.latest_recorded_at_ms,
    }
}

fn scheduled_error_codes(records: &[ScheduledTaskAuditRecord], limit: u32) -> Vec<String> {
    let mut codes = BTreeSet::new();
    for record in records {
        if let Some(code) = &record.error_code {
            codes.insert(code.clone());
        }
        if codes.len() >= limit as usize {
            break;
        }
    }
    codes.into_iter().collect()
}

fn project_runtime_metrics(
    snapshot: Option<RuntimeMetricSnapshot>,
    limit: u32,
) -> DiagnosticRuntimeSection {
    let snapshot = snapshot.unwrap_or_else(|| RuntimeObservability::new().snapshot());
    DiagnosticRuntimeSection {
        process_started_at_ms: snapshot.process_started_at_ms,
        snapshot_at_ms: snapshot.snapshot_at_ms,
        series_limit: snapshot.series_limit.min(u32::MAX as usize) as u32,
        series: snapshot
            .series
            .into_iter()
            .take(limit as usize)
            .map(|series| DiagnosticRuntimeMetric {
                name: series.name.as_str().to_string(),
                operation: series
                    .operation
                    .map(|operation| operation.as_str().to_string()),
                result: series.result.as_str().to_string(),
                count: series.count,
                duration_total_ms: series.duration_total_ms,
                duration_min_ms: series.duration_min_ms,
                duration_max_ms: series.duration_max_ms,
                duration_last_ms: series.duration_last_ms,
            })
            .collect(),
    }
}

fn bounded_limit(limit: Option<u32>) -> u32 {
    match limit {
        Some(1..=MAX_DIAGNOSTIC_RECORD_LIMIT) => limit.unwrap(),
        Some(value) if value > MAX_DIAGNOSTIC_RECORD_LIMIT => MAX_DIAGNOSTIC_RECORD_LIMIT,
        _ => DEFAULT_DIAGNOSTIC_RECORD_LIMIT,
    }
}

fn saturating_len<T>(values: &[T]) -> u32 {
    values.len().min(u32::MAX as usize) as u32
}

fn classify_database_path(path: &Path) -> (DiagnosticDatabasePathKind, String) {
    if std::env::var_os("VIBEX_DB_PATH").is_some() {
        return (
            DiagnosticDatabasePathKind::ExplicitOverride,
            "VIBEX_DB_PATH override (path redacted)".to_string(),
        );
    }

    let path_string = path.to_string_lossy();
    if path_string.ends_with(".vibex/vibex.db") {
        (
            DiagnosticDatabasePathKind::DefaultVibexHome,
            "~/.vibex/vibex.db".to_string(),
        )
    } else if path_string.starts_with("target/")
        || path_string.contains("/target/")
        || path.starts_with(std::env::temp_dir())
    {
        (
            DiagnosticDatabasePathKind::Temporary,
            "temporary/disposable diagnostic database".to_string(),
        )
    } else {
        (
            DiagnosticDatabasePathKind::Unknown,
            "non-default database path (path redacted)".to_string(),
        )
    }
}

fn smoke_references() -> Vec<DiagnosticSmokeCommandReference> {
    vec![
        smoke_reference(
            "diagnostics",
            "pnpm smoke:diagnostics",
            DiagnosticSmokeCommandKind::Deterministic,
            false,
            "Exports a redacted diagnostic bundle from disposable local state.",
        ),
        smoke_reference(
            "database",
            "pnpm smoke:db",
            DiagnosticSmokeCommandKind::Deterministic,
            false,
            "Runs SQLite migration and sentinel round-trip smoke.",
        ),
        smoke_reference(
            "backup-restore",
            "pnpm smoke:backup",
            DiagnosticSmokeCommandKind::Deterministic,
            false,
            "Runs backup/restore round-trip smoke against disposable local databases.",
        ),
        smoke_reference(
            "files",
            "pnpm smoke:files",
            DiagnosticSmokeCommandKind::Deterministic,
            false,
            "Runs workspace file service smoke without reading arbitrary user files.",
        ),
        smoke_reference(
            "git",
            "pnpm smoke:git",
            DiagnosticSmokeCommandKind::Deterministic,
            false,
            "Runs Git status/diff smoke against an explicit repository path.",
        ),
        smoke_reference(
            "pty",
            "pnpm smoke:pty",
            DiagnosticSmokeCommandKind::Deterministic,
            false,
            "Runs PTY lifecycle smoke with a deterministic marker.",
        ),
        smoke_reference(
            "relay-local",
            "pnpm smoke:relay:local",
            DiagnosticSmokeCommandKind::Deterministic,
            false,
            "Runs local Relay health/info smoke.",
        ),
        smoke_reference(
            "codex-session",
            "pnpm smoke:session:codex",
            DiagnosticSmokeCommandKind::ExplicitManual,
            true,
            "Explicit real Codex session smoke; excluded from default checks.",
        ),
        smoke_reference(
            "claude-session",
            "pnpm smoke:session:claude",
            DiagnosticSmokeCommandKind::ExplicitManual,
            true,
            "Explicit real Claude session smoke; excluded from default checks.",
        ),
        smoke_reference(
            "acp-opencode",
            "pnpm smoke:acp:opencode",
            DiagnosticSmokeCommandKind::ExplicitManual,
            true,
            "Explicit OpenCode ACP smoke; excluded from default checks.",
        ),
        smoke_reference(
            "scheduled-codex",
            "pnpm smoke:scheduled:codex",
            DiagnosticSmokeCommandKind::ExplicitManual,
            true,
            "Explicit scheduled Codex evidence command; may return blocked evidence.",
        ),
        smoke_reference(
            "scheduled-claude",
            "pnpm smoke:scheduled:claude",
            DiagnosticSmokeCommandKind::ExplicitManual,
            true,
            "Explicit scheduled Claude evidence command; may return blocked evidence.",
        ),
        smoke_reference(
            "scheduled-acp",
            "pnpm smoke:scheduled:acp",
            DiagnosticSmokeCommandKind::ExplicitManual,
            true,
            "Explicit scheduled ACP evidence command; may return blocked evidence.",
        ),
    ]
}

fn smoke_reference(
    name: &str,
    command: &str,
    kind: DiagnosticSmokeCommandKind,
    starts_real_provider: bool,
    description: &str,
) -> DiagnosticSmokeCommandReference {
    DiagnosticSmokeCommandReference {
        name: name.to_string(),
        command: command.to_string(),
        kind,
        starts_real_provider,
        description: description.to_string(),
    }
}

fn target_family() -> &'static str {
    if cfg!(target_family = "unix") {
        "unix"
    } else if cfg!(target_family = "windows") {
        "windows"
    } else if cfg!(target_family = "wasm") {
        "wasm"
    } else {
        "unknown"
    }
}

fn sensitive_sentinels() -> [&'static str; 4] {
    [
        "DIAG_TASK_TITLE_SENTINEL",
        "DIAG_PROMPT_SENTINEL",
        "DIAG_TERMINAL_OUTPUT_SENTINEL",
        "DIAG_FILE_PATH_SENTINEL",
    ]
}

fn remove_database_family(path: &Path) -> VibexResult<()> {
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        match std::fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(VibexError::storage(
                    "diagnostic_bundle_smoke_db_cleanup_failed",
                    "failed to remove previous diagnostic smoke database",
                )
                .with_diagnostic("error", err.to_string()));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceBaselineOverallStatus {
    Pass,
    PassWithFollowUps,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceBaselineCheckStatus {
    Pass,
    FollowUp,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceBaselineClassification {
    Blocker,
    FollowUp,
    AcceptableMvpLimit,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceBaselineCheck {
    pub name: String,
    pub status: PerformanceBaselineCheckStatus,
    pub classification: PerformanceBaselineClassification,
    pub fixture_size: BTreeMap<String, u64>,
    pub elapsed_ms: u64,
    pub output_count: u64,
    pub limit: String,
    pub notes: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceBaselineResult {
    pub schema_version: u32,
    pub generated_at_ms: i64,
    pub overall_status: PerformanceBaselineOverallStatus,
    pub checks: Vec<PerformanceBaselineCheck>,
}

impl PerformanceBaselineResult {
    pub fn has_blocker(&self) -> bool {
        self.overall_status == PerformanceBaselineOverallStatus::Fail
    }
}

pub fn run_performance_baseline() -> VibexResult<PerformanceBaselineResult> {
    let root = performance_baseline_root();
    match fs::remove_dir_all(&root) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(VibexError::storage(
                "performance_baseline_cleanup_failed",
                "failed to clean previous performance baseline fixture",
            )
            .with_diagnostic("error", err.to_string()));
        }
    }
    fs::create_dir_all(&root).map_err(|err| {
        VibexError::storage(
            "performance_baseline_fixture_create_failed",
            "failed to create performance baseline fixture root",
        )
        .with_diagnostic("error", err.to_string())
    })?;

    let result = run_performance_baseline_in(&root);
    match fs::remove_dir_all(&root) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(VibexError::storage(
                "performance_baseline_cleanup_failed",
                "failed to remove performance baseline fixture root",
            )
            .with_diagnostic("error", err.to_string()));
        }
    }
    result
}

fn run_performance_baseline_in(root: &Path) -> VibexResult<PerformanceBaselineResult> {
    let mut checks = Vec::new();
    checks.push(capture_check("timeline_fetch_large", || {
        baseline_timeline(root)
    }));
    checks.push(capture_check("db_recent_file_limit", || {
        baseline_db_query_limit(root)
    }));
    checks.push(capture_check("file_tree_search", || {
        baseline_file_tree_search(root)
    }));
    checks.push(capture_check("git_status_fixture", || {
        baseline_git_status(root)
    }));
    checks.push(capture_check("terminal_ring_buffer", || {
        baseline_terminal_buffer(root)
    }));

    let has_failure = checks
        .iter()
        .any(|check| check.status == PerformanceBaselineCheckStatus::Fail);
    let has_follow_up = checks
        .iter()
        .any(|check| check.status == PerformanceBaselineCheckStatus::FollowUp);
    let overall_status = if has_failure {
        PerformanceBaselineOverallStatus::Fail
    } else if has_follow_up {
        PerformanceBaselineOverallStatus::PassWithFollowUps
    } else {
        PerformanceBaselineOverallStatus::Pass
    };

    Ok(PerformanceBaselineResult {
        schema_version: 1,
        generated_at_ms: unix_timestamp_ms(),
        overall_status,
        checks,
    })
}

fn capture_check(
    name: &str,
    run: impl FnOnce() -> VibexResult<PerformanceBaselineCheck>,
) -> PerformanceBaselineCheck {
    match run() {
        Ok(check) => check,
        Err(err) => PerformanceBaselineCheck {
            name: name.to_string(),
            status: PerformanceBaselineCheckStatus::Fail,
            classification: PerformanceBaselineClassification::Blocker,
            fixture_size: BTreeMap::new(),
            elapsed_ms: 0,
            output_count: 0,
            limit: "n/a".to_string(),
            notes: format!("check failed with {}", err.code),
        },
    }
}

fn baseline_timeline(root: &Path) -> VibexResult<PerformanceBaselineCheck> {
    let db_path = root.join("timeline.db");
    let workspace_root = root.join("timeline-workspace");
    fs::create_dir_all(&workspace_root).map_err(storage_io("timeline_fixture_create_failed"))?;

    let mut conn = open_database(&db_path)?;
    apply_migrations(&mut conn)?;
    let (project, workspace) =
        WorkspaceRepository::ensure(&conn, &workspace_root, WorkspaceMode::CurrentCheckout)?;
    let now = unix_timestamp_ms();
    let session = AgentSession {
        id: VibexSessionId::new(),
        title: "Performance baseline timeline".to_string(),
        project_id: project.id,
        workspace_id: workspace.id.clone(),
        workspace_root: workspace.root_path.clone(),
        workspace_mode: workspace.mode,
        agent_id: vibex_core::AgentId::parse("codex")?,
        state: AgentSessionState::Idle,
        safety: AgentSessionSafety::workspace_write_ask_on_risk(),
        created_at_ms: now,
        updated_at_ms: now,
        last_message_at_ms: now,
        archived_at_ms: None,
        deleted_at_ms: None,
    };
    SessionRepository::insert(&conn, &session)?;

    let fixture_items = 1_200_u64;
    for index in 0..fixture_items {
        TimelineRepository::append(
            &mut conn,
            &session.id,
            TimelineSource::System,
            TimelinePayload::SystemNotice(SystemNoticePayload {
                level: SystemNoticeLevel::Info,
                message: format!("performance baseline event {index}"),
            }),
            None,
            None,
            TimelineRedactionState::None,
        )?;
    }

    let started = Instant::now();
    let page = TimelineRepository::fetch_after(&conn, &session.id, None, 100)?;
    let elapsed_ms = elapsed_ms(started);
    let output_count = page.items.len() as u64;
    let status = if output_count == 100 && page.has_older {
        PerformanceBaselineCheckStatus::Pass
    } else {
        PerformanceBaselineCheckStatus::Fail
    };
    Ok(PerformanceBaselineCheck {
        name: "timeline_fetch_large".to_string(),
        status,
        classification: classification_for_status(status),
        fixture_size: fixture_size([("timeline_items", fixture_items)]),
        elapsed_ms,
        output_count,
        limit: "fetch_after(after_sequence=null, limit=100)".to_string(),
        notes: "bounded latest timeline page fetched from disposable SQLite".to_string(),
    })
}

fn baseline_db_query_limit(root: &Path) -> VibexResult<PerformanceBaselineCheck> {
    let db_path = root.join("recent-files.db");
    let workspace_root = root.join("recent-files-workspace");
    fs::create_dir_all(&workspace_root).map_err(storage_io("recent_file_fixture_create_failed"))?;

    let mut conn = open_database(&db_path)?;
    apply_migrations(&mut conn)?;
    let (_, workspace) =
        WorkspaceRepository::ensure(&conn, &workspace_root, WorkspaceMode::CurrentCheckout)?;

    let fixture_rows = 150_u64;
    for index in 0..fixture_rows {
        RecentFileRepository::touch(&conn, &workspace.id, &format!("src/file_{index:03}.rs"))?;
    }

    let requested_limit = 250_u32;
    let started = Instant::now();
    let rows = RecentFileRepository::list(&conn, &workspace.id, requested_limit)?;
    let elapsed_ms = elapsed_ms(started);
    let output_count = rows.len() as u64;
    let status = if output_count == 100 {
        PerformanceBaselineCheckStatus::Pass
    } else {
        PerformanceBaselineCheckStatus::Fail
    };
    Ok(PerformanceBaselineCheck {
        name: "db_recent_file_limit".to_string(),
        status,
        classification: classification_for_status(status),
        fixture_size: fixture_size([("recent_file_rows", fixture_rows)]),
        elapsed_ms,
        output_count,
        limit: "RecentFileRepository::list(limit=250) clamps to 100".to_string(),
        notes: "capped DB query returned repository maximum without raw paths".to_string(),
    })
}

fn baseline_file_tree_search(root: &Path) -> VibexResult<PerformanceBaselineCheck> {
    let workspace_root = root.join("file-workspace");
    fs::create_dir_all(&workspace_root).map_err(storage_io("file_fixture_create_failed"))?;
    let workspace_id = vibex_core::WorkspaceId::new();

    let directories = 20_u64;
    let files_per_directory = 15_u64;
    for dir_index in 0..directories {
        let dir = workspace_root
            .join(format!("module_{dir_index:02}"))
            .join("src");
        fs::create_dir_all(&dir).map_err(storage_io("file_fixture_create_failed"))?;
        for file_index in 0..files_per_directory {
            let marker = if file_index % 5 == 0 {
                "needle"
            } else {
                "ordinary"
            };
            fs::write(
                dir.join(format!("baseline_{file_index:02}_{marker}.txt")),
                format!("{marker} fixture line {dir_index} {file_index}\n"),
            )
            .map_err(storage_io("file_fixture_write_failed"))?;
        }
    }

    let service = WorkspaceFileService::new(&workspace_root, workspace_id.clone())?;
    let started = Instant::now();
    let tree = service.list_tree(&FileTreeRequest {
        workspace_id: workspace_id.clone(),
        path: None,
        max_depth: Some(4),
        include_hidden: false,
    })?;
    let search = service.search(&FileSearchRequest {
        workspace_id,
        query: "needle".to_string(),
        include_content: true,
        limit: Some(120),
    })?;
    let elapsed_ms = elapsed_ms(started);
    let status = if !tree.is_empty() && search.len() >= 60 && search.len() <= 120 {
        PerformanceBaselineCheckStatus::Pass
    } else {
        PerformanceBaselineCheckStatus::Fail
    };

    Ok(PerformanceBaselineCheck {
        name: "file_tree_search".to_string(),
        status,
        classification: classification_for_status(status),
        fixture_size: fixture_size([
            ("directories", directories),
            ("files", directories * files_per_directory),
            ("minimum_search_matches", 60),
        ]),
        elapsed_ms,
        output_count: (tree.len() + search.len()) as u64,
        limit: "list_tree(max_depth=4), search(limit=120)".to_string(),
        notes: "file traversal and search counted generated fixture entries and capped search results only".to_string(),
    })
}

fn baseline_git_status(root: &Path) -> VibexResult<PerformanceBaselineCheck> {
    if Command::new("git").arg("--version").output().is_err() {
        return Ok(PerformanceBaselineCheck {
            name: "git_status_fixture".to_string(),
            status: PerformanceBaselineCheckStatus::FollowUp,
            classification: PerformanceBaselineClassification::FollowUp,
            fixture_size: BTreeMap::new(),
            elapsed_ms: 0,
            output_count: 0,
            limit: "git status --porcelain=v1".to_string(),
            notes: "git binary unavailable in this environment".to_string(),
        });
    }

    let repo = root.join("git-workspace");
    fs::create_dir_all(&repo).map_err(storage_io("git_fixture_create_failed"))?;
    run_git_command(&repo, &["init"])?;
    run_git_command(&repo, &["config", "user.email", "baseline@example.invalid"])?;
    run_git_command(&repo, &["config", "user.name", "Vibex Baseline"])?;
    fs::write(repo.join("tracked.txt"), "tracked baseline\n")
        .map_err(storage_io("git_fixture_write_failed"))?;
    run_git_command(&repo, &["add", "tracked.txt"])?;
    run_git_command(&repo, &["commit", "-m", "baseline fixture"])?;
    fs::write(repo.join("tracked.txt"), "tracked baseline modified\n")
        .map_err(storage_io("git_fixture_write_failed"))?;
    fs::write(repo.join("untracked.txt"), "untracked baseline\n")
        .map_err(storage_io("git_fixture_write_failed"))?;

    let started = Instant::now();
    let status_summary = vibex_git::status(vibex_core::WorkspaceId::new(), &repo)?;
    let elapsed_ms = elapsed_ms(started);
    let output_count = status_summary.changes.len() as u64;
    let status = if status_summary.dirty && status_summary.untracked_count == 1 {
        PerformanceBaselineCheckStatus::Pass
    } else {
        PerformanceBaselineCheckStatus::Fail
    };
    Ok(PerformanceBaselineCheck {
        name: "git_status_fixture".to_string(),
        status,
        classification: classification_for_status(status),
        fixture_size: fixture_size([("changed_files", 2)]),
        elapsed_ms,
        output_count,
        limit: "git status --porcelain=v1".to_string(),
        notes: "Git status summary recorded counts only; raw diffs are excluded".to_string(),
    })
}

fn baseline_terminal_buffer(root: &Path) -> VibexResult<PerformanceBaselineCheck> {
    let workspace_root = root.join("terminal-workspace");
    fs::create_dir_all(&workspace_root).map_err(storage_io("terminal_fixture_create_failed"))?;

    let ring_capacity = 4_usize;
    let manager = TerminalManager::with_ring_capacity(ring_capacity);
    let workspace_id = vibex_core::WorkspaceId::new();
    let session = manager.create(
        &workspace_root,
        TerminalCreateRequest {
            workspace_id,
            title: Some("performance baseline".to_string()),
            shell: Some(baseline_shell()),
            cwd: None,
            rows: 24,
            cols: 80,
        },
    )?;

    let started = Instant::now();
    manager.write(&TerminalWriteRequest {
        terminal_id: session.id.clone(),
        data: terminal_output_command(),
    })?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut snapshot = manager.snapshot(&session.id)?;
    while Instant::now() < deadline {
        snapshot = manager.snapshot(&session.id)?;
        if snapshot.next_sequence > ring_capacity as i64 + 1 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let elapsed_ms = elapsed_ms(started);
    let _ = manager.kill(&session.id);

    let appended_chunks = snapshot.next_sequence.saturating_sub(1) as u64;
    let retained_chunks = snapshot.chunks.len() as u64;
    let status = if appended_chunks > retained_chunks && retained_chunks <= ring_capacity as u64 {
        PerformanceBaselineCheckStatus::Pass
    } else {
        PerformanceBaselineCheckStatus::Fail
    };

    Ok(PerformanceBaselineCheck {
        name: "terminal_ring_buffer".to_string(),
        status,
        classification: classification_for_status(status),
        fixture_size: fixture_size([
            ("ring_capacity_chunks", ring_capacity as u64),
            ("requested_output_lines", 1_200),
        ]),
        elapsed_ms,
        output_count: retained_chunks,
        limit: "TerminalManager::with_ring_capacity(4)".to_string(),
        notes: format!(
            "terminal buffer retained {retained_chunks} of {appended_chunks} chunks without serializing output"
        ),
    })
}

fn performance_baseline_root() -> PathBuf {
    PathBuf::from("target").join("stage0").join(format!(
        "performance-baseline-{}-{}",
        std::process::id(),
        unix_timestamp_ms()
    ))
}

fn fixture_size(items: impl IntoIterator<Item = (&'static str, u64)>) -> BTreeMap<String, u64> {
    items
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

fn classification_for_status(
    status: PerformanceBaselineCheckStatus,
) -> PerformanceBaselineClassification {
    match status {
        PerformanceBaselineCheckStatus::Pass => {
            PerformanceBaselineClassification::AcceptableMvpLimit
        }
        PerformanceBaselineCheckStatus::FollowUp => PerformanceBaselineClassification::FollowUp,
        PerformanceBaselineCheckStatus::Fail => PerformanceBaselineClassification::Blocker,
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

fn storage_io(code: &'static str) -> impl Fn(std::io::Error) -> VibexError {
    move |err| {
        VibexError::storage(code, "performance baseline fixture IO failed")
            .with_diagnostic("error", err.to_string())
    }
}

fn run_git_command(repo: &Path, args: &[&str]) -> VibexResult<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|err| {
            VibexError::process("git_spawn_failed", "failed to spawn git")
                .with_diagnostic("error", err.to_string())
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(
        VibexError::process("git_fixture_command_failed", "git fixture command failed")
            .with_diagnostic("command", args.join(" ")),
    )
}

fn baseline_shell() -> String {
    #[cfg(target_os = "windows")]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }

    #[cfg(not(target_os = "windows"))]
    {
        "/bin/sh".to_string()
    }
}

fn terminal_output_command() -> String {
    #[cfg(target_os = "windows")]
    {
        "for /L %i in (1,1,1200) do @echo baseline-line-%i\r\n".to_string()
    }

    #[cfg(not(target_os = "windows"))]
    {
        "i=0; while [ $i -lt 1200 ]; do printf 'baseline-line-%04d\\n' \"$i\"; i=$((i+1)); done\n"
            .to_string()
    }
}

pub fn default_diagnostic_database_path() -> VibexResult<PathBuf> {
    default_database_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_agent::{RuntimeMetricName, RuntimeMetricResult};

    #[test]
    fn default_bundle_excludes_sensitive_fixture_fields() {
        let path = temp_db_path("redaction");
        remove_database_family(&path).unwrap();
        seed_diagnostic_smoke_fixture(&path).unwrap();

        let service =
            DiagnosticBundleService::new(DiagnosticBundleServiceConfig::new(path.clone()));
        let bundle = service
            .export_bundle(DiagnosticBundleRequest {
                record_limit: Some(10),
                include_smoke_references: Some(true),
            })
            .unwrap();
        let json = serde_json::to_string(&bundle).unwrap();
        assert_no_sensitive_sentinels(&json).unwrap();
        assert_eq!(bundle.scheduled_tasks.audit_count, 1);
        assert_eq!(
            bundle.scheduled_tasks.audit[0].error_code.as_deref(),
            Some("diagnostic_fixture_failed")
        );

        remove_database_family(&path).unwrap();
    }

    #[test]
    fn record_limit_is_bounded_and_recorded() {
        let path = temp_db_path("bounds");
        remove_database_family(&path).unwrap();
        seed_diagnostic_smoke_fixture(&path).unwrap();

        let service =
            DiagnosticBundleService::new(DiagnosticBundleServiceConfig::new(path.clone()));
        let bundle = service
            .export_bundle(DiagnosticBundleRequest {
                record_limit: Some(500),
                include_smoke_references: Some(false),
            })
            .unwrap();
        assert_eq!(
            bundle.redaction.max_section_records,
            MAX_DIAGNOSTIC_RECORD_LIMIT
        );
        assert!(bundle.smokes.references.is_empty());

        remove_database_family(&path).unwrap();
    }

    #[test]
    fn release_context_is_allowlisted_and_content_free() {
        let path = temp_db_path("release-context");
        remove_database_family(&path).unwrap();
        seed_diagnostic_smoke_fixture(&path).unwrap();
        let bundle = DiagnosticBundleService::new(DiagnosticBundleServiceConfig::new(path.clone()))
            .export_bundle(DiagnosticBundleRequest::default())
            .unwrap();
        let context = bundle.metadata.release_context.unwrap();
        assert_eq!(
            context.ui_state_schema.as_deref(),
            Some("desktop-ui-state.v1")
        );
        assert_eq!(
            context.pdf_backend.as_deref(),
            Some("pdfium_7881_supervised")
        );
        assert_eq!(context.cache_budgets.len(), 2);
        let json = serde_json::to_string(&context).unwrap();
        for forbidden in [
            "prompt",
            "terminalOutput",
            "fileContent",
            "secret",
            "authToken",
        ] {
            assert!(!json.contains(forbidden));
        }
        remove_database_family(&path).unwrap();
    }

    #[test]
    fn release_context_text_rejects_sensitive_build_values() {
        assert_eq!(
            safe_release_text(Some("deadbeef")),
            Some("deadbeef".to_string())
        );
        assert_eq!(safe_release_text(Some("DIAG_SECRET_SENTINEL")), None);
        assert_eq!(safe_release_text(Some("prompt-body")), None);
    }

    #[test]
    fn runtime_metrics_are_bounded_aggregates_without_business_identifiers() {
        let path = temp_db_path("runtime-metrics");
        remove_database_family(&path).unwrap();
        seed_diagnostic_smoke_fixture(&path).unwrap();
        let observability = Arc::new(RuntimeObservability::new());
        observability.observe_duration_ms(
            RuntimeMetricName::PromptLatency,
            None,
            RuntimeMetricResult::Success,
            42,
        );
        let service = DiagnosticBundleService::new(
            DiagnosticBundleServiceConfig::new(path.clone())
                .with_runtime_observability(observability),
        );
        let bundle = service
            .export_bundle(DiagnosticBundleRequest {
                record_limit: Some(10),
                include_smoke_references: Some(false),
            })
            .unwrap();
        assert_eq!(bundle.metadata.schema_version, "diagnostic_bundle.v2");
        assert_eq!(bundle.runtime.series.len(), 1);
        assert_eq!(bundle.runtime.series[0].name, "runtime_prompt_latency_ms");
        assert_eq!(bundle.runtime.series[0].duration_total_ms, Some(42));
        let json = serde_json::to_string(&bundle.runtime).unwrap();
        for forbidden in [
            "logical_session_id",
            "binding_id",
            "process_instance_id",
            "native_session_id",
            "process_spawn_fingerprint",
            "DIAG_PROMPT_SENTINEL",
        ] {
            assert!(!json.contains(forbidden));
        }
        remove_database_family(&path).unwrap();
    }

    #[test]
    fn performance_baseline_output_is_bounded_and_redacted() {
        let result = run_performance_baseline().unwrap();
        assert!(!result.has_blocker());
        assert_eq!(result.checks.len(), 5);

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("timeline_fetch_large"));
        assert!(!json.contains("baseline-line-"));
        assert!(!json.contains("tracked baseline"));
        assert!(!json.contains("needle fixture"));
        assert!(!json.contains("performance-baseline-"));
        assert!(!json.contains(".db"));
    }

    fn temp_db_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vibex-diagnostics-{name}-{}.db",
            unix_timestamp_ms()
        ))
    }
}

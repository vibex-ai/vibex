use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{Context, IntoElement, Render, Task, WeakEntity, Window, div, prelude::*, px};
use gpui_component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use serde::Serialize;
use tokio::sync::mpsc;
use vibex_agent::{
    AgentManager, AgentProvider, ProviderCreateRequest, ProviderEvent, ProviderRuntimeResources,
    ProviderSessionHandle, ProviderTurnRequest, RuntimeLifecycleBackend,
};
use vibex_agent_acp::{
    AcpAgentProvider, AcpRuntimeClient, AcpRuntimeLifecycleBackend, AcpRuntimeSwitchBridge,
    sanitize_inherited_appimage_environment,
};
use vibex_config_switch::ProviderConfigService;
use vibex_core::{
    AcpProcessStrategy, AcpProviderConfig, AcpProviderProfileCreateRequest, AgentId,
    AgentSessionSafety, TimelineItemKind, TimelinePayload, VibexError, VibexResult, VibexSessionId,
    unix_timestamp_ms,
};

const PROMPT: &str = "Reply with exactly: vibex-acp-ok. Do not run tools or edit files.";
const DEFAULT_HOLD_MS: u64 = 1_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpLifecycleReport {
    pub schema_version: &'static str,
    pub status: &'static str,
    pub provider: ProviderObservation,
    pub gpui: GpuiObservation,
    pub session: SessionObservation,
    pub error_surface: ErrorObservation,
    pub shutdown: ShutdownObservation,
    pub failure: Option<FailureObservation>,
    pub limitations: Vec<&'static str>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderObservation {
    pub id: String,
    pub version: String,
    pub real_process: bool,
    pub adapter_boundary: bool,
    pub transport: &'static str,
    pub raw_output_stored: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuiObservation {
    pub tokio_task_owned: bool,
    pub foreground_entity_updates: u64,
    pub response_text_stored: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionObservation {
    pub created: bool,
    pub native_id_present: bool,
    pub completed: bool,
    pub streamed_event_count: u64,
    pub streamed_text_bytes: u64,
    pub event_counts: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorObservation {
    pub exercised: bool,
    pub structured_error_code: Option<String>,
    pub raw_provider_error_stored: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShutdownObservation {
    pub session_closed: bool,
    pub sweep_completed: bool,
    pub processes_removed: u32,
    pub elapsed_ms: u64,
    pub bounded: bool,
    pub temporary_root_removed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureObservation {
    pub code: String,
    pub category: String,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
enum Progress {
    Phase(&'static str),
    StreamEvent { kind: &'static str },
    Finished(AcpLifecycleReport),
}

pub struct AcpLifecycleView {
    phase: &'static str,
    stream_events: u64,
    foreground_updates: u64,
    report: Option<AcpLifecycleReport>,
    _task: Task<()>,
}

impl AcpLifecycleView {
    pub fn new(output: PathBuf, cx: &mut Context<Self>) -> Self {
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            let report = run_lifecycle(progress_tx.clone()).await;
            let _ = progress_tx.send(Progress::Finished(report));
        });
        let task = cx.spawn(
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                while let Some(progress) = progress_rx.recv().await {
                    let finished = matches!(progress, Progress::Finished(_));
                    let output = output.clone();
                    let _ = entity.update(cx, move |this, cx| {
                        this.foreground_updates = this.foreground_updates.saturating_add(1);
                        match progress {
                            Progress::Phase(phase) => this.phase = phase,
                            Progress::StreamEvent { kind } => {
                                this.phase = kind;
                                this.stream_events = this.stream_events.saturating_add(1);
                            }
                            Progress::Finished(mut report) => {
                                report.gpui.foreground_entity_updates = this.foreground_updates;
                                finalize_report(&mut report);
                                if let Err(error) = write_report(&output, &report) {
                                    report.status = "failed";
                                    report.failure = Some(failure_observation(&error));
                                }
                                this.phase = report.status;
                                this.report = Some(report);
                            }
                        }
                        cx.notify();
                    });
                    if finished {
                        cx.background_executor()
                            .timer(Duration::from_millis(hold_ms()))
                            .await;
                        cx.update(|cx| cx.quit());
                        break;
                    }
                }
                let _ = runner.await;
            },
        );
        Self {
            phase: "starting",
            stream_events: 0,
            foreground_updates: 0,
            report: None,
            _task: task,
        }
    }
}

impl Render for AcpLifecycleView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let report = self.report.as_ref();
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .p_8()
            .gap_5()
            .child(
                div()
                    .text_2xl()
                    .font_semibold()
                    .child("GPUI / ACP lifecycle"),
            )
            .child(status_row("Phase", self.phase, cx))
            .child(status_row(
                "Foreground updates",
                &self.foreground_updates.to_string(),
                cx,
            ))
            .child(status_row(
                "Stream events",
                &self.stream_events.to_string(),
                cx,
            ))
            .child(status_row(
                "Real turn",
                if report.is_some_and(|report| report.session.completed) {
                    "completed"
                } else {
                    "pending"
                },
                cx,
            ))
            .child(status_row(
                "Bounded shutdown",
                if report.is_some_and(|report| report.shutdown.bounded) {
                    "verified"
                } else {
                    "pending"
                },
                cx,
            ))
            .child(
                div()
                    .mt_4()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Provider response text is intentionally not retained."),
            )
    }
}

fn status_row(
    label: &'static str,
    value: &str,
    cx: &mut Context<AcpLifecycleView>,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .max_w(px(640.0))
        .justify_between()
        .border_b_1()
        .border_color(cx.theme().border)
        .py_3()
        .child(div().text_color(cx.theme().muted_foreground).child(label))
        .child(div().font_semibold().child(value.to_string()))
}

async fn run_lifecycle(progress: mpsc::UnboundedSender<Progress>) -> AcpLifecycleReport {
    let started = Instant::now();
    let root = std::env::temp_dir().join(format!(
        "vibex-acp-lifecycle-{}",
        vibex_core::RequestId::new().as_str()
    ));
    let workspace = root.join("workspace");
    let db_path = root.join("vibex.db");
    let mut report = empty_report();
    let outcome = run_lifecycle_inner(&root, &workspace, &db_path, &progress, &mut report).await;
    report.shutdown.elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    report.shutdown.bounded = report.shutdown.elapsed_ms < 120_000;
    let _ = fs::remove_dir_all(&root);
    report.shutdown.temporary_root_removed = !root.exists();
    if let Err(error) = outcome {
        report.failure = Some(failure_observation(&error));
    }
    report
}

async fn run_lifecycle_inner(
    root: &Path,
    workspace: &Path,
    db_path: &Path,
    progress: &mpsc::UnboundedSender<Progress>,
    report: &mut AcpLifecycleReport,
) -> VibexResult<()> {
    fs::create_dir_all(workspace).map_err(|error| {
        VibexError::storage(
            "acp_workspace_create_failed",
            "GPUI ACP workspace could not be created",
        )
        .with_diagnostic("error", error.kind().to_string())
    })?;
    let binary = find_binary("opencode")?;
    report.provider.id = "opencode".to_string();
    report.provider.version = command_version(&binary)?;
    report.provider.real_process = true;
    let _ = progress.send(Progress::Phase("configuring"));

    let service = ProviderConfigService::new(db_path);
    let profile = service.create_acp_profile(AcpProviderProfileCreateRequest {
        agent_id: Some(AgentId::parse("opencode")?),
        display_name: "GPUI ACP lifecycle".to_string(),
        account_alias: None,
        preset_id: None,
        config: Some(opencode_config(binary.to_string_lossy().into_owned())),
    })?;
    let client = Arc::new(AcpRuntimeClient::new(ProviderConfigService::new(db_path)));
    let provider =
        AcpAgentProvider::with_config_service(client.clone(), ProviderConfigService::new(db_path));
    let manager = Arc::new(AgentManager::new(db_path)?);
    let bridge = Arc::new(AcpRuntimeSwitchBridge::new(
        db_path,
        client.clone(),
        manager,
    )?);
    let backend = AcpRuntimeLifecycleBackend::new(bridge).with_limits(
        Duration::from_millis(1),
        1,
        Duration::from_millis(1),
        1,
    )?;

    let _ = progress.send(Progress::Phase("creating_session"));
    let session_id = VibexSessionId::new();
    let handle = provider
        .create_session(ProviderCreateRequest {
            session_id: session_id.clone(),
            provider_profile_id: profile.id.clone(),
            model: None,
            workspace_root: workspace.display().to_string(),
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            runtime_resources: ProviderRuntimeResources::default(),
        })
        .await?;
    report.session.created = true;
    report.session.native_id_present = handle.binding.native.native_session_id.is_some();
    report.provider.adapter_boundary = true;
    let binding = handle.binding.clone();

    let exercise = run_session_exercise(
        root, workspace, &service, &provider, handle, progress, report,
    )
    .await;

    let _ = progress.send(Progress::Phase("shutting_down"));
    let close = provider.close_session(binding).await;
    if close.is_ok() {
        report.shutdown.session_closed = true;
    }
    let sweep = backend
        .sweep(unix_timestamp_ms().saturating_add(10_000), &[])
        .await;
    if let Ok(sweep) = &sweep {
        report.shutdown.sweep_completed = true;
        report.shutdown.processes_removed = sweep.processes_removed.min(u32::MAX as usize) as u32;
    }

    exercise?;
    close?;
    sweep?;
    Ok(())
}

async fn run_session_exercise(
    root: &Path,
    workspace: &Path,
    service: &ProviderConfigService,
    provider: &AcpAgentProvider,
    handle: ProviderSessionHandle,
    progress: &mpsc::UnboundedSender<Progress>,
    report: &mut AcpLifecycleReport,
) -> VibexResult<()> {
    let session_id = handle.binding.session_id.clone();
    let binding = handle.binding.clone();
    let _ = progress.send(Progress::Phase("streaming_turn"));
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let progress_for_events = progress.clone();
    let event_task = tokio::spawn(async move {
        let mut counts = BTreeMap::new();
        let mut text_bytes = 0_u64;
        while let Some(event) = event_rx.recv().await {
            let kind = event_kind(&event);
            *counts.entry(kind.to_string()).or_insert(0_u64) += 1;
            text_bytes = text_bytes.saturating_add(event_text_bytes(&event));
            let _ = progress_for_events.send(Progress::StreamEvent { kind });
        }
        (counts, text_bytes)
    });
    let mut request = ProviderTurnRequest {
        session_id: session_id.clone(),
        message_submission_id: None,
        required_runtime: None,
        text: PROMPT.to_string(),
        attachments: Vec::new(),
        workspace_root: workspace.display().to_string(),
        binding,
        safety: AgentSessionSafety::workspace_write_ask_on_risk(),
        runtime_resources: ProviderRuntimeResources::default(),
        execution_identity: None,
        event_sender: Some(event_tx),
        binding_update_sender: None,
        usage_execution_context: None,
        usage_counter_origin: vibex_core::AgentUsageCounterOrigin::Unknown,
        usage_event_sender: None,
    };
    request.execution_identity = provider.prepare_turn_execution(&handle, &request).await?;
    let turn = tokio::time::timeout(Duration::from_secs(90), provider.send_turn(handle, request))
        .await
        .map_err(|_| {
            VibexError::process(
                "acp_turn_timeout",
                "GPUI ACP turn exceeded the bounded timeout",
            )
        })??;
    let (event_counts, text_bytes) = tokio::time::timeout(Duration::from_secs(2), event_task)
        .await
        .map_err(|_| {
            VibexError::process(
                "acp_event_join_timeout",
                "GPUI ACP event collector did not stop",
            )
        })?
        .map_err(|_| {
            VibexError::process(
                "acp_event_join_failed",
                "GPUI ACP event collector stopped unexpectedly",
            )
        })?;
    report.session.completed = turn.completed;
    report.session.streamed_event_count = event_counts.values().copied().sum();
    report.session.streamed_text_bytes = text_bytes;
    report.session.event_counts = event_counts;

    let _ = progress.send(Progress::Phase("structured_error"));
    let invalid_profile = service.create_acp_profile(AcpProviderProfileCreateRequest {
        agent_id: Some(AgentId::parse("opencode")?),
        display_name: "GPUI ACP missing executable".to_string(),
        account_alias: None,
        preset_id: None,
        config: Some(opencode_config(
            root.join("missing-opencode").display().to_string(),
        )),
    })?;
    let invalid_result = provider
        .create_session(ProviderCreateRequest {
            session_id: VibexSessionId::new(),
            provider_profile_id: invalid_profile.id,
            model: None,
            workspace_root: workspace.display().to_string(),
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            runtime_resources: ProviderRuntimeResources::default(),
        })
        .await;
    let error = invalid_result.err().ok_or_else(|| {
        VibexError::validation(
            "acp_error_surface_missing",
            "Invalid ACP executable unexpectedly created a session",
        )
    })?;
    report.error_surface.exercised = true;
    report.error_surface.structured_error_code = Some(error.code.clone());
    Ok(())
}

fn empty_report() -> AcpLifecycleReport {
    AcpLifecycleReport {
        schema_version: "acp-lifecycle-run.v1",
        status: "running",
        provider: ProviderObservation {
            transport: "acp",
            raw_output_stored: false,
            ..ProviderObservation::default()
        },
        gpui: GpuiObservation {
            tokio_task_owned: true,
            response_text_stored: false,
            ..GpuiObservation::default()
        },
        session: SessionObservation::default(),
        error_surface: ErrorObservation {
            raw_provider_error_stored: false,
            ..ErrorObservation::default()
        },
        shutdown: ShutdownObservation::default(),
        failure: None,
        limitations: vec![
            "This Linux spike proves one real OpenCode ACP turn, not every Agent or provider.",
            "Response text is counted and discarded; semantic answer quality is outside this gate.",
        ],
    }
}

fn report_is_complete(report: &AcpLifecycleReport) -> bool {
    report.provider.real_process
        && report.provider.adapter_boundary
        && report.provider.transport == "acp"
        && !report.provider.raw_output_stored
        && report.gpui.tokio_task_owned
        && report.gpui.foreground_entity_updates > 0
        && !report.gpui.response_text_stored
        && report.session.created
        && report.session.native_id_present
        && report.session.completed
        && report.session.streamed_event_count > 0
        && report.session.streamed_text_bytes > 0
        && report.error_surface.exercised
        && report.error_surface.structured_error_code.is_some()
        && !report.error_surface.raw_provider_error_stored
        && report.shutdown.session_closed
        && report.shutdown.sweep_completed
        && report.shutdown.bounded
        && report.shutdown.temporary_root_removed
}

fn finalize_report(report: &mut AcpLifecycleReport) {
    if report.failure.is_some() {
        report.status = "failed";
    } else if report_is_complete(report) {
        report.status = "passed";
    } else {
        report.status = "failed";
        report.failure = Some(FailureObservation {
            code: "acp_evidence_incomplete".to_string(),
            category: "validation".to_string(),
        });
    }
}

fn opencode_config(command: String) -> AcpProviderConfig {
    AcpProviderConfig {
        command,
        args: vec!["acp".to_string()],
        env: Vec::new(),
        cwd_template: Some("{workspaceRoot}".to_string()),
        process_strategy: AcpProcessStrategy::default(),
        terminal_tools: false,
        terminal_auth: false,
        models: Vec::new(),
        modes: Vec::new(),
        features: vec![
            "streaming".to_string(),
            "tool_calls".to_string(),
            "permission_requests".to_string(),
            "reasoning".to_string(),
            "interrupt".to_string(),
        ],
        disabled_tools: Vec::new(),
    }
}

fn event_kind(event: &ProviderEvent) -> &'static str {
    match event.payload.kind() {
        TimelineItemKind::UserMessage => "user_message",
        TimelineItemKind::AgentMessageDelta => "agent_message_delta",
        TimelineItemKind::AgentMessage => "agent_message",
        TimelineItemKind::Reasoning => "reasoning",
        TimelineItemKind::Plan => "plan",
        TimelineItemKind::ToolCall => "tool_call",
        TimelineItemKind::Command => "command",
        TimelineItemKind::FileOperation => "file_operation",
        TimelineItemKind::WebSearch => "web_search",
        TimelineItemKind::TodoUpdate => "todo_update",
        TimelineItemKind::Collaboration => "collaboration",
        TimelineItemKind::ImageGeneration => "image_generation",
        TimelineItemKind::GitNotice => "git_notice",
        TimelineItemKind::SystemNotice => "system_notice",
        TimelineItemKind::PermissionRequest => "permission_request",
        TimelineItemKind::PermissionResolution => "permission_resolution",
        TimelineItemKind::ElicitationRequest => "elicitation_request",
        TimelineItemKind::ElicitationResolution => "elicitation_resolution",
        TimelineItemKind::Retry => "retry",
        TimelineItemKind::Error => "error",
    }
}

fn event_text_bytes(event: &ProviderEvent) -> u64 {
    match &event.payload {
        TimelinePayload::AgentMessageDelta(payload) => payload.text_delta.len() as u64,
        _ => 0,
    }
}

fn find_binary(name: &str) -> VibexResult<PathBuf> {
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join(name))
                .find(|candidate| candidate.is_file())
        })
        .ok_or_else(|| {
            VibexError::process(
                "acp_binary_missing",
                "Required ACP executable was not found",
            )
            .with_diagnostic("binary", name)
        })
}

fn command_version(binary: &Path) -> VibexResult<String> {
    let mut command = Command::new(binary);
    command.arg("--version");
    sanitize_inherited_appimage_environment(&mut command);
    let output = command.output().map_err(|error| {
        VibexError::process(
            "acp_version_probe_failed",
            "ACP executable version probe failed",
        )
        .with_diagnostic("error", error.kind().to_string())
    })?;
    if !output.status.success() {
        return Err(VibexError::process(
            "acp_version_probe_failed",
            "ACP executable version probe returned a failure status",
        ));
    }
    let version = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("unknown")
        .trim()
        .chars()
        .take(64)
        .collect::<String>();
    Ok(version)
}

fn write_report(path: &Path, report: &AcpLifecycleReport) -> VibexResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            VibexError::storage(
                "acp_report_directory_failed",
                "GPUI ACP report directory could not be created",
            )
            .with_diagnostic("error", error.kind().to_string())
        })?;
    }
    let bytes = serde_json::to_vec_pretty(report).map_err(|_| {
        VibexError::validation(
            "acp_report_encode_failed",
            "GPUI ACP report could not be encoded",
        )
    })?;
    fs::write(path, bytes).map_err(|error| {
        VibexError::storage(
            "acp_report_write_failed",
            "GPUI ACP report could not be written",
        )
        .with_diagnostic("error", error.kind().to_string())
    })
}

fn failure_observation(error: &VibexError) -> FailureObservation {
    FailureObservation {
        code: error.code.clone(),
        category: format!("{:?}", error.category).to_ascii_lowercase(),
    }
}

fn hold_ms() -> u64 {
    std::env::var("VIBEX_SPIKE_HOLD_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value <= 10_000)
        .unwrap_or(DEFAULT_HOLD_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_requires_real_stream_error_and_shutdown_evidence() {
        let mut report = empty_report();
        assert!(!report_is_complete(&report));
        report.provider.real_process = true;
        report.provider.adapter_boundary = true;
        report.gpui.foreground_entity_updates = 1;
        report.session.created = true;
        report.session.native_id_present = true;
        report.session.completed = true;
        report.session.streamed_event_count = 1;
        report.session.streamed_text_bytes = 1;
        report.error_surface.exercised = true;
        report.error_surface.structured_error_code = Some("structured_error".to_string());
        report.shutdown.session_closed = true;
        report.shutdown.sweep_completed = true;
        report.shutdown.bounded = true;
        report.shutdown.temporary_root_removed = true;
        assert!(report_is_complete(&report));
    }

    #[test]
    fn report_serialization_retains_no_prompt_or_response_text_field() {
        let serialized = serde_json::to_string(&empty_report()).unwrap();
        let value = serde_json::from_str::<serde_json::Value>(&serialized).unwrap();
        assert!(!serialized.contains(PROMPT));
        assert!(!serialized.contains("workspaceRoot"));
        assert!(!serialized.contains("nativeSessionId"));
        assert!(value.get("responseText").is_none());
        assert_eq!(value["gpui"]["responseTextStored"], false);
    }
}

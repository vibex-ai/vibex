use vibex_core::{
    AgentSessionState, CreateAgentSessionRequest, ErrorCategory, RedactedDiagnostic, ScheduledTask,
    ScheduledTaskDailySchedule, ScheduledTaskId, ScheduledTaskIntervalSchedule, ScheduledTaskRun,
    ScheduledTaskRunId, ScheduledTaskRunStatus, ScheduledTaskRunUpdateRequest,
    ScheduledTaskSchedule, ScheduledTaskStatus, SendAgentMessageRequest, VibexError, VibexResult,
    VibexSessionId,
};
use vibex_db::ScheduledTaskRepository;

use crate::manager::AgentManager;

pub const DEFAULT_SCHEDULED_TASK_DUE_LIMIT: u32 = 16;
pub const DEFAULT_SCHEDULED_TASK_STALE_AFTER_MS: i64 = 15 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledTaskTickResult {
    pub checked: u32,
    pub claimed: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub skipped: u32,
    pub recovered: u32,
    pub outcomes: Vec<ScheduledTaskRunOutcome>,
}

impl ScheduledTaskTickResult {
    fn empty() -> Self {
        Self {
            checked: 0,
            claimed: 0,
            succeeded: 0,
            failed: 0,
            skipped: 0,
            recovered: 0,
            outcomes: Vec::new(),
        }
    }

    fn push_outcome(&mut self, outcome: ScheduledTaskRunOutcome) {
        match outcome.status {
            ScheduledTaskRunStatus::Succeeded => self.succeeded += 1,
            ScheduledTaskRunStatus::Failed => self.failed += 1,
            ScheduledTaskRunStatus::Skipped | ScheduledTaskRunStatus::Canceled => self.skipped += 1,
            ScheduledTaskRunStatus::Queued | ScheduledTaskRunStatus::Running => {}
        }
        self.outcomes.push(outcome);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledTaskRunOutcome {
    pub task_id: ScheduledTaskId,
    pub run_id: ScheduledTaskRunId,
    pub session_id: Option<VibexSessionId>,
    pub status: ScheduledTaskRunStatus,
    pub error_code: Option<String>,
}

pub struct ScheduledTaskRunner<'a> {
    manager: &'a AgentManager,
    due_limit: u32,
    stale_after_ms: i64,
}

impl<'a> ScheduledTaskRunner<'a> {
    pub fn new(manager: &'a AgentManager) -> Self {
        Self {
            manager,
            due_limit: DEFAULT_SCHEDULED_TASK_DUE_LIMIT,
            stale_after_ms: DEFAULT_SCHEDULED_TASK_STALE_AFTER_MS,
        }
    }

    pub const fn with_due_limit(mut self, due_limit: u32) -> Self {
        self.due_limit = due_limit;
        self
    }

    pub const fn with_stale_after_ms(mut self, stale_after_ms: i64) -> Self {
        self.stale_after_ms = stale_after_ms;
        self
    }

    pub async fn tick(&self, now_ms: i64) -> VibexResult<ScheduledTaskTickResult> {
        let recovered = self.recover_stale_runs(now_ms)?;
        let mut result = ScheduledTaskTickResult::empty();
        result.recovered = recovered.len() as u32;
        for run in recovered {
            result.push_outcome(ScheduledTaskRunOutcome {
                task_id: run.task_id,
                run_id: run.id,
                session_id: run.session_id,
                status: run.status,
                error_code: run.error_code,
            });
        }

        let conn = self.manager.open_migrated()?;
        let due = ScheduledTaskRepository::list_due(&conn, now_ms, Some(self.due_limit))?;
        result.checked = due.len() as u32;
        drop(conn);

        for task in due {
            let mut conn = self.manager.open_migrated()?;
            let Some((claimed_task, run)) =
                ScheduledTaskRepository::claim_due(&mut conn, &task.id, now_ms)?
            else {
                continue;
            };
            result.claimed += 1;
            drop(conn);

            let outcome = self.execute_claimed_task(claimed_task, run, now_ms).await?;
            result.push_outcome(outcome);
        }

        Ok(result)
    }

    pub fn recover_stale_runs(&self, now_ms: i64) -> VibexResult<Vec<ScheduledTaskRun>> {
        let before_ms = now_ms.saturating_sub(self.stale_after_ms);
        let conn = self.manager.open_migrated()?;
        let stale_runs = ScheduledTaskRepository::list_stale_running_runs(
            &conn,
            before_ms,
            Some(self.due_limit),
        )?;
        drop(conn);

        let mut recovered = Vec::with_capacity(stale_runs.len());
        for run in stale_runs {
            let task = self.load_task(&run.task_id)?;
            let (next_run_at_ms, task_status, mut diagnostics) =
                next_task_state_after_run(task.as_ref(), run.due_at_ms, now_ms);
            diagnostics.push(RedactedDiagnostic {
                key: "recoveredBy".to_string(),
                value: "scheduler_tick".to_string(),
            });

            let conn = self.manager.open_migrated()?;
            let updated = ScheduledTaskRepository::update_run(
                &conn,
                ScheduledTaskRunUpdateRequest {
                    id: run.id,
                    status: Some(ScheduledTaskRunStatus::Failed),
                    session_id: None,
                    clear_session_id: false,
                    started_at_ms: None,
                    clear_started_at_ms: false,
                    ended_at_ms: Some(now_ms),
                    clear_ended_at_ms: false,
                    attempt: None,
                    error_code: Some("scheduler/recovered_stale_run".to_string()),
                    clear_error_code: false,
                    error_message: Some(
                        "scheduled task run was recovered after restart".to_string(),
                    ),
                    clear_error_message: false,
                    redacted_diagnostics: Some(diagnostics),
                },
            )?;
            if let Some(task) = task {
                ScheduledTaskRepository::mark_task_after_run(
                    &conn,
                    &task.id,
                    task_status,
                    next_run_at_ms,
                    now_ms,
                )?;
            }
            recovered.push(updated);
        }
        Ok(recovered)
    }

    async fn execute_claimed_task(
        &self,
        task: ScheduledTask,
        run: ScheduledTaskRun,
        now_ms: i64,
    ) -> VibexResult<ScheduledTaskRunOutcome> {
        let runtime = self.manager.resolve_initial_runtime_selection(
            task.provider_profile_id.clone(),
            task.provider_kind,
            task.project_id.as_ref(),
            task.workspace_id.as_ref(),
        )?;
        let session = match self
            .manager
            .create_session(CreateAgentSessionRequest {
                runtime: runtime.clone(),
                workspace_root: task.workspace_root.clone(),
                workspace_mode: task.workspace_mode,
                title: Some(format!("Scheduled: {}", task.title)),
                safety: Some(task.safety.clone()),
            })
            .await
        {
            Ok(session) => session,
            Err(err) => {
                let completed = self.complete_run_with_error(
                    &task,
                    run,
                    ScheduledTaskRunStatus::Failed,
                    now_ms,
                    &err,
                )?;
                return Ok(outcome_from_run(completed));
            }
        };

        let conn = self.manager.open_migrated()?;
        let mut run = ScheduledTaskRepository::update_run(
            &conn,
            ScheduledTaskRunUpdateRequest {
                id: run.id,
                status: Some(ScheduledTaskRunStatus::Running),
                session_id: Some(session.id.clone()),
                clear_session_id: false,
                started_at_ms: None,
                clear_started_at_ms: false,
                ended_at_ms: None,
                clear_ended_at_ms: false,
                attempt: None,
                error_code: None,
                clear_error_code: false,
                error_message: None,
                clear_error_message: false,
                redacted_diagnostics: None,
            },
        )?;
        drop(conn);

        match self
            .manager
            .send_message(SendAgentMessageRequest {
                session_id: session.id.clone(),
                message_idempotency_key: format!(
                    "scheduled-task:{}:{}",
                    task.id.as_str(),
                    run.id.as_str()
                ),
                desired_runtime: runtime.clone(),
                text: task.prompt.clone(),
                attachments: Vec::new(),
                reasoning_effort: runtime.reasoning_effort.clone(),
                correlation_id: None,
            })
            .await
        {
            Ok(_) => {
                let session = self.manager.get_session(&session.id).await?;
                if session.state == AgentSessionState::NeedsInput {
                    let err = VibexError::new(
                        ErrorCategory::Permission,
                        "scheduler/permission_required",
                        "scheduled task run requires user input or permission",
                    );
                    run = self.complete_run_with_error(
                        &task,
                        run,
                        ScheduledTaskRunStatus::Skipped,
                        now_ms,
                        &err,
                    )?;
                } else {
                    run = self.complete_run_success(&task, run, now_ms)?;
                }
            }
            Err(err) => {
                run = self.complete_run_with_error(
                    &task,
                    run,
                    ScheduledTaskRunStatus::Failed,
                    now_ms,
                    &err,
                )?;
            }
        }

        Ok(outcome_from_run(run))
    }

    fn complete_run_success(
        &self,
        task: &ScheduledTask,
        run: ScheduledTaskRun,
        now_ms: i64,
    ) -> VibexResult<ScheduledTaskRun> {
        match next_task_state_after_run(Some(task), run.due_at_ms, now_ms) {
            (next_run_at_ms, task_status, diagnostics) if diagnostics.is_empty() => {
                let conn = self.manager.open_migrated()?;
                let run = ScheduledTaskRepository::update_run(
                    &conn,
                    ScheduledTaskRunUpdateRequest {
                        id: run.id,
                        status: Some(ScheduledTaskRunStatus::Succeeded),
                        session_id: run.session_id,
                        clear_session_id: false,
                        started_at_ms: None,
                        clear_started_at_ms: false,
                        ended_at_ms: Some(now_ms),
                        clear_ended_at_ms: false,
                        attempt: None,
                        error_code: None,
                        clear_error_code: true,
                        error_message: None,
                        clear_error_message: true,
                        redacted_diagnostics: Some(Vec::new()),
                    },
                )?;
                ScheduledTaskRepository::mark_task_after_run(
                    &conn,
                    &task.id,
                    task_status,
                    next_run_at_ms,
                    now_ms,
                )?;
                Ok(run)
            }
            (_, task_status, diagnostics) => {
                let err = VibexError::validation(
                    diagnostics
                        .iter()
                        .find(|entry| entry.key == "scheduleErrorCode")
                        .map(|entry| entry.value.as_str())
                        .unwrap_or("scheduler/schedule_next_run_failed"),
                    "scheduled task next run could not be computed",
                );
                let conn = self.manager.open_migrated()?;
                ScheduledTaskRepository::mark_task_after_run(
                    &conn,
                    &task.id,
                    task_status,
                    None,
                    now_ms,
                )?;
                ScheduledTaskRepository::update_run(
                    &conn,
                    ScheduledTaskRunUpdateRequest {
                        id: run.id,
                        status: Some(ScheduledTaskRunStatus::Failed),
                        session_id: run.session_id,
                        clear_session_id: false,
                        started_at_ms: None,
                        clear_started_at_ms: false,
                        ended_at_ms: Some(now_ms),
                        clear_ended_at_ms: false,
                        attempt: None,
                        error_code: Some(err.code),
                        clear_error_code: false,
                        error_message: Some(err.message),
                        clear_error_message: false,
                        redacted_diagnostics: Some(diagnostics),
                    },
                )
            }
        }
    }

    fn complete_run_with_error(
        &self,
        task: &ScheduledTask,
        run: ScheduledTaskRun,
        status: ScheduledTaskRunStatus,
        now_ms: i64,
        err: &VibexError,
    ) -> VibexResult<ScheduledTaskRun> {
        let (next_run_at_ms, task_status, mut diagnostics) =
            next_task_state_after_run(Some(task), run.due_at_ms, now_ms);
        diagnostics.push(RedactedDiagnostic {
            key: "errorCategory".to_string(),
            value: format!("{:?}", err.category),
        });
        diagnostics.extend(err.diagnostics.clone());

        let conn = self.manager.open_migrated()?;
        let run = ScheduledTaskRepository::update_run(
            &conn,
            ScheduledTaskRunUpdateRequest {
                id: run.id,
                status: Some(status),
                session_id: run.session_id,
                clear_session_id: false,
                started_at_ms: None,
                clear_started_at_ms: false,
                ended_at_ms: Some(now_ms),
                clear_ended_at_ms: false,
                attempt: None,
                error_code: Some(err.code.clone()),
                clear_error_code: false,
                error_message: Some(err.message.clone()),
                clear_error_message: false,
                redacted_diagnostics: Some(diagnostics),
            },
        )?;
        ScheduledTaskRepository::mark_task_after_run(
            &conn,
            &task.id,
            task_status,
            next_run_at_ms,
            now_ms,
        )?;
        Ok(run)
    }

    fn load_task(&self, task_id: &ScheduledTaskId) -> VibexResult<Option<ScheduledTask>> {
        let conn = self.manager.open_migrated()?;
        ScheduledTaskRepository::get(&conn, task_id)
    }
}

pub fn next_run_after(
    schedule: &ScheduledTaskSchedule,
    completed_due_at_ms: i64,
    completed_at_ms: i64,
) -> VibexResult<Option<i64>> {
    match schedule {
        ScheduledTaskSchedule::OneShot(_) => Ok(None),
        ScheduledTaskSchedule::Interval(schedule) => {
            next_interval_run_after(schedule, completed_due_at_ms, completed_at_ms)
        }
        ScheduledTaskSchedule::Daily(schedule) => next_daily_run_after(schedule, completed_at_ms),
    }
}

fn next_task_state_after_run(
    task: Option<&ScheduledTask>,
    completed_due_at_ms: i64,
    completed_at_ms: i64,
) -> (Option<i64>, ScheduledTaskStatus, Vec<RedactedDiagnostic>) {
    let Some(task) = task else {
        return (None, ScheduledTaskStatus::Paused, Vec::new());
    };
    match next_run_after(&task.schedule, completed_due_at_ms, completed_at_ms) {
        Ok(next_run_at_ms) => (
            next_run_at_ms,
            if next_run_at_ms.is_some() {
                ScheduledTaskStatus::Active
            } else {
                ScheduledTaskStatus::Paused
            },
            Vec::new(),
        ),
        Err(err) => (
            None,
            ScheduledTaskStatus::Paused,
            vec![
                RedactedDiagnostic {
                    key: "scheduleErrorCode".to_string(),
                    value: err.code,
                },
                RedactedDiagnostic {
                    key: "scheduleErrorMessage".to_string(),
                    value: err.message,
                },
            ],
        ),
    }
}

fn next_interval_run_after(
    schedule: &ScheduledTaskIntervalSchedule,
    completed_due_at_ms: i64,
    completed_at_ms: i64,
) -> VibexResult<Option<i64>> {
    if schedule.every_seconds == 0 {
        return Err(VibexError::validation(
            "scheduler/invalid_interval_schedule",
            "interval schedule everySeconds must be greater than zero",
        ));
    }

    let step_ms = i64::from(schedule.every_seconds) * 1000;
    let mut candidate = completed_due_at_ms.saturating_add(step_ms);
    if candidate < schedule.start_at_ms {
        candidate = schedule.start_at_ms;
    }
    while candidate <= completed_at_ms {
        candidate = candidate.saturating_add(step_ms);
        if schedule
            .end_at_ms
            .is_some_and(|end_at_ms| candidate > end_at_ms)
        {
            return Ok(None);
        }
    }
    if schedule
        .end_at_ms
        .is_some_and(|end_at_ms| candidate > end_at_ms)
    {
        return Ok(None);
    }
    Ok(Some(candidate))
}

fn next_daily_run_after(
    schedule: &ScheduledTaskDailySchedule,
    completed_at_ms: i64,
) -> VibexResult<Option<i64>> {
    if schedule.local_time_minutes >= 24 * 60 {
        return Err(VibexError::validation(
            "scheduler/invalid_daily_schedule",
            "daily schedule localTimeMinutes must be less than 1440",
        ));
    }
    let offset_minutes = timezone_offset_minutes(&schedule.timezone).ok_or_else(|| {
        VibexError::validation(
            "scheduler/unsupported_timezone",
            "daily scheduled task timezone is not supported by the local scheduler",
        )
        .with_diagnostic("timezone", &schedule.timezone)
    })?;

    let candidate = next_daily_candidate(
        completed_at_ms,
        i64::from(schedule.local_time_minutes),
        offset_minutes,
    );
    let candidate = if candidate < schedule.start_at_ms {
        next_daily_candidate(
            schedule.start_at_ms.saturating_sub(1),
            i64::from(schedule.local_time_minutes),
            offset_minutes,
        )
    } else {
        candidate
    };

    if schedule
        .end_at_ms
        .is_some_and(|end_at_ms| candidate > end_at_ms)
    {
        return Ok(None);
    }
    Ok(Some(candidate))
}

fn next_daily_candidate(after_ms: i64, local_time_minutes: i64, offset_minutes: i64) -> i64 {
    const MINUTE_MS: i64 = 60 * 1000;
    const DAY_MS: i64 = 24 * 60 * MINUTE_MS;

    let offset_ms = offset_minutes * MINUTE_MS;
    let local_after_ms = after_ms.saturating_add(offset_ms);
    let local_day_start = local_after_ms.div_euclid(DAY_MS) * DAY_MS;
    let mut local_candidate = local_day_start + local_time_minutes * MINUTE_MS;
    if local_candidate <= local_after_ms {
        local_candidate += DAY_MS;
    }
    local_candidate - offset_ms
}

fn timezone_offset_minutes(timezone: &str) -> Option<i64> {
    match timezone {
        "UTC" | "Etc/UTC" | "Z" => Some(0),
        "Asia/Shanghai" => Some(8 * 60),
        _ => None,
    }
}

fn outcome_from_run(run: ScheduledTaskRun) -> ScheduledTaskRunOutcome {
    ScheduledTaskRunOutcome {
        task_id: run.task_id,
        run_id: run.id,
        session_id: run.session_id,
        status: run.status,
        error_code: run.error_code,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        AgentProvider, ProviderCreateRequest, ProviderEvent, ProviderSessionHandle,
        ProviderTurnRequest, ProviderTurnResult,
    };
    use vibex_core::{
        FetchTimelineRequest, PermissionRequest, ProviderBinding, ProviderCapabilities,
        ProviderKind, RequestId, ScheduledTaskCreateRequest, ScheduledTaskIntervalSchedule,
        ScheduledTaskOneShotSchedule, TimelinePayload, TimelineRedactionState, TimelineSource,
        VibexResult, WorkspaceMode, unix_timestamp_ms,
    };
    use vibex_db::{apply_migrations, open_database};

    use super::*;

    struct TestAgentProvider;

    fn test_capabilities() -> ProviderCapabilities {
        ProviderCapabilities {
            kind: ProviderKind::Acp,
            version: vibex_core::ProviderVersionInfo {
                provider_version: Some("test".to_string()),
                adapter_version: env!("CARGO_PKG_VERSION").to_string(),
                capability_source: "test_provider".to_string(),
            },
            streaming: true,
            session_persistence: true,
            session_listing: true,
            dynamic_modes: false,
            model_list: false,
            mcp_servers: false,
            slash_commands: false,
            skills: false,
            reasoning_stream: false,
            plan: false,
            tool_invocations: false,
            permission_requests: true,
            elicitation: false,
            image_input: false,
            file_attachments: false,
            fork_rollback: false,
            interrupt: false,
            terminal_tools: false,
            terminal_auth: false,
            terminal_activity_hooks: false,
        }
    }

    #[async_trait::async_trait]
    impl AgentProvider for TestAgentProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Acp
        }

        fn capabilities(&self) -> ProviderCapabilities {
            test_capabilities()
        }

        async fn create_session(
            &self,
            request: ProviderCreateRequest,
        ) -> VibexResult<ProviderSessionHandle> {
            Ok(ProviderSessionHandle {
                binding: test_binding(request.session_id, request.provider_profile_id),
                capabilities: self.capabilities(),
            })
        }

        async fn resume_session(
            &self,
            binding: ProviderBinding,
        ) -> VibexResult<ProviderSessionHandle> {
            Ok(ProviderSessionHandle {
                binding,
                capabilities: self.capabilities(),
            })
        }

        async fn prepare_turn_execution(
            &self,
            _handle: &ProviderSessionHandle,
            request: &ProviderTurnRequest,
        ) -> VibexResult<Option<crate::adapter::ProviderTurnExecutionIdentity>> {
            Ok(request.execution_identity.clone())
        }

        async fn send_turn(
            &self,
            _handle: ProviderSessionHandle,
            request: ProviderTurnRequest,
        ) -> VibexResult<ProviderTurnResult> {
            if request.text.to_ascii_lowercase().contains("error") {
                return Err(VibexError::provider(
                    "test_provider_error",
                    "test provider was asked to fail",
                ));
            }

            if request.text.to_ascii_lowercase().contains("permission") {
                return Ok(ProviderTurnResult {
                    events: vec![ProviderEvent {
                        source: TimelineSource::Provider,
                        payload: TimelinePayload::PermissionRequest(PermissionRequest {
                            id: RequestId::new(),
                            session_id: request.session_id,
                            project_id: None,
                            workspace_id: None,
                            provider_request_id: Some("test-permission".to_string()),
                            risk_category: vibex_core::PermissionRiskCategory::Command,
                            title: "Run test command".to_string(),
                            details: vec![vibex_core::PermissionActionDetail {
                                label: "command".to_string(),
                                value: "echo test-permission".to_string(),
                            }],
                            allowed_responses: vec![
                                vibex_core::PermissionResponseKind::Approve,
                                vibex_core::PermissionResponseKind::Deny,
                            ],
                            response_options: Vec::new(),
                            status: vibex_core::PermissionRequestStatus::Pending,
                            requested_at_ms: unix_timestamp_ms(),
                            expires_at_ms: None,
                        }),
                        provider_correlation_id: Some("test-permission".to_string()),
                        redaction_state: TimelineRedactionState::None,
                        session_title: None,
                    }],
                    binding_update: None,
                    completed: false,
                });
            }

            Ok(ProviderTurnResult {
                events: vec![ProviderEvent::agent(TimelinePayload::AgentMessage(
                    vibex_core::AgentMessagePayload {
                        text: format!("Test response to: {}", request.text),
                        is_final: true,
                    },
                ))],
                binding_update: None,
                completed: true,
            })
        }
    }

    fn test_binding(
        session_id: vibex_core::VibexSessionId,
        provider_profile_id: vibex_core::ProviderProfileId,
    ) -> ProviderBinding {
        let now = unix_timestamp_ms();
        ProviderBinding {
            session_id,
            provider_kind: ProviderKind::Acp,
            auth_source: vibex_core::RuntimeAuthSource::provider_profile(provider_profile_id),
            auth_source_revision: 1,
            native: vibex_core::ProviderNativeBinding {
                native_session_id: Some(format!("test-session-{now}")),
                native_thread_id: None,
                native_resume_token: None,
                session_config_state: None,
                redacted_metadata: Vec::new(),
            },
            created_at_ms: now,
            updated_at_ms: now,
        }
    }

    #[test]
    fn next_run_after_handles_interval_and_daily_schedules() {
        let interval = ScheduledTaskSchedule::Interval(ScheduledTaskIntervalSchedule {
            every_seconds: 60,
            start_at_ms: 1_000,
            end_at_ms: Some(300_000),
        });
        assert_eq!(
            next_run_after(&interval, 1_000, 1_000).unwrap(),
            Some(61_000)
        );
        assert_eq!(
            next_run_after(&interval, 1_000, 130_000).unwrap(),
            Some(181_000)
        );

        let daily = ScheduledTaskSchedule::Daily(ScheduledTaskDailySchedule {
            local_time_minutes: 9 * 60,
            timezone: "UTC".to_string(),
            start_at_ms: 0,
            end_at_ms: None,
        });
        assert_eq!(
            next_run_after(&daily, 0, 8 * 60 * 60 * 1000).unwrap(),
            Some(9 * 60 * 60 * 1000)
        );
        assert_eq!(
            next_run_after(&daily, 0, 10 * 60 * 60 * 1000).unwrap(),
            Some((24 + 9) * 60 * 60 * 1000)
        );
    }

    #[test]
    fn next_run_after_rejects_unsupported_daily_timezone() {
        let daily = ScheduledTaskSchedule::Daily(ScheduledTaskDailySchedule {
            local_time_minutes: 9 * 60,
            timezone: "Mars/Base".to_string(),
            start_at_ms: 0,
            end_at_ms: None,
        });
        let err = next_run_after(&daily, 0, 0).unwrap_err();
        assert_eq!(err.code, "scheduler/unsupported_timezone");
    }

    #[tokio::test]
    async fn scheduled_one_shot_runs_once_through_test_provider() {
        let db_path = temp_db_path("scheduler-one-shot");
        let manager = test_manager(&db_path);
        let task = insert_task(
            &db_path,
            "One shot",
            "scheduled hello",
            ScheduledTaskSchedule::OneShot(ScheduledTaskOneShotSchedule { run_at_ms: 1_000 }),
            Some(1_000),
        );

        let runner = ScheduledTaskRunner::new(&manager).with_stale_after_ms(10_000);
        let first_result = runner.tick(1_000).await.unwrap();
        assert_eq!(first_result.checked, 1);
        assert_eq!(first_result.claimed, 1);
        assert_eq!(first_result.succeeded, 1);
        assert_eq!(first_result.outcomes[0].task_id, task.id);
        assert!(first_result.outcomes[0].session_id.is_some());

        let result = runner.tick(1_000).await.unwrap();
        assert_eq!(result.checked, 0);
        assert_eq!(result.claimed, 0);

        let conn = open_database(&db_path).unwrap();
        let stored = ScheduledTaskRepository::get(&conn, &task.id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, ScheduledTaskStatus::Paused);
        assert_eq!(stored.next_run_at_ms, None);
        let runs = ScheduledTaskRepository::list_runs(
            &conn,
            vibex_core::ScheduledTaskRunListRequest {
                task_id: Some(task.id.clone()),
                session_id: None,
                status: Some(ScheduledTaskRunStatus::Succeeded),
                limit: Some(10),
            },
        )
        .unwrap();
        assert_eq!(runs.len(), 1);

        let page = manager
            .fetch_timeline(FetchTimelineRequest {
                session_id: result
                    .outcomes
                    .first()
                    .and_then(|outcome| outcome.session_id.clone())
                    .or_else(|| first_result.outcomes[0].session_id.clone())
                    .unwrap_or_else(|| runs[0].session_id.clone().unwrap()),
                after_sequence: Some(0),
                limit: 100,
            })
            .await
            .unwrap();
        assert!(page.items.iter().any(|item| {
            matches!(
                &item.payload,
                TimelinePayload::UserMessage(payload) if payload.text == "scheduled hello"
            )
        }));

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn scheduled_interval_advances_next_run_after_success() {
        let db_path = temp_db_path("scheduler-interval");
        let manager = test_manager(&db_path);
        let task = insert_task(
            &db_path,
            "Interval",
            "interval hello",
            ScheduledTaskSchedule::Interval(ScheduledTaskIntervalSchedule {
                every_seconds: 60,
                start_at_ms: 1_000,
                end_at_ms: None,
            }),
            Some(1_000),
        );

        let result = ScheduledTaskRunner::new(&manager)
            .tick(1_000)
            .await
            .unwrap();
        assert_eq!(result.succeeded, 1);

        let conn = open_database(&db_path).unwrap();
        let stored = ScheduledTaskRepository::get(&conn, &task.id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, ScheduledTaskStatus::Active);
        assert_eq!(stored.next_run_at_ms, Some(61_000));

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn scheduled_prompt_failure_records_ambiguous_failed_run() {
        let db_path = temp_db_path("scheduler-failure");
        let manager = test_manager(&db_path);
        let task = insert_task(
            &db_path,
            "Fail once",
            "please error",
            ScheduledTaskSchedule::OneShot(ScheduledTaskOneShotSchedule { run_at_ms: 1_000 }),
            Some(1_000),
        );

        let result = ScheduledTaskRunner::new(&manager)
            .tick(1_000)
            .await
            .unwrap();
        assert_eq!(result.failed, 1);
        assert_eq!(
            result.outcomes[0].error_code.as_deref(),
            Some("message_submission_prompt_dispatch_ambiguous")
        );

        let conn = open_database(&db_path).unwrap();
        let runs = ScheduledTaskRepository::list_runs(
            &conn,
            vibex_core::ScheduledTaskRunListRequest {
                task_id: Some(task.id.clone()),
                session_id: None,
                status: Some(ScheduledTaskRunStatus::Failed),
                limit: Some(10),
            },
        )
        .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].error_code.as_deref(),
            Some("message_submission_prompt_dispatch_ambiguous")
        );
        assert_eq!(
            ScheduledTaskRepository::get(&conn, &task.id)
                .unwrap()
                .unwrap()
                .status,
            ScheduledTaskStatus::Paused
        );

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn scheduled_permission_request_records_skipped_run() {
        let db_path = temp_db_path("scheduler-permission");
        let manager = test_manager(&db_path);
        let task = insert_task(
            &db_path,
            "Needs permission",
            "please request permission",
            ScheduledTaskSchedule::OneShot(ScheduledTaskOneShotSchedule { run_at_ms: 1_000 }),
            Some(1_000),
        );

        let result = ScheduledTaskRunner::new(&manager)
            .tick(1_000)
            .await
            .unwrap();
        assert_eq!(result.skipped, 1);
        assert_eq!(
            result.outcomes[0].error_code.as_deref(),
            Some("scheduler/permission_required")
        );

        let conn = open_database(&db_path).unwrap();
        let runs = ScheduledTaskRepository::list_runs(
            &conn,
            vibex_core::ScheduledTaskRunListRequest {
                task_id: Some(task.id.clone()),
                session_id: None,
                status: Some(ScheduledTaskRunStatus::Skipped),
                limit: Some(10),
            },
        )
        .unwrap();
        assert_eq!(runs.len(), 1);
        assert!(runs[0].session_id.is_some());
        let session = manager
            .get_session(runs[0].session_id.as_ref().unwrap())
            .await
            .unwrap();
        assert_eq!(session.state, AgentSessionState::NeedsInput);

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn stale_running_run_recovers_and_reschedules_interval() {
        let db_path = temp_db_path("scheduler-recovery");
        let manager = test_manager(&db_path);
        let task = insert_task(
            &db_path,
            "Recover interval",
            "recover",
            ScheduledTaskSchedule::Interval(ScheduledTaskIntervalSchedule {
                every_seconds: 60,
                start_at_ms: 1_000,
                end_at_ms: None,
            }),
            Some(1_000),
        );

        let mut conn = open_database(&db_path).unwrap();
        let (_task, run) = ScheduledTaskRepository::claim_due(&mut conn, &task.id, 1_000)
            .unwrap()
            .unwrap();
        assert_eq!(run.status, ScheduledTaskRunStatus::Running);
        drop(conn);

        let result = ScheduledTaskRunner::new(&manager)
            .with_stale_after_ms(1)
            .tick(2_000)
            .await
            .unwrap();
        assert_eq!(result.recovered, 1);
        assert_eq!(result.failed, 1);
        assert_eq!(
            result.outcomes[0].error_code.as_deref(),
            Some("scheduler/recovered_stale_run")
        );

        let conn = open_database(&db_path).unwrap();
        let stored = ScheduledTaskRepository::get(&conn, &task.id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, ScheduledTaskStatus::Active);
        assert_eq!(stored.next_run_at_ms, Some(61_000));

        cleanup_db(db_path);
    }

    fn insert_task(
        db_path: &std::path::Path,
        title: &str,
        prompt: &str,
        schedule: ScheduledTaskSchedule,
        next_run_at_ms: Option<i64>,
    ) -> ScheduledTask {
        let mut conn = open_database(db_path).unwrap();
        apply_migrations(&mut conn).unwrap();
        ScheduledTaskRepository::create(
            &conn,
            ScheduledTaskCreateRequest {
                title: title.to_string(),
                prompt: prompt.to_string(),
                project_id: None,
                workspace_id: None,
                workspace_root: format!("/tmp/vibex-agent-scheduler-{title}"),
                workspace_mode: WorkspaceMode::CurrentCheckout,
                provider_kind: ProviderKind::Codex,
                provider_profile_id: None,
                schedule,
                safety: None,
                next_run_at_ms,
            },
        )
        .unwrap()
    }

    fn test_manager(path: &std::path::Path) -> crate::test_support::TestRuntimeHarness {
        crate::test_support::TestRuntimeHarness::new(
            path,
            vibex_core::AgentId::parse("codex").unwrap(),
            Arc::new(TestAgentProvider),
        )
    }

    fn temp_db_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "vibex-agent-scheduler-{label}-{}.db",
            RequestId::new().as_str()
        ))
    }

    fn cleanup_db(path: std::path::PathBuf) {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }
}

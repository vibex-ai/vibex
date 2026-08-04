use std::collections::{HashMap, HashSet, VecDeque};

use vibex_core::{
    AgentSessionSafety, AgentSessionState, AutomationEdgeConditionKind, AutomationGraph,
    AutomationGraphStatus, AutomationNode, AutomationNodeConfig, AutomationNodeId, AutomationRun,
    AutomationRunCancelRequest, AutomationRunCreateRequest, AutomationRunResumeRequest,
    AutomationRunStartRequest, AutomationRunStatus, AutomationRunStep,
    AutomationRunStepCreateRequest, AutomationRunStepId, AutomationRunStepListRequest,
    AutomationRunStepStatus, AutomationRunStepUpdateRequest, AutomationRunUpdateRequest,
    CreateAgentSessionRequest, PermissionRequest, PermissionRequestStatus, RedactedDiagnostic,
    SendAgentMessageRequest, VibexError, VibexResult, unix_timestamp_ms,
};
use vibex_db::{AutomationGraphRepository, PermissionRepository};

use crate::manager::AgentManager;

pub const DEFAULT_AUTOMATION_STALE_AFTER_MS: i64 = 15 * 60 * 1000;

pub struct AutomationGraphRunner<'a> {
    manager: &'a AgentManager,
    stale_after_ms: i64,
}

impl<'a> AutomationGraphRunner<'a> {
    pub const fn new(manager: &'a AgentManager) -> Self {
        Self {
            manager,
            stale_after_ms: DEFAULT_AUTOMATION_STALE_AFTER_MS,
        }
    }

    pub const fn with_stale_after_ms(mut self, stale_after_ms: i64) -> Self {
        self.stale_after_ms = stale_after_ms;
        self
    }

    pub async fn start_graph(
        &self,
        request: AutomationRunStartRequest,
    ) -> VibexResult<AutomationRun> {
        let now_ms = request.now_ms.unwrap_or_else(unix_timestamp_ms);
        let graph = self.load_executable_graph(&request.graph_id)?;
        validate_graph_for_runtime(&graph)?;

        let conn = self.manager.open_migrated()?;
        let run = AutomationGraphRepository::create_run(
            &conn,
            AutomationRunCreateRequest {
                graph_id: graph.id.clone(),
                status: AutomationRunStatus::Running,
                trigger: request.trigger,
                scheduled_task_id: request.scheduled_task_id,
                session_id: None,
                started_at_ms: Some(now_ms),
                ended_at_ms: None,
                error_code: None,
                error_message: None,
                redacted_diagnostics: Vec::new(),
            },
        )?;
        drop(conn);

        self.execute_ready_nodes(graph, run, now_ms).await
    }

    pub async fn resume_run(
        &self,
        request: AutomationRunResumeRequest,
    ) -> VibexResult<AutomationRun> {
        let now_ms = request.now_ms.unwrap_or_else(unix_timestamp_ms);
        let conn = self.manager.open_migrated()?;
        let run = AutomationGraphRepository::get_run(&conn, &request.run_id)?.ok_or_else(|| {
            VibexError::storage(
                "automation_run_not_found",
                "automation graph run was not found",
            )
            .with_diagnostic("automationRunId", request.run_id.as_str())
        })?;
        drop(conn);

        if run.status != AutomationRunStatus::WaitingForApproval {
            return Err(VibexError::conflict(
                "automation_run_not_waiting_for_approval",
                "automation graph run is not waiting for approval",
            )
            .with_diagnostic("automationRunId", run.id.as_str()));
        }

        let graph = self.load_executable_graph(&run.graph_id)?;
        let waiting_steps = self.waiting_steps_for_run(&run.id)?;
        if waiting_steps.is_empty() {
            let failed = self.update_run_failure(
                run,
                now_ms,
                "automation/waiting_step_missing",
                "automation run was waiting but no waiting step was found",
                Vec::new(),
            )?;
            return Ok(failed);
        }

        for step in waiting_steps {
            let Some(permission_request_id) = step.permission_request_id.clone() else {
                continue;
            };
            let conn = self.manager.open_migrated()?;
            let Some(permission) =
                PermissionRepository::get_request(&conn, &permission_request_id)?
            else {
                drop(conn);
                let failed_step = self.update_step_failure(
                    step,
                    now_ms,
                    "automation/permission_missing",
                    "automation approval request was not found",
                    Vec::new(),
                )?;
                let failed_run = self.update_run_failure(
                    run,
                    now_ms,
                    "automation/permission_missing",
                    "automation approval request was not found",
                    vec![RedactedDiagnostic {
                        key: "stepId".to_string(),
                        value: failed_step.id.to_string(),
                    }],
                )?;
                return Ok(failed_run);
            };
            drop(conn);

            match permission.status {
                PermissionRequestStatus::Approved => {
                    self.update_step_success(step, now_ms)?;
                }
                PermissionRequestStatus::Denied | PermissionRequestStatus::Expired => {
                    let failed_step = self.update_step_failure(
                        step,
                        now_ms,
                        "automation/permission_denied",
                        "automation approval was denied or expired",
                        vec![RedactedDiagnostic {
                            key: "permissionRequestId".to_string(),
                            value: permission.id.to_string(),
                        }],
                    )?;
                    let failed_run = self.update_run_failure(
                        run,
                        now_ms,
                        "automation/permission_denied",
                        "automation approval was denied or expired",
                        vec![RedactedDiagnostic {
                            key: "stepId".to_string(),
                            value: failed_step.id.to_string(),
                        }],
                    )?;
                    return Ok(failed_run);
                }
                PermissionRequestStatus::Pending => {
                    return Ok(run);
                }
            }
        }

        let conn = self.manager.open_migrated()?;
        let running = AutomationGraphRepository::update_run(
            &conn,
            AutomationRunUpdateRequest {
                id: run.id.clone(),
                status: Some(AutomationRunStatus::Running),
                scheduled_task_id: None,
                clear_scheduled_task_id: false,
                session_id: run.session_id.clone(),
                clear_session_id: false,
                started_at_ms: None,
                clear_started_at_ms: false,
                ended_at_ms: None,
                clear_ended_at_ms: true,
                error_code: None,
                clear_error_code: true,
                error_message: None,
                clear_error_message: true,
                redacted_diagnostics: Some(Vec::new()),
            },
        )?;
        drop(conn);

        self.execute_ready_nodes(graph, running, now_ms).await
    }

    pub fn cancel_run(&self, request: AutomationRunCancelRequest) -> VibexResult<AutomationRun> {
        let now_ms = request.now_ms.unwrap_or_else(unix_timestamp_ms);
        let conn = self.manager.open_migrated()?;
        let run = AutomationGraphRepository::get_run(&conn, &request.run_id)?.ok_or_else(|| {
            VibexError::storage(
                "automation_run_not_found",
                "automation graph run was not found",
            )
            .with_diagnostic("automationRunId", request.run_id.as_str())
        })?;
        drop(conn);

        if !matches!(
            run.status,
            AutomationRunStatus::Running | AutomationRunStatus::WaitingForApproval
        ) {
            return Err(VibexError::conflict(
                "automation_run_not_cancelable",
                "automation graph run cannot be canceled from its current status",
            )
            .with_diagnostic("automationRunId", run.id.as_str()));
        }

        let diagnostics = request
            .reason
            .filter(|value| !value.trim().is_empty())
            .map(|value| {
                vec![RedactedDiagnostic {
                    key: "cancelReason".to_string(),
                    value: truncate_diagnostic(&value),
                }]
            })
            .unwrap_or_default();

        let conn = self.manager.open_migrated()?;
        AutomationGraphRepository::update_run(
            &conn,
            AutomationRunUpdateRequest {
                id: run.id,
                status: Some(AutomationRunStatus::Canceled),
                scheduled_task_id: None,
                clear_scheduled_task_id: false,
                session_id: run.session_id,
                clear_session_id: false,
                started_at_ms: None,
                clear_started_at_ms: false,
                ended_at_ms: Some(now_ms),
                clear_ended_at_ms: false,
                error_code: Some("automation/run_canceled".to_string()),
                clear_error_code: false,
                error_message: Some("automation graph run was canceled".to_string()),
                clear_error_message: false,
                redacted_diagnostics: Some(diagnostics),
            },
        )
    }

    pub fn recover_stale_runs(&self, now_ms: i64) -> VibexResult<Vec<AutomationRun>> {
        let before_ms = now_ms.saturating_sub(self.stale_after_ms);
        let conn = self.manager.open_migrated()?;
        let runs = AutomationGraphRepository::list_runs(
            &conn,
            vibex_core::AutomationRunListRequest {
                graph_id: None,
                status: Some(AutomationRunStatus::Running),
                limit: Some(100),
            },
        )?;
        drop(conn);

        let mut recovered = Vec::new();
        for run in runs
            .into_iter()
            .filter(|run| run.updated_at_ms <= before_ms)
        {
            recovered.push(self.update_run_failure(
                run,
                now_ms,
                "automation/recovered_stale_run",
                "automation graph run was recovered after restart",
                vec![RedactedDiagnostic {
                    key: "recoveredBy".to_string(),
                    value: "automation_recovery".to_string(),
                }],
            )?);
        }
        Ok(recovered)
    }

    async fn execute_ready_nodes(
        &self,
        graph: AutomationGraph,
        run: AutomationRun,
        now_ms: i64,
    ) -> VibexResult<AutomationRun> {
        let order = topological_order(&graph)?;
        for node in order {
            if self.step_for_node(&run.id, &node.id)?.is_some() {
                continue;
            }
            match self.execute_node(&graph, &run, &node, now_ms).await? {
                NodeExecution::Succeeded => {}
                NodeExecution::Waiting => {
                    let conn = self.manager.open_migrated()?;
                    return AutomationGraphRepository::get_run(&conn, &run.id)?.ok_or_else(|| {
                        VibexError::storage(
                            "automation_run_not_found",
                            "automation graph run was not found",
                        )
                        .with_diagnostic("automationRunId", run.id.as_str())
                    });
                }
                NodeExecution::Failed(failed_run) => return Ok(failed_run),
            }
        }

        let conn = self.manager.open_migrated()?;
        AutomationGraphRepository::update_run(
            &conn,
            AutomationRunUpdateRequest {
                id: run.id,
                status: Some(AutomationRunStatus::Succeeded),
                scheduled_task_id: None,
                clear_scheduled_task_id: false,
                session_id: run.session_id,
                clear_session_id: false,
                started_at_ms: None,
                clear_started_at_ms: false,
                ended_at_ms: Some(now_ms),
                clear_ended_at_ms: false,
                error_code: None,
                clear_error_code: true,
                error_message: None,
                clear_error_message: true,
                redacted_diagnostics: Some(Vec::new()),
            },
        )
    }

    async fn execute_node(
        &self,
        graph: &AutomationGraph,
        run: &AutomationRun,
        node: &AutomationNode,
        now_ms: i64,
    ) -> VibexResult<NodeExecution> {
        let step = self.create_running_step(run, node, now_ms)?;
        match &node.config {
            AutomationNodeConfig::AgentPrompt(config) => {
                self.execute_agent_prompt_node(graph, run, node, step, config, now_ms)
                    .await
            }
            AutomationNodeConfig::ApprovalGate(config) => {
                self.execute_approval_gate_node(graph, run, node, step, config, now_ms)
                    .await
            }
            AutomationNodeConfig::FileCheck(_)
            | AutomationNodeConfig::GitCheck(_)
            | AutomationNodeConfig::TerminalCheck(_) => {
                let failed_step = self.update_step_failure(
                    step,
                    now_ms,
                    "automation/unsupported_node_kind",
                    "automation node kind is not executable in this runtime child",
                    vec![RedactedDiagnostic {
                        key: "nodeKind".to_string(),
                        value: format!("{:?}", node.kind),
                    }],
                )?;
                let failed_run = self.update_run_failure(
                    run.clone(),
                    now_ms,
                    "automation/unsupported_node_kind",
                    "automation node kind is not executable in this runtime child",
                    vec![RedactedDiagnostic {
                        key: "stepId".to_string(),
                        value: failed_step.id.to_string(),
                    }],
                )?;
                Ok(NodeExecution::Failed(failed_run))
            }
        }
    }

    async fn execute_agent_prompt_node(
        &self,
        graph: &AutomationGraph,
        run: &AutomationRun,
        node: &AutomationNode,
        step: AutomationRunStep,
        config: &vibex_core::AutomationAgentPromptConfig,
        now_ms: i64,
    ) -> VibexResult<NodeExecution> {
        let provider_kind = config
            .provider_kind
            .or(graph.provider_kind)
            .unwrap_or(vibex_core::ProviderKind::Codex);
        let workspace_root = config
            .workspace_root
            .clone()
            .unwrap_or_else(|| graph.workspace_root.clone());
        let workspace_mode = config.workspace_mode.unwrap_or(graph.workspace_mode);
        let runtime = self.manager.resolve_initial_runtime_selection(
            config
                .provider_profile_id
                .clone()
                .or_else(|| graph.provider_profile_id.clone()),
            provider_kind,
            graph.project_id.as_ref(),
            graph.workspace_id.as_ref(),
        )?;
        let session = match self
            .manager
            .create_session(CreateAgentSessionRequest {
                runtime: runtime.clone(),
                workspace_root,
                workspace_mode,
                title: Some(format!("Automation: {} / {}", graph.title, node.title)),
                safety: config
                    .safety
                    .clone()
                    .or_else(|| Some(AgentSessionSafety::workspace_write_ask_on_risk())),
            })
            .await
        {
            Ok(session) => session,
            Err(err) => {
                let failed_step = self.update_step_from_error(step, now_ms, &err)?;
                let failed_run =
                    self.update_run_from_error(run.clone(), now_ms, &err, failed_step.id)?;
                return Ok(NodeExecution::Failed(failed_run));
            }
        };

        let step = self.update_step_session(step, Some(session.id.clone()))?;
        let run = self.update_run_session(run.clone(), Some(session.id.clone()))?;

        match self
            .manager
            .send_message(SendAgentMessageRequest {
                session_id: session.id.clone(),
                message_idempotency_key: format!(
                    "automation:{}:{}",
                    run.id.as_str(),
                    step.id.as_str()
                ),
                desired_runtime: runtime.clone(),
                text: config.prompt_template.clone(),
                attachments: Vec::new(),
                reasoning_effort: runtime.reasoning_effort.clone(),
                correlation_id: None,
            })
            .await
        {
            Ok(_) => {
                let session = self.manager.get_session(&session.id).await?;
                if session.state == AgentSessionState::NeedsInput {
                    let pending = {
                        let conn = self.manager.open_migrated()?;
                        PermissionRepository::pending_for_session(&conn, &session.id)?
                    };
                    let permission_request_id = pending.first().map(|request| request.id.clone());
                    self.update_step_waiting(step, permission_request_id, now_ms)?;
                    self.update_run_waiting(run, now_ms)?;
                    Ok(NodeExecution::Waiting)
                } else {
                    self.update_step_success(step, now_ms)?;
                    Ok(NodeExecution::Succeeded)
                }
            }
            Err(err) => {
                let failed_step = self.update_step_from_error(step, now_ms, &err)?;
                let failed_run = self.update_run_from_error(run, now_ms, &err, failed_step.id)?;
                Ok(NodeExecution::Failed(failed_run))
            }
        }
    }

    async fn execute_approval_gate_node(
        &self,
        graph: &AutomationGraph,
        run: &AutomationRun,
        node: &AutomationNode,
        step: AutomationRunStep,
        config: &vibex_core::AutomationApprovalGateConfig,
        now_ms: i64,
    ) -> VibexResult<NodeExecution> {
        let provider_kind = graph
            .provider_kind
            .unwrap_or(vibex_core::ProviderKind::Codex);
        let runtime = self.manager.resolve_initial_runtime_selection(
            graph.provider_profile_id.clone(),
            provider_kind,
            graph.project_id.as_ref(),
            graph.workspace_id.as_ref(),
        )?;
        let session = self
            .manager
            .create_session(CreateAgentSessionRequest {
                runtime,
                workspace_root: graph.workspace_root.clone(),
                workspace_mode: graph.workspace_mode,
                title: Some(format!(
                    "Automation approval: {} / {}",
                    graph.title, node.title
                )),
                safety: Some(AgentSessionSafety::workspace_write_ask_on_risk()),
            })
            .await?;
        let step = self.update_step_session(step, Some(session.id.clone()))?;
        let run = self.update_run_session(run.clone(), Some(session.id.clone()))?;
        let permission = PermissionRequest {
            id: vibex_core::RequestId::new(),
            session_id: session.id.clone(),
            project_id: graph.project_id.clone(),
            workspace_id: graph.workspace_id.clone(),
            provider_request_id: Some(format!("automation:{}", step.id.as_str())),
            risk_category: config.risk_category,
            title: config.title.clone(),
            details: vec![vibex_core::PermissionActionDetail {
                label: "approval".to_string(),
                value: truncate_diagnostic(&config.details),
            }],
            allowed_responses: config.allowed_responses.clone(),
            response_options: Vec::new(),
            status: PermissionRequestStatus::Pending,
            requested_at_ms: now_ms,
            expires_at_ms: None,
        };
        self.manager
            .record_permission_request(permission.clone())
            .await?;
        self.update_step_waiting(step, Some(permission.id), now_ms)?;
        self.update_run_waiting(run, now_ms)?;
        Ok(NodeExecution::Waiting)
    }

    fn load_executable_graph(
        &self,
        graph_id: &vibex_core::AutomationGraphId,
    ) -> VibexResult<AutomationGraph> {
        let conn = self.manager.open_migrated()?;
        let graph = AutomationGraphRepository::get(&conn, graph_id)?.ok_or_else(|| {
            VibexError::storage(
                "automation_graph_not_found",
                "automation graph was not found",
            )
            .with_diagnostic("automationGraphId", graph_id.as_str())
        })?;
        match graph.status {
            AutomationGraphStatus::Active => Ok(graph),
            AutomationGraphStatus::Paused => Err(VibexError::conflict(
                "automation_graph_paused",
                "paused automation graph cannot be started",
            )),
            AutomationGraphStatus::Deleted => Err(VibexError::conflict(
                "automation_graph_deleted",
                "deleted automation graph cannot be started",
            )),
        }
    }

    fn create_running_step(
        &self,
        run: &AutomationRun,
        node: &AutomationNode,
        now_ms: i64,
    ) -> VibexResult<AutomationRunStep> {
        let conn = self.manager.open_migrated()?;
        AutomationGraphRepository::create_run_step(
            &conn,
            AutomationRunStepCreateRequest {
                run_id: run.id.clone(),
                node_id: node.id.clone(),
                status: AutomationRunStepStatus::Running,
                session_id: None,
                permission_request_id: None,
                started_at_ms: Some(now_ms),
                ended_at_ms: None,
                error_code: None,
                error_message: None,
                redacted_diagnostics: Vec::new(),
            },
        )
    }

    fn step_for_node(
        &self,
        run_id: &vibex_core::AutomationRunId,
        node_id: &AutomationNodeId,
    ) -> VibexResult<Option<AutomationRunStep>> {
        let conn = self.manager.open_migrated()?;
        Ok(AutomationGraphRepository::list_run_steps(
            &conn,
            AutomationRunStepListRequest {
                run_id: Some(run_id.clone()),
                node_id: Some(node_id.clone()),
                status: None,
                limit: Some(1),
            },
        )?
        .into_iter()
        .next())
    }

    fn waiting_steps_for_run(
        &self,
        run_id: &vibex_core::AutomationRunId,
    ) -> VibexResult<Vec<AutomationRunStep>> {
        let conn = self.manager.open_migrated()?;
        AutomationGraphRepository::list_run_steps(
            &conn,
            AutomationRunStepListRequest {
                run_id: Some(run_id.clone()),
                node_id: None,
                status: Some(AutomationRunStepStatus::WaitingForApproval),
                limit: Some(100),
            },
        )
    }

    fn update_step_session(
        &self,
        step: AutomationRunStep,
        session_id: Option<vibex_core::VibexSessionId>,
    ) -> VibexResult<AutomationRunStep> {
        let conn = self.manager.open_migrated()?;
        AutomationGraphRepository::update_run_step(
            &conn,
            AutomationRunStepUpdateRequest {
                id: step.id,
                status: None,
                session_id,
                clear_session_id: false,
                permission_request_id: None,
                clear_permission_request_id: false,
                started_at_ms: None,
                clear_started_at_ms: false,
                ended_at_ms: None,
                clear_ended_at_ms: false,
                error_code: None,
                clear_error_code: false,
                error_message: None,
                clear_error_message: false,
                redacted_diagnostics: None,
            },
        )
    }

    fn update_run_session(
        &self,
        run: AutomationRun,
        session_id: Option<vibex_core::VibexSessionId>,
    ) -> VibexResult<AutomationRun> {
        let conn = self.manager.open_migrated()?;
        AutomationGraphRepository::update_run(
            &conn,
            AutomationRunUpdateRequest {
                id: run.id,
                status: None,
                scheduled_task_id: None,
                clear_scheduled_task_id: false,
                session_id,
                clear_session_id: false,
                started_at_ms: None,
                clear_started_at_ms: false,
                ended_at_ms: None,
                clear_ended_at_ms: false,
                error_code: None,
                clear_error_code: false,
                error_message: None,
                clear_error_message: false,
                redacted_diagnostics: None,
            },
        )
    }

    fn update_step_success(
        &self,
        step: AutomationRunStep,
        now_ms: i64,
    ) -> VibexResult<AutomationRunStep> {
        let conn = self.manager.open_migrated()?;
        AutomationGraphRepository::update_run_step(
            &conn,
            AutomationRunStepUpdateRequest {
                id: step.id,
                status: Some(AutomationRunStepStatus::Succeeded),
                session_id: step.session_id,
                clear_session_id: false,
                permission_request_id: step.permission_request_id,
                clear_permission_request_id: false,
                started_at_ms: None,
                clear_started_at_ms: false,
                ended_at_ms: Some(now_ms),
                clear_ended_at_ms: false,
                error_code: None,
                clear_error_code: true,
                error_message: None,
                clear_error_message: true,
                redacted_diagnostics: Some(Vec::new()),
            },
        )
    }

    fn update_step_waiting(
        &self,
        step: AutomationRunStep,
        permission_request_id: Option<vibex_core::RequestId>,
        now_ms: i64,
    ) -> VibexResult<AutomationRunStep> {
        let conn = self.manager.open_migrated()?;
        AutomationGraphRepository::update_run_step(
            &conn,
            AutomationRunStepUpdateRequest {
                id: step.id,
                status: Some(AutomationRunStepStatus::WaitingForApproval),
                session_id: step.session_id,
                clear_session_id: false,
                permission_request_id,
                clear_permission_request_id: false,
                started_at_ms: None,
                clear_started_at_ms: false,
                ended_at_ms: Some(now_ms),
                clear_ended_at_ms: false,
                error_code: Some("automation/permission_required".to_string()),
                clear_error_code: false,
                error_message: Some("automation graph step requires approval".to_string()),
                clear_error_message: false,
                redacted_diagnostics: Some(Vec::new()),
            },
        )
    }

    fn update_run_waiting(&self, run: AutomationRun, _now_ms: i64) -> VibexResult<AutomationRun> {
        let conn = self.manager.open_migrated()?;
        AutomationGraphRepository::update_run(
            &conn,
            AutomationRunUpdateRequest {
                id: run.id,
                status: Some(AutomationRunStatus::WaitingForApproval),
                scheduled_task_id: None,
                clear_scheduled_task_id: false,
                session_id: run.session_id,
                clear_session_id: false,
                started_at_ms: None,
                clear_started_at_ms: false,
                ended_at_ms: None,
                clear_ended_at_ms: false,
                error_code: Some("automation/permission_required".to_string()),
                clear_error_code: false,
                error_message: Some("automation graph run requires approval".to_string()),
                clear_error_message: false,
                redacted_diagnostics: Some(Vec::new()),
            },
        )
    }

    fn update_step_from_error(
        &self,
        step: AutomationRunStep,
        now_ms: i64,
        err: &VibexError,
    ) -> VibexResult<AutomationRunStep> {
        self.update_step_failure(
            step,
            now_ms,
            &err.code,
            &err.message,
            redacted_diagnostics_from_error(err),
        )
    }

    fn update_step_failure(
        &self,
        step: AutomationRunStep,
        now_ms: i64,
        error_code: &str,
        error_message: &str,
        diagnostics: Vec<RedactedDiagnostic>,
    ) -> VibexResult<AutomationRunStep> {
        let conn = self.manager.open_migrated()?;
        AutomationGraphRepository::update_run_step(
            &conn,
            AutomationRunStepUpdateRequest {
                id: step.id,
                status: Some(AutomationRunStepStatus::Failed),
                session_id: step.session_id,
                clear_session_id: false,
                permission_request_id: step.permission_request_id,
                clear_permission_request_id: false,
                started_at_ms: None,
                clear_started_at_ms: false,
                ended_at_ms: Some(now_ms),
                clear_ended_at_ms: false,
                error_code: Some(error_code.to_string()),
                clear_error_code: false,
                error_message: Some(error_message.to_string()),
                clear_error_message: false,
                redacted_diagnostics: Some(diagnostics),
            },
        )
    }

    fn update_run_from_error(
        &self,
        run: AutomationRun,
        now_ms: i64,
        err: &VibexError,
        step_id: AutomationRunStepId,
    ) -> VibexResult<AutomationRun> {
        let mut diagnostics = redacted_diagnostics_from_error(err);
        diagnostics.push(RedactedDiagnostic {
            key: "stepId".to_string(),
            value: step_id.to_string(),
        });
        self.update_run_failure(run, now_ms, &err.code, &err.message, diagnostics)
    }

    fn update_run_failure(
        &self,
        run: AutomationRun,
        now_ms: i64,
        error_code: &str,
        error_message: &str,
        diagnostics: Vec<RedactedDiagnostic>,
    ) -> VibexResult<AutomationRun> {
        let conn = self.manager.open_migrated()?;
        AutomationGraphRepository::update_run(
            &conn,
            AutomationRunUpdateRequest {
                id: run.id,
                status: Some(AutomationRunStatus::Failed),
                scheduled_task_id: None,
                clear_scheduled_task_id: false,
                session_id: run.session_id,
                clear_session_id: false,
                started_at_ms: None,
                clear_started_at_ms: false,
                ended_at_ms: Some(now_ms),
                clear_ended_at_ms: false,
                error_code: Some(error_code.to_string()),
                clear_error_code: false,
                error_message: Some(error_message.to_string()),
                clear_error_message: false,
                redacted_diagnostics: Some(diagnostics),
            },
        )
    }
}

enum NodeExecution {
    Succeeded,
    Waiting,
    Failed(AutomationRun),
}

fn validate_graph_for_runtime(graph: &AutomationGraph) -> VibexResult<()> {
    if graph.nodes.is_empty() {
        return Err(VibexError::validation(
            "automation_graph_empty",
            "automation graph has no executable nodes",
        ));
    }
    for edge in &graph.edges {
        if edge
            .condition
            .expression
            .as_deref()
            .is_some_and(|expression| !expression.trim().is_empty())
        {
            return Err(VibexError::capability(
                "automation_edge_expression_unsupported",
                "automation edge expressions are not supported yet",
            ));
        }
    }
    let _ = topological_order(graph)?;
    Ok(())
}

fn topological_order(graph: &AutomationGraph) -> VibexResult<Vec<AutomationNode>> {
    let node_ids = graph
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    let mut indegree = graph
        .nodes
        .iter()
        .map(|node| (node.id.clone(), 0_usize))
        .collect::<HashMap<_, _>>();
    let mut outgoing: HashMap<AutomationNodeId, Vec<AutomationNodeId>> = HashMap::new();

    for edge in &graph.edges {
        if !node_ids.contains(&edge.source_node_id) || !node_ids.contains(&edge.target_node_id) {
            return Err(VibexError::validation(
                "automation_graph_edge_endpoint_missing",
                "automation graph edge references a missing node",
            ));
        }
        if !matches!(
            edge.condition.kind,
            AutomationEdgeConditionKind::Always
                | AutomationEdgeConditionKind::OnSuccess
                | AutomationEdgeConditionKind::OnFailure
                | AutomationEdgeConditionKind::OnApproval
        ) {
            return Err(VibexError::capability(
                "automation_edge_condition_unsupported",
                "automation edge condition is not supported",
            ));
        }
        *indegree.entry(edge.target_node_id.clone()).or_default() += 1;
        outgoing
            .entry(edge.source_node_id.clone())
            .or_default()
            .push(edge.target_node_id.clone());
    }

    let node_map = graph
        .nodes
        .iter()
        .cloned()
        .map(|node| (node.id.clone(), node))
        .collect::<HashMap<_, _>>();
    let mut queue = graph
        .nodes
        .iter()
        .filter(|node| indegree.get(&node.id).copied().unwrap_or(0) == 0)
        .map(|node| node.id.clone())
        .collect::<VecDeque<_>>();
    let mut ordered = Vec::with_capacity(graph.nodes.len());

    while let Some(node_id) = queue.pop_front() {
        if let Some(node) = node_map.get(&node_id) {
            ordered.push(node.clone());
        }
        for target_id in outgoing.get(&node_id).into_iter().flatten() {
            let entry = indegree.entry(target_id.clone()).or_default();
            *entry = entry.saturating_sub(1);
            if *entry == 0 {
                queue.push_back(target_id.clone());
            }
        }
    }

    if ordered.len() != graph.nodes.len() {
        return Err(VibexError::validation(
            "automation_graph_cycle_unsupported",
            "automation graph cycles are not supported yet",
        ));
    }

    Ok(ordered)
}

fn redacted_diagnostics_from_error(err: &VibexError) -> Vec<RedactedDiagnostic> {
    let mut diagnostics = vec![RedactedDiagnostic {
        key: "errorCategory".to_string(),
        value: format!("{:?}", err.category),
    }];
    diagnostics.extend(err.diagnostics.clone());
    diagnostics
}

fn truncate_diagnostic(value: &str) -> String {
    const MAX_CHARS: usize = 240;
    value.chars().take(MAX_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use vibex_core::{
        AutomationAgentPromptConfig, AutomationApprovalGateConfig, AutomationEdgeCondition,
        AutomationEdgeCreateRequest, AutomationFileCheckConfig, AutomationGraphCreateRequest,
        AutomationGraphId, AutomationGraphTrigger, AutomationNodeConfig,
        AutomationNodeCreateRequest, AutomationNodeId, AutomationNodeKind, AutomationRunTrigger,
        PermissionActionDetail, PermissionMode, PermissionResolution, PermissionResponseKind,
        PermissionRiskCategory, ProviderKind, ResolvePermissionRequest, WorkspaceMode,
    };

    use super::*;

    struct TestAgentProvider;

    fn test_capabilities() -> vibex_core::ProviderCapabilities {
        vibex_core::ProviderCapabilities {
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
    impl crate::adapter::AgentProvider for TestAgentProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Acp
        }

        fn capabilities(&self) -> vibex_core::ProviderCapabilities {
            test_capabilities()
        }

        async fn create_session(
            &self,
            request: crate::adapter::ProviderCreateRequest,
        ) -> VibexResult<crate::adapter::ProviderSessionHandle> {
            Ok(crate::adapter::ProviderSessionHandle {
                binding: test_binding(request.session_id, request.provider_profile_id),
                capabilities: self.capabilities(),
            })
        }

        async fn resume_session(
            &self,
            binding: vibex_core::ProviderBinding,
        ) -> VibexResult<crate::adapter::ProviderSessionHandle> {
            Ok(crate::adapter::ProviderSessionHandle {
                binding,
                capabilities: self.capabilities(),
            })
        }

        async fn prepare_turn_execution(
            &self,
            _handle: &crate::adapter::ProviderSessionHandle,
            request: &crate::adapter::ProviderTurnRequest,
        ) -> VibexResult<Option<crate::adapter::ProviderTurnExecutionIdentity>> {
            Ok(request.execution_identity.clone())
        }

        async fn send_turn(
            &self,
            _handle: crate::adapter::ProviderSessionHandle,
            request: crate::adapter::ProviderTurnRequest,
        ) -> VibexResult<crate::adapter::ProviderTurnResult> {
            if request.text.to_ascii_lowercase().contains("error") {
                return Err(VibexError::provider(
                    "test_provider_error",
                    "test provider was asked to fail",
                ));
            }

            if request.text.to_ascii_lowercase().contains("permission") {
                return Ok(crate::adapter::ProviderTurnResult {
                    events: vec![crate::adapter::ProviderEvent {
                        source: vibex_core::TimelineSource::Provider,
                        payload: vibex_core::TimelinePayload::PermissionRequest(
                            PermissionRequest {
                                id: vibex_core::RequestId::new(),
                                session_id: request.session_id,
                                project_id: None,
                                workspace_id: None,
                                provider_request_id: Some("test-permission".to_string()),
                                risk_category: PermissionRiskCategory::Command,
                                title: "Run test command".to_string(),
                                details: vec![PermissionActionDetail {
                                    label: "command".to_string(),
                                    value: "echo test-permission".to_string(),
                                }],
                                allowed_responses: vec![
                                    PermissionResponseKind::Approve,
                                    PermissionResponseKind::Deny,
                                ],
                                response_options: Vec::new(),
                                status: vibex_core::PermissionRequestStatus::Pending,
                                requested_at_ms: unix_timestamp_ms(),
                                expires_at_ms: None,
                            },
                        ),
                        provider_correlation_id: Some("test-permission".to_string()),
                        redaction_state: vibex_core::TimelineRedactionState::None,
                    }],
                    binding_update: None,
                    completed: false,
                });
            }

            Ok(crate::adapter::ProviderTurnResult {
                events: vec![crate::adapter::ProviderEvent::agent(
                    vibex_core::TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                        text: format!("Test response to: {}", request.text),
                        is_final: true,
                    }),
                )],
                binding_update: None,
                completed: true,
            })
        }

        async fn resolve_permission(
            &self,
            _request: crate::adapter::ProviderPermissionResolution,
        ) -> VibexResult<()> {
            Ok(())
        }
    }

    fn test_binding(
        session_id: vibex_core::VibexSessionId,
        provider_profile_id: vibex_core::ProviderProfileId,
    ) -> vibex_core::ProviderBinding {
        let now = unix_timestamp_ms();
        vibex_core::ProviderBinding {
            session_id,
            provider_kind: ProviderKind::Acp,
            provider_profile_id,
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

    #[tokio::test]
    async fn automation_runner_completes_test_agent_prompt() {
        let db_path = temp_db_path("automation-success");
        let manager = test_manager(&db_path);
        let graph = create_agent_graph(&manager, "hello automation", Vec::new());

        let run = AutomationGraphRunner::new(&manager)
            .start_graph(start_request(graph.id.clone()))
            .await
            .unwrap();

        assert_eq!(run.status, AutomationRunStatus::Succeeded);
        let steps = run_steps(&manager, &run.id);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].status, AutomationRunStepStatus::Succeeded);
        assert!(steps[0].session_id.is_some());

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn automation_runner_records_ambiguous_prompt_failure() {
        let db_path = temp_db_path("automation-error");
        let manager = test_manager(&db_path);
        let graph = create_agent_graph(&manager, "please error", Vec::new());

        let run = AutomationGraphRunner::new(&manager)
            .start_graph(start_request(graph.id.clone()))
            .await
            .unwrap();

        assert_eq!(run.status, AutomationRunStatus::Failed);
        assert_eq!(
            run.error_code.as_deref(),
            Some("message_submission_prompt_dispatch_ambiguous")
        );
        assert!(
            run.redacted_diagnostics
                .iter()
                .all(|entry| !entry.value.contains("please error"))
        );

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn automation_runner_waits_for_permission_and_resumes_after_approval() {
        let db_path = temp_db_path("automation-permission");
        let manager = test_manager(&db_path);
        let graph = create_agent_graph(&manager, "please request permission", Vec::new());
        let runner = AutomationGraphRunner::new(&manager);

        let waiting = runner
            .start_graph(start_request(graph.id.clone()))
            .await
            .unwrap();
        assert_eq!(waiting.status, AutomationRunStatus::WaitingForApproval);
        let waiting_step = run_steps(&manager, &waiting.id).remove(0);
        assert_eq!(
            waiting_step.status,
            AutomationRunStepStatus::WaitingForApproval
        );
        let permission_request_id = waiting_step.permission_request_id.clone().unwrap();
        let session_id = waiting_step.session_id.clone().unwrap();

        manager
            .resolve_permission(ResolvePermissionRequest {
                session_id: session_id.clone(),
                request_id: permission_request_id.clone(),
                resolution: PermissionResolution {
                    request_id: permission_request_id,
                    session_id,
                    response: PermissionResponseKind::Approve,
                    responder_device_id: None,
                    provider_resolution_id: None,
                    note: None,
                    resolved_at_ms: unix_timestamp_ms(),
                },
            })
            .await
            .unwrap();

        let resumed = runner
            .resume_run(AutomationRunResumeRequest {
                run_id: waiting.id,
                now_ms: None,
            })
            .await
            .unwrap();
        assert_eq!(resumed.status, AutomationRunStatus::Succeeded);

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn automation_approval_gate_resumes_or_fails_from_permission_resolution() {
        let db_path = temp_db_path("automation-approval");
        let manager = test_manager(&db_path);
        let graph = create_approval_graph(&manager);
        let runner = AutomationGraphRunner::new(&manager);

        let waiting = runner
            .start_graph(start_request(graph.id.clone()))
            .await
            .unwrap();
        assert_eq!(waiting.status, AutomationRunStatus::WaitingForApproval);
        let waiting_step = run_steps(&manager, &waiting.id).remove(0);
        let permission_request_id = waiting_step.permission_request_id.clone().unwrap();
        let session_id = waiting_step.session_id.clone().unwrap();

        manager
            .resolve_permission(ResolvePermissionRequest {
                session_id: session_id.clone(),
                request_id: permission_request_id.clone(),
                resolution: PermissionResolution {
                    request_id: permission_request_id,
                    session_id,
                    response: PermissionResponseKind::Approve,
                    responder_device_id: None,
                    provider_resolution_id: None,
                    note: None,
                    resolved_at_ms: unix_timestamp_ms(),
                },
            })
            .await
            .unwrap();

        let resumed = runner
            .resume_run(AutomationRunResumeRequest {
                run_id: waiting.id,
                now_ms: None,
            })
            .await
            .unwrap();
        assert_eq!(resumed.status, AutomationRunStatus::Succeeded);

        let denied_graph = create_approval_graph(&manager);
        let denied_waiting = runner
            .start_graph(start_request(denied_graph.id.clone()))
            .await
            .unwrap();
        let denied_step = run_steps(&manager, &denied_waiting.id).remove(0);
        let denied_permission_id = denied_step.permission_request_id.clone().unwrap();
        let denied_session_id = denied_step.session_id.clone().unwrap();
        manager
            .resolve_permission(ResolvePermissionRequest {
                session_id: denied_session_id.clone(),
                request_id: denied_permission_id.clone(),
                resolution: PermissionResolution {
                    request_id: denied_permission_id,
                    session_id: denied_session_id,
                    response: PermissionResponseKind::Deny,
                    responder_device_id: None,
                    provider_resolution_id: None,
                    note: None,
                    resolved_at_ms: unix_timestamp_ms(),
                },
            })
            .await
            .unwrap();
        let denied = runner
            .resume_run(AutomationRunResumeRequest {
                run_id: denied_waiting.id,
                now_ms: None,
            })
            .await
            .unwrap();
        assert_eq!(denied.status, AutomationRunStatus::Failed);
        assert_eq!(
            denied.error_code.as_deref(),
            Some("automation/permission_denied")
        );

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn automation_runner_rejects_unsupported_node_and_recovers_stale_run() {
        let db_path = temp_db_path("automation-unsupported");
        let manager = test_manager(&db_path);
        let graph = create_file_check_graph(&manager);
        let runner = AutomationGraphRunner::new(&manager).with_stale_after_ms(10);

        let failed = runner
            .start_graph(start_request(graph.id.clone()))
            .await
            .unwrap();
        assert_eq!(failed.status, AutomationRunStatus::Failed);
        assert_eq!(
            failed.error_code.as_deref(),
            Some("automation/unsupported_node_kind")
        );

        let conn = manager.open_migrated().unwrap();
        let stale = AutomationGraphRepository::create_run(
            &conn,
            AutomationRunCreateRequest {
                graph_id: graph.id,
                status: AutomationRunStatus::Running,
                trigger: AutomationRunTrigger::Manual,
                scheduled_task_id: None,
                session_id: None,
                started_at_ms: Some(1),
                ended_at_ms: None,
                error_code: None,
                error_message: None,
                redacted_diagnostics: Vec::new(),
            },
        )
        .unwrap();
        drop(conn);

        let recovered = runner.recover_stale_runs(stale.updated_at_ms + 20).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].status, AutomationRunStatus::Failed);
        assert_eq!(
            recovered[0].error_code.as_deref(),
            Some("automation/recovered_stale_run")
        );

        cleanup_db(db_path);
    }

    fn test_manager(path: &std::path::Path) -> crate::test_support::TestRuntimeHarness {
        crate::test_support::TestRuntimeHarness::new(
            path,
            vibex_core::AgentId::parse("codex").unwrap(),
            Arc::new(TestAgentProvider),
        )
    }

    fn create_agent_graph(
        manager: &AgentManager,
        prompt: &str,
        edges: Vec<AutomationEdgeCreateRequest>,
    ) -> AutomationGraph {
        let node_id = AutomationNodeId::new();
        create_graph(
            manager,
            vec![AutomationNodeCreateRequest {
                id: Some(node_id),
                kind: AutomationNodeKind::AgentPrompt,
                title: "Prompt".to_string(),
                config: AutomationNodeConfig::AgentPrompt(AutomationAgentPromptConfig {
                    prompt_template: prompt.to_string(),
                    provider_kind: Some(ProviderKind::Codex),
                    provider_profile_id: None,
                    safety: Some(AgentSessionSafety {
                        permission_mode: PermissionMode::WorkspaceWrite,
                        ask_on_risk: true,
                        bypass_all_permissions: false,
                    }),
                    workspace_root: Some("/tmp/vibex-automation-test".to_string()),
                    workspace_mode: Some(WorkspaceMode::CurrentCheckout),
                }),
                position: None,
            }],
            edges,
        )
    }

    fn create_approval_graph(manager: &AgentManager) -> AutomationGraph {
        create_graph(
            manager,
            vec![AutomationNodeCreateRequest {
                id: Some(AutomationNodeId::new()),
                kind: AutomationNodeKind::ApprovalGate,
                title: "Approval".to_string(),
                config: AutomationNodeConfig::ApprovalGate(AutomationApprovalGateConfig {
                    title: "Approve automation".to_string(),
                    details: "Continue the deterministic automation test.".to_string(),
                    risk_category: PermissionRiskCategory::Command,
                    allowed_responses: vec![
                        PermissionResponseKind::Approve,
                        PermissionResponseKind::Deny,
                    ],
                }),
                position: None,
            }],
            Vec::new(),
        )
    }

    fn create_file_check_graph(manager: &AgentManager) -> AutomationGraph {
        create_graph(
            manager,
            vec![AutomationNodeCreateRequest {
                id: Some(AutomationNodeId::new()),
                kind: AutomationNodeKind::FileCheck,
                title: "File check".to_string(),
                config: AutomationNodeConfig::FileCheck(AutomationFileCheckConfig {
                    path_pattern: "*.rs".to_string(),
                    condition: "exists".to_string(),
                }),
                position: None,
            }],
            Vec::new(),
        )
    }

    fn create_graph(
        manager: &AgentManager,
        nodes: Vec<AutomationNodeCreateRequest>,
        edges: Vec<AutomationEdgeCreateRequest>,
    ) -> AutomationGraph {
        let mut conn = manager.open_migrated().unwrap();
        AutomationGraphRepository::create(
            &mut conn,
            AutomationGraphCreateRequest {
                title: "Automation test".to_string(),
                description: None,
                project_id: None,
                workspace_id: None,
                workspace_root: "/tmp/vibex-automation-test".to_string(),
                workspace_mode: WorkspaceMode::CurrentCheckout,
                provider_kind: Some(ProviderKind::Codex),
                provider_profile_id: None,
                trigger: AutomationGraphTrigger::Manual,
                nodes,
                edges,
            },
        )
        .unwrap()
    }

    fn start_request(graph_id: AutomationGraphId) -> AutomationRunStartRequest {
        AutomationRunStartRequest {
            graph_id,
            trigger: AutomationRunTrigger::Manual,
            scheduled_task_id: None,
            now_ms: None,
        }
    }

    fn run_steps(
        manager: &AgentManager,
        run_id: &vibex_core::AutomationRunId,
    ) -> Vec<AutomationRunStep> {
        let conn = manager.open_migrated().unwrap();
        AutomationGraphRepository::list_run_steps(
            &conn,
            AutomationRunStepListRequest {
                run_id: Some(run_id.clone()),
                node_id: None,
                status: None,
                limit: Some(100),
            },
        )
        .unwrap()
    }

    fn temp_db_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("vibex-{label}-{}.db", unix_timestamp_ms()))
    }

    fn cleanup_db(path: std::path::PathBuf) {
        let _ = std::fs::remove_file(path);
    }

    #[allow(dead_code)]
    fn _edge(source: &str, target: &str) -> AutomationEdgeCreateRequest {
        AutomationEdgeCreateRequest {
            source_node_id: AutomationNodeId::parse(source).unwrap(),
            target_node_id: AutomationNodeId::parse(target).unwrap(),
            condition: AutomationEdgeCondition {
                kind: AutomationEdgeConditionKind::OnSuccess,
                expression: None,
            },
        }
    }

    #[allow(dead_code)]
    fn _detail(label: &str, value: &str) -> PermissionActionDetail {
        PermissionActionDetail {
            label: label.to_string(),
            value: value.to_string(),
        }
    }
}

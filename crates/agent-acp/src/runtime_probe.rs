//! Durable, isolated ACP runtime verification.
//!
//! This module deliberately sits beside the ACP process implementation. It
//! can therefore reuse the same spawn and JSON-RPC lifecycle as a live
//! session while keeping all probe-owned state under a disposable root.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::time::{MissedTickBehavior, interval, timeout};
use url::Url;
use vibex_config_switch::{
    AgentProviderProjectionEngine, ProviderConfigService, secrets::resolve_provider_secret,
};
use vibex_core::{
    AgentProviderProjectionPreview, AgentProviderProjectionRegistry, AgentRuntimeProbeCapability,
    AgentRuntimeProbeFact, AgentRuntimeProbeFactStatus, AgentRuntimeProbeId,
    AgentRuntimeProbeListRequest, AgentRuntimeProbeRecord, AgentRuntimeProbeRequest,
    AgentRuntimeProbeStage, AgentRuntimeProbeStartRequest, AgentRuntimeProbeStatus,
    AgentRuntimeProfile, ProjectionDescriptorMatch, ProviderSwitchBehavior, VibexError,
    VibexResult, unix_timestamp_ms,
};
use vibex_db::{
    AgentModelProviderBindingRepository, AgentRuntimeProbeRepository,
    AgentRuntimeProfileRepository, apply_migrations, open_database,
};

use crate::AcpProcessInstanceId;
use crate::protocol::{
    self, AcpOperation, build_initialize_params, build_session_load_params,
    build_session_new_params, build_session_resume_params,
};
use crate::runtime::{
    ACP_PROBE_TIMEOUT, AcpProcess, AcpProcessLaunch, AcpProcessPurpose, AcpRuntimeClient,
    append_projection_process_args, effective_acp_process_args, extract_current_model_id,
    extract_model_ids, validate_restore_response,
};

const PROBE_SOURCE_REVISION: &str = "runtime-probe-v2";
const MIN_DIAGNOSTIC_LEN: usize = 1;
const PROBE_ENV_KEYS: &[&str] = &[
    "HOME",
    "USERPROFILE",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "CODEX_HOME",
];

/// Management-facing coordinator. Request/list/cancel operations are
/// synchronous durable DB operations; `spawn` starts the bounded async worker.
#[derive(Clone)]
pub struct AgentRuntimeProbeService {
    client: Arc<AcpRuntimeClient>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AgentRuntimeProbeReconcileReport {
    pub recovered: usize,
    pub cancelled: usize,
}

impl AgentRuntimeProbeService {
    pub fn new(client: Arc<AcpRuntimeClient>) -> Self {
        Self { client }
    }

    pub fn request(
        &self,
        request: AgentRuntimeProbeStartRequest,
    ) -> VibexResult<AgentRuntimeProbeRecord> {
        request.validate()?;
        let record = self.client.create_probe_record(request)?;
        Ok(record)
    }

    /// Start a worker for a previously persisted request. A separate method
    /// keeps request durability independent from executor availability.
    pub fn spawn(&self, probe_id: AgentRuntimeProbeId) -> VibexResult<()> {
        let client = self.client.clone();
        tokio::runtime::Handle::try_current()
            .map_err(|_| {
                VibexError::process(
                    "agent_runtime_probe_executor_unavailable",
                    "runtime probe requires an active async executor",
                )
            })?
            .spawn(async move {
                if let Err(error) = client.execute_probe(probe_id).await {
                    tracing::warn!(
                        target: "vibex_agent_runtime_probe",
                        error_code = %error.code,
                        "runtime probe worker failed"
                    );
                }
            });
        Ok(())
    }

    pub async fn run(&self, probe_id: AgentRuntimeProbeId) -> VibexResult<AgentRuntimeProbeRecord> {
        self.client.execute_probe(probe_id).await
    }

    pub fn reconcile_on_startup(&self) -> VibexResult<AgentRuntimeProbeReconcileReport> {
        let conn = self.client.open_probe_connection()?;
        let pending = AgentRuntimeProbeRepository::list_non_terminal(&conn)?;
        let mut report = AgentRuntimeProbeReconcileReport::default();
        for record in pending {
            self.client.cleanup_probe_root(&record.id)?;
            let recovered = AgentRuntimeProbeRepository::reset_for_startup(
                &conn,
                &record.id,
                record.revision,
                unix_timestamp_ms(),
            )?;
            if recovered.status == AgentRuntimeProbeStatus::Requested {
                self.spawn(recovered.id)?;
                report.recovered = report.recovered.saturating_add(1);
            } else if recovered.status == AgentRuntimeProbeStatus::Cancelled {
                report.cancelled = report.cancelled.saturating_add(1);
            }
        }
        Ok(report)
    }

    pub fn get(
        &self,
        probe_id: &AgentRuntimeProbeId,
    ) -> VibexResult<Option<AgentRuntimeProbeRecord>> {
        let conn = self.client.open_probe_connection()?;
        AgentRuntimeProbeRepository::get(&conn, probe_id)
    }

    pub fn list(
        &self,
        request: AgentRuntimeProbeListRequest,
    ) -> VibexResult<Vec<AgentRuntimeProbeRecord>> {
        let conn = self.client.open_probe_connection()?;
        AgentRuntimeProbeRepository::list(
            &conn,
            request.runtime_profile_id.as_ref(),
            request.limit.unwrap_or(100),
        )
    }

    pub fn cancel(
        &self,
        probe_id: &AgentRuntimeProbeId,
        expected_revision: i64,
    ) -> VibexResult<AgentRuntimeProbeRecord> {
        let conn = self.client.open_probe_connection()?;
        AgentRuntimeProbeRepository::request_cancel(
            &conn,
            probe_id,
            expected_revision,
            unix_timestamp_ms(),
        )
    }
}

#[derive(Debug)]
struct ProbeExecutionResult {
    facts: Vec<AgentRuntimeProbeFact>,
    agent_version: Option<String>,
    adapter_version: Option<String>,
    descriptor_match: ProjectionDescriptorMatch,
    projection_fingerprint: Option<String>,
    switch_behavior: ProviderSwitchBehavior,
    source_survived_prepare_failure: bool,
}

struct ProbeProjectionContext {
    provider_profile_id: Option<vibex_core::ProviderProfileId>,
    descriptor_match: ProjectionDescriptorMatch,
    projection: Option<vibex_config_switch::ResolvedAgentProviderProjection>,
    expected_provider_identities: Vec<String>,
}

struct RunningProbeContext<'a> {
    runtime: &'a AgentRuntimeProfile,
    profile_id: &'a vibex_core::ProviderProfileId,
    descriptor_match: ProjectionDescriptorMatch,
    projection: Option<&'a vibex_config_switch::ResolvedAgentProviderProjection>,
    expected_provider_identities: &'a [String],
    cwd: &'a Path,
    process: Arc<AcpProcess>,
    deadline: Instant,
}

struct ProbeRootGuard {
    path: PathBuf,
}

impl ProbeRootGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for ProbeRootGuard {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                target: "vibex_agent_runtime_probe",
                error_kind = ?error.kind(),
                "failed to remove isolated runtime probe root"
            );
        }
    }
}

impl AcpRuntimeClient {
    pub fn runtime_probe_service(self: &Arc<Self>) -> AgentRuntimeProbeService {
        AgentRuntimeProbeService::new(self.clone())
    }

    fn open_probe_connection(&self) -> VibexResult<vibex_db::DbConnection> {
        let mut conn = open_database(self.provider_config_service().database_path())?;
        apply_migrations(&mut conn)?;
        Ok(conn)
    }

    fn create_probe_record(
        &self,
        request: AgentRuntimeProbeStartRequest,
    ) -> VibexResult<AgentRuntimeProbeRecord> {
        let conn = self.open_probe_connection()?;
        let runtime = AgentRuntimeProfileRepository::get(&conn, &request.runtime_profile_id)?
            .ok_or_else(|| {
                VibexError::validation(
                    "agent_runtime_profile_not_found",
                    "Agent runtime profile was not found for the probe",
                )
            })?;
        let resolution =
            AgentProviderProjectionRegistry::builtin()?.resolve(&runtime.version_identity)?;
        let binding = request
            .binding_id
            .as_ref()
            .map(|id| AgentModelProviderBindingRepository::get(&conn, id))
            .transpose()?
            .flatten();
        if let Some(binding) = &binding
            && binding.runtime_profile_id != runtime.id
        {
            return Err(VibexError::validation(
                "agent_runtime_probe_binding_mismatch",
                "probe binding does not belong to the selected runtime profile",
            ));
        }
        let record = AgentRuntimeProbeRecord::requested(
            AgentRuntimeProbeId::new(),
            AgentRuntimeProbeRequest {
                runtime_profile_id: request.runtime_profile_id,
                binding_id: request.binding_id,
                workspace_key: request.workspace_key,
                timeout_ms: request.timeout_ms,
                minimal_prompt: request.minimal_prompt,
            },
            runtime.version_identity.route.agent_id.clone(),
            runtime.version_identity.route.adapter_id.clone(),
            resolution.descriptor.id,
            resolution.descriptor.descriptor_version,
            unix_timestamp_ms(),
        )?;
        AgentRuntimeProbeRepository::insert(&conn, &record)?;
        Ok(record)
    }

    async fn execute_probe(
        &self,
        probe_id: AgentRuntimeProbeId,
    ) -> VibexResult<AgentRuntimeProbeRecord> {
        let mut record = {
            let conn = self.open_probe_connection()?;
            let record = AgentRuntimeProbeRepository::claim_requested(
                &conn,
                &probe_id,
                unix_timestamp_ms(),
            )?
            .ok_or_else(|| {
                VibexError::validation(
                    "agent_runtime_probe_not_found",
                    "Agent runtime probe was not found",
                )
            })?;
            if record.status != AgentRuntimeProbeStatus::Running {
                return Ok(record);
            }
            record
        };

        let outcome = self.execute_probe_stages(&mut record).await;
        let conn = self.open_probe_connection()?;
        let mut latest = AgentRuntimeProbeRepository::get(&conn, &probe_id)?.ok_or_else(|| {
            VibexError::validation(
                "agent_runtime_probe_not_found",
                "Agent runtime probe disappeared before completion",
            )
        })?;
        if latest.is_terminal() {
            return Ok(latest);
        }

        match outcome {
            Ok(result) => {
                let evidence = self.build_evidence(&record, result);
                latest.status = aggregate_probe_status(&evidence);
                latest.diagnostic_code = aggregate_probe_diagnostic(&evidence, latest.status);
                latest.facts = evidence.facts.clone();
                latest.evidence = Some(evidence);
            }
            Err(error) if error.code == "agent_runtime_probe_cancelled" => {
                latest.status = AgentRuntimeProbeStatus::Cancelled;
                latest.diagnostic_code = Some("probe_cancelled".to_string());
                latest.facts = record.facts;
            }
            Err(error) if error.code == "agent_runtime_probe_timeout" => {
                latest.status = AgentRuntimeProbeStatus::TimedOut;
                latest.diagnostic_code = Some("probe_timeout".to_string());
                latest.facts = record.facts;
            }
            Err(error) => {
                latest.status = if is_blocking_probe_error(&error.code) {
                    AgentRuntimeProbeStatus::Blocked
                } else {
                    AgentRuntimeProbeStatus::Failed
                };
                latest.diagnostic_code = Some(safe_code(&error.code));
                latest.facts = record.facts;
            }
        }
        latest.stage = AgentRuntimeProbeStage::Completed;
        let now_ms = unix_timestamp_ms();
        latest.finished_at_ms = Some(now_ms);
        latest.updated_at_ms = now_ms.max(latest.updated_at_ms);
        let expected_revision = latest.revision;
        latest.revision = latest.revision.saturating_add(1).max(1);
        AgentRuntimeProbeRepository::update(&conn, &latest, expected_revision)
    }

    async fn execute_probe_stages(
        &self,
        record: &mut AgentRuntimeProbeRecord,
    ) -> VibexResult<ProbeExecutionResult> {
        let deadline = Instant::now() + Duration::from_millis(record.request.timeout_ms);
        self.transition_probe_stage(record, AgentRuntimeProbeStage::ResolvingIdentity)?;
        let conn = self.open_probe_connection()?;
        let runtime =
            AgentRuntimeProfileRepository::get(&conn, &record.request.runtime_profile_id)?
                .ok_or_else(|| {
                    VibexError::validation(
                        "agent_runtime_probe_prerequisite_missing",
                        "runtime profile disappeared before probe execution",
                    )
                })?;
        let resolution =
            AgentProviderProjectionRegistry::builtin()?.resolve(&runtime.version_identity)?;
        if resolution.descriptor.id != record.descriptor_id
            || resolution.descriptor.descriptor_version != record.descriptor_version
        {
            return Err(VibexError::conflict(
                "agent_runtime_probe_descriptor_stale",
                "runtime identity no longer matches the descriptor reserved for this probe",
            ));
        }
        self.ensure_probe_not_cancelled(record)?;

        let root = self.probe_root(&record.id)?;
        let _root_guard = ProbeRootGuard::new(root.clone());
        self.transition_probe_stage(record, AgentRuntimeProbeStage::PlanningProjection)?;
        if Instant::now() >= deadline {
            return Err(VibexError::process(
                "agent_runtime_probe_timeout",
                "runtime probe exceeded its deadline while planning projection",
            ));
        }
        let projection_context =
            self.plan_probe_projection(&conn, record, &runtime, resolution.match_kind, &root)?;
        if Instant::now() >= deadline {
            return Err(VibexError::process(
                "agent_runtime_probe_timeout",
                "runtime probe exceeded its deadline while planning projection",
            ));
        }
        drop(conn);
        self.ensure_probe_not_cancelled(record)?;

        self.transition_probe_stage(record, AgentRuntimeProbeStage::StartingProcess)?;
        let workspace = root.join("workspace");
        let home = root.join("home");
        let config_home = root.join("config");
        let data_home = root.join("data");
        let state_home = root.join("state");
        let cache_home = root.join("cache");
        for path in [
            &workspace,
            &home,
            &config_home,
            &data_home,
            &state_home,
            &cache_home,
        ] {
            fs::create_dir_all(path).map_err(|error| {
                VibexError::storage(
                    "agent_runtime_probe_root_create_failed",
                    "failed to create the isolated runtime probe root",
                )
                .with_diagnostic("errorKind", format!("{:?}", error.kind()))
            })?;
        }

        let profile_id = projection_context.provider_profile_id.ok_or_else(|| {
            VibexError::validation(
                "agent_runtime_probe_prerequisite_missing",
                "no ACP provider profile is available for this runtime probe",
            )
        })?;
        let profile = self
            .provider_config_service()
            .get_profile(&profile_id)?
            .ok_or_else(|| {
                VibexError::validation(
                    "agent_runtime_probe_prerequisite_missing",
                    "ACP provider profile is unavailable for this runtime probe",
                )
            })?;
        let config = self.profile_config(&profile_id)?;
        let process_args = materialized_probe_process_args(
            profile.agent_id.as_str(),
            &config,
            projection_context.projection.as_ref(),
        );
        let mut env = self.resolve_probe_config_env(&profile, &config)?;
        if let Some(projection) = &projection_context.projection {
            env.extend(projection.child_environment());
        }
        add_isolation_environment(
            &mut env,
            &home,
            &config_home,
            &data_home,
            &state_home,
            &cache_home,
        );
        let cwd = workspace;
        let (strategy, fallback) = self.pool_decision(&profile_id, &config)?;
        let runtime_resources = vibex_agent::ProviderRuntimeResources::default();
        let auth_source = vibex_core::RuntimeAuthSource::provider_profile(profile_id.clone());
        let env_unsets = Vec::new();
        let process = tokio::select! {
            result = timeout(
                deadline.saturating_duration_since(Instant::now()),
                self.spawn_process(
                    AcpProcessInstanceId::new(),
                    AcpProcessLaunch {
                        auth_source: &auth_source,
                        auth_source_revision: profile.updated_at_ms,
                        agent_id: &profile.agent_id,
                        config: &config,
                        cwd: &cwd,
                        runtime_resources: &runtime_resources,
                        env_unsets: &env_unsets,
                        purpose: AcpProcessPurpose::Probe,
                        process_strategy_effective: strategy,
                        pool_fallback_reason: fallback,
                    },
                    Some(process_args),
                    Some(env),
                    None,
                ),
            ) => match result {
                Ok(result) => result?,
                Err(_) => return Err(VibexError::process(
                    "agent_runtime_probe_timeout",
                    "runtime probe exceeded its deadline while starting process",
                )),
            },
            cancellation = self.wait_for_probe_cancel(&record.id) => {
                cancellation?;
                return Err(VibexError::conflict(
                    "agent_runtime_probe_cancelled",
                    "runtime probe cancellation was requested",
                ));
            }
        };

        let probe_result = self
            .run_process_probe(
                record,
                RunningProbeContext {
                    runtime: &runtime,
                    profile_id: &profile_id,
                    descriptor_match: projection_context.descriptor_match,
                    projection: projection_context.projection.as_ref(),
                    expected_provider_identities: &projection_context.expected_provider_identities,
                    cwd: &cwd,
                    process: process.clone(),
                    deadline,
                },
            )
            .await;
        process.shutdown().await;
        probe_result
    }

    async fn run_process_probe(
        &self,
        record: &mut AgentRuntimeProbeRecord,
        context: RunningProbeContext<'_>,
    ) -> VibexResult<ProbeExecutionResult> {
        let RunningProbeContext {
            runtime,
            profile_id,
            descriptor_match,
            projection,
            expected_provider_identities,
            cwd,
            process,
            deadline,
        } = context;
        let timeout_duration = deadline.saturating_duration_since(Instant::now());
        if timeout_duration.is_zero() {
            return Err(VibexError::process(
                "agent_runtime_probe_timeout",
                "runtime probe exceeded its deadline before ACP execution",
            ));
        }
        let probe_id = record.id.clone();
        let operation = async {
            self.transition_probe_stage(record, AgentRuntimeProbeStage::InitializingAcp)?;
            let initialize = process
                .request(
                    AcpOperation::Initialize.method(),
                    build_initialize_params(false, false, false, false, false),
                    timeout_duration.min(ACP_PROBE_TIMEOUT),
                )
                .await?;
            let agent_version = initialize
                .get("agentInfo")
                .and_then(|info| info.get("version"))
                .and_then(Value::as_str)
                .map(safe_identity_value)
                .filter(|value| !value.is_empty());
            let adapter_version = runtime.version_identity.adapter_version.clone();
            let mut facts = vec![
                binary_identity_fact(runtime, agent_version.as_deref()),
                AgentRuntimeProbeFact::passed(AgentRuntimeProbeCapability::AcpHandshake),
            ];
            record.facts = facts.clone();

            self.transition_probe_stage(record, AgentRuntimeProbeStage::Authenticating)?;
            facts.push(auth_fact(
                &initialize,
                profile_id,
                self.provider_config_service(),
            )?);
            record.facts = facts.clone();

            self.transition_probe_stage(record, AgentRuntimeProbeStage::CreatingOrLoadingSession)?;
            let session = process
                .request(
                    AcpOperation::SessionNew.method(),
                    build_session_new_params(cwd, json!([])),
                    timeout_duration.min(ACP_PROBE_TIMEOUT),
                )
                .await?;
            let native_session_id = session
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    VibexError::provider(
                        "agent_runtime_probe_session_id_missing",
                        "ACP session/new did not return a session identity",
                    )
                })?;
            // Keep the native id in this stack frame only. It is never copied
            // into the durable record or evidence.
            let _native_session_id_guard = native_session_id;
            facts.push(AgentRuntimeProbeFact::passed(
                AgentRuntimeProbeCapability::Session,
            ));
            record.facts = facts.clone();

            self.transition_probe_stage(record, AgentRuntimeProbeStage::DiscoveringModels)?;
            let models = extract_model_ids(&session);
            if models.is_empty() {
                facts.push(AgentRuntimeProbeFact {
                    capability: AgentRuntimeProbeCapability::ModelCatalog,
                    status: AgentRuntimeProbeFactStatus::Unsupported,
                    diagnostic_code: Some("model_catalog_unavailable".to_string()),
                });
            } else {
                facts.push(AgentRuntimeProbeFact::passed(
                    AgentRuntimeProbeCapability::ModelCatalog,
                ));
            }
            record.facts = facts.clone();

            self.transition_probe_stage(record, AgentRuntimeProbeStage::ApplyingModelAndConfig)?;
            let target_model = projection.and_then(|value| value.effective_model.as_deref());
            let mut model_apply_response = None;
            let model_fact = if let Some(model) = target_model {
                if !models.is_empty() && !models.iter().any(|candidate| candidate == model) {
                    AgentRuntimeProbeFact {
                        capability: AgentRuntimeProbeCapability::ModelSelection,
                        status: AgentRuntimeProbeFactStatus::Failed,
                        diagnostic_code: Some("target_model_not_advertised".to_string()),
                    }
                } else {
                    let result = process
                        .request(
                            AcpOperation::SessionSetModel.method(),
                            protocol::build_session_set_model_params(
                                _native_session_id_guard,
                                model,
                            ),
                            timeout_duration.min(ACP_PROBE_TIMEOUT),
                        )
                        .await;
                    match result {
                        Ok(response) => {
                            model_apply_response = Some(response);
                            AgentRuntimeProbeFact::passed(
                                AgentRuntimeProbeCapability::ModelSelection,
                            )
                        }
                        Err(error) if is_method_unsupported(&error) => AgentRuntimeProbeFact {
                            capability: AgentRuntimeProbeCapability::ModelSelection,
                            status: AgentRuntimeProbeFactStatus::Unsupported,
                            diagnostic_code: Some("session_model_mutation_unsupported".to_string()),
                        },
                        Err(_) => AgentRuntimeProbeFact {
                            capability: AgentRuntimeProbeCapability::ModelSelection,
                            status: AgentRuntimeProbeFactStatus::Failed,
                            diagnostic_code: Some("model_selection_failed".to_string()),
                        },
                    }
                }
            } else if extract_current_model_id(&session).is_some() {
                AgentRuntimeProbeFact::passed(AgentRuntimeProbeCapability::ModelSelection)
            } else {
                AgentRuntimeProbeFact {
                    capability: AgentRuntimeProbeCapability::ModelSelection,
                    status: AgentRuntimeProbeFactStatus::Unsupported,
                    diagnostic_code: Some("model_selection_unavailable".to_string()),
                }
            };
            facts.push(model_fact);
            record.facts = facts.clone();

            if record.request.minimal_prompt {
                self.transition_probe_stage(record, AgentRuntimeProbeStage::OptionalMinimalPrompt)?;
                let _ = process
                    .request(
                        AcpOperation::SessionPrompt.method(),
                        protocol::build_session_prompt_params(
                            _native_session_id_guard,
                            vec![json!({"type":"text","text":"VIBEX_RUNTIME_PROBE"})],
                        ),
                        timeout_duration.min(ACP_PROBE_TIMEOUT),
                    )
                    .await?;
            }

            self.transition_probe_stage(
                record,
                AgentRuntimeProbeStage::ConfirmingEffectiveProvider,
            )?;
            let provider_fact = effective_provider_fact(
                &initialize,
                &session,
                model_apply_response.as_ref(),
                projection,
                expected_provider_identities,
            );
            facts.push(provider_fact);
            let resume_fact = probe_session_resume(
                &initialize,
                &process,
                _native_session_id_guard,
                cwd,
                timeout_duration.min(ACP_PROBE_TIMEOUT),
            )
            .await;
            facts.push(resume_fact);
            facts.push(AgentRuntimeProbeFact::blocked(
                AgentRuntimeProbeCapability::SwitchCompatibility,
                "switch_compatibility_not_confirmed",
            ));
            facts.push(AgentRuntimeProbeFact::passed(
                AgentRuntimeProbeCapability::Redaction,
            ));
            record.facts = facts.clone();
            self.transition_probe_stage(record, AgentRuntimeProbeStage::CleaningUp)?;
            if Instant::now() >= deadline {
                return Err(VibexError::process(
                    "agent_runtime_probe_timeout",
                    "runtime probe exceeded its deadline",
                ));
            }
            Ok(ProbeExecutionResult {
                facts,
                agent_version,
                adapter_version,
                descriptor_match,
                projection_fingerprint: projection.map(|value| value.fingerprint.clone()),
                switch_behavior: projection
                    .map(|value| value.switch_behavior)
                    .unwrap_or(ProviderSwitchBehavior::RestartAndResume),
                source_survived_prepare_failure: false,
            })
        };
        tokio::select! {
            result = timeout(timeout_duration, operation) => match result {
                Ok(result) => result,
                Err(_) => Err(VibexError::process(
                    "agent_runtime_probe_timeout",
                    "runtime probe exceeded its deadline",
                )),
            },
            cancellation = self.wait_for_probe_cancel(&probe_id) => {
                cancellation?;
                Err(VibexError::conflict(
                    "agent_runtime_probe_cancelled",
                    "runtime probe cancellation was requested",
                ))
            }
        }
    }

    fn transition_probe_stage(
        &self,
        record: &mut AgentRuntimeProbeRecord,
        stage: AgentRuntimeProbeStage,
    ) -> VibexResult<()> {
        self.ensure_probe_not_cancelled(record)?;
        let expected_revision = record.revision;
        record.stage = stage;
        record.updated_at_ms = unix_timestamp_ms().max(record.updated_at_ms);
        record.revision = record.revision.saturating_add(1).max(1);
        let conn = self.open_probe_connection()?;
        match AgentRuntimeProbeRepository::update(&conn, record, expected_revision) {
            Ok(updated) => {
                *record = updated;
                Ok(())
            }
            Err(error) if error.code == "agent_runtime_probe_revision_conflict" => {
                let current = AgentRuntimeProbeRepository::get(&conn, &record.id)?;
                if current.as_ref().is_some_and(|value| {
                    value.cancel_requested || value.status == AgentRuntimeProbeStatus::Cancelled
                }) {
                    Err(VibexError::conflict(
                        "agent_runtime_probe_cancelled",
                        "runtime probe cancellation was requested",
                    ))
                } else {
                    Err(error)
                }
            }
            Err(error) => Err(error),
        }
    }

    fn ensure_probe_not_cancelled(&self, record: &AgentRuntimeProbeRecord) -> VibexResult<()> {
        let conn = self.open_probe_connection()?;
        let current = AgentRuntimeProbeRepository::get(&conn, &record.id)?;
        if record.cancel_requested
            || current.as_ref().is_some_and(|value| {
                value.cancel_requested || value.status == AgentRuntimeProbeStatus::Cancelled
            })
        {
            Err(VibexError::conflict(
                "agent_runtime_probe_cancelled",
                "runtime probe cancellation was requested",
            ))
        } else {
            Ok(())
        }
    }

    async fn wait_for_probe_cancel(&self, probe_id: &AgentRuntimeProbeId) -> VibexResult<()> {
        let mut ticker = interval(Duration::from_millis(100));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let conn = self.open_probe_connection()?;
            let record = AgentRuntimeProbeRepository::get(&conn, probe_id)?.ok_or_else(|| {
                VibexError::validation(
                    "agent_runtime_probe_not_found",
                    "Agent runtime probe disappeared during execution",
                )
            })?;
            if record.cancel_requested || record.status == AgentRuntimeProbeStatus::Cancelled {
                return Ok(());
            }
        }
    }

    fn plan_probe_projection(
        &self,
        conn: &vibex_db::DbConnection,
        record: &AgentRuntimeProbeRecord,
        runtime: &AgentRuntimeProfile,
        descriptor_match: ProjectionDescriptorMatch,
        probe_root: &Path,
    ) -> VibexResult<ProbeProjectionContext> {
        let provider_profile_id = runtime.legacy_provider_profile_id.clone().or_else(|| {
            vibex_db::ProviderProfileRepository::list_by_agent(
                conn,
                &runtime.version_identity.route.agent_id,
                true,
            )
            .ok()
            .and_then(|profiles| {
                profiles
                    .into_iter()
                    .find(|profile| profile.status == vibex_core::ProviderProfileStatus::Enabled)
                    .map(|profile| profile.id)
            })
        });
        let Some(binding_id) = record.request.binding_id.as_ref() else {
            return Ok(ProbeProjectionContext {
                provider_profile_id,
                descriptor_match,
                projection: None,
                expected_provider_identities: Vec::new(),
            });
        };
        let binding =
            AgentModelProviderBindingRepository::get(conn, binding_id)?.ok_or_else(|| {
                VibexError::validation(
                    "agent_runtime_probe_binding_missing",
                    "runtime probe binding was not found",
                )
            })?;
        let plan = self
            .provider_config_service()
            .plan_agent_provider_projection(&vibex_core::AgentProviderProjectionPreviewRequest {
                binding_id: binding.id,
                workspace_key: record.request.workspace_key.clone(),
            })?;
        let expected_provider_identities = safe_projection_provider_identities(&plan.preview);
        let resolved = AgentProviderProjectionEngine::resolve_and_materialize(
            &plan,
            probe_root,
            &record.request.workspace_key,
        )?;
        Ok(ProbeProjectionContext {
            provider_profile_id,
            descriptor_match,
            projection: Some(resolved),
            expected_provider_identities,
        })
    }

    fn resolve_probe_config_env(
        &self,
        profile: &vibex_core::ProviderProfile,
        config: &vibex_core::AcpProviderConfig,
    ) -> VibexResult<Vec<(String, String)>> {
        let mut values = Vec::new();
        for reference in &config.env {
            let key = reference.key.trim();
            if key.is_empty() || is_probe_environment_key(key) {
                continue;
            }
            let value = match reference.source {
                vibex_core::AcpProviderEnvSource::Literal => reference.value.clone(),
                vibex_core::AcpProviderEnvSource::ProcessEnvironment => std::env::var(key).ok(),
                vibex_core::AcpProviderEnvSource::SecretReference => reference
                    .secret_lookup_key
                    .as_deref()
                    .and_then(|lookup_key| {
                        profile
                            .secrets
                            .iter()
                            .find(|secret| secret.lookup_key == lookup_key)
                            .and_then(|secret| resolve_provider_secret(secret).ok().flatten())
                    }),
            };
            if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
                values.push((key.to_string(), value));
            }
        }
        Ok(values)
    }

    fn probe_root(&self, probe_id: &AgentRuntimeProbeId) -> VibexResult<PathBuf> {
        let root = self.probe_root_path(probe_id)?;
        fs::create_dir_all(&root).map_err(|error| {
            VibexError::storage(
                "agent_runtime_probe_root_create_failed",
                "failed to create isolated runtime probe root",
            )
            .with_diagnostic("errorKind", format!("{:?}", error.kind()))
        })?;
        Ok(root)
    }

    fn probe_root_path(&self, probe_id: &AgentRuntimeProbeId) -> VibexResult<PathBuf> {
        let parent = self
            .provider_config_service()
            .database_path()
            .parent()
            .ok_or_else(|| {
                VibexError::storage(
                    "agent_runtime_probe_parent_missing",
                    "database path has no parent for runtime probe isolation",
                )
            })?;
        let root = parent
            .join("runtime")
            .join("probes")
            .join(probe_id.as_str());
        Ok(root)
    }

    fn cleanup_probe_root(&self, probe_id: &AgentRuntimeProbeId) -> VibexResult<()> {
        let root = self.probe_root_path(probe_id)?;
        if !root.exists() {
            return Ok(());
        }
        fs::remove_dir_all(&root).map_err(|error| {
            VibexError::storage(
                "agent_runtime_probe_recovery_cleanup_failed",
                "failed to clean an interrupted runtime probe root",
            )
            .with_diagnostic("errorKind", format!("{:?}", error.kind()))
        })
    }

    fn build_evidence(
        &self,
        record: &AgentRuntimeProbeRecord,
        result: ProbeExecutionResult,
    ) -> vibex_core::AgentRuntimeProbeEvidence {
        vibex_core::AgentRuntimeProbeEvidence {
            schema_version: vibex_core::AGENT_RUNTIME_PROBE_SCHEMA_VERSION,
            agent_id: record.agent_id.clone(),
            agent_version: result.agent_version,
            adapter_id: record.adapter_id.clone(),
            adapter_version: result.adapter_version,
            descriptor_id: record.descriptor_id.clone(),
            descriptor_version: record.descriptor_version.clone(),
            descriptor_match: result.descriptor_match,
            projection_fingerprint: result.projection_fingerprint,
            source_revision: PROBE_SOURCE_REVISION.to_string(),
            platform_os: std::env::consts::OS.to_string(),
            platform_arch: std::env::consts::ARCH.to_string(),
            facts: result.facts,
            switch_behavior: result.switch_behavior,
            source_survived_prepare_failure: result.source_survived_prepare_failure,
            redaction_passed: true,
            recorded_at_ms: unix_timestamp_ms(),
        }
    }
}

fn add_isolation_environment(
    env: &mut Vec<(String, String)>,
    home: &Path,
    config: &Path,
    data: &Path,
    state: &Path,
    cache: &Path,
) {
    // Projection env is assembled from several sources and may contain the
    // same key more than once. Remove every protected entry before appending
    // the probe-owned values so a later duplicate cannot escape the sandbox.
    env.retain(|(name, _)| !is_probe_environment_key(name));
    for (key, value) in [
        ("HOME", home),
        ("USERPROFILE", home),
        ("XDG_CONFIG_HOME", config),
        ("XDG_DATA_HOME", data),
        ("XDG_STATE_HOME", state),
        ("XDG_CACHE_HOME", cache),
        ("CODEX_HOME", home),
    ] {
        let value = value.to_string_lossy().into_owned();
        env.push((key.to_string(), value));
    }
}

fn is_probe_environment_key(name: &str) -> bool {
    PROBE_ENV_KEYS
        .iter()
        .any(|protected| name.eq_ignore_ascii_case(protected))
}

async fn probe_session_resume(
    initialize: &Value,
    process: &Arc<AcpProcess>,
    native_session_id: &str,
    cwd: &Path,
    request_timeout: Duration,
) -> AgentRuntimeProbeFact {
    let Some(capabilities) = initialize.get("agentCapabilities") else {
        return AgentRuntimeProbeFact {
            capability: AgentRuntimeProbeCapability::SessionResume,
            status: AgentRuntimeProbeFactStatus::Unsupported,
            diagnostic_code: Some("session_resume_unsupported".to_string()),
        };
    };
    let supports_load = capabilities
        .get("loadSession")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let supports_resume = capabilities
        .get("sessionCapabilities")
        .and_then(|value| value.get("resume"))
        .is_some();
    if !supports_resume && !supports_load {
        return AgentRuntimeProbeFact {
            capability: AgentRuntimeProbeCapability::SessionResume,
            status: AgentRuntimeProbeFactStatus::Unsupported,
            diagnostic_code: Some("session_resume_unsupported".to_string()),
        };
    }

    // Match the live runtime's preference order. If negotiated resume is
    // unavailable at the operation boundary, a declared session/load method
    // is a valid fallback for this capability.
    let operations = [
        (supports_resume, AcpOperation::SessionResume),
        (supports_load, AcpOperation::SessionLoad),
    ];
    let mut saw_unsupported = false;
    for (advertised, operation) in operations {
        if !advertised {
            continue;
        }
        let params = match operation {
            AcpOperation::SessionResume => {
                build_session_resume_params(native_session_id, cwd, json!([]))
            }
            AcpOperation::SessionLoad => {
                build_session_load_params(native_session_id, cwd, json!([]))
            }
            _ => unreachable!("session restore probe only uses restore operations"),
        };
        match process
            .request(operation.method(), params, request_timeout)
            .await
        {
            Ok(response) => {
                match validate_restore_response(operation, &response, native_session_id) {
                    Ok(()) => {
                        return AgentRuntimeProbeFact::passed(
                            AgentRuntimeProbeCapability::SessionResume,
                        );
                    }
                    Err(error) => {
                        return AgentRuntimeProbeFact {
                            capability: AgentRuntimeProbeCapability::SessionResume,
                            status: AgentRuntimeProbeFactStatus::Failed,
                            diagnostic_code: Some(safe_code(&error.code)),
                        };
                    }
                }
            }
            Err(error) if is_method_unsupported(&error) => {
                saw_unsupported = true;
            }
            Err(error) => {
                return AgentRuntimeProbeFact {
                    capability: AgentRuntimeProbeCapability::SessionResume,
                    status: AgentRuntimeProbeFactStatus::Failed,
                    diagnostic_code: Some(safe_code(&error.code)),
                };
            }
        }
    }
    AgentRuntimeProbeFact {
        capability: AgentRuntimeProbeCapability::SessionResume,
        status: AgentRuntimeProbeFactStatus::Unsupported,
        diagnostic_code: Some(
            if saw_unsupported {
                "session_resume_method_unsupported"
            } else {
                "session_resume_unsupported"
            }
            .to_string(),
        ),
    }
}

fn auth_fact(
    initialize: &Value,
    profile_id: &vibex_core::ProviderProfileId,
    config_service: &ProviderConfigService,
) -> VibexResult<AgentRuntimeProbeFact> {
    let explicit_ready = initialize
        .get("auth")
        .and_then(|auth| auth.get("ready").or_else(|| auth.get("authenticated")))
        .and_then(Value::as_bool)
        .or_else(|| initialize.get("authenticated").and_then(Value::as_bool));
    if explicit_ready.is_some() {
        return Ok(classify_authentication(explicit_ready, false));
    }
    let profile = config_service.get_profile(profile_id)?;
    let Some(profile) = profile else {
        return Ok(AgentRuntimeProbeFact::blocked(
            AgentRuntimeProbeCapability::Authentication,
            "provider_profile_missing",
        ));
    };
    let has_credential = profile.secrets.iter().any(|secret| {
        matches!(
            secret.setup_state,
            vibex_core::ProviderSecretSetupState::Available
                | vibex_core::ProviderSecretSetupState::Referenced
        )
    });
    Ok(classify_authentication(None, has_credential))
}

fn classify_authentication(
    explicit_ready: Option<bool>,
    has_credential_reference: bool,
) -> AgentRuntimeProbeFact {
    match explicit_ready {
        Some(true) => AgentRuntimeProbeFact::passed(AgentRuntimeProbeCapability::Authentication),
        Some(false) => AgentRuntimeProbeFact {
            capability: AgentRuntimeProbeCapability::Authentication,
            status: AgentRuntimeProbeFactStatus::Failed,
            diagnostic_code: Some("authentication_not_ready".to_string()),
        },
        None if has_credential_reference => AgentRuntimeProbeFact::blocked(
            AgentRuntimeProbeCapability::Authentication,
            "authentication_not_confirmed",
        ),
        None => AgentRuntimeProbeFact::blocked(
            AgentRuntimeProbeCapability::Authentication,
            "authentication_not_configured",
        ),
    }
}

fn effective_provider_fact(
    initialize: &Value,
    session: &Value,
    model_apply_response: Option<&Value>,
    projection: Option<&vibex_config_switch::ResolvedAgentProviderProjection>,
    expected_provider_identities: &[String],
) -> AgentRuntimeProbeFact {
    let values = model_apply_response
        .into_iter()
        .chain([session, initialize]);
    let explicit = values.clone().find_map(|value| {
        [
            "effectiveProvider",
            "provider",
            "providerId",
            "modelProvider",
            "endpoint",
        ]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str).map(str::trim))
        .filter(|value| !value.is_empty())
    });
    let target_model = projection.and_then(|value| value.effective_model.as_deref());
    let current_model = model_apply_response
        .and_then(extract_current_model_id)
        .or_else(|| extract_current_model_id(session))
        .or_else(|| extract_current_model_id(initialize));
    classify_effective_provider(
        explicit,
        expected_provider_identities,
        target_model,
        current_model.as_deref(),
    )
}

fn classify_effective_provider(
    explicit_provider: Option<&str>,
    expected_provider_identities: &[String],
    target_model: Option<&str>,
    current_model: Option<&str>,
) -> AgentRuntimeProbeFact {
    let observed_provider = explicit_provider.and_then(normalize_provider_identity);
    let provider_matches = observed_provider.as_ref().is_some_and(|observed| {
        expected_provider_identities
            .iter()
            .any(|expected| expected.eq_ignore_ascii_case(observed))
    });
    if provider_matches && target_model.is_some() && current_model == target_model {
        AgentRuntimeProbeFact::passed(AgentRuntimeProbeCapability::ProviderProjection)
    } else {
        AgentRuntimeProbeFact {
            capability: AgentRuntimeProbeCapability::ProviderProjection,
            status: AgentRuntimeProbeFactStatus::Blocked,
            diagnostic_code: Some(
                if explicit_provider.is_some()
                    && !expected_provider_identities.is_empty()
                    && !provider_matches
                {
                    "effective_provider_mismatch"
                } else if provider_matches && target_model.is_some() {
                    "effective_model_not_confirmed"
                } else {
                    "effective_provider_not_confirmed"
                }
                .to_string(),
            ),
        }
    }
}

fn safe_projection_provider_identities(preview: &AgentProviderProjectionPreview) -> Vec<String> {
    let mut identities = preview
        .targets
        .iter()
        .filter(|target| {
            target.field == "endpoint"
                && !target.value_preview.eq_ignore_ascii_case("not configured")
        })
        .filter_map(|target| normalize_provider_identity(&target.value_preview))
        .collect::<Vec<_>>();
    identities.sort();
    identities.dedup();
    identities
}

fn normalize_provider_identity(value: &str) -> Option<String> {
    let url = Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    Some(url.origin().ascii_serialization().to_ascii_lowercase())
}

fn binary_identity_fact(
    runtime: &AgentRuntimeProfile,
    observed_agent_version: Option<&str>,
) -> AgentRuntimeProbeFact {
    match (
        runtime.version_identity.agent_version.as_deref(),
        observed_agent_version,
    ) {
        (Some(expected), Some(observed)) if expected == observed => {
            AgentRuntimeProbeFact::passed(AgentRuntimeProbeCapability::BinaryIdentity)
        }
        (Some(_), Some(_)) => AgentRuntimeProbeFact {
            capability: AgentRuntimeProbeCapability::BinaryIdentity,
            status: AgentRuntimeProbeFactStatus::Failed,
            diagnostic_code: Some("agent_version_mismatch".to_string()),
        },
        (Some(_), None) | (None, None) => AgentRuntimeProbeFact::blocked(
            AgentRuntimeProbeCapability::BinaryIdentity,
            "agent_version_not_reported",
        ),
        (None, Some(_)) => {
            AgentRuntimeProbeFact::passed(AgentRuntimeProbeCapability::BinaryIdentity)
        }
    }
}

fn aggregate_probe_status(
    evidence: &vibex_core::AgentRuntimeProbeEvidence,
) -> AgentRuntimeProbeStatus {
    if evidence
        .facts
        .iter()
        .any(|fact| fact.status == AgentRuntimeProbeFactStatus::Failed)
    {
        return AgentRuntimeProbeStatus::Failed;
    }
    if evidence.facts.iter().any(|fact| {
        is_provider_verification_fact(fact.capability)
            && fact.status != AgentRuntimeProbeFactStatus::Passed
    }) || !evidence.provider_projection_verified()
    {
        return AgentRuntimeProbeStatus::Blocked;
    }
    AgentRuntimeProbeStatus::Passed
}

fn aggregate_probe_diagnostic(
    evidence: &vibex_core::AgentRuntimeProbeEvidence,
    status: AgentRuntimeProbeStatus,
) -> Option<String> {
    if status == AgentRuntimeProbeStatus::Passed {
        return None;
    }
    evidence
        .facts
        .iter()
        .find(|fact| fact.status == AgentRuntimeProbeFactStatus::Failed)
        .or_else(|| {
            evidence.facts.iter().find(|fact| {
                is_provider_verification_fact(fact.capability)
                    && fact.status != AgentRuntimeProbeFactStatus::Passed
            })
        })
        .and_then(|fact| fact.diagnostic_code.clone())
        .or_else(|| Some("provider_projection_unverified".to_string()))
}

fn is_provider_verification_fact(capability: AgentRuntimeProbeCapability) -> bool {
    matches!(
        capability,
        AgentRuntimeProbeCapability::BinaryIdentity
            | AgentRuntimeProbeCapability::AcpHandshake
            | AgentRuntimeProbeCapability::Authentication
            | AgentRuntimeProbeCapability::Session
            | AgentRuntimeProbeCapability::ModelSelection
            | AgentRuntimeProbeCapability::ProviderProjection
            | AgentRuntimeProbeCapability::Redaction
    )
}

fn is_blocking_probe_error(code: &str) -> bool {
    matches!(
        code,
        "agent_runtime_probe_prerequisite_missing"
            | "agent_runtime_probe_descriptor_stale"
            | "agent_runtime_profile_not_found"
            | "agent_model_provider_binding_not_found"
            | "provider_profile_missing"
    ) || (code.contains("secret")
        && (code.contains("missing") || code.contains("unavailable") || code.contains("reference")))
}

fn is_method_unsupported(error: &VibexError) -> bool {
    error.code.contains("method_not_found")
        || error.code.contains("unsupported")
        || error.code.contains("not_supported")
}

fn safe_code(code: &str) -> String {
    let mut value = code
        .chars()
        .filter(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || *ch == '_')
        .take(96)
        .collect::<String>();
    if value.len() < MIN_DIAGNOSTIC_LEN {
        value = "probe_failed".to_string();
    }
    value
}

fn safe_identity_value(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '@' | '+' | '/')
        })
        .take(192)
        .collect()
}

fn materialized_probe_process_args(
    agent_id: &str,
    config: &vibex_core::AcpProviderConfig,
    projection: Option<&vibex_config_switch::ResolvedAgentProviderProjection>,
) -> Vec<String> {
    let mut args = effective_acp_process_args(config, agent_id == "opencode");
    if let Some(projection) = projection {
        append_projection_process_args(agent_id, &mut args, &projection.process_args);
    }
    args
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::tempdir;
    use vibex_config_switch::ResolvedAgentProviderProjection;
    use vibex_core::{
        AGENT_RUNTIME_PROBE_SCHEMA_VERSION, AcpAdapterId, AcpProcessStrategy, AcpProviderConfig,
        AgentId, AgentModelProviderBindingId, AgentProviderProjectionDescriptorId,
        AgentRuntimeProbeEvidence,
    };

    use super::*;

    fn evidence(facts: Vec<AgentRuntimeProbeFact>) -> AgentRuntimeProbeEvidence {
        AgentRuntimeProbeEvidence {
            schema_version: AGENT_RUNTIME_PROBE_SCHEMA_VERSION,
            agent_id: AgentId::parse("fixture").unwrap(),
            agent_version: Some("1.0.0".to_string()),
            adapter_id: AcpAdapterId::parse("fixture-acp").unwrap(),
            adapter_version: Some("1.0.0".to_string()),
            descriptor_id: AgentProviderProjectionDescriptorId::parse("projection_fixture_v1")
                .unwrap(),
            descriptor_version: "1".to_string(),
            descriptor_match: ProjectionDescriptorMatch::Exact,
            projection_fingerprint: Some("sha256:0123456789abcdef".to_string()),
            source_revision: "fixture".to_string(),
            platform_os: "linux".to_string(),
            platform_arch: "x86_64".to_string(),
            facts,
            switch_behavior: ProviderSwitchBehavior::RestartAndResume,
            source_survived_prepare_failure: false,
            redaction_passed: true,
            recorded_at_ms: 1,
        }
    }

    fn required_facts() -> Vec<AgentRuntimeProbeFact> {
        [
            AgentRuntimeProbeCapability::BinaryIdentity,
            AgentRuntimeProbeCapability::AcpHandshake,
            AgentRuntimeProbeCapability::Authentication,
            AgentRuntimeProbeCapability::Session,
            AgentRuntimeProbeCapability::ModelSelection,
            AgentRuntimeProbeCapability::ProviderProjection,
            AgentRuntimeProbeCapability::Redaction,
        ]
        .into_iter()
        .map(AgentRuntimeProbeFact::passed)
        .collect()
    }

    #[test]
    fn credential_reference_does_not_confirm_authentication() {
        let fact = classify_authentication(None, true);
        assert_eq!(fact.status, AgentRuntimeProbeFactStatus::Blocked);
        assert_eq!(
            fact.diagnostic_code.as_deref(),
            Some("authentication_not_confirmed")
        );
        assert_eq!(
            classify_authentication(Some(true), false).status,
            AgentRuntimeProbeFactStatus::Passed
        );
    }

    #[test]
    fn model_catalog_does_not_confirm_effective_provider_or_model() {
        let projection = ResolvedAgentProviderProjection {
            binding_id: AgentModelProviderBindingId::new(),
            non_secret_env: BTreeMap::new(),
            secret_env: Vec::new(),
            overlay_root: PathBuf::from("probe-root"),
            overlay_files: Vec::new(),
            process_args: Vec::new(),
            session_config: Vec::new(),
            effective_model: Some("target-model".to_string()),
            switch_behavior: ProviderSwitchBehavior::RestartAndResume,
            fingerprint: "sha256:0123456789abcdef".to_string(),
        };
        let session = json!({"models": {"availableModels": [{"modelId": "target-model"}]}});
        let fact = effective_provider_fact(&json!({}), &session, None, Some(&projection), &[]);
        assert_eq!(fact.status, AgentRuntimeProbeFactStatus::Blocked);
        assert_eq!(
            fact.diagnostic_code.as_deref(),
            Some("effective_provider_not_confirmed")
        );
    }

    #[test]
    fn matching_model_does_not_confirm_the_wrong_provider_identity() {
        let fact = classify_effective_provider(
            Some("https://profile-a.example/v1"),
            &["https://profile-b.example".to_string()],
            Some("shared-model"),
            Some("shared-model"),
        );
        assert_eq!(fact.status, AgentRuntimeProbeFactStatus::Blocked);
        assert_eq!(
            fact.diagnostic_code.as_deref(),
            Some("effective_provider_mismatch")
        );
    }

    #[test]
    fn matching_safe_origin_and_model_confirm_provider_projection() {
        let fact = classify_effective_provider(
            Some("HTTPS://PROFILE-B.EXAMPLE:443/v1/models?request=probe"),
            &["https://profile-b.example".to_string()],
            Some("shared-model"),
            Some("shared-model"),
        );
        assert_eq!(fact.status, AgentRuntimeProbeFactStatus::Passed);
        assert_eq!(fact.diagnostic_code, None);
    }

    #[test]
    fn missing_expected_provider_identity_remains_blocked() {
        let fact = classify_effective_provider(
            Some("https://profile-b.example"),
            &[],
            Some("shared-model"),
            Some("shared-model"),
        );
        assert_eq!(fact.status, AgentRuntimeProbeFactStatus::Blocked);
        assert_eq!(
            fact.diagnostic_code.as_deref(),
            Some("effective_provider_not_confirmed")
        );
    }

    #[test]
    fn aggregate_fails_on_any_failed_fact_and_blocks_partial_success() {
        let mut facts = required_facts();
        assert_eq!(
            aggregate_probe_status(&evidence(facts.clone())),
            AgentRuntimeProbeStatus::Passed
        );
        facts.retain(|fact| fact.capability != AgentRuntimeProbeCapability::ProviderProjection);
        facts.push(AgentRuntimeProbeFact::blocked(
            AgentRuntimeProbeCapability::ProviderProjection,
            "effective_provider_not_confirmed",
        ));
        assert_eq!(
            aggregate_probe_status(&evidence(facts.clone())),
            AgentRuntimeProbeStatus::Blocked
        );
        facts.push(AgentRuntimeProbeFact {
            capability: AgentRuntimeProbeCapability::ModelCatalog,
            status: AgentRuntimeProbeFactStatus::Failed,
            diagnostic_code: Some("model_catalog_failed".to_string()),
        });
        assert_eq!(
            aggregate_probe_status(&evidence(facts)),
            AgentRuntimeProbeStatus::Failed
        );
    }

    #[test]
    fn probe_root_guard_removes_isolated_state() {
        let parent = tempdir().unwrap();
        let root = parent.path().join("probe");
        fs::create_dir_all(root.join("overlay")).unwrap();
        fs::write(root.join("overlay/config.json"), b"fixture").unwrap();
        drop(ProbeRootGuard::new(root.clone()));
        assert!(!root.exists());
    }

    #[test]
    fn probe_process_args_use_materialized_probe_overlay_paths() {
        let config = AcpProviderConfig {
            command: "fixture".to_string(),
            args: vec!["--verbose".to_string(), "acp".to_string()],
            env: Vec::new(),
            cwd_template: Some("{workspaceRoot}".to_string()),
            process_strategy: AcpProcessStrategy::PerSession,
            terminal_tools: false,
            terminal_auth: false,
            models: Vec::new(),
            modes: Vec::new(),
            features: Vec::new(),
            disabled_tools: Vec::new(),
        };
        let probe_root = PathBuf::from("probe-root")
            .join("binding")
            .join("workspace");

        let crow_projection = ResolvedAgentProviderProjection {
            binding_id: AgentModelProviderBindingId::new(),
            non_secret_env: BTreeMap::new(),
            secret_env: Vec::new(),
            overlay_root: probe_root.clone(),
            overlay_files: Vec::new(),
            process_args: vec![
                "--config-dir".to_string(),
                probe_root.to_string_lossy().into_owned(),
            ],
            session_config: Vec::new(),
            effective_model: Some("target-model".to_string()),
            switch_behavior: ProviderSwitchBehavior::RestartAndResume,
            fingerprint: "sha256:crow".to_string(),
        };
        assert_eq!(
            materialized_probe_process_args("crow-cli", &config, Some(&crow_projection)),
            vec![
                "--verbose",
                "acp",
                "--config-dir",
                probe_root.to_string_lossy().as_ref(),
            ]
        );

        let stakpak_config_path = probe_root.join("stakpak.toml");
        let stakpak_projection = ResolvedAgentProviderProjection {
            process_args: vec![
                "--profile".to_string(),
                "vibex".to_string(),
                "--config".to_string(),
                stakpak_config_path.to_string_lossy().into_owned(),
            ],
            fingerprint: "sha256:stakpak".to_string(),
            ..crow_projection
        };
        assert_eq!(
            materialized_probe_process_args("stakpak", &config, Some(&stakpak_projection)),
            vec![
                "--verbose",
                "--profile",
                "vibex",
                "--config",
                stakpak_config_path.to_string_lossy().as_ref(),
                "acp",
            ]
        );
    }

    #[test]
    fn isolation_environment_replaces_all_protected_duplicates() {
        let parent = tempdir().unwrap();
        let home = parent.path().join("home");
        let config = parent.path().join("config");
        let data = parent.path().join("data");
        let state = parent.path().join("state");
        let cache = parent.path().join("cache");
        let mut env = vec![
            ("HOME".to_string(), "/user/home".to_string()),
            ("HOME".to_string(), "/shadow/home".to_string()),
            ("Home".to_string(), "/windows-shadow/home".to_string()),
            ("CODEX_HOME".to_string(), "/user/codex".to_string()),
            (
                "Codex_Home".to_string(),
                "/windows-shadow/codex".to_string(),
            ),
            ("UNRELATED".to_string(), "keep".to_string()),
        ];

        add_isolation_environment(&mut env, &home, &config, &data, &state, &cache);

        assert_eq!(
            env.iter()
                .filter(|(key, _)| key.eq_ignore_ascii_case("HOME"))
                .count(),
            1
        );
        assert_eq!(
            env.iter()
                .filter(|(key, _)| key.eq_ignore_ascii_case("CODEX_HOME"))
                .count(),
            1
        );
        assert_eq!(
            env.iter()
                .find(|(key, _)| key == "HOME")
                .map(|(_, value)| value),
            Some(&home.to_string_lossy().into_owned())
        );
        assert_eq!(
            env.iter().find(|(key, _)| key == "UNRELATED"),
            Some(&("UNRELATED".to_string(), "keep".to_string()))
        );
    }
}

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio::time::sleep;
use vibex_core::{
    AcpAdapterId, ActiveWorkKind, AgentSessionRestoreResult, BindingState, BusyDisposition,
    RetrySemantics, RuntimeBinding, RuntimeBindingId, RuntimeSwitchActiveWorkPolicy,
    RuntimeSwitchId, RuntimeSwitchOperationId, RuntimeSwitchPolicy, RuntimeSwitchStatus,
    SessionRuntimeSelection, SwitchOperationStatus, VibexError, VibexResult, VibexSessionId,
    unix_timestamp_ms,
};
use vibex_db::{
    AgentSessionRuntimeRepository, DbConnection, RequestedSwitchClaimOutcome,
    RuntimeBindingRepository, RuntimeSwitchCommitRequest, RuntimeSwitchRecord,
    RuntimeSwitchRepository, RuntimeSwitchReserveRequest, SwitchOperationAppendRequest,
    SwitchOperationJournalRepository, SwitchOperationRecord, apply_migrations, open_database,
};

use crate::observability::{
    RuntimeLogContext, RuntimeLogLevel, RuntimeMetricName, RuntimeMetricOperation,
    RuntimeMetricResult, RuntimeObservability,
};

pub const OP_SPAWN_PROCESS: &str = "spawn_process";
pub const OP_RESTORE_SESSION: &str = "restore_session";
pub const OP_CREATE_SESSION: &str = "create_session";
pub const OP_APPLY_SESSION_CONFIG: &str = "apply_session_config";
pub const OP_APPLY_LIVE_MUTATION: &str = "apply_live_mutation";
pub const OP_CANCEL_ACTIVE_TURN: &str = "cancel_active_turn";
pub const OP_RESOLVE_PENDING_PERMISSION: &str = "resolve_pending_permission";
pub const OP_CLOSE_TERMINAL: &str = "close_terminal";
pub const OP_CANCEL_BACKGROUND_WORK: &str = "cancel_background_work";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSwitchRequest {
    pub session_id: VibexSessionId,
    pub idempotency_key: String,
    pub expected_revision: i64,
    pub expected_current_binding_id: Option<RuntimeBindingId>,
    pub desired_selection_revision: i64,
    pub target_adapter_id: AcpAdapterId,
    pub target_selection: SessionRuntimeSelection,
    pub requested_policy: RuntimeSwitchPolicy,
    pub active_work_policy: RuntimeSwitchActiveWorkPolicy,
    pub requested_session_config: Option<serde_json::Value>,
}

impl fmt::Debug for RuntimeSwitchRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeSwitchRequest")
            .field("session_id", &self.session_id)
            .field("has_idempotency_key", &!self.idempotency_key.is_empty())
            .field("expected_revision", &self.expected_revision)
            .field(
                "expected_current_binding_id",
                &self.expected_current_binding_id,
            )
            .field(
                "desired_selection_revision",
                &self.desired_selection_revision,
            )
            .field("target_adapter_id", &self.target_adapter_id)
            .field("target_selection", &self.target_selection)
            .field("requested_policy", &self.requested_policy)
            .field("active_work_policy", &self.active_work_policy)
            .field(
                "has_requested_session_config",
                &self.requested_session_config.is_some(),
            )
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SwitchIntent {
    pub switch_id: RuntimeSwitchId,
    pub session_id: VibexSessionId,
    pub source_revision: i64,
    pub source_binding_id: Option<RuntimeBindingId>,
    pub desired_selection_revision: i64,
    pub target_binding_id: Option<RuntimeBindingId>,
    pub target_adapter_id: AcpAdapterId,
    pub target_selection: SessionRuntimeSelection,
    pub requested_policy: RuntimeSwitchPolicy,
    pub active_work_policy: RuntimeSwitchActiveWorkPolicy,
    pub requested_session_config: Option<serde_json::Value>,
    pub created_at_ms: i64,
}

impl fmt::Debug for SwitchIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SwitchIntent")
            .field("switch_id", &self.switch_id)
            .field("session_id", &self.session_id)
            .field("source_revision", &self.source_revision)
            .field("source_binding_id", &self.source_binding_id)
            .field(
                "desired_selection_revision",
                &self.desired_selection_revision,
            )
            .field("target_binding_id", &self.target_binding_id)
            .field("target_adapter_id", &self.target_adapter_id)
            .field("target_selection", &self.target_selection)
            .field("requested_policy", &self.requested_policy)
            .field("active_work_policy", &self.active_work_policy)
            .field(
                "has_requested_session_config",
                &self.requested_session_config.is_some(),
            )
            .field("created_at_ms", &self.created_at_ms)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DurableRequestedConfig {
    effective_selection: SessionRuntimeSelection,
    session_config: Option<serde_json::Value>,
}

impl SwitchIntent {
    fn from_record(record: &RuntimeSwitchRecord) -> VibexResult<Self> {
        let durable: DurableRequestedConfig = record
            .requested_session_config
            .clone()
            .ok_or_else(|| {
                VibexError::validation(
                    "runtime_switch_requested_config_missing",
                    "runtime switch durable requested configuration is missing",
                )
            })
            .and_then(|value| {
                serde_json::from_value(value).map_err(|_| {
                    VibexError::validation(
                        "runtime_switch_requested_config_invalid",
                        "runtime switch durable requested configuration is invalid",
                    )
                })
            })?;
        if durable.effective_selection.agent_id != record.target_agent_id
            || durable.effective_selection.provider_profile_id != record.target_profile_id
        {
            return Err(VibexError::conflict(
                "runtime_switch_intent_route_mismatch",
                "runtime switch durable selection does not match its target route",
            ));
        }
        let active_work_policy = record.active_work_policy.unwrap_or_default();
        active_work_policy.validate()?;
        Ok(Self {
            switch_id: record.switch_id.clone(),
            session_id: record.session_id.clone(),
            source_revision: record.source_revision,
            source_binding_id: record.source_binding_id.clone(),
            desired_selection_revision: record.desired_selection_revision,
            target_binding_id: record.target_binding_id.clone(),
            target_adapter_id: record.target_adapter_id.clone(),
            target_selection: durable.effective_selection,
            requested_policy: record
                .requested_policy
                .unwrap_or(RuntimeSwitchPolicy::Automatic),
            active_work_policy,
            requested_session_config: durable.session_config,
            created_at_ms: record.created_at_ms,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreAssessment {
    Compatible,
    ProbeAllowed,
    Incompatible,
    NoNativeSession,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchTargetAssessment {
    pub same_route: bool,
    pub process_config_changed: bool,
    pub session_scoped_changes_only: bool,
    pub live_ops_supported: bool,
    pub exact_descriptor: bool,
    pub runtime_evidence_verified: bool,
    pub projection_fingerprint_matches: bool,
    pub active_turn: bool,
    pub restore: RestoreAssessment,
    pub resumable_historical_binding: bool,
    pub supports_client_idempotency: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSwitchStrategy {
    LiveMutation,
    RestartAndResume,
    RestartFreshAndBridge,
}

pub fn decide_switch_strategy(
    policy: RuntimeSwitchPolicy,
    assessment: &SwitchTargetAssessment,
) -> RuntimeSwitchStrategy {
    if policy == RuntimeSwitchPolicy::ForceFreshSession {
        return RuntimeSwitchStrategy::RestartFreshAndBridge;
    }
    if policy != RuntimeSwitchPolicy::PreferResume
        && assessment.same_route
        && !assessment.process_config_changed
        && assessment.session_scoped_changes_only
        && assessment.live_ops_supported
        && assessment.exact_descriptor
        && assessment.runtime_evidence_verified
        && assessment.projection_fingerprint_matches
        && !assessment.active_turn
    {
        return RuntimeSwitchStrategy::LiveMutation;
    }
    if matches!(
        assessment.restore,
        RestoreAssessment::Compatible | RestoreAssessment::ProbeAllowed
    ) || assessment.resumable_historical_binding
    {
        RuntimeSwitchStrategy::RestartAndResume
    } else {
        RuntimeSwitchStrategy::RestartFreshAndBridge
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PreparedProcess {
    pub opaque_handle: String,
}

impl fmt::Debug for PreparedProcess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedProcess")
            .field("opaque_handle", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PreparedAttachment {
    pub binding: RuntimeBinding,
    pub opaque_handle: String,
    pub restore_result: Option<AgentSessionRestoreResult>,
}

impl fmt::Debug for PreparedAttachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedAttachment")
            .field("binding_id", &self.binding.binding_id)
            .field("session_id", &self.binding.session_id)
            .field("opaque_handle", &"<redacted>")
            .field("has_restore_result", &self.restore_result.is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct JournaledOperation {
    pub operation_id: RuntimeSwitchOperationId,
    pub sequence: i64,
    pub operation_kind: String,
    pub adapter_idempotency_token: Option<String>,
}

impl fmt::Debug for JournaledOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JournaledOperation")
            .field("operation_id", &self.operation_id)
            .field("sequence", &self.sequence)
            .field("operation_kind", &self.operation_kind)
            .field(
                "has_adapter_idempotency_token",
                &self.adapter_idempotency_token.is_some(),
            )
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum OperationReconcileOutcome {
    Confirmed {
        native_result_reference: Option<String>,
    },
    NotFound,
    Ambiguous,
}

impl fmt::Debug for OperationReconcileOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Confirmed {
                native_result_reference,
            } => formatter
                .debug_struct("Confirmed")
                .field(
                    "has_native_result_reference",
                    &native_result_reference.is_some(),
                )
                .finish(),
            Self::NotFound => formatter.write_str("NotFound"),
            Self::Ambiguous => formatter.write_str("Ambiguous"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ActiveWorkSnapshot {
    pub active_turn: bool,
    pub pending_permission: bool,
    pub active_terminal: bool,
    pub background_work: bool,
}

impl ActiveWorkSnapshot {
    pub fn is_active(self, kind: ActiveWorkKind) -> bool {
        match kind {
            ActiveWorkKind::ActiveTurn => self.active_turn,
            ActiveWorkKind::PendingPermission => self.pending_permission,
            ActiveWorkKind::ActiveTerminal => self.active_terminal,
            ActiveWorkKind::BackgroundWork => self.background_work,
        }
    }
}

#[async_trait]
pub trait SwitchTargetExecutor: Send + Sync {
    async fn assess_target(&self, intent: &SwitchIntent) -> VibexResult<SwitchTargetAssessment>;

    async fn ensure_process(
        &self,
        intent: &SwitchIntent,
        operation: &JournaledOperation,
    ) -> VibexResult<PreparedProcess>;

    async fn reacquire_process(&self, intent: &SwitchIntent) -> VibexResult<PreparedProcess>;

    async fn restore_or_create_session(
        &self,
        intent: &SwitchIntent,
        process: &PreparedProcess,
        strategy: RuntimeSwitchStrategy,
        operation: &JournaledOperation,
    ) -> VibexResult<PreparedAttachment>;

    async fn recover_attachment(
        &self,
        intent: &SwitchIntent,
        operation: &SwitchOperationRecord,
    ) -> VibexResult<PreparedAttachment>;

    async fn acquire_prepared(
        &self,
        intent: &SwitchIntent,
        binding: &RuntimeBinding,
    ) -> VibexResult<PreparedAttachment>;

    async fn apply_session_config(
        &self,
        intent: &SwitchIntent,
        attachment: &PreparedAttachment,
        operation: &JournaledOperation,
    ) -> VibexResult<()>;

    async fn apply_live_mutation(
        &self,
        intent: &SwitchIntent,
        attachment: &PreparedAttachment,
        operation: &JournaledOperation,
    ) -> VibexResult<()>;

    async fn build_context_delta(
        &self,
        _intent: &SwitchIntent,
        _attachment: &PreparedAttachment,
    ) -> VibexResult<()> {
        Ok(())
    }

    async fn revalidate_prepared(
        &self,
        intent: &SwitchIntent,
        attachment: &PreparedAttachment,
    ) -> VibexResult<()>;

    async fn activate(
        &self,
        intent: &SwitchIntent,
        attachment: &PreparedAttachment,
        activation_generation: i64,
    ) -> VibexResult<()>;

    async fn cleanup_target(
        &self,
        intent: &SwitchIntent,
        attachment: Option<&PreparedAttachment>,
    ) -> VibexResult<()>;

    async fn cleanup_source_after_commit(&self, _intent: &SwitchIntent) -> VibexResult<()> {
        Ok(())
    }

    async fn reconcile_operation(
        &self,
        intent: &SwitchIntent,
        operation: &SwitchOperationRecord,
    ) -> VibexResult<OperationReconcileOutcome>;
}

#[async_trait]
pub trait ActiveWorkGate: Send + Sync {
    async fn probe(&self, session_id: &VibexSessionId) -> VibexResult<ActiveWorkSnapshot>;

    async fn set_prompt_gate(&self, session_id: &VibexSessionId, closed: bool) -> VibexResult<()>;

    async fn cancel(
        &self,
        session_id: &VibexSessionId,
        kind: ActiveWorkKind,
        operation: &JournaledOperation,
    ) -> VibexResult<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchOutcome {
    pub switch_id: RuntimeSwitchId,
    pub status: RuntimeSwitchStatus,
    pub error_code: Option<String>,
}

impl From<&RuntimeSwitchRecord> for SwitchOutcome {
    fn from(record: &RuntimeSwitchRecord) -> Self {
        Self {
            switch_id: record.switch_id.clone(),
            status: record.status,
            error_code: record.error_code.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSwitchReconcileError {
    pub switch_id: RuntimeSwitchId,
    pub error_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeSwitchReconcileReport {
    pub outcomes: Vec<SwitchOutcome>,
    pub errors: Vec<RuntimeSwitchReconcileError>,
}

#[derive(Debug, Clone)]
pub struct RuntimeSwitchCoordinatorConfig {
    pub lease_duration_ms: i64,
    pub idle_poll_interval: Duration,
}

impl Default for RuntimeSwitchCoordinatorConfig {
    fn default() -> Self {
        Self {
            lease_duration_ms: 30_000,
            idle_poll_interval: Duration::from_millis(25),
        }
    }
}

struct RuntimeSwitchCoordinatorInner {
    db_path: PathBuf,
    executor: Arc<dyn SwitchTargetExecutor>,
    active_work: Arc<dyn ActiveWorkGate>,
    config: RuntimeSwitchCoordinatorConfig,
    observability: Arc<RuntimeObservability>,
    session_driver_locks: StdMutex<HashMap<String, Weak<AsyncMutex<()>>>>,
}

#[derive(Clone)]
pub struct RuntimeSwitchCoordinator {
    inner: Arc<RuntimeSwitchCoordinatorInner>,
}

enum OperationPlan {
    Skip(SwitchOperationRecord),
    Execute(JournaledOperation),
}

const ACTIVE_WORK_KINDS: [ActiveWorkKind; 4] = [
    ActiveWorkKind::ActiveTurn,
    ActiveWorkKind::PendingPermission,
    ActiveWorkKind::ActiveTerminal,
    ActiveWorkKind::BackgroundWork,
];

impl RuntimeSwitchCoordinator {
    pub fn new(
        db_path: impl Into<PathBuf>,
        executor: Arc<dyn SwitchTargetExecutor>,
        active_work: Arc<dyn ActiveWorkGate>,
        config: RuntimeSwitchCoordinatorConfig,
    ) -> VibexResult<Self> {
        Self::new_with_observability(
            db_path,
            executor,
            active_work,
            config,
            Arc::new(RuntimeObservability::new()),
        )
    }

    pub fn new_with_observability(
        db_path: impl Into<PathBuf>,
        executor: Arc<dyn SwitchTargetExecutor>,
        active_work: Arc<dyn ActiveWorkGate>,
        config: RuntimeSwitchCoordinatorConfig,
        observability: Arc<RuntimeObservability>,
    ) -> VibexResult<Self> {
        if config.lease_duration_ms <= 0 || config.idle_poll_interval.is_zero() {
            return Err(VibexError::validation(
                "runtime_switch_coordinator_config_invalid",
                "runtime switch coordinator durations must be positive",
            ));
        }
        let db_path = db_path.into();
        let mut conn = open_database(&db_path)?;
        apply_migrations(&mut conn)?;
        Ok(Self {
            inner: Arc::new(RuntimeSwitchCoordinatorInner {
                db_path,
                executor,
                active_work,
                config,
                observability,
                session_driver_locks: StdMutex::new(HashMap::new()),
            }),
        })
    }

    pub fn database_path(&self) -> &Path {
        &self.inner.db_path
    }

    pub fn observability(&self) -> Arc<RuntimeObservability> {
        self.inner.observability.clone()
    }

    pub async fn request_switch(
        &self,
        mut request: RuntimeSwitchRequest,
    ) -> VibexResult<SwitchOutcome> {
        request.idempotency_key = request.idempotency_key.trim().to_string();
        if request.idempotency_key.is_empty() {
            return Err(VibexError::validation(
                "runtime_switch_idempotency_key_required",
                "runtime switch idempotency key must not be empty",
            ));
        }
        if request.idempotency_key.len() > 256
            || request
                .idempotency_key
                .chars()
                .any(|character| character.is_control())
        {
            return Err(VibexError::validation(
                "runtime_switch_idempotency_key_invalid",
                "runtime switch idempotency key must be bounded and contain no control characters",
            ));
        }
        request.active_work_policy.validate()?;
        let durable_config = Self::encode_durable_requested_config(&request)?;
        RuntimeSwitchRepository::validate_requested_config(&durable_config)?;
        let coordinator = self.clone();
        tokio::spawn(async move { coordinator.request_switch_inner(request).await })
            .await
            .map_err(|_| {
                VibexError::process(
                    "runtime_switch_driver_join_failed",
                    "runtime switch driver stopped unexpectedly",
                )
            })?
    }

    /// Drives an already durable switch intent, including product-level
    /// `Requested` rows created atomically with desired selection state.
    pub async fn drive_switch(&self, switch_id: &RuntimeSwitchId) -> VibexResult<SwitchOutcome> {
        let record = self.required_switch(switch_id)?;
        self.claim_and_drive(record).await
    }

    pub fn encode_requested_config(
        target_selection: &SessionRuntimeSelection,
        session_config: Option<serde_json::Value>,
    ) -> VibexResult<serde_json::Value> {
        serde_json::to_value(DurableRequestedConfig {
            effective_selection: target_selection.clone(),
            session_config,
        })
        .map_err(|_| {
            VibexError::validation(
                "runtime_switch_requested_config_invalid",
                "runtime switch requested configuration could not be serialized",
            )
        })
    }

    pub async fn cancel_switch(
        &self,
        session_id: &VibexSessionId,
        switch_id: &RuntimeSwitchId,
    ) -> VibexResult<SwitchOutcome> {
        let owner = Self::new_worker_owner();
        let record = self.required_switch(switch_id)?;
        if record.session_id != *session_id {
            return Err(VibexError::conflict(
                "runtime_switch_session_mismatch",
                "runtime switch does not belong to the requested session",
            ));
        }
        if record.status.is_terminal() {
            return Ok(SwitchOutcome::from(&record));
        }
        if matches!(
            record.status,
            RuntimeSwitchStatus::Requested | RuntimeSwitchStatus::WaitingForIdle
        ) {
            let mut conn = self.open_connection()?;
            RuntimeSwitchRepository::cancel(&mut conn, session_id, switch_id)?;
            if record.status == RuntimeSwitchStatus::WaitingForIdle {
                let _ = self
                    .inner
                    .active_work
                    .set_prompt_gate(session_id, false)
                    .await;
            }
            return Ok(SwitchOutcome::from(&self.required_switch(switch_id)?));
        }
        if !self.try_acquire_lease(switch_id, &owner)? {
            return Err(VibexError::conflict(
                "runtime_switch_lease_held",
                "runtime switch is currently being driven by another worker",
            ));
        }
        let result = async {
            let current = self.required_switch(switch_id)?;
            if current.status == RuntimeSwitchStatus::Committing {
                return Err(VibexError::conflict(
                    "runtime_switch_commit_in_progress",
                    "runtime switch cannot be cancelled while commit is in progress",
                ));
            }
            if current.status.is_terminal() {
                return Ok(SwitchOutcome::from(&current));
            }
            let mut conn = self.open_connection()?;
            RuntimeSwitchRepository::cancel(&mut conn, session_id, switch_id)?;
            let intent = SwitchIntent::from_record(&current)?;
            if intent.target_binding_id != intent.source_binding_id
                && let Some(target_binding_id) = &intent.target_binding_id
                && self.get_binding(target_binding_id)?.is_some()
            {
                let conn = self.open_connection()?;
                RuntimeBindingRepository::update_state(
                    &conn,
                    target_binding_id,
                    BindingState::Failed,
                )?;
            }
            let _ = self.inner.executor.cleanup_target(&intent, None).await;
            let _ = self
                .inner
                .active_work
                .set_prompt_gate(session_id, false)
                .await;
            Ok(SwitchOutcome::from(&self.required_switch(switch_id)?))
        }
        .await;
        let _ = self.release_lease(switch_id, &owner);
        result
    }

    pub async fn reconcile_on_startup(&self) -> VibexResult<RuntimeSwitchReconcileReport> {
        let (switches, committed_current) = {
            let conn = self.open_connection()?;
            (
                RuntimeSwitchRepository::list_non_terminal(&conn)?,
                RuntimeSwitchRepository::list_committed_current(&conn)?,
            )
        };
        let mut report = RuntimeSwitchReconcileReport::default();
        for record in switches {
            let switch_id = record.switch_id.clone();
            match self.claim_and_drive(record).await {
                Ok(outcome) => report.outcomes.push(outcome),
                Err(error) => {
                    if let Ok(current) = self.required_switch(&switch_id) {
                        report.outcomes.push(SwitchOutcome::from(&current));
                    }
                    report.errors.push(RuntimeSwitchReconcileError {
                        switch_id,
                        error_code: error.code,
                    });
                }
            }
        }
        for record in committed_current {
            let intent = match SwitchIntent::from_record(&record) {
                Ok(intent) => intent,
                Err(error) => {
                    report.errors.push(RuntimeSwitchReconcileError {
                        switch_id: record.switch_id.clone(),
                        error_code: error.code,
                    });
                    continue;
                }
            };
            match self.activate_committed(&intent).await {
                Ok(()) => report.outcomes.push(SwitchOutcome::from(&record)),
                Err(error) => report.errors.push(RuntimeSwitchReconcileError {
                    switch_id: record.switch_id,
                    error_code: error.code,
                }),
            }
        }

        let pending = {
            let conn = self.open_connection()?;
            RuntimeSwitchRepository::list_sessions_with_pending_switch(&conn)?
        };
        for (session_id, switch_id) in pending {
            let Some(record) = self.get_switch(&switch_id)? else {
                let conn = self.open_connection()?;
                RuntimeSwitchRepository::clear_pending(&conn, &session_id, &switch_id)?;
                continue;
            };
            if record.status.is_terminal() {
                let conn = self.open_connection()?;
                RuntimeSwitchRepository::clear_pending(&conn, &session_id, &switch_id)?;
            }
        }
        for outcome in &report.outcomes {
            self.inner.observability.increment(
                RuntimeMetricName::SwitchReconciliation,
                None,
                metric_result_for_switch_status(outcome.status),
            );
        }
        for _ in &report.errors {
            self.inner.observability.increment(
                RuntimeMetricName::SwitchReconciliation,
                None,
                RuntimeMetricResult::Failure,
            );
        }
        Ok(report)
    }

    async fn request_switch_inner(
        &self,
        request: RuntimeSwitchRequest,
    ) -> VibexResult<SwitchOutcome> {
        if let Some(existing) =
            self.get_by_idempotency_key(&request.session_id, &request.idempotency_key)?
        {
            return self.claim_and_drive(existing).await;
        }

        let switch_id = RuntimeSwitchId::new();
        let target_binding_id = RuntimeBindingId::new();
        let durable_config = Self::encode_durable_requested_config(&request)?;
        let reserve_request = RuntimeSwitchReserveRequest {
            session_id: request.session_id,
            idempotency_key: request.idempotency_key,
            expected_revision: request.expected_revision,
            expected_current_binding_id: request.expected_current_binding_id,
            desired_selection_revision: request.desired_selection_revision,
            target_binding_id: Some(target_binding_id),
            target_agent_id: request.target_selection.agent_id,
            target_adapter_id: request.target_adapter_id,
            target_profile_id: request.target_selection.provider_profile_id,
            requested_policy: Some(serde_json::to_value(request.requested_policy).map_err(
                |_| {
                    VibexError::validation(
                        "runtime_switch_requested_policy_invalid",
                        "runtime switch policy could not be serialized",
                    )
                },
            )?),
            active_work_policy: Some(serde_json::to_value(request.active_work_policy).map_err(
                |_| {
                    VibexError::validation(
                        "runtime_switch_active_work_policy_invalid",
                        "runtime switch active work policy could not be serialized",
                    )
                },
            )?),
            requested_session_config: Some(durable_config),
        };
        let record = {
            let mut conn = self.open_connection()?;
            RuntimeSwitchRepository::reserve(&mut conn, switch_id, &reserve_request)?
        };
        self.claim_and_drive(record).await
    }

    async fn claim_and_drive(&self, record: RuntimeSwitchRecord) -> VibexResult<SwitchOutcome> {
        if record.status.is_terminal() {
            return Ok(SwitchOutcome::from(&record));
        }
        let session_lock = self.session_driver_lock(&record.session_id)?;
        let _session_guard = session_lock.lock_owned().await;
        let record = self.required_switch(&record.switch_id)?;
        if record.status.is_terminal() {
            return Ok(SwitchOutcome::from(&record));
        }
        let owner = Self::new_worker_owner();
        if !self.try_acquire_lease(&record.switch_id, &owner)? {
            return Ok(SwitchOutcome::from(
                &self.required_switch(&record.switch_id)?,
            ));
        }
        let (stop_heartbeat, heartbeat) =
            self.spawn_lease_heartbeat(record.switch_id.clone(), owner.clone());
        let result = self.drive_claimed(record.switch_id.clone(), &owner).await;
        let _ = stop_heartbeat.send(());
        let heartbeat_result = match heartbeat.await {
            Ok(result) => result,
            Err(_) => Err(VibexError::process(
                "runtime_switch_lease_heartbeat_stopped",
                "runtime switch lease heartbeat stopped unexpectedly",
            )),
        };
        let release_result = self.release_lease(&record.switch_id, &owner);
        match result {
            Ok(outcome) => {
                heartbeat_result?;
                release_result?;
                Ok(outcome)
            }
            Err(error) => Err(error),
        }
    }

    async fn drive_claimed(
        &self,
        switch_id: RuntimeSwitchId,
        owner: &str,
    ) -> VibexResult<SwitchOutcome> {
        let result = self.drive_state_machine(&switch_id, owner).await;
        let Err(error) = result else {
            return result;
        };

        if error.code == "runtime_switch_lease_lost" {
            return Err(error.with_diagnostic("switchId", switch_id.as_str()));
        }

        let current = self.required_switch(&switch_id)?;
        if !current.status.is_terminal() {
            let mut conn = self.open_connection()?;
            RuntimeSwitchRepository::fail(
                &mut conn,
                &current.session_id,
                &switch_id,
                &error.code,
                Some(&error.code),
            )?;
        }
        let finished = self.required_switch(&switch_id)?;
        if matches!(
            current.status,
            RuntimeSwitchStatus::Preparing
                | RuntimeSwitchStatus::Prepared
                | RuntimeSwitchStatus::Committing
                | RuntimeSwitchStatus::Superseded
                | RuntimeSwitchStatus::AmbiguousExternalEffect
        ) && let Ok(intent) = SwitchIntent::from_record(&current)
        {
            if intent.target_binding_id != intent.source_binding_id
                && let Some(target_binding_id) = &intent.target_binding_id
                && self.get_binding(target_binding_id)?.is_some()
            {
                let conn = self.open_connection()?;
                RuntimeBindingRepository::update_state(
                    &conn,
                    target_binding_id,
                    BindingState::Failed,
                )?;
            }
            let _ = self.inner.executor.cleanup_target(&intent, None).await;
        }
        if matches!(
            current.status,
            RuntimeSwitchStatus::WaitingForIdle
                | RuntimeSwitchStatus::Preparing
                | RuntimeSwitchStatus::Prepared
                | RuntimeSwitchStatus::Committing
                | RuntimeSwitchStatus::Superseded
                | RuntimeSwitchStatus::AmbiguousExternalEffect
        ) {
            let _ = self
                .inner
                .active_work
                .set_prompt_gate(&current.session_id, false)
                .await;
        }
        Err(error.with_diagnostic("switchId", finished.switch_id.as_str()))
    }

    async fn drive_state_machine(
        &self,
        switch_id: &RuntimeSwitchId,
        owner: &str,
    ) -> VibexResult<SwitchOutcome> {
        loop {
            self.renew_lease(switch_id, owner)?;
            let record = self.required_switch(switch_id)?;
            if record.status.is_terminal() {
                return Ok(SwitchOutcome::from(&record));
            }
            if !matches!(
                record.status,
                RuntimeSwitchStatus::Requested | RuntimeSwitchStatus::Committing
            ) {
                self.validate_switch_ownership(&record)?;
            }
            let intent = SwitchIntent::from_record(&record)?;
            match record.status {
                RuntimeSwitchStatus::Requested => {
                    let outcome = {
                        let mut conn = self.open_connection()?;
                        RuntimeSwitchRepository::claim_requested(&mut conn, switch_id)?
                    };
                    match outcome {
                        RequestedSwitchClaimOutcome::Claimed => {}
                        RequestedSwitchClaimOutcome::WaitingForPending => {
                            sleep(self.inner.config.idle_poll_interval).await;
                        }
                        RequestedSwitchClaimOutcome::Superseded => {
                            return Ok(SwitchOutcome::from(&self.required_switch(switch_id)?));
                        }
                    }
                }
                RuntimeSwitchStatus::Reserved => {
                    let intent = self.stabilize_reserved_target(&record, owner).await?;
                    self.settle_active_work(&intent, record.status, owner)
                        .await?;
                }
                RuntimeSwitchStatus::WaitingForIdle => {
                    self.settle_active_work(&intent, record.status, owner)
                        .await?;
                }
                RuntimeSwitchStatus::Preparing => {
                    self.inner
                        .active_work
                        .set_prompt_gate(&intent.session_id, true)
                        .await?;
                    self.ensure_commit_idle(&intent, owner).await?;
                    self.prepare_target(&intent, owner).await?;
                }
                RuntimeSwitchStatus::Prepared => {
                    return self.commit_prepared(&intent, owner).await;
                }
                RuntimeSwitchStatus::Committing => {
                    self.reconcile_committing(&intent).await?;
                }
                RuntimeSwitchStatus::Committed
                | RuntimeSwitchStatus::Failed
                | RuntimeSwitchStatus::Cancelled
                | RuntimeSwitchStatus::Superseded
                | RuntimeSwitchStatus::AmbiguousExternalEffect => unreachable!(),
            }
        }
    }

    fn open_connection(&self) -> VibexResult<DbConnection> {
        open_database(&self.inner.db_path)
    }

    fn encode_durable_requested_config(
        request: &RuntimeSwitchRequest,
    ) -> VibexResult<serde_json::Value> {
        Self::encode_requested_config(
            &request.target_selection,
            request.requested_session_config.clone(),
        )
    }

    fn validate_switch_ownership(&self, record: &RuntimeSwitchRecord) -> VibexResult<()> {
        let state = {
            let conn = self.open_connection()?;
            AgentSessionRuntimeRepository::get_runtime_state(&conn, &record.session_id)?
                .ok_or_else(|| {
                    VibexError::validation("session_not_found", "Agent session was not found")
                })?
        };
        if state.selection_revision != record.desired_selection_revision {
            let mut conn = self.open_connection()?;
            RuntimeSwitchRepository::supersede(&mut conn, &record.session_id, &record.switch_id)?;
            return Err(VibexError::conflict(
                "runtime_switch_superseded",
                "desired selection changed; runtime switch was superseded",
            ));
        }
        if state.pending_switch_id.as_ref() != Some(&record.switch_id)
            || state.current_binding_id != record.source_binding_id
            || state.revision != record.source_revision
        {
            return Err(VibexError::conflict(
                "runtime_switch_source_conflict",
                "runtime switch no longer owns its durable source state",
            ));
        }
        Ok(())
    }

    async fn stabilize_reserved_target(
        &self,
        record: &RuntimeSwitchRecord,
        owner: &str,
    ) -> VibexResult<SwitchIntent> {
        let intent = SwitchIntent::from_record(record)?;
        let assessment = self.inner.executor.assess_target(&intent).await?;
        self.renew_lease(&intent.switch_id, owner)?;
        let strategy = decide_switch_strategy(intent.requested_policy, &assessment);
        let desired_target = match strategy {
            RuntimeSwitchStrategy::LiveMutation => {
                intent.source_binding_id.clone().ok_or_else(|| {
                    VibexError::validation(
                        "runtime_switch_live_source_missing",
                        "live mutation requires an existing source binding",
                    )
                })?
            }
            RuntimeSwitchStrategy::RestartAndResume
            | RuntimeSwitchStrategy::RestartFreshAndBridge => {
                if intent.target_binding_id == intent.source_binding_id {
                    RuntimeBindingId::new()
                } else {
                    intent.target_binding_id.clone().ok_or_else(|| {
                        VibexError::validation(
                            "runtime_switch_target_binding_missing",
                            "runtime switch has no reserved target binding",
                        )
                    })?
                }
            }
        };
        if intent.target_binding_id.as_ref() != Some(&desired_target) {
            let conn = self.open_connection()?;
            RuntimeSwitchRepository::compare_and_set_target_binding(
                &conn,
                &intent.switch_id,
                intent.target_binding_id.as_ref(),
                &desired_target,
            )?;
        }
        SwitchIntent::from_record(&self.required_switch(&intent.switch_id)?)
    }

    fn get_switch(&self, switch_id: &RuntimeSwitchId) -> VibexResult<Option<RuntimeSwitchRecord>> {
        let conn = self.open_connection()?;
        RuntimeSwitchRepository::get(&conn, switch_id)
    }

    fn required_switch(&self, switch_id: &RuntimeSwitchId) -> VibexResult<RuntimeSwitchRecord> {
        self.get_switch(switch_id)?.ok_or_else(|| {
            VibexError::validation("runtime_switch_not_found", "runtime switch was not found")
        })
    }

    fn session_driver_lock(&self, session_id: &VibexSessionId) -> VibexResult<Arc<AsyncMutex<()>>> {
        let mut locks = self.inner.session_driver_locks.lock().map_err(|_| {
            VibexError::process(
                "runtime_switch_session_lock_poisoned",
                "runtime switch session driver lock is unavailable",
            )
        })?;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(session_id.as_str()).and_then(Weak::upgrade) {
            return Ok(lock);
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(session_id.as_str().to_string(), Arc::downgrade(&lock));
        Ok(lock)
    }

    fn get_by_idempotency_key(
        &self,
        session_id: &VibexSessionId,
        idempotency_key: &str,
    ) -> VibexResult<Option<RuntimeSwitchRecord>> {
        let conn = self.open_connection()?;
        RuntimeSwitchRepository::get_by_idempotency_key(&conn, session_id, idempotency_key)
    }

    fn try_acquire_lease(&self, switch_id: &RuntimeSwitchId, owner: &str) -> VibexResult<bool> {
        let conn = self.open_connection()?;
        RuntimeSwitchRepository::try_acquire_worker_lease(
            &conn,
            switch_id,
            owner,
            self.inner.config.lease_duration_ms,
            unix_timestamp_ms(),
        )
    }

    fn renew_lease(&self, switch_id: &RuntimeSwitchId, owner: &str) -> VibexResult<()> {
        let conn = self.open_connection()?;
        if RuntimeSwitchRepository::renew_worker_lease(
            &conn,
            switch_id,
            owner,
            self.inner.config.lease_duration_ms,
            unix_timestamp_ms(),
        )? {
            Ok(())
        } else {
            Err(VibexError::conflict(
                "runtime_switch_lease_lost",
                "runtime switch worker lease was lost",
            ))
        }
    }

    fn release_lease(&self, switch_id: &RuntimeSwitchId, owner: &str) -> VibexResult<()> {
        let conn = self.open_connection()?;
        RuntimeSwitchRepository::release_worker_lease(&conn, switch_id, owner)
    }

    fn new_worker_owner() -> String {
        format!("worker:{}", RuntimeSwitchOperationId::new().as_str())
    }

    fn spawn_lease_heartbeat(
        &self,
        switch_id: RuntimeSwitchId,
        owner: String,
    ) -> (
        oneshot::Sender<()>,
        tokio::task::JoinHandle<VibexResult<()>>,
    ) {
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let coordinator = self.clone();
        let interval_ms = (self.inner.config.lease_duration_ms / 3).max(1) as u64;
        let heartbeat = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stop_rx => return Ok(()),
                    _ = sleep(Duration::from_millis(interval_ms)) => {
                        coordinator.renew_lease(&switch_id, &owner)?;
                    }
                }
            }
        });
        (stop_tx, heartbeat)
    }
}

impl RuntimeSwitchCoordinator {
    async fn settle_active_work(
        &self,
        intent: &SwitchIntent,
        status: RuntimeSwitchStatus,
        owner: &str,
    ) -> VibexResult<()> {
        if status == RuntimeSwitchStatus::WaitingForIdle {
            self.inner
                .active_work
                .set_prompt_gate(&intent.session_id, true)
                .await?;
        }
        loop {
            self.renew_lease(&intent.switch_id, owner)?;
            self.validate_switch_ownership(&self.required_switch(&intent.switch_id)?)?;
            let mut snapshot = self.inner.active_work.probe(&intent.session_id).await?;
            self.reject_active_work(intent, snapshot)?;
            self.cancel_active_work(intent, snapshot, owner).await?;
            snapshot = self.inner.active_work.probe(&intent.session_id).await?;
            self.reject_active_work(intent, snapshot)?;

            let mut waiting = false;
            let mut wait_timed_out = false;
            for kind in ACTIVE_WORK_KINDS {
                if !snapshot.is_active(kind) {
                    continue;
                }
                match intent.active_work_policy.disposition(kind) {
                    BusyDisposition::Wait { deadline_ms } => {
                        let deadline_ms = i64::try_from(deadline_ms).unwrap_or(i64::MAX);
                        wait_timed_out |=
                            unix_timestamp_ms() >= intent.created_at_ms.saturating_add(deadline_ms);
                        waiting = true;
                    }
                    BusyDisposition::Cancel => {
                        return Err(VibexError::conflict(
                            "runtime_switch_cancel_unconfirmed",
                            "runtime switch cancellation was not confirmed",
                        ));
                    }
                    BusyDisposition::Reject => unreachable!("rejects are checked above"),
                }
            }

            let current = self.required_switch(&intent.switch_id)?;
            if !waiting {
                self.inner
                    .active_work
                    .set_prompt_gate(&intent.session_id, true)
                    .await?;
                match current.status {
                    RuntimeSwitchStatus::Reserved => {
                        let conn = self.open_connection()?;
                        RuntimeSwitchRepository::advance_status(
                            &conn,
                            &intent.switch_id,
                            RuntimeSwitchStatus::Reserved,
                            RuntimeSwitchStatus::Preparing,
                        )?;
                    }
                    RuntimeSwitchStatus::WaitingForIdle => {
                        let conn = self.open_connection()?;
                        RuntimeSwitchRepository::advance_status(
                            &conn,
                            &intent.switch_id,
                            RuntimeSwitchStatus::WaitingForIdle,
                            RuntimeSwitchStatus::Preparing,
                        )?;
                    }
                    _ => {
                        return Err(VibexError::conflict(
                            "runtime_switch_status_conflict",
                            "runtime switch left the active-work phase unexpectedly",
                        ));
                    }
                }
                let fenced_snapshot = self.inner.active_work.probe(&intent.session_id).await?;
                if ACTIVE_WORK_KINDS
                    .into_iter()
                    .any(|kind| fenced_snapshot.is_active(kind))
                {
                    return Err(VibexError::conflict(
                        "runtime_switch_active_work_changed",
                        "active work changed while the runtime switch prompt gate was closing",
                    ));
                }
                return Ok(());
            }

            if current.status == RuntimeSwitchStatus::Reserved {
                for kind in ACTIVE_WORK_KINDS {
                    if snapshot.is_active(kind)
                        && matches!(
                            intent.active_work_policy.disposition(kind),
                            BusyDisposition::Wait { .. }
                        )
                    {
                        self.record_active_work(kind, RuntimeMetricResult::Waited);
                    }
                }
                let conn = self.open_connection()?;
                RuntimeSwitchRepository::advance_status(
                    &conn,
                    &intent.switch_id,
                    RuntimeSwitchStatus::Reserved,
                    RuntimeSwitchStatus::WaitingForIdle,
                )?;
            } else if current.status != RuntimeSwitchStatus::WaitingForIdle {
                return Err(VibexError::conflict(
                    "runtime_switch_status_conflict",
                    "runtime switch left the waiting phase unexpectedly",
                ));
            }
            self.inner
                .active_work
                .set_prompt_gate(&intent.session_id, true)
                .await?;
            if wait_timed_out {
                for kind in ACTIVE_WORK_KINDS {
                    if snapshot.is_active(kind)
                        && matches!(
                            intent.active_work_policy.disposition(kind),
                            BusyDisposition::Wait { .. }
                        )
                    {
                        self.record_active_work(kind, RuntimeMetricResult::TimedOut);
                    }
                }
                return Err(VibexError::conflict(
                    "runtime_switch_wait_timeout",
                    "runtime switch timed out waiting for active work",
                ));
            }
            sleep(self.inner.config.idle_poll_interval).await;
        }
    }

    async fn ensure_commit_idle(&self, intent: &SwitchIntent, owner: &str) -> VibexResult<()> {
        self.inner
            .active_work
            .set_prompt_gate(&intent.session_id, true)
            .await?;
        loop {
            self.renew_lease(&intent.switch_id, owner)?;
            self.validate_switch_ownership(&self.required_switch(&intent.switch_id)?)?;
            let mut snapshot = self.inner.active_work.probe(&intent.session_id).await?;
            self.reject_active_work(intent, snapshot)?;
            self.cancel_active_work(intent, snapshot, owner).await?;
            snapshot = self.inner.active_work.probe(&intent.session_id).await?;
            self.reject_active_work(intent, snapshot)?;

            let mut waiting = false;
            for kind in ACTIVE_WORK_KINDS {
                if !snapshot.is_active(kind) {
                    continue;
                }
                match intent.active_work_policy.disposition(kind) {
                    BusyDisposition::Wait { deadline_ms } => {
                        let deadline_ms = i64::try_from(deadline_ms).unwrap_or(i64::MAX);
                        if unix_timestamp_ms() >= intent.created_at_ms.saturating_add(deadline_ms) {
                            return Err(VibexError::conflict(
                                "runtime_switch_wait_timeout",
                                "runtime switch timed out waiting for active work",
                            ));
                        }
                        waiting = true;
                    }
                    BusyDisposition::Cancel => {
                        return Err(VibexError::conflict(
                            "runtime_switch_cancel_unconfirmed",
                            "runtime switch cancellation was not confirmed",
                        ));
                    }
                    BusyDisposition::Reject => unreachable!("rejects are checked above"),
                }
            }
            if !waiting {
                return Ok(());
            }
            self.inner
                .active_work
                .set_prompt_gate(&intent.session_id, true)
                .await?;
            sleep(self.inner.config.idle_poll_interval).await;
        }
    }

    fn reject_active_work(
        &self,
        intent: &SwitchIntent,
        snapshot: ActiveWorkSnapshot,
    ) -> VibexResult<()> {
        for kind in ACTIVE_WORK_KINDS {
            if snapshot.is_active(kind)
                && intent.active_work_policy.disposition(kind) == BusyDisposition::Reject
            {
                self.record_active_work(kind, RuntimeMetricResult::Rejected);
                return Err(VibexError::conflict(
                    busy_error_code(kind),
                    "runtime switch was rejected because the session has active work",
                ));
            }
        }
        Ok(())
    }

    async fn cancel_active_work(
        &self,
        intent: &SwitchIntent,
        snapshot: ActiveWorkSnapshot,
        owner: &str,
    ) -> VibexResult<()> {
        for kind in ACTIVE_WORK_KINDS {
            if !snapshot.is_active(kind)
                || intent.active_work_policy.disposition(kind) != BusyDisposition::Cancel
            {
                continue;
            }
            let operation_kind = cancel_operation_kind(kind);
            let plan = self
                .begin_operation(
                    intent,
                    operation_kind,
                    format!("active_work:{operation_kind}"),
                    RetrySemantics::ReconcileBeforeRetry,
                    true,
                    owner,
                )
                .await?;
            if let OperationPlan::Execute(operation) = plan {
                self.renew_lease(&intent.switch_id, owner)?;
                let cancellation = self
                    .inner
                    .active_work
                    .cancel(&intent.session_id, kind, &operation)
                    .await;
                self.renew_lease(&intent.switch_id, owner)?;
                match cancellation {
                    Ok(()) => {
                        self.mark_operation_succeeded(&operation, None)?;
                        self.record_active_work(kind, RuntimeMetricResult::Cancelled);
                    }
                    Err(error) => {
                        self.mark_operation_failed(&operation, &error.code)?;
                        self.record_active_work(kind, RuntimeMetricResult::Failure);
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }

    async fn prepare_target(&self, intent: &SwitchIntent, owner: &str) -> VibexResult<()> {
        let started = Instant::now();
        let result = self.prepare_target_inner(intent, owner).await;
        let metric_result = result
            .as_ref()
            .map(|_| RuntimeMetricResult::Success)
            .unwrap_or_else(|error| metric_result_for_error_code(&error.code));
        self.inner.observability.observe_duration(
            RuntimeMetricName::SwitchPhaseDuration,
            Some(RuntimeMetricOperation::Prepare),
            metric_result,
            started.elapsed(),
        );
        switch_log_context(intent, "switch_prepare").emit(
            if result.is_ok() {
                RuntimeLogLevel::Info
            } else {
                RuntimeLogLevel::Warn
            },
            "runtime_switch_prepare",
            metric_result,
            result.as_ref().err().map(|error| error.code.as_str()),
            Some(elapsed_ms(started)),
        );
        result
    }

    async fn prepare_target_inner(&self, intent: &SwitchIntent, owner: &str) -> VibexResult<()> {
        let assessment = self.inner.executor.assess_target(intent).await?;
        let strategy = decide_switch_strategy(intent.requested_policy, &assessment);
        let target_is_source = intent.target_binding_id == intent.source_binding_id;
        if (strategy == RuntimeSwitchStrategy::LiveMutation) != target_is_source {
            return Err(VibexError::conflict(
                "runtime_switch_strategy_changed",
                "runtime switch strategy no longer matches its reserved target binding",
            ));
        }

        match strategy {
            RuntimeSwitchStrategy::LiveMutation => {
                let source_binding_id = intent.source_binding_id.as_ref().ok_or_else(|| {
                    VibexError::validation(
                        "runtime_switch_live_source_missing",
                        "live mutation requires an existing source binding",
                    )
                })?;
                let binding = self.required_binding(source_binding_id)?;
                let attachment = self
                    .inner
                    .executor
                    .acquire_prepared(intent, &binding)
                    .await?;
                self.renew_lease(&intent.switch_id, owner)?;
                let plan = self
                    .begin_operation(
                        intent,
                        OP_APPLY_LIVE_MUTATION,
                        "live_mutation:session_fields".to_string(),
                        RetrySemantics::Idempotent,
                        false,
                        owner,
                    )
                    .await?;
                if let OperationPlan::Execute(operation) = plan {
                    self.renew_lease(&intent.switch_id, owner)?;
                    let mutation = self
                        .inner
                        .executor
                        .apply_live_mutation(intent, &attachment, &operation)
                        .await;
                    self.renew_lease(&intent.switch_id, owner)?;
                    match mutation {
                        Ok(()) => self.mark_operation_succeeded(&operation, None)?,
                        Err(error) => {
                            self.mark_operation_failed(&operation, &error.code)?;
                            return Err(error);
                        }
                    }
                }
            }
            RuntimeSwitchStrategy::RestartAndResume
            | RuntimeSwitchStrategy::RestartFreshAndBridge => {
                let process = self.prepare_process(intent, owner).await?;
                let mut attachment = self
                    .prepare_session(intent, &process, strategy, &assessment, owner)
                    .await?;
                attachment.binding = self.ensure_prepared_binding(intent, &attachment.binding)?;

                let config_plan = self
                    .begin_operation(
                        intent,
                        OP_APPLY_SESSION_CONFIG,
                        format!(
                            "session_config:present={}",
                            intent.requested_session_config.is_some()
                        ),
                        RetrySemantics::Idempotent,
                        false,
                        owner,
                    )
                    .await?;
                if let OperationPlan::Execute(operation) = config_plan {
                    self.renew_lease(&intent.switch_id, owner)?;
                    let apply = self
                        .inner
                        .executor
                        .apply_session_config(intent, &attachment, &operation)
                        .await;
                    self.renew_lease(&intent.switch_id, owner)?;
                    match apply {
                        Ok(()) => self.mark_operation_succeeded(&operation, None)?,
                        Err(error) => {
                            self.mark_operation_failed(&operation, &error.code)?;
                            return Err(error);
                        }
                    }
                }
                let context_delta = self
                    .inner
                    .executor
                    .build_context_delta(intent, &attachment)
                    .await;
                self.renew_lease(&intent.switch_id, owner)?;
                context_delta?;
                self.persist_restore_result(intent, attachment.restore_result.as_ref())?;
                if strategy == RuntimeSwitchStrategy::RestartFreshAndBridge {
                    self.inner.observability.increment(
                        RuntimeMetricName::FreshBridge,
                        None,
                        RuntimeMetricResult::Fresh,
                    );
                }
            }
        }

        let conn = self.open_connection()?;
        RuntimeSwitchRepository::advance_status(
            &conn,
            &intent.switch_id,
            RuntimeSwitchStatus::Preparing,
            RuntimeSwitchStatus::Prepared,
        )
    }

    async fn prepare_process(
        &self,
        intent: &SwitchIntent,
        owner: &str,
    ) -> VibexResult<PreparedProcess> {
        match self
            .begin_operation(
                intent,
                OP_SPAWN_PROCESS,
                format!(
                    "process:{}:{}",
                    intent.target_selection.agent_id.as_str(),
                    intent.target_adapter_id.as_str()
                ),
                RetrySemantics::Idempotent,
                true,
                owner,
            )
            .await?
        {
            OperationPlan::Skip(_) => {
                let process = self.inner.executor.reacquire_process(intent).await;
                self.renew_lease(&intent.switch_id, owner)?;
                process
            }
            OperationPlan::Execute(operation) => {
                self.renew_lease(&intent.switch_id, owner)?;
                let ensured = self.inner.executor.ensure_process(intent, &operation).await;
                self.renew_lease(&intent.switch_id, owner)?;
                match ensured {
                    Ok(process) => {
                        self.mark_operation_succeeded(&operation, None)?;
                        Ok(process)
                    }
                    Err(error) => {
                        self.mark_operation_failed(&operation, &error.code)?;
                        Err(error)
                    }
                }
            }
        }
    }

    async fn prepare_session(
        &self,
        intent: &SwitchIntent,
        process: &PreparedProcess,
        strategy: RuntimeSwitchStrategy,
        assessment: &SwitchTargetAssessment,
        owner: &str,
    ) -> VibexResult<PreparedAttachment> {
        let operation_kind = match strategy {
            RuntimeSwitchStrategy::RestartAndResume => OP_RESTORE_SESSION,
            RuntimeSwitchStrategy::RestartFreshAndBridge => OP_CREATE_SESSION,
            RuntimeSwitchStrategy::LiveMutation => unreachable!(),
        };
        let retry_semantics = match strategy {
            RuntimeSwitchStrategy::RestartAndResume => RetrySemantics::ReconcileBeforeRetry,
            RuntimeSwitchStrategy::RestartFreshAndBridge
                if assessment.supports_client_idempotency =>
            {
                RetrySemantics::ReconcileBeforeRetry
            }
            RuntimeSwitchStrategy::RestartFreshAndBridge => {
                RetrySemantics::NonRetryableWhenAmbiguous
            }
            RuntimeSwitchStrategy::LiveMutation => unreachable!(),
        };
        self.confirm_session_operation_from_binding(intent, operation_kind)?;
        match self
            .begin_operation(
                intent,
                operation_kind,
                format!(
                    "session:{}:{}:{}",
                    intent.target_selection.agent_id.as_str(),
                    intent.target_adapter_id.as_str(),
                    operation_kind
                ),
                retry_semantics,
                assessment.supports_client_idempotency,
                owner,
            )
            .await?
        {
            OperationPlan::Skip(operation) => {
                if let Some(target_binding_id) = &intent.target_binding_id
                    && let Some(binding) = self.get_binding(target_binding_id)?
                {
                    let attachment = self.inner.executor.acquire_prepared(intent, &binding).await;
                    self.renew_lease(&intent.switch_id, owner)?;
                    return attachment;
                }
                let attachment = self
                    .inner
                    .executor
                    .recover_attachment(intent, &operation)
                    .await;
                self.renew_lease(&intent.switch_id, owner)?;
                attachment
            }
            OperationPlan::Execute(operation) => {
                self.renew_lease(&intent.switch_id, owner)?;
                let prepared = self
                    .inner
                    .executor
                    .restore_or_create_session(intent, process, strategy, &operation)
                    .await;
                self.renew_lease(&intent.switch_id, owner)?;
                match prepared {
                    Ok(attachment) => {
                        let binding = self.ensure_prepared_binding(intent, &attachment.binding)?;
                        let mut attachment = attachment;
                        attachment.binding = binding;
                        self.mark_operation_succeeded(
                            &operation,
                            attachment.binding.native_session_id.as_deref(),
                        )?;
                        Ok(attachment)
                    }
                    Err(error) => {
                        self.mark_operation_failed(&operation, &error.code)?;
                        Err(error)
                    }
                }
            }
        }
    }

    fn ensure_prepared_binding(
        &self,
        intent: &SwitchIntent,
        binding: &RuntimeBinding,
    ) -> VibexResult<RuntimeBinding> {
        let expected_binding_id = intent.target_binding_id.as_ref().ok_or_else(|| {
            VibexError::validation(
                "runtime_switch_target_binding_missing",
                "restart strategy requires a reserved target binding",
            )
        })?;
        if binding.binding_id != *expected_binding_id
            || binding.session_id != intent.session_id
            || binding.agent_id != intent.target_selection.agent_id
            || binding.provider_profile_id != intent.target_selection.provider_profile_id
            || binding.adapter_id != intent.target_adapter_id
        {
            return Err(VibexError::conflict(
                "runtime_switch_prepared_binding_mismatch",
                "prepared binding does not match the durable switch intent",
            ));
        }
        if let Some(existing) = self.get_binding(expected_binding_id)? {
            if existing.session_id != binding.session_id
                || existing.agent_id != binding.agent_id
                || existing.provider_profile_id != binding.provider_profile_id
                || existing.adapter_id != binding.adapter_id
                || existing.native_session_id != binding.native_session_id
                || existing.created_by_switch_id.as_ref() != Some(&intent.switch_id)
            {
                return Err(VibexError::conflict(
                    "runtime_switch_prepared_binding_conflict",
                    "reserved target binding conflicts with an existing binding",
                ));
            }
            return Ok(existing);
        }

        let state = {
            let conn = self.open_connection()?;
            AgentSessionRuntimeRepository::get_runtime_state(&conn, &intent.session_id)?
                .ok_or_else(|| {
                    VibexError::validation("session_not_found", "Agent session was not found")
                })?
        };
        let now = unix_timestamp_ms();
        let mut prepared = binding.clone();
        prepared.binding_state = BindingState::Preparing;
        prepared.created_by_switch_id = Some(intent.switch_id.clone());
        prepared.activation_generation = state.activation_generation.saturating_add(1);
        if prepared.created_at_ms <= 0 {
            prepared.created_at_ms = now;
        }
        prepared.updated_at_ms = now;
        let conn = self.open_connection()?;
        RuntimeBindingRepository::insert(&conn, &prepared)?;
        Ok(prepared)
    }

    fn confirm_session_operation_from_binding(
        &self,
        intent: &SwitchIntent,
        operation_kind: &str,
    ) -> VibexResult<()> {
        let Some(target_binding_id) = &intent.target_binding_id else {
            return Ok(());
        };
        let Some(binding) = self.get_binding(target_binding_id)? else {
            return Ok(());
        };
        if binding.created_by_switch_id.as_ref() != Some(&intent.switch_id)
            || binding.session_id != intent.session_id
            || binding.agent_id != intent.target_selection.agent_id
            || binding.provider_profile_id != intent.target_selection.provider_profile_id
            || binding.adapter_id != intent.target_adapter_id
        {
            return Err(VibexError::conflict(
                "runtime_switch_prepared_binding_conflict",
                "reserved target binding conflicts with the durable switch intent",
            ));
        }
        let operation = {
            let conn = self.open_connection()?;
            SwitchOperationJournalRepository::list_by_switch(&conn, &intent.switch_id)?
                .into_iter()
                .rev()
                .find(|operation| operation.operation_kind == operation_kind)
        };
        if let Some(operation) = operation
            && operation.status == SwitchOperationStatus::AboutToSend
        {
            let conn = self.open_connection()?;
            SwitchOperationJournalRepository::mark_succeeded(
                &conn,
                &operation.operation_id,
                binding.native_session_id.as_deref(),
            )?;
        }
        Ok(())
    }

    fn persist_restore_result(
        &self,
        intent: &SwitchIntent,
        next: Option<&AgentSessionRestoreResult>,
    ) -> VibexResult<()> {
        let Some(next) = next else {
            return Ok(());
        };
        let current = self.required_switch(&intent.switch_id)?;
        if current.restore_compatibility_result.as_ref() == Some(next) {
            return Ok(());
        }
        if current.restore_compatibility_result.is_some() {
            return Err(VibexError::conflict(
                "runtime_switch_restore_result_conflict",
                "runtime switch restore result changed during preparation",
            ));
        }
        let conn = self.open_connection()?;
        RuntimeSwitchRepository::compare_and_set_restore_compatibility_result(
            &conn,
            &intent.switch_id,
            &intent.session_id,
            intent.source_revision,
            RuntimeSwitchStatus::Preparing,
            None,
            next,
        )
    }

    async fn commit_prepared(
        &self,
        intent: &SwitchIntent,
        owner: &str,
    ) -> VibexResult<SwitchOutcome> {
        let started = Instant::now();
        let result = self.commit_prepared_inner(intent, owner).await;
        let metric_result = result
            .as_ref()
            .map(|_| RuntimeMetricResult::Committed)
            .unwrap_or_else(|error| metric_result_for_error_code(&error.code));
        self.inner.observability.observe_duration(
            RuntimeMetricName::SwitchPhaseDuration,
            Some(RuntimeMetricOperation::Commit),
            metric_result,
            started.elapsed(),
        );
        switch_log_context(intent, "switch_commit").emit(
            if result.is_ok() {
                RuntimeLogLevel::Info
            } else {
                RuntimeLogLevel::Warn
            },
            "runtime_switch_commit",
            metric_result,
            result.as_ref().err().map(|error| error.code.as_str()),
            Some(elapsed_ms(started)),
        );
        result
    }

    async fn commit_prepared_inner(
        &self,
        intent: &SwitchIntent,
        owner: &str,
    ) -> VibexResult<SwitchOutcome> {
        self.ensure_commit_idle(intent, owner).await?;
        let target_binding_id = intent.target_binding_id.as_ref().ok_or_else(|| {
            VibexError::validation(
                "runtime_switch_target_binding_missing",
                "runtime switch has no target binding",
            )
        })?;
        let binding = self.required_binding(target_binding_id)?;
        let attachment = self
            .inner
            .executor
            .acquire_prepared(intent, &binding)
            .await?;
        self.renew_lease(&intent.switch_id, owner)?;
        let revalidated = self
            .inner
            .executor
            .revalidate_prepared(intent, &attachment)
            .await;
        self.renew_lease(&intent.switch_id, owner)?;
        revalidated?;
        {
            let conn = self.open_connection()?;
            RuntimeSwitchRepository::advance_status(
                &conn,
                &intent.switch_id,
                RuntimeSwitchStatus::Prepared,
                RuntimeSwitchStatus::Committing,
            )?;
        }
        let commit = {
            let mut conn = self.open_connection()?;
            RuntimeSwitchRepository::commit(
                &mut conn,
                &RuntimeSwitchCommitRequest {
                    switch_id: intent.switch_id.clone(),
                    session_id: intent.session_id.clone(),
                    source_revision: intent.source_revision,
                    desired_selection_revision: intent.desired_selection_revision,
                    source_binding_id: intent.source_binding_id.clone(),
                    target_binding_id: target_binding_id.clone(),
                    target_agent_id: intent.target_selection.agent_id.clone(),
                    effective_selection: intent.target_selection.clone(),
                },
            )
        };
        if let Err(error) = commit {
            if error.code != "runtime_switch_superseded" {
                let mut conn = self.open_connection()?;
                let _ = RuntimeSwitchRepository::revert_committing_to_prepared(
                    &mut conn,
                    &intent.switch_id,
                );
            }
            return Err(error);
        }

        let state = {
            let conn = self.open_connection()?;
            AgentSessionRuntimeRepository::get_runtime_state(&conn, &intent.session_id)?
                .ok_or_else(|| {
                    VibexError::validation("session_not_found", "Agent session was not found")
                })?
        };
        self.inner
            .executor
            .activate(intent, &attachment, state.activation_generation)
            .await
            .map_err(|error| {
                VibexError::process(
                    "runtime_switch_activation_failed",
                    "runtime switch committed but target activation failed",
                )
                .with_diagnostic("causeCode", error.code)
            })?;
        self.inner
            .active_work
            .set_prompt_gate(&intent.session_id, false)
            .await?;
        if intent.source_binding_id != intent.target_binding_id {
            self.inner
                .executor
                .cleanup_source_after_commit(intent)
                .await?;
        }
        Ok(SwitchOutcome::from(
            &self.required_switch(&intent.switch_id)?,
        ))
    }

    fn record_active_work(&self, kind: ActiveWorkKind, result: RuntimeMetricResult) {
        self.inner.observability.increment(
            RuntimeMetricName::SwitchActiveWork,
            Some(metric_operation_for_active_work(kind)),
            result,
        );
    }

    async fn reconcile_committing(&self, intent: &SwitchIntent) -> VibexResult<()> {
        let confirmed = {
            let mut conn = self.open_connection()?;
            RuntimeSwitchRepository::confirm_committed(&mut conn, &intent.switch_id)?
        };
        if confirmed {
            self.activate_committed(intent).await?;
            return Ok(());
        }
        let reverted = {
            let mut conn = self.open_connection()?;
            RuntimeSwitchRepository::revert_committing_to_prepared(&mut conn, &intent.switch_id)?
        };
        if reverted {
            Ok(())
        } else {
            Err(VibexError::conflict(
                "runtime_switch_commit_reconcile_failed",
                "runtime switch commit outcome could not be reconciled",
            ))
        }
    }

    async fn activate_committed(&self, intent: &SwitchIntent) -> VibexResult<()> {
        let target_binding_id = intent.target_binding_id.as_ref().ok_or_else(|| {
            VibexError::validation(
                "runtime_switch_target_binding_missing",
                "committed runtime switch has no target binding",
            )
        })?;
        let binding = self.required_binding(target_binding_id)?;
        let attachment = self
            .inner
            .executor
            .acquire_prepared(intent, &binding)
            .await?;
        let state = {
            let conn = self.open_connection()?;
            AgentSessionRuntimeRepository::get_runtime_state(&conn, &intent.session_id)?
                .ok_or_else(|| {
                    VibexError::validation("session_not_found", "Agent session was not found")
                })?
        };
        self.inner
            .executor
            .activate(intent, &attachment, state.activation_generation)
            .await?;
        self.inner
            .active_work
            .set_prompt_gate(&intent.session_id, false)
            .await?;
        if intent.source_binding_id != intent.target_binding_id {
            self.inner
                .executor
                .cleanup_source_after_commit(intent)
                .await?;
        }
        Ok(())
    }

    async fn begin_operation(
        &self,
        intent: &SwitchIntent,
        operation_kind: &str,
        request_fingerprint: String,
        retry_semantics: RetrySemantics,
        use_adapter_token: bool,
        owner: &str,
    ) -> VibexResult<OperationPlan> {
        self.renew_lease(&intent.switch_id, owner)?;
        let latest = {
            let conn = self.open_connection()?;
            SwitchOperationJournalRepository::list_by_switch(&conn, &intent.switch_id)?
                .into_iter()
                .rev()
                .find(|operation| operation.operation_kind == operation_kind)
        };
        if let Some(mut operation) = latest {
            match operation.status {
                SwitchOperationStatus::Succeeded => return Ok(OperationPlan::Skip(operation)),
                SwitchOperationStatus::AboutToSend => match operation.retry_semantics {
                    RetrySemantics::Idempotent => {
                        let conn = self.open_connection()?;
                        SwitchOperationJournalRepository::mark_failed(
                            &conn,
                            &operation.operation_id,
                            Some("retrying_idempotent_operation"),
                        )?;
                    }
                    RetrySemantics::ReconcileBeforeRetry => {
                        let reconciliation = self
                            .inner
                            .executor
                            .reconcile_operation(intent, &operation)
                            .await;
                        self.renew_lease(&intent.switch_id, owner)?;
                        match reconciliation? {
                            OperationReconcileOutcome::Confirmed {
                                native_result_reference,
                            } => {
                                let conn = self.open_connection()?;
                                SwitchOperationJournalRepository::mark_succeeded(
                                    &conn,
                                    &operation.operation_id,
                                    native_result_reference.as_deref(),
                                )?;
                                operation.status = SwitchOperationStatus::Succeeded;
                                operation.native_result_reference = native_result_reference;
                                return Ok(OperationPlan::Skip(operation));
                            }
                            OperationReconcileOutcome::NotFound => {
                                let conn = self.open_connection()?;
                                SwitchOperationJournalRepository::mark_failed(
                                    &conn,
                                    &operation.operation_id,
                                    Some("operation_not_found_during_reconcile"),
                                )?;
                            }
                            OperationReconcileOutcome::Ambiguous => {
                                self.mark_operation_and_switch_ambiguous(intent, &operation)?;
                                return Err(ambiguous_operation_error());
                            }
                        }
                    }
                    RetrySemantics::NonRetryableWhenAmbiguous => {
                        self.mark_operation_and_switch_ambiguous(intent, &operation)?;
                        return Err(ambiguous_operation_error());
                    }
                },
                SwitchOperationStatus::Failed => {}
                SwitchOperationStatus::AmbiguousExternalEffect => {
                    self.ensure_switch_ambiguous(intent)?;
                    return Err(ambiguous_operation_error());
                }
            }
        }

        let operation_id = RuntimeSwitchOperationId::new();
        let adapter_idempotency_token =
            use_adapter_token.then(|| operation_id.as_str().to_string());
        let sequence = {
            let conn = self.open_connection()?;
            SwitchOperationJournalRepository::max_sequence(&conn, &intent.switch_id)?
                .unwrap_or(-1)
                .saturating_add(1)
        };
        {
            let conn = self.open_connection()?;
            SwitchOperationJournalRepository::append_about_to_send(
                &conn,
                &SwitchOperationAppendRequest {
                    operation_id: operation_id.clone(),
                    switch_id: intent.switch_id.clone(),
                    sequence,
                    operation_kind: operation_kind.to_string(),
                    request_fingerprint,
                    adapter_idempotency_token: adapter_idempotency_token.clone(),
                    retry_semantics,
                },
            )?;
        }
        Ok(OperationPlan::Execute(JournaledOperation {
            operation_id,
            sequence,
            operation_kind: operation_kind.to_string(),
            adapter_idempotency_token,
        }))
    }

    fn mark_operation_succeeded(
        &self,
        operation: &JournaledOperation,
        native_result_reference: Option<&str>,
    ) -> VibexResult<()> {
        let conn = self.open_connection()?;
        SwitchOperationJournalRepository::mark_succeeded(
            &conn,
            &operation.operation_id,
            native_result_reference,
        )
    }

    fn mark_operation_failed(
        &self,
        operation: &JournaledOperation,
        error_code: &str,
    ) -> VibexResult<()> {
        let conn = self.open_connection()?;
        SwitchOperationJournalRepository::mark_failed(
            &conn,
            &operation.operation_id,
            Some(error_code),
        )
    }

    fn mark_operation_and_switch_ambiguous(
        &self,
        intent: &SwitchIntent,
        operation: &SwitchOperationRecord,
    ) -> VibexResult<()> {
        {
            let conn = self.open_connection()?;
            SwitchOperationJournalRepository::mark_ambiguous(
                &conn,
                &operation.operation_id,
                Some("external_effect_outcome_ambiguous"),
            )?;
        }
        self.ensure_switch_ambiguous(intent)
    }

    fn ensure_switch_ambiguous(&self, intent: &SwitchIntent) -> VibexResult<()> {
        let current = self.required_switch(&intent.switch_id)?;
        if current.status == RuntimeSwitchStatus::AmbiguousExternalEffect {
            return Ok(());
        }
        let mut conn = self.open_connection()?;
        RuntimeSwitchRepository::mark_ambiguous_external_effect(
            &mut conn,
            &intent.session_id,
            &intent.switch_id,
            "runtime_switch_ambiguous_external_effect",
            Some("external_effect_outcome_ambiguous"),
        )
    }

    fn get_binding(&self, binding_id: &RuntimeBindingId) -> VibexResult<Option<RuntimeBinding>> {
        let conn = self.open_connection()?;
        RuntimeBindingRepository::get(&conn, binding_id)
    }

    fn required_binding(&self, binding_id: &RuntimeBindingId) -> VibexResult<RuntimeBinding> {
        self.get_binding(binding_id)?.ok_or_else(|| {
            VibexError::validation("runtime_binding_not_found", "runtime binding was not found")
        })
    }
}

fn busy_error_code(kind: ActiveWorkKind) -> &'static str {
    match kind {
        ActiveWorkKind::ActiveTurn => "runtime_switch_busy_active_turn",
        ActiveWorkKind::PendingPermission => "runtime_switch_busy_pending_permission",
        ActiveWorkKind::ActiveTerminal => "runtime_switch_busy_active_terminal",
        ActiveWorkKind::BackgroundWork => "runtime_switch_busy_background_work",
    }
}

fn cancel_operation_kind(kind: ActiveWorkKind) -> &'static str {
    match kind {
        ActiveWorkKind::ActiveTurn => OP_CANCEL_ACTIVE_TURN,
        ActiveWorkKind::PendingPermission => OP_RESOLVE_PENDING_PERMISSION,
        ActiveWorkKind::ActiveTerminal => OP_CLOSE_TERMINAL,
        ActiveWorkKind::BackgroundWork => OP_CANCEL_BACKGROUND_WORK,
    }
}

fn metric_operation_for_active_work(kind: ActiveWorkKind) -> RuntimeMetricOperation {
    match kind {
        ActiveWorkKind::ActiveTurn => RuntimeMetricOperation::ActiveTurn,
        ActiveWorkKind::PendingPermission => RuntimeMetricOperation::PendingPermission,
        ActiveWorkKind::ActiveTerminal => RuntimeMetricOperation::ActiveTerminal,
        ActiveWorkKind::BackgroundWork => RuntimeMetricOperation::BackgroundWork,
    }
}

fn metric_result_for_switch_status(status: RuntimeSwitchStatus) -> RuntimeMetricResult {
    match status {
        RuntimeSwitchStatus::Committed => RuntimeMetricResult::Committed,
        RuntimeSwitchStatus::Cancelled => RuntimeMetricResult::Cancelled,
        RuntimeSwitchStatus::Superseded => RuntimeMetricResult::Superseded,
        RuntimeSwitchStatus::AmbiguousExternalEffect => RuntimeMetricResult::Ambiguous,
        RuntimeSwitchStatus::Failed => RuntimeMetricResult::Failure,
        RuntimeSwitchStatus::Requested
        | RuntimeSwitchStatus::Reserved
        | RuntimeSwitchStatus::WaitingForIdle
        | RuntimeSwitchStatus::Preparing
        | RuntimeSwitchStatus::Prepared
        | RuntimeSwitchStatus::Committing => RuntimeMetricResult::Success,
    }
}

fn metric_result_for_error_code(code: &str) -> RuntimeMetricResult {
    if code.contains("ambiguous") {
        RuntimeMetricResult::Ambiguous
    } else if code.contains("timeout") {
        RuntimeMetricResult::TimedOut
    } else if code.contains("superseded") {
        RuntimeMetricResult::Superseded
    } else {
        RuntimeMetricResult::Failure
    }
}

fn switch_log_context(intent: &SwitchIntent, operation: &'static str) -> RuntimeLogContext {
    let mut context = RuntimeLogContext::new(operation)
        .with_logical_session_id(&intent.session_id)
        .with_switch_id(&intent.switch_id)
        .with_agent_id(&intent.target_selection.agent_id)
        .with_adapter_id(&intent.target_adapter_id)
        .with_provider_profile_id(&intent.target_selection.provider_profile_id);
    if let Some(binding_id) = intent
        .target_binding_id
        .as_ref()
        .or(intent.source_binding_id.as_ref())
    {
        context = context.with_binding_id(binding_id);
    }
    context
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn ambiguous_operation_error() -> VibexError {
    VibexError::conflict(
        "runtime_switch_ambiguous_external_effect",
        "runtime switch stopped because an external effect could not be reconciled",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use vibex_core::{
        AgentId, AgentSession, AgentSessionSafety, AgentSessionState, NativeStateHomeId,
        ProviderKind, ProviderProfileId, RequestId, SessionRuntimeConfigState,
        SessionRuntimeSelectionStatus, TransportKind, WorkspaceMode,
    };
    use vibex_db::{
        DesiredRuntimeSwitchEnqueueRequest, DesiredRuntimeSwitchEnqueueResult, SessionRepository,
        WorkspaceRepository,
    };

    use super::*;

    #[derive(Clone)]
    struct MockExecutor {
        state: Arc<Mutex<MockExecutorState>>,
    }

    #[derive(Clone)]
    struct MockExecutorState {
        assessment: SwitchTargetAssessment,
        calls: Vec<String>,
        fail_on: Option<String>,
        delay_ms: u64,
        delay_on: Option<String>,
        reconcile_outcome: OperationReconcileOutcome,
    }

    impl Default for MockExecutorState {
        fn default() -> Self {
            Self {
                assessment: restart_assessment(),
                calls: Vec::new(),
                fail_on: None,
                delay_ms: 0,
                delay_on: None,
                reconcile_outcome: OperationReconcileOutcome::NotFound,
            }
        }
    }

    impl MockExecutor {
        fn new() -> Self {
            Self {
                state: Arc::new(Mutex::new(MockExecutorState::default())),
            }
        }

        fn record(&self, call: impl Into<String>) {
            self.state.lock().unwrap().calls.push(call.into());
        }

        fn calls(&self) -> Vec<String> {
            self.state.lock().unwrap().calls.clone()
        }

        fn clear_calls(&self) {
            self.state.lock().unwrap().calls.clear();
        }

        fn set_assessment(&self, assessment: SwitchTargetAssessment) {
            self.state.lock().unwrap().assessment = assessment;
        }

        fn fail_on(&self, operation: &str) {
            self.state.lock().unwrap().fail_on = Some(operation.to_string());
        }

        fn set_delay_ms(&self, delay_ms: u64) {
            let mut state = self.state.lock().unwrap();
            state.delay_ms = delay_ms;
            state.delay_on = None;
        }

        fn set_delay_on(&self, operation: &str, delay_ms: u64) {
            let mut state = self.state.lock().unwrap();
            state.delay_ms = delay_ms;
            state.delay_on = Some(operation.to_string());
        }

        fn set_reconcile_outcome(&self, outcome: OperationReconcileOutcome) {
            self.state.lock().unwrap().reconcile_outcome = outcome;
        }

        async fn before_operation(&self, operation: &str) -> VibexResult<()> {
            self.record(operation);
            let (delay_ms, fail) = {
                let state = self.state.lock().unwrap();
                (
                    if state
                        .delay_on
                        .as_deref()
                        .is_none_or(|value| value == operation)
                    {
                        state.delay_ms
                    } else {
                        0
                    },
                    state.fail_on.as_deref() == Some(operation),
                )
            };
            if delay_ms > 0 {
                sleep(Duration::from_millis(delay_ms)).await;
            }
            if fail {
                return Err(VibexError::provider(
                    "mock_prepare_failed",
                    "mock target preparation failed",
                ));
            }
            Ok(())
        }

        fn prepared_binding(intent: &SwitchIntent) -> RuntimeBinding {
            let now = unix_timestamp_ms();
            RuntimeBinding {
                binding_id: intent.target_binding_id.clone().unwrap(),
                session_id: intent.session_id.clone(),
                agent_id: intent.target_selection.agent_id.clone(),
                transport_kind: TransportKind::Acp,
                provider_profile_id: intent.target_selection.provider_profile_id.clone(),
                adapter_id: intent.target_adapter_id.clone(),
                adapter_version: "mock-v1".to_string(),
                adapter_compatibility_identity: "mock-compatible-v1".to_string(),
                native_session_id: Some(format!(
                    "native:{}",
                    intent.target_binding_id.as_ref().unwrap().as_str()
                )),
                native_state_home_id: NativeStateHomeId::new(),
                provider_resume_identity: None,
                process_spawn_fingerprint: "mock-fingerprint".to_string(),
                session_runtime_config_state: SessionRuntimeConfigState::default(),
                capability_snapshot: None,
                restore_compatibility_key: None,
                profile_revision: 1,
                last_context_sequence: 0,
                last_summary_sequence: 0,
                context_bridge_version: 0,
                activation_generation: 0,
                binding_state: BindingState::Preparing,
                created_by_switch_id: Some(intent.switch_id.clone()),
                created_at_ms: now,
                updated_at_ms: now,
            }
        }

        fn attachment(intent: &SwitchIntent) -> PreparedAttachment {
            PreparedAttachment {
                binding: Self::prepared_binding(intent),
                opaque_handle: "opaque-attachment".to_string(),
                restore_result: None,
            }
        }
    }

    #[async_trait]
    impl SwitchTargetExecutor for MockExecutor {
        async fn assess_target(
            &self,
            _intent: &SwitchIntent,
        ) -> VibexResult<SwitchTargetAssessment> {
            self.record("assess_target");
            Ok(self.state.lock().unwrap().assessment.clone())
        }

        async fn ensure_process(
            &self,
            _intent: &SwitchIntent,
            _operation: &JournaledOperation,
        ) -> VibexResult<PreparedProcess> {
            self.before_operation(OP_SPAWN_PROCESS).await?;
            Ok(PreparedProcess {
                opaque_handle: "opaque-process".to_string(),
            })
        }

        async fn reacquire_process(&self, _intent: &SwitchIntent) -> VibexResult<PreparedProcess> {
            self.before_operation("reacquire_process").await?;
            Ok(PreparedProcess {
                opaque_handle: "opaque-process".to_string(),
            })
        }

        async fn restore_or_create_session(
            &self,
            intent: &SwitchIntent,
            _process: &PreparedProcess,
            strategy: RuntimeSwitchStrategy,
            _operation: &JournaledOperation,
        ) -> VibexResult<PreparedAttachment> {
            let operation = match strategy {
                RuntimeSwitchStrategy::RestartAndResume => OP_RESTORE_SESSION,
                RuntimeSwitchStrategy::RestartFreshAndBridge => OP_CREATE_SESSION,
                RuntimeSwitchStrategy::LiveMutation => unreachable!(),
            };
            self.before_operation(operation).await?;
            Ok(Self::attachment(intent))
        }

        async fn recover_attachment(
            &self,
            intent: &SwitchIntent,
            operation: &SwitchOperationRecord,
        ) -> VibexResult<PreparedAttachment> {
            self.before_operation("recover_attachment").await?;
            let mut attachment = Self::attachment(intent);
            if operation.native_result_reference.is_some() {
                attachment.binding.native_session_id = operation.native_result_reference.clone();
            }
            Ok(attachment)
        }

        async fn acquire_prepared(
            &self,
            _intent: &SwitchIntent,
            binding: &RuntimeBinding,
        ) -> VibexResult<PreparedAttachment> {
            self.before_operation("acquire_prepared").await?;
            Ok(PreparedAttachment {
                binding: binding.clone(),
                opaque_handle: "opaque-attachment".to_string(),
                restore_result: None,
            })
        }

        async fn apply_session_config(
            &self,
            _intent: &SwitchIntent,
            _attachment: &PreparedAttachment,
            _operation: &JournaledOperation,
        ) -> VibexResult<()> {
            self.before_operation(OP_APPLY_SESSION_CONFIG).await
        }

        async fn apply_live_mutation(
            &self,
            _intent: &SwitchIntent,
            _attachment: &PreparedAttachment,
            _operation: &JournaledOperation,
        ) -> VibexResult<()> {
            self.before_operation(OP_APPLY_LIVE_MUTATION).await
        }

        async fn build_context_delta(
            &self,
            _intent: &SwitchIntent,
            _attachment: &PreparedAttachment,
        ) -> VibexResult<()> {
            self.before_operation("build_context_delta").await
        }

        async fn revalidate_prepared(
            &self,
            _intent: &SwitchIntent,
            _attachment: &PreparedAttachment,
        ) -> VibexResult<()> {
            self.before_operation("revalidate_prepared").await
        }

        async fn activate(
            &self,
            _intent: &SwitchIntent,
            _attachment: &PreparedAttachment,
            activation_generation: i64,
        ) -> VibexResult<()> {
            self.before_operation(&format!("activate:{activation_generation}"))
                .await
        }

        async fn cleanup_target(
            &self,
            _intent: &SwitchIntent,
            _attachment: Option<&PreparedAttachment>,
        ) -> VibexResult<()> {
            self.before_operation("cleanup_target").await
        }

        async fn cleanup_source_after_commit(&self, _intent: &SwitchIntent) -> VibexResult<()> {
            self.before_operation("cleanup_source").await
        }

        async fn reconcile_operation(
            &self,
            _intent: &SwitchIntent,
            operation: &SwitchOperationRecord,
        ) -> VibexResult<OperationReconcileOutcome> {
            self.before_operation(&format!("reconcile: {}", operation.operation_kind))
                .await?;
            Ok(self.state.lock().unwrap().reconcile_outcome.clone())
        }
    }

    #[derive(Clone)]
    struct MockActiveWorkGate {
        state: Arc<Mutex<MockGateState>>,
    }

    #[derive(Default)]
    struct MockGateState {
        snapshot: ActiveWorkSnapshot,
        prompt_gate_events: Vec<bool>,
        cancel_calls: Vec<ActiveWorkKind>,
        cancel_supported: bool,
    }

    impl MockActiveWorkGate {
        fn new() -> Self {
            Self {
                state: Arc::new(Mutex::new(MockGateState {
                    cancel_supported: true,
                    ..MockGateState::default()
                })),
            }
        }

        fn set_active(&self, kind: ActiveWorkKind, active: bool) {
            let mut state = self.state.lock().unwrap();
            set_snapshot_kind(&mut state.snapshot, kind, active);
        }

        fn set_cancel_supported(&self, supported: bool) {
            self.state.lock().unwrap().cancel_supported = supported;
        }

        fn prompt_gate_events(&self) -> Vec<bool> {
            self.state.lock().unwrap().prompt_gate_events.clone()
        }

        fn cancel_calls(&self) -> Vec<ActiveWorkKind> {
            self.state.lock().unwrap().cancel_calls.clone()
        }
    }

    #[async_trait]
    impl ActiveWorkGate for MockActiveWorkGate {
        async fn probe(&self, _session_id: &VibexSessionId) -> VibexResult<ActiveWorkSnapshot> {
            Ok(self.state.lock().unwrap().snapshot)
        }

        async fn set_prompt_gate(
            &self,
            _session_id: &VibexSessionId,
            closed: bool,
        ) -> VibexResult<()> {
            self.state.lock().unwrap().prompt_gate_events.push(closed);
            Ok(())
        }

        async fn cancel(
            &self,
            _session_id: &VibexSessionId,
            kind: ActiveWorkKind,
            _operation: &JournaledOperation,
        ) -> VibexResult<()> {
            let mut state = self.state.lock().unwrap();
            state.cancel_calls.push(kind);
            if !state.cancel_supported {
                return Err(VibexError::capability(
                    "runtime_switch_cancel_unsupported",
                    "mock active work cannot be cancelled",
                ));
            }
            set_snapshot_kind(&mut state.snapshot, kind, false);
            Ok(())
        }
    }

    struct TestEnvironment {
        db_path: PathBuf,
        project_dir: PathBuf,
        session_id: VibexSessionId,
        source_binding: RuntimeBinding,
        selection: SessionRuntimeSelection,
        executor: MockExecutor,
        gate: MockActiveWorkGate,
        coordinator: RuntimeSwitchCoordinator,
    }

    impl TestEnvironment {
        fn new(label: &str) -> Self {
            let db_path = std::env::temp_dir().join(format!(
                "vibex-agent-runtime-switch-{label}-{}.db",
                RequestId::new().as_str()
            ));
            let project_dir = std::env::temp_dir().join(format!(
                "vibex-agent-runtime-switch-project-{label}-{}",
                RequestId::new().as_str()
            ));
            fs::create_dir_all(&project_dir).unwrap();
            let mut conn = open_database(&db_path).unwrap();
            apply_migrations(&mut conn).unwrap();
            let (project, workspace) =
                WorkspaceRepository::ensure(&conn, &project_dir, WorkspaceMode::CurrentCheckout)
                    .unwrap();
            let profile_id =
                ProviderProfileId::parse(ProviderKind::Acp.local_default_profile_id().to_string())
                    .unwrap();
            let agent_id = AgentId::parse("claude-code").unwrap();
            let session_id = VibexSessionId::new();
            let now = unix_timestamp_ms();
            SessionRepository::insert(
                &conn,
                &AgentSession {
                    id: session_id.clone(),
                    title: format!("runtime switch {label}"),
                    project_id: project.id,
                    workspace_id: workspace.id,
                    workspace_root: workspace.root_path,
                    workspace_mode: workspace.mode,
                    agent_id: agent_id.clone(),
                    state: AgentSessionState::Idle,
                    safety: AgentSessionSafety::workspace_write_ask_on_risk(),
                    created_at_ms: now,
                    updated_at_ms: now,
                    archived_at_ms: None,
                    deleted_at_ms: None,
                },
            )
            .unwrap();
            let selection = SessionRuntimeSelection {
                agent_id: agent_id.clone(),
                provider_profile_id: profile_id.clone(),
                model_id: "model-next".to_string(),
                reasoning_effort: Some("high".to_string()),
                mode_id: None,
                config_values: Default::default(),
            };
            let source_binding = RuntimeBinding {
                binding_id: RuntimeBindingId::new(),
                session_id: session_id.clone(),
                agent_id: agent_id.clone(),
                transport_kind: TransportKind::Acp,
                provider_profile_id: profile_id,
                adapter_id: AcpAdapterId::parse("claude-code-acp").unwrap(),
                adapter_version: "source-v1".to_string(),
                adapter_compatibility_identity: "source-compatible-v1".to_string(),
                native_session_id: Some("source-native-session".to_string()),
                native_state_home_id: NativeStateHomeId::new(),
                provider_resume_identity: None,
                process_spawn_fingerprint: "source-fingerprint".to_string(),
                session_runtime_config_state: SessionRuntimeConfigState::default(),
                capability_snapshot: None,
                restore_compatibility_key: None,
                profile_revision: 1,
                last_context_sequence: 0,
                last_summary_sequence: 0,
                context_bridge_version: 0,
                activation_generation: 0,
                binding_state: BindingState::Current,
                created_by_switch_id: None,
                created_at_ms: now,
                updated_at_ms: now,
            };
            RuntimeBindingRepository::insert(&conn, &source_binding).unwrap();
            let selection_json = serde_json::to_string(&selection).unwrap();
            conn.execute(
                "UPDATE agent_sessions
                 SET current_agent_id = ?2, current_binding_id = ?3,
                     desired_runtime_selection_json = ?4,
                     effective_runtime_selection_json = ?4,
                     runtime_selection_status = ?5
                 WHERE session_id = ?1",
                (
                    session_id.as_str(),
                    agent_id.as_str(),
                    source_binding.binding_id.as_str(),
                    selection_json,
                    "ready",
                ),
            )
            .unwrap();
            drop(conn);

            let executor = MockExecutor::new();
            let gate = MockActiveWorkGate::new();
            let coordinator = RuntimeSwitchCoordinator::new(
                &db_path,
                Arc::new(executor.clone()),
                Arc::new(gate.clone()),
                RuntimeSwitchCoordinatorConfig {
                    lease_duration_ms: 500,
                    idle_poll_interval: Duration::from_millis(2),
                },
            )
            .unwrap();
            Self {
                db_path,
                project_dir,
                session_id,
                source_binding,
                selection,
                executor,
                gate,
                coordinator,
            }
        }

        fn request(&self, key: &str) -> RuntimeSwitchRequest {
            RuntimeSwitchRequest {
                session_id: self.session_id.clone(),
                idempotency_key: key.to_string(),
                expected_revision: 0,
                expected_current_binding_id: Some(self.source_binding.binding_id.clone()),
                desired_selection_revision: 0,
                target_adapter_id: self.source_binding.adapter_id.clone(),
                target_selection: self.selection.clone(),
                requested_policy: RuntimeSwitchPolicy::ForceFreshSession,
                active_work_policy: RuntimeSwitchActiveWorkPolicy::default(),
                requested_session_config: Some(serde_json::json!({"model": "model-next"})),
            }
        }

        fn connection(&self) -> DbConnection {
            open_database(&self.db_path).unwrap()
        }

        fn switch_by_key(&self, key: &str) -> RuntimeSwitchRecord {
            RuntimeSwitchRepository::get_by_idempotency_key(
                &self.connection(),
                &self.session_id,
                key,
            )
            .unwrap()
            .unwrap()
        }

        fn runtime_state(&self) -> vibex_db::AgentSessionRuntimeState {
            AgentSessionRuntimeRepository::get_runtime_state(&self.connection(), &self.session_id)
                .unwrap()
                .unwrap()
        }

        fn seed_reserved_switch(&self, key: &str) -> RuntimeSwitchRecord {
            let durable_config = serde_json::to_value(DurableRequestedConfig {
                effective_selection: self.selection.clone(),
                session_config: Some(serde_json::json!({"model": "model-next"})),
            })
            .unwrap();
            let mut conn = self.connection();
            RuntimeSwitchRepository::reserve(
                &mut conn,
                RuntimeSwitchId::new(),
                &RuntimeSwitchReserveRequest {
                    session_id: self.session_id.clone(),
                    idempotency_key: key.to_string(),
                    expected_revision: 0,
                    expected_current_binding_id: Some(self.source_binding.binding_id.clone()),
                    desired_selection_revision: 0,
                    target_binding_id: Some(RuntimeBindingId::new()),
                    target_agent_id: self.selection.agent_id.clone(),
                    target_adapter_id: self.source_binding.adapter_id.clone(),
                    target_profile_id: self.selection.provider_profile_id.clone(),
                    requested_policy: Some(
                        serde_json::to_value(RuntimeSwitchPolicy::ForceFreshSession).unwrap(),
                    ),
                    active_work_policy: Some(
                        serde_json::to_value(RuntimeSwitchActiveWorkPolicy::default()).unwrap(),
                    ),
                    requested_session_config: Some(durable_config),
                },
            )
            .unwrap()
        }

        fn seed_requested_switch(
            &self,
            key: &str,
            expected_selection_revision: i64,
            selection: SessionRuntimeSelection,
        ) -> RuntimeSwitchRecord {
            let requested_session_config = RuntimeSwitchCoordinator::encode_requested_config(
                &selection,
                Some(serde_json::json!({"model": selection.model_id})),
            )
            .unwrap();
            let result = AgentSessionRuntimeRepository::enqueue_desired_switch(
                &mut self.connection(),
                RuntimeSwitchId::new(),
                &DesiredRuntimeSwitchEnqueueRequest {
                    session_id: self.session_id.clone(),
                    idempotency_key: key.to_string(),
                    expected_revision: 0,
                    expected_selection_revision,
                    target_binding_id: RuntimeBindingId::new(),
                    target_adapter_id: self.source_binding.adapter_id.clone(),
                    desired: selection,
                    requested_policy: RuntimeSwitchPolicy::ForceFreshSession,
                    active_work_policy: RuntimeSwitchActiveWorkPolicy::default(),
                    requested_session_config,
                },
            )
            .unwrap();
            let DesiredRuntimeSwitchEnqueueResult::Enqueued(record) = result else {
                panic!("changed desired selection must enqueue a switch");
            };
            record
        }

        fn seed_preparing_switch(
            &self,
            key: &str,
            create_retry_semantics: RetrySemantics,
            adapter_token: Option<&str>,
        ) -> RuntimeSwitchRecord {
            let record = self.seed_reserved_switch(key);
            let conn = self.connection();
            RuntimeSwitchRepository::advance_status(
                &conn,
                &record.switch_id,
                RuntimeSwitchStatus::Reserved,
                RuntimeSwitchStatus::Preparing,
            )
            .unwrap();
            let spawn_id = RuntimeSwitchOperationId::new();
            SwitchOperationJournalRepository::append_about_to_send(
                &conn,
                &SwitchOperationAppendRequest {
                    operation_id: spawn_id.clone(),
                    switch_id: record.switch_id.clone(),
                    sequence: 0,
                    operation_kind: OP_SPAWN_PROCESS.to_string(),
                    request_fingerprint: "process:seeded".to_string(),
                    adapter_idempotency_token: Some(spawn_id.as_str().to_string()),
                    retry_semantics: RetrySemantics::Idempotent,
                },
            )
            .unwrap();
            SwitchOperationJournalRepository::mark_succeeded(&conn, &spawn_id, None).unwrap();
            SwitchOperationJournalRepository::append_about_to_send(
                &conn,
                &SwitchOperationAppendRequest {
                    operation_id: RuntimeSwitchOperationId::new(),
                    switch_id: record.switch_id.clone(),
                    sequence: 1,
                    operation_kind: OP_CREATE_SESSION.to_string(),
                    request_fingerprint: "session:seeded:create".to_string(),
                    adapter_idempotency_token: adapter_token.map(str::to_string),
                    retry_semantics: create_retry_semantics,
                },
            )
            .unwrap();
            RuntimeSwitchRepository::get(&conn, &record.switch_id)
                .unwrap()
                .unwrap()
        }

        fn seed_prepared_switch(&self, key: &str) -> RuntimeSwitchRecord {
            let record =
                self.seed_preparing_switch(key, RetrySemantics::NonRetryableWhenAmbiguous, None);
            let intent = SwitchIntent::from_record(&record).unwrap();
            let binding = MockExecutor::prepared_binding(&intent);
            let conn = self.connection();
            RuntimeBindingRepository::insert(&conn, &binding).unwrap();
            let create = SwitchOperationJournalRepository::list_by_switch(&conn, &record.switch_id)
                .unwrap()
                .into_iter()
                .find(|operation| operation.operation_kind == OP_CREATE_SESSION)
                .unwrap();
            SwitchOperationJournalRepository::mark_succeeded(
                &conn,
                &create.operation_id,
                binding.native_session_id.as_deref(),
            )
            .unwrap();
            let config_id = RuntimeSwitchOperationId::new();
            SwitchOperationJournalRepository::append_about_to_send(
                &conn,
                &SwitchOperationAppendRequest {
                    operation_id: config_id.clone(),
                    switch_id: record.switch_id.clone(),
                    sequence: 2,
                    operation_kind: OP_APPLY_SESSION_CONFIG.to_string(),
                    request_fingerprint: "session_config:present=true".to_string(),
                    adapter_idempotency_token: None,
                    retry_semantics: RetrySemantics::Idempotent,
                },
            )
            .unwrap();
            SwitchOperationJournalRepository::mark_succeeded(&conn, &config_id, None).unwrap();
            RuntimeSwitchRepository::advance_status(
                &conn,
                &record.switch_id,
                RuntimeSwitchStatus::Preparing,
                RuntimeSwitchStatus::Prepared,
            )
            .unwrap();
            RuntimeSwitchRepository::get(&conn, &record.switch_id)
                .unwrap()
                .unwrap()
        }

        fn seed_config_about_to_send(&self, key: &str) -> RuntimeSwitchRecord {
            let record =
                self.seed_preparing_switch(key, RetrySemantics::NonRetryableWhenAmbiguous, None);
            let intent = SwitchIntent::from_record(&record).unwrap();
            let binding = MockExecutor::prepared_binding(&intent);
            let conn = self.connection();
            RuntimeBindingRepository::insert(&conn, &binding).unwrap();
            let create = SwitchOperationJournalRepository::list_by_switch(&conn, &record.switch_id)
                .unwrap()
                .into_iter()
                .find(|operation| operation.operation_kind == OP_CREATE_SESSION)
                .unwrap();
            SwitchOperationJournalRepository::mark_succeeded(
                &conn,
                &create.operation_id,
                binding.native_session_id.as_deref(),
            )
            .unwrap();
            SwitchOperationJournalRepository::append_about_to_send(
                &conn,
                &SwitchOperationAppendRequest {
                    operation_id: RuntimeSwitchOperationId::new(),
                    switch_id: record.switch_id.clone(),
                    sequence: 2,
                    operation_kind: OP_APPLY_SESSION_CONFIG.to_string(),
                    request_fingerprint: "session_config:present=true".to_string(),
                    adapter_idempotency_token: None,
                    retry_semantics: RetrySemantics::Idempotent,
                },
            )
            .unwrap();
            RuntimeSwitchRepository::get(&conn, &record.switch_id)
                .unwrap()
                .unwrap()
        }
    }

    impl Drop for TestEnvironment {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.db_path);
            let _ = fs::remove_file(self.db_path.with_extension("db-wal"));
            let _ = fs::remove_file(self.db_path.with_extension("db-shm"));
            let _ = fs::remove_dir_all(&self.project_dir);
        }
    }

    fn restart_assessment() -> SwitchTargetAssessment {
        SwitchTargetAssessment {
            same_route: true,
            process_config_changed: true,
            session_scoped_changes_only: false,
            live_ops_supported: true,
            exact_descriptor: true,
            runtime_evidence_verified: true,
            projection_fingerprint_matches: true,
            active_turn: false,
            restore: RestoreAssessment::Compatible,
            resumable_historical_binding: false,
            supports_client_idempotency: true,
        }
    }

    fn fresh_assessment() -> SwitchTargetAssessment {
        SwitchTargetAssessment {
            same_route: false,
            process_config_changed: true,
            session_scoped_changes_only: false,
            live_ops_supported: false,
            exact_descriptor: false,
            runtime_evidence_verified: false,
            projection_fingerprint_matches: false,
            active_turn: false,
            restore: RestoreAssessment::Incompatible,
            resumable_historical_binding: false,
            supports_client_idempotency: false,
        }
    }

    fn live_assessment() -> SwitchTargetAssessment {
        SwitchTargetAssessment {
            same_route: true,
            process_config_changed: false,
            session_scoped_changes_only: true,
            live_ops_supported: true,
            exact_descriptor: true,
            runtime_evidence_verified: true,
            projection_fingerprint_matches: true,
            active_turn: false,
            restore: RestoreAssessment::Compatible,
            resumable_historical_binding: false,
            supports_client_idempotency: true,
        }
    }

    fn set_snapshot_kind(snapshot: &mut ActiveWorkSnapshot, kind: ActiveWorkKind, active: bool) {
        match kind {
            ActiveWorkKind::ActiveTurn => snapshot.active_turn = active,
            ActiveWorkKind::PendingPermission => snapshot.pending_permission = active,
            ActiveWorkKind::ActiveTerminal => snapshot.active_terminal = active,
            ActiveWorkKind::BackgroundWork => snapshot.background_work = active,
        }
    }

    fn set_policy_disposition(
        policy: &mut RuntimeSwitchActiveWorkPolicy,
        kind: ActiveWorkKind,
        disposition: BusyDisposition,
    ) {
        match kind {
            ActiveWorkKind::ActiveTurn => policy.active_turn = disposition,
            ActiveWorkKind::PendingPermission => policy.pending_permission = disposition,
            ActiveWorkKind::ActiveTerminal => policy.active_terminal = disposition,
            ActiveWorkKind::BackgroundWork => policy.background_work = disposition,
        }
    }

    async fn wait_for_executor_call(executor: &MockExecutor, call: &str) {
        for _ in 0..100 {
            if executor.calls().iter().any(|value| value == call) {
                return;
            }
            sleep(Duration::from_millis(2)).await;
        }
        panic!("executor did not record {call}");
    }

    #[test]
    fn strategy_matrix_covers_live_resume_and_fresh_fallbacks() {
        assert_eq!(
            decide_switch_strategy(RuntimeSwitchPolicy::Automatic, &live_assessment()),
            RuntimeSwitchStrategy::LiveMutation
        );
        assert_eq!(
            decide_switch_strategy(RuntimeSwitchPolicy::PreferResume, &live_assessment()),
            RuntimeSwitchStrategy::RestartAndResume
        );
        assert_eq!(
            decide_switch_strategy(RuntimeSwitchPolicy::Automatic, &restart_assessment()),
            RuntimeSwitchStrategy::RestartAndResume
        );
        assert_eq!(
            decide_switch_strategy(RuntimeSwitchPolicy::Automatic, &fresh_assessment()),
            RuntimeSwitchStrategy::RestartFreshAndBridge
        );
        assert_eq!(
            decide_switch_strategy(
                RuntimeSwitchPolicy::ForceFreshSession,
                &restart_assessment()
            ),
            RuntimeSwitchStrategy::RestartFreshAndBridge
        );

        let mut no_live_capability = live_assessment();
        no_live_capability.live_ops_supported = false;
        assert_eq!(
            decide_switch_strategy(RuntimeSwitchPolicy::PreferLiveMutation, &no_live_capability),
            RuntimeSwitchStrategy::RestartAndResume
        );
        let mut active_turn = live_assessment();
        active_turn.active_turn = true;
        assert_eq!(
            decide_switch_strategy(RuntimeSwitchPolicy::Automatic, &active_turn),
            RuntimeSwitchStrategy::RestartAndResume
        );
        for missing_gate in [
            |assessment: &mut SwitchTargetAssessment| assessment.exact_descriptor = false,
            |assessment: &mut SwitchTargetAssessment| assessment.runtime_evidence_verified = false,
            |assessment: &mut SwitchTargetAssessment| {
                assessment.projection_fingerprint_matches = false
            },
        ] {
            let mut assessment = live_assessment();
            missing_gate(&mut assessment);
            assert_eq!(
                decide_switch_strategy(RuntimeSwitchPolicy::Automatic, &assessment),
                RuntimeSwitchStrategy::RestartAndResume
            );
        }
    }

    #[tokio::test]
    async fn reserve_conflict_has_zero_executor_and_journal_side_effects() {
        let env = TestEnvironment::new("reserve-conflict");
        let mut request = env.request("reserve-conflict");
        request.expected_revision = 99;
        let error = env.coordinator.request_switch(request).await.unwrap_err();
        assert_eq!(error.code, "runtime_switch_revision_conflict");
        assert!(env.executor.calls().is_empty());
        let conn = env.connection();
        let switches: i64 = conn
            .query_row("SELECT COUNT(*) FROM runtime_switches", [], |row| {
                row.get(0)
            })
            .unwrap();
        let operations: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM runtime_switch_operations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((switches, operations), (0, 0));
    }

    #[tokio::test]
    async fn idempotent_retry_returns_same_committed_switch_without_repeating_effects() {
        let env = TestEnvironment::new("idempotent");
        let first = env
            .coordinator
            .request_switch(env.request("same-key"))
            .await
            .unwrap();
        let second = env
            .coordinator
            .request_switch(env.request("same-key"))
            .await
            .unwrap();
        assert_eq!(first.switch_id, second.switch_id);
        assert_eq!(first.status, RuntimeSwitchStatus::Committed);
        assert_eq!(second.status, RuntimeSwitchStatus::Committed);
        let calls = env.executor.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|call| *call == OP_SPAWN_PROCESS)
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| *call == OP_CREATE_SESSION)
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("activate:"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn concurrent_idempotent_requests_have_one_external_prepare() {
        let env = TestEnvironment::new("concurrent-idempotent");
        env.executor.set_delay_ms(10);
        let coordinator_a = env.coordinator.clone();
        let coordinator_b = env.coordinator.clone();
        let request_a = env.request("concurrent-key");
        let request_b = request_a.clone();
        let (a, b) = tokio::join!(
            coordinator_a.request_switch(request_a),
            coordinator_b.request_switch(request_b)
        );
        let a = a.unwrap();
        let b = b.unwrap();
        assert_eq!(a.switch_id, b.switch_id);
        assert_eq!(
            env.switch_by_key("concurrent-key").status,
            RuntimeSwitchStatus::Committed
        );
        let calls = env.executor.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|call| *call == OP_SPAWN_PROCESS)
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| *call == OP_CREATE_SESSION)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn prepare_failure_keeps_source_current_and_cleans_target() {
        let env = TestEnvironment::new("prepare-failure");
        env.executor.fail_on(OP_CREATE_SESSION);
        let error = env
            .coordinator
            .request_switch(env.request("prepare-failure"))
            .await
            .unwrap_err();
        assert_eq!(error.code, "mock_prepare_failed");
        assert_eq!(
            env.switch_by_key("prepare-failure").status,
            RuntimeSwitchStatus::Failed
        );
        let state = env.runtime_state();
        assert_eq!(
            state.current_binding_id.as_ref(),
            Some(&env.source_binding.binding_id)
        );
        assert_eq!(
            RuntimeBindingRepository::get(&env.connection(), &env.source_binding.binding_id)
                .unwrap()
                .unwrap()
                .binding_state,
            BindingState::Current
        );
        assert!(
            env.executor
                .calls()
                .iter()
                .any(|call| call == "cleanup_target")
        );
        assert_eq!(env.gate.prompt_gate_events().last(), Some(&false));
    }

    #[tokio::test]
    async fn busy_reject_covers_all_active_work_categories_without_journal_effects() {
        for kind in ACTIVE_WORK_KINDS {
            let env = TestEnvironment::new(busy_error_code(kind));
            env.gate.set_active(kind, true);
            let error = env
                .coordinator
                .request_switch(env.request("busy-reject"))
                .await
                .unwrap_err();
            assert_eq!(error.code, busy_error_code(kind));
            let record = env.switch_by_key("busy-reject");
            assert_eq!(record.status, RuntimeSwitchStatus::Failed);
            assert!(
                SwitchOperationJournalRepository::list_by_switch(
                    &env.connection(),
                    &record.switch_id
                )
                .unwrap()
                .is_empty()
            );
            assert!(
                !env.executor
                    .calls()
                    .iter()
                    .any(|call| call == OP_SPAWN_PROCESS || call == OP_CREATE_SESSION)
            );
        }
    }

    #[tokio::test]
    async fn busy_wait_closes_gate_then_continues_after_idle() {
        for kind in ACTIVE_WORK_KINDS {
            let env = TestEnvironment::new(busy_error_code(kind));
            env.gate.set_active(kind, true);
            let mut request = env.request("busy-wait");
            set_policy_disposition(
                &mut request.active_work_policy,
                kind,
                BusyDisposition::Wait { deadline_ms: 500 },
            );
            let coordinator = env.coordinator.clone();
            let task = tokio::spawn(async move { coordinator.request_switch(request).await });
            for _ in 0..100 {
                if env.gate.prompt_gate_events().contains(&true) {
                    break;
                }
                sleep(Duration::from_millis(2)).await;
            }
            env.gate.set_active(kind, false);
            let outcome = task.await.unwrap().unwrap();
            assert_eq!(outcome.status, RuntimeSwitchStatus::Committed);
            let events = env.gate.prompt_gate_events();
            assert!(events.contains(&true));
            assert_eq!(events.last(), Some(&false));
        }
    }

    #[tokio::test]
    async fn busy_wait_timeout_fails_and_reopens_prompt_gate() {
        let env = TestEnvironment::new("busy-wait-timeout");
        env.gate.set_active(ActiveWorkKind::ActiveTurn, true);
        let mut request = env.request("busy-wait-timeout");
        request.active_work_policy.active_turn = BusyDisposition::Wait { deadline_ms: 80 };
        let error = env.coordinator.request_switch(request).await.unwrap_err();
        assert_eq!(error.code, "runtime_switch_wait_timeout");
        assert_eq!(
            env.switch_by_key("busy-wait-timeout").status,
            RuntimeSwitchStatus::Failed
        );
        assert_eq!(env.gate.prompt_gate_events().last(), Some(&false));
    }

    #[tokio::test]
    async fn busy_wait_uses_the_earliest_active_deadline() {
        let env = TestEnvironment::new("busy-wait-earliest-deadline");
        let record = env.seed_reserved_switch("busy-wait-earliest-deadline");
        let policy = RuntimeSwitchActiveWorkPolicy {
            active_turn: BusyDisposition::Wait { deadline_ms: 5 },
            background_work: BusyDisposition::Wait { deadline_ms: 500 },
            ..RuntimeSwitchActiveWorkPolicy::default()
        };
        env.connection()
            .execute(
                "UPDATE runtime_switches
                 SET active_work_policy_json = ?2, created_at_ms = ?3
                 WHERE switch_id = ?1",
                (
                    record.switch_id.as_str(),
                    serde_json::to_string(&policy).unwrap(),
                    unix_timestamp_ms() - 20,
                ),
            )
            .unwrap();
        env.gate.set_active(ActiveWorkKind::ActiveTurn, true);
        env.gate.set_active(ActiveWorkKind::BackgroundWork, true);

        let report = tokio::time::timeout(
            Duration::from_millis(100),
            env.coordinator.reconcile_on_startup(),
        )
        .await
        .expect("the earliest expired wait deadline must stop the switch")
        .unwrap();
        assert!(report.errors.iter().any(|error| {
            error.switch_id == record.switch_id && error.error_code == "runtime_switch_wait_timeout"
        }));
        assert_eq!(
            env.switch_by_key("busy-wait-earliest-deadline").status,
            RuntimeSwitchStatus::Failed
        );
        assert_eq!(env.gate.prompt_gate_events().last(), Some(&false));
    }

    #[tokio::test]
    async fn busy_cancel_journals_each_category_and_requires_confirmation() {
        for kind in ACTIVE_WORK_KINDS {
            let env = TestEnvironment::new(cancel_operation_kind(kind));
            env.gate.set_active(kind, true);
            let mut request = env.request("busy-cancel");
            set_policy_disposition(
                &mut request.active_work_policy,
                kind,
                BusyDisposition::Cancel,
            );
            let outcome = env.coordinator.request_switch(request).await.unwrap();
            assert_eq!(outcome.status, RuntimeSwitchStatus::Committed);
            assert_eq!(env.gate.cancel_calls(), vec![kind]);
            let operations = SwitchOperationJournalRepository::list_by_switch(
                &env.connection(),
                &outcome.switch_id,
            )
            .unwrap();
            assert!(operations.iter().any(|operation| {
                operation.operation_kind == cancel_operation_kind(kind)
                    && operation.status == SwitchOperationStatus::Succeeded
            }));
        }
    }

    #[tokio::test]
    async fn unsupported_busy_cancel_fails_without_faking_idle() {
        let env = TestEnvironment::new("cancel-unsupported");
        env.gate.set_active(ActiveWorkKind::ActiveTerminal, true);
        env.gate.set_cancel_supported(false);
        let mut request = env.request("cancel-unsupported");
        request.active_work_policy.active_terminal = BusyDisposition::Cancel;
        let error = env.coordinator.request_switch(request).await.unwrap_err();
        assert_eq!(error.code, "runtime_switch_cancel_unsupported");
        assert_eq!(
            env.switch_by_key("cancel-unsupported").status,
            RuntimeSwitchStatus::Failed
        );
        assert!(env.gate.state.lock().unwrap().snapshot.active_terminal);
    }

    #[tokio::test]
    async fn ambiguous_cancel_operation_stops_reserved_switch_without_retry() {
        let env = TestEnvironment::new("ambiguous-cancel");
        let record = env.seed_reserved_switch("ambiguous-cancel");
        let policy = RuntimeSwitchActiveWorkPolicy {
            active_turn: BusyDisposition::Cancel,
            ..RuntimeSwitchActiveWorkPolicy::default()
        };
        env.connection()
            .execute(
                "UPDATE runtime_switches SET active_work_policy_json = ?2 WHERE switch_id = ?1",
                (
                    record.switch_id.as_str(),
                    serde_json::to_string(&policy).unwrap(),
                ),
            )
            .unwrap();
        let operation_id = RuntimeSwitchOperationId::new();
        SwitchOperationJournalRepository::append_about_to_send(
            &env.connection(),
            &SwitchOperationAppendRequest {
                operation_id,
                switch_id: record.switch_id.clone(),
                sequence: 0,
                operation_kind: OP_CANCEL_ACTIVE_TURN.to_string(),
                request_fingerprint: "active_work:cancel_active_turn".to_string(),
                adapter_idempotency_token: Some("cancel-operation-token".to_string()),
                retry_semantics: RetrySemantics::ReconcileBeforeRetry,
            },
        )
        .unwrap();
        env.gate.set_active(ActiveWorkKind::ActiveTurn, true);
        env.executor
            .set_reconcile_outcome(OperationReconcileOutcome::Ambiguous);

        let report = env.coordinator.reconcile_on_startup().await.unwrap();
        assert!(report.errors.iter().any(|error| {
            error.switch_id == record.switch_id
                && error.error_code == "runtime_switch_ambiguous_external_effect"
        }));
        assert_eq!(
            env.switch_by_key("ambiguous-cancel").status,
            RuntimeSwitchStatus::AmbiguousExternalEffect
        );
        assert!(env.gate.cancel_calls().is_empty());
        assert_eq!(env.gate.prompt_gate_events().last(), Some(&false));
    }

    #[tokio::test]
    async fn cancel_switch_finishes_reserved_work_and_rejects_committing_work() {
        let env = TestEnvironment::new("cancel-api-reserved");
        let reserved = env.seed_reserved_switch("cancel-api-reserved");
        let outcome = env
            .coordinator
            .cancel_switch(&env.session_id, &reserved.switch_id)
            .await
            .unwrap();
        assert_eq!(outcome.status, RuntimeSwitchStatus::Cancelled);
        assert_eq!(env.runtime_state().pending_switch_id, None);
        assert_eq!(
            env.runtime_state().current_binding_id.as_ref(),
            Some(&env.source_binding.binding_id)
        );

        let prepared_env = TestEnvironment::new("cancel-api-prepared");
        let prepared_switch = prepared_env.seed_prepared_switch("cancel-api-prepared");
        let outcome = prepared_env
            .coordinator
            .cancel_switch(&prepared_env.session_id, &prepared_switch.switch_id)
            .await
            .unwrap();
        assert_eq!(outcome.status, RuntimeSwitchStatus::Cancelled);
        assert_eq!(
            RuntimeBindingRepository::get(
                &prepared_env.connection(),
                prepared_switch.target_binding_id.as_ref().unwrap(),
            )
            .unwrap()
            .unwrap()
            .binding_state,
            BindingState::Failed
        );

        let other = TestEnvironment::new("cancel-api-committing");
        let prepared = other.seed_prepared_switch("cancel-api-committing");
        RuntimeSwitchRepository::advance_status(
            &other.connection(),
            &prepared.switch_id,
            RuntimeSwitchStatus::Prepared,
            RuntimeSwitchStatus::Committing,
        )
        .unwrap();
        let error = other
            .coordinator
            .cancel_switch(&other.session_id, &prepared.switch_id)
            .await
            .unwrap_err();
        assert_eq!(error.code, "runtime_switch_commit_in_progress");
        assert_eq!(
            other.switch_by_key("cancel-api-committing").status,
            RuntimeSwitchStatus::Committing
        );
        assert_eq!(
            other.runtime_state().pending_switch_id.as_ref(),
            Some(&prepared.switch_id)
        );
    }

    #[tokio::test]
    async fn live_mutation_commits_same_binding_and_advances_generation() {
        let env = TestEnvironment::new("live-mutation");
        env.executor.set_assessment(live_assessment());
        let mut request = env.request("live-mutation");
        request.requested_policy = RuntimeSwitchPolicy::PreferLiveMutation;
        let outcome = env.coordinator.request_switch(request).await.unwrap();
        assert_eq!(outcome.status, RuntimeSwitchStatus::Committed);
        let record = env.switch_by_key("live-mutation");
        assert_eq!(
            record.target_binding_id.as_ref(),
            Some(&env.source_binding.binding_id)
        );
        let state = env.runtime_state();
        assert_eq!(
            state.current_binding_id.as_ref(),
            Some(&env.source_binding.binding_id)
        );
        assert_eq!((state.revision, state.activation_generation), (1, 1));
        let calls = env.executor.calls();
        assert!(calls.iter().any(|call| call == OP_APPLY_LIVE_MUTATION));
        assert!(
            !calls
                .iter()
                .any(|call| call == OP_SPAWN_PROCESS || call == OP_CREATE_SESSION)
        );
    }

    #[tokio::test]
    async fn dropping_caller_does_not_cancel_reserved_switch_driver() {
        let env = TestEnvironment::new("cancellation-shield");
        env.executor.set_delay_ms(20);
        let coordinator = env.coordinator.clone();
        let request = env.request("cancellation-shield");
        let caller = tokio::spawn(async move { coordinator.request_switch(request).await });
        wait_for_executor_call(&env.executor, OP_SPAWN_PROCESS).await;
        caller.abort();
        for _ in 0..200 {
            if env.switch_by_key("cancellation-shield").status == RuntimeSwitchStatus::Committed {
                return;
            }
            sleep(Duration::from_millis(5)).await;
        }
        panic!("shielded runtime switch did not commit");
    }

    #[tokio::test]
    async fn activation_failure_keeps_prompts_closed_until_startup_replay() {
        let env = TestEnvironment::new("activation-replay");
        env.executor.fail_on("activate:1");
        let error = env
            .coordinator
            .request_switch(env.request("activation-replay"))
            .await
            .unwrap_err();
        assert_eq!(error.code, "runtime_switch_activation_failed");
        assert_eq!(
            env.switch_by_key("activation-replay").status,
            RuntimeSwitchStatus::Committed
        );
        assert_eq!(env.gate.prompt_gate_events().last(), Some(&true));

        env.executor.fail_on("never");
        let report = env.coordinator.reconcile_on_startup().await.unwrap();
        assert!(report.errors.is_empty());
        assert_eq!(env.gate.prompt_gate_events().last(), Some(&false));
        assert_eq!(
            env.executor
                .calls()
                .iter()
                .filter(|call| call.as_str() == "activate:1")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn startup_reconcile_does_not_replay_ambiguous_fresh_session_creation() {
        let env = TestEnvironment::new("ambiguous-create");
        env.executor.set_assessment(fresh_assessment());
        let seeded = env.seed_preparing_switch(
            "ambiguous-create",
            RetrySemantics::NonRetryableWhenAmbiguous,
            None,
        );
        let report = env.coordinator.reconcile_on_startup().await.unwrap();
        assert!(report.errors.iter().any(|error| {
            error.switch_id == seeded.switch_id
                && error.error_code == "runtime_switch_ambiguous_external_effect"
        }));
        assert_eq!(
            env.switch_by_key("ambiguous-create").status,
            RuntimeSwitchStatus::AmbiguousExternalEffect
        );
        assert!(
            !env.executor
                .calls()
                .iter()
                .any(|call| call == OP_CREATE_SESSION)
        );
        assert_eq!(
            env.runtime_state().current_binding_id.as_ref(),
            Some(&env.source_binding.binding_id)
        );
        let operations =
            SwitchOperationJournalRepository::list_by_switch(&env.connection(), &seeded.switch_id)
                .unwrap();
        assert!(operations.iter().any(|operation| {
            operation.operation_kind == OP_CREATE_SESSION
                && operation.status == SwitchOperationStatus::AmbiguousExternalEffect
        }));
        assert_eq!(env.gate.prompt_gate_events().last(), Some(&false));
    }

    #[tokio::test]
    async fn startup_reconcile_recovers_confirmed_create_without_replaying_it() {
        let env = TestEnvironment::new("confirmed-create");
        env.executor.set_assessment(fresh_assessment());
        env.executor
            .set_reconcile_outcome(OperationReconcileOutcome::Confirmed {
                native_result_reference: Some("reconciled-native-session".to_string()),
            });
        let seeded = env.seed_preparing_switch(
            "confirmed-create",
            RetrySemantics::ReconcileBeforeRetry,
            Some("adapter-token"),
        );
        let report = env.coordinator.reconcile_on_startup().await.unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            env.switch_by_key("confirmed-create").status,
            RuntimeSwitchStatus::Committed
        );
        let calls = env.executor.calls();
        assert!(!calls.iter().any(|call| call == OP_CREATE_SESSION));
        assert!(calls.iter().any(|call| call == "recover_attachment"));
        let target_binding = RuntimeBindingRepository::get(
            &env.connection(),
            seeded.target_binding_id.as_ref().unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            target_binding.native_session_id.as_deref(),
            Some("reconciled-native-session")
        );
    }

    #[tokio::test]
    async fn startup_uses_prepared_binding_as_exactly_once_create_evidence() {
        let env = TestEnvironment::new("binding-create-evidence");
        env.executor.set_assessment(fresh_assessment());
        let seeded = env.seed_preparing_switch(
            "binding-create-evidence",
            RetrySemantics::NonRetryableWhenAmbiguous,
            None,
        );
        let intent = SwitchIntent::from_record(&seeded).unwrap();
        let binding = MockExecutor::prepared_binding(&intent);
        RuntimeBindingRepository::insert(&env.connection(), &binding).unwrap();

        let report = env.coordinator.reconcile_on_startup().await.unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            env.switch_by_key("binding-create-evidence").status,
            RuntimeSwitchStatus::Committed
        );
        let calls = env.executor.calls();
        assert!(!calls.iter().any(|call| call == OP_CREATE_SESSION));
        assert!(!calls.iter().any(|call| call.starts_with("reconcile:")));
        let create =
            SwitchOperationJournalRepository::list_by_switch(&env.connection(), &seeded.switch_id)
                .unwrap()
                .into_iter()
                .find(|operation| operation.operation_kind == OP_CREATE_SESSION)
                .unwrap();
        assert_eq!(create.status, SwitchOperationStatus::Succeeded);
    }

    #[tokio::test]
    async fn startup_replays_idempotent_config_after_about_to_send_crash() {
        let env = TestEnvironment::new("config-about-to-send");
        env.executor.set_assessment(fresh_assessment());
        let seeded = env.seed_config_about_to_send("config-about-to-send");
        let report = env.coordinator.reconcile_on_startup().await.unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            env.switch_by_key("config-about-to-send").status,
            RuntimeSwitchStatus::Committed
        );
        assert_eq!(
            env.executor
                .calls()
                .iter()
                .filter(|call| *call == OP_APPLY_SESSION_CONFIG)
                .count(),
            1
        );
        let config_attempts: Vec<_> =
            SwitchOperationJournalRepository::list_by_switch(&env.connection(), &seeded.switch_id)
                .unwrap()
                .into_iter()
                .filter(|operation| operation.operation_kind == OP_APPLY_SESSION_CONFIG)
                .collect();
        assert_eq!(config_attempts.len(), 2);
        assert_eq!(config_attempts[0].status, SwitchOperationStatus::Failed);
        assert_eq!(config_attempts[1].status, SwitchOperationStatus::Succeeded);
    }

    #[tokio::test]
    async fn startup_revalidates_prepared_target_then_commits() {
        let env = TestEnvironment::new("prepared-reconcile");
        env.executor.set_assessment(fresh_assessment());
        env.seed_prepared_switch("prepared-reconcile");
        let report = env.coordinator.reconcile_on_startup().await.unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            env.switch_by_key("prepared-reconcile").status,
            RuntimeSwitchStatus::Committed
        );
        let calls = env.executor.calls();
        assert!(calls.iter().any(|call| call == "acquire_prepared"));
        assert!(calls.iter().any(|call| call == "revalidate_prepared"));
        assert!(!calls.iter().any(|call| call == OP_CREATE_SESSION));
    }

    #[tokio::test]
    async fn startup_reverts_uncommitted_committing_state_then_commits_once() {
        let env = TestEnvironment::new("committing-reconcile");
        env.executor.set_assessment(fresh_assessment());
        let prepared = env.seed_prepared_switch("committing-reconcile");
        RuntimeSwitchRepository::advance_status(
            &env.connection(),
            &prepared.switch_id,
            RuntimeSwitchStatus::Prepared,
            RuntimeSwitchStatus::Committing,
        )
        .unwrap();
        let report = env.coordinator.reconcile_on_startup().await.unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            env.switch_by_key("committing-reconcile").status,
            RuntimeSwitchStatus::Committed
        );
        assert_eq!(env.runtime_state().revision, 1);
        assert_eq!(
            env.executor
                .calls()
                .iter()
                .filter(|call| call.starts_with("activate:"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn startup_replays_activation_for_a_previously_committed_current_switch() {
        let env = TestEnvironment::new("committed-activation");
        env.coordinator
            .request_switch(env.request("committed-activation"))
            .await
            .unwrap();
        env.executor.clear_calls();
        let report = env.coordinator.reconcile_on_startup().await.unwrap();
        assert!(report.errors.is_empty());
        assert!(report.outcomes.iter().any(|outcome| {
            outcome.status == RuntimeSwitchStatus::Committed
                && outcome.switch_id == env.switch_by_key("committed-activation").switch_id
        }));
        let calls = env.executor.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("activate:"))
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| *call == "cleanup_source")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn startup_skips_live_lease_then_takes_over_after_expiry() {
        let env = TestEnvironment::new("lease-takeover");
        env.executor.set_assessment(fresh_assessment());
        env.executor
            .set_reconcile_outcome(OperationReconcileOutcome::Confirmed {
                native_result_reference: Some("lease-recovered-native".to_string()),
            });
        let seeded = env.seed_preparing_switch(
            "lease-takeover",
            RetrySemantics::ReconcileBeforeRetry,
            Some("lease-token"),
        );
        let now = unix_timestamp_ms();
        assert!(
            RuntimeSwitchRepository::try_acquire_worker_lease(
                &env.connection(),
                &seeded.switch_id,
                "dead-worker",
                60_000,
                now,
            )
            .unwrap()
        );
        let first = env.coordinator.reconcile_on_startup().await.unwrap();
        assert!(first.errors.is_empty());
        assert_eq!(
            env.switch_by_key("lease-takeover").status,
            RuntimeSwitchStatus::Preparing
        );
        assert!(env.executor.calls().is_empty());

        env.connection()
            .execute(
                "UPDATE runtime_switches SET worker_lease_deadline_ms = ?2 WHERE switch_id = ?1",
                (seeded.switch_id.as_str(), unix_timestamp_ms() - 1),
            )
            .unwrap();
        let second = env.coordinator.reconcile_on_startup().await.unwrap();
        assert!(second.errors.is_empty(), "{:?}", second.errors);
        assert_eq!(
            env.switch_by_key("lease-takeover").status,
            RuntimeSwitchStatus::Committed
        );
    }

    #[tokio::test]
    async fn lease_heartbeat_prevents_takeover_during_slow_external_operation() {
        let env = TestEnvironment::new("lease-heartbeat");
        env.executor.set_delay_on(OP_SPAWN_PROCESS, 900);
        let coordinator = RuntimeSwitchCoordinator::new(
            &env.db_path,
            Arc::new(env.executor.clone()),
            Arc::new(env.gate.clone()),
            RuntimeSwitchCoordinatorConfig {
                lease_duration_ms: 300,
                idle_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        let request = env.request("lease-heartbeat");
        let task = tokio::spawn(async move { coordinator.request_switch(request).await });
        wait_for_executor_call(&env.executor, OP_SPAWN_PROCESS).await;
        let initial_deadline_ms = env
            .switch_by_key("lease-heartbeat")
            .worker_lease_deadline_ms
            .expect("worker lease should exist before the external operation");
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let now_ms = unix_timestamp_ms();
                let deadline_ms = env
                    .switch_by_key("lease-heartbeat")
                    .worker_lease_deadline_ms;
                if now_ms > initial_deadline_ms
                    && deadline_ms.is_some_and(|deadline_ms| deadline_ms > now_ms + 100)
                {
                    break;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("heartbeat should renew the lease beyond its original deadline");
        let record = env.switch_by_key("lease-heartbeat");
        assert!(
            !RuntimeSwitchRepository::try_acquire_worker_lease(
                &env.connection(),
                &record.switch_id,
                "intruding-worker",
                100,
                unix_timestamp_ms(),
            )
            .unwrap()
        );
        assert_eq!(
            task.await.unwrap().unwrap().status,
            RuntimeSwitchStatus::Committed
        );
    }

    #[tokio::test]
    async fn worker_that_loses_lease_stops_without_failing_durable_switch() {
        let env = TestEnvironment::new("lease-lost");
        env.executor.set_delay_on(OP_SPAWN_PROCESS, 100);
        let coordinator = RuntimeSwitchCoordinator::new(
            &env.db_path,
            Arc::new(env.executor.clone()),
            Arc::new(env.gate.clone()),
            RuntimeSwitchCoordinatorConfig {
                lease_duration_ms: 90,
                idle_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        let request = env.request("lease-lost");
        let task = tokio::spawn(async move { coordinator.request_switch(request).await });
        wait_for_executor_call(&env.executor, OP_SPAWN_PROCESS).await;
        let switch_id = env.switch_by_key("lease-lost").switch_id;
        env.connection()
            .execute(
                "UPDATE runtime_switches
                 SET worker_lease_owner = 'replacement-worker',
                     worker_lease_deadline_ms = ?2
                 WHERE switch_id = ?1",
                (switch_id.as_str(), unix_timestamp_ms() + 60_000),
            )
            .unwrap();
        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.code, "runtime_switch_lease_lost");
        let interrupted = env.switch_by_key("lease-lost");
        assert_eq!(interrupted.status, RuntimeSwitchStatus::Preparing);
        assert_eq!(
            env.runtime_state().pending_switch_id.as_ref(),
            Some(&switch_id)
        );

        env.connection()
            .execute(
                "UPDATE runtime_switches SET worker_lease_deadline_ms = ?2 WHERE switch_id = ?1",
                (switch_id.as_str(), unix_timestamp_ms() - 1),
            )
            .unwrap();
        env.executor.set_delay_ms(0);
        let report = env.coordinator.reconcile_on_startup().await.unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            env.switch_by_key("lease-lost").status,
            RuntimeSwitchStatus::Committed
        );
    }

    #[tokio::test]
    async fn startup_resumes_reserved_switch_after_process_loss() {
        let env = TestEnvironment::new("resume-reserved");
        env.executor.set_assessment(fresh_assessment());
        env.seed_reserved_switch("resume-reserved");
        let report = env.coordinator.reconcile_on_startup().await.unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            env.switch_by_key("resume-reserved").status,
            RuntimeSwitchStatus::Committed
        );
        assert_eq!(
            env.executor
                .calls()
                .iter()
                .filter(|call| *call == OP_CREATE_SESSION)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn drives_pre_enqueued_requested_switch_through_pending_claim() {
        let env = TestEnvironment::new("drive-requested");
        let mut desired = env.selection.clone();
        desired.model_id = "model-requested".to_string();
        let requested = env.seed_requested_switch("drive-requested", 0, desired.clone());
        assert_eq!(requested.status, RuntimeSwitchStatus::Requested);
        assert_eq!(env.runtime_state().pending_switch_id, None);

        let outcome = env
            .coordinator
            .drive_switch(&requested.switch_id)
            .await
            .unwrap();
        assert_eq!(outcome.status, RuntimeSwitchStatus::Committed);
        let state = env.runtime_state();
        assert_eq!(state.desired_runtime_selection, Some(desired.clone()));
        assert_eq!(state.effective_runtime_selection, Some(desired));
        assert_eq!(state.selection_revision, 1);
        assert_eq!(state.pending_switch_id, None);
        assert_eq!(
            state.runtime_selection_status,
            Some(SessionRuntimeSelectionStatus::Ready)
        );
    }

    #[tokio::test]
    async fn queued_requested_driver_starts_after_superseded_cleanup_finishes() {
        let env = TestEnvironment::new("requested-after-cleanup");
        let previous = env.seed_reserved_switch("requested-previous");
        let mut latest_selection = env.selection.clone();
        latest_selection.model_id = "model-latest".to_string();
        let latest = env.seed_requested_switch("requested-latest", 0, latest_selection.clone());
        env.executor.set_delay_on("cleanup_target", 25);

        let previous_coordinator = env.coordinator.clone();
        let previous_id = previous.switch_id.clone();
        let previous_task =
            tokio::spawn(async move { previous_coordinator.drive_switch(&previous_id).await });
        wait_for_executor_call(&env.executor, "cleanup_target").await;

        let latest_coordinator = env.coordinator.clone();
        let latest_id = latest.switch_id.clone();
        let latest_task =
            tokio::spawn(async move { latest_coordinator.drive_switch(&latest_id).await });

        let previous_error = previous_task.await.unwrap().unwrap_err();
        assert_eq!(previous_error.code, "runtime_switch_superseded");
        let latest_outcome = latest_task.await.unwrap().unwrap();
        assert_eq!(latest_outcome.status, RuntimeSwitchStatus::Committed);

        let calls = env.executor.calls();
        let cleanup_index = calls
            .iter()
            .position(|call| call == "cleanup_target")
            .unwrap();
        let latest_assessment_index = calls
            .iter()
            .enumerate()
            .skip(cleanup_index + 1)
            .find_map(|(index, call)| (call == "assess_target").then_some(index))
            .unwrap();
        assert!(cleanup_index < latest_assessment_index);
        assert_eq!(
            env.runtime_state().effective_runtime_selection,
            Some(latest_selection)
        );
        assert_eq!(env.gate.prompt_gate_events().last(), Some(&false));
    }

    #[tokio::test]
    async fn startup_clears_orphan_pending_switch_pointer() {
        let env = TestEnvironment::new("orphan-pending");
        let missing = RuntimeSwitchId::new();
        env.connection()
            .execute(
                "UPDATE agent_sessions SET pending_switch_id = ?2 WHERE session_id = ?1",
                (env.session_id.as_str(), missing.as_str()),
            )
            .unwrap();
        let report = env.coordinator.reconcile_on_startup().await.unwrap();
        assert!(report.errors.is_empty());
        assert_eq!(env.runtime_state().pending_switch_id, None);
    }

    #[tokio::test]
    async fn desired_selection_change_supersedes_commit_and_preserves_source() {
        let env = TestEnvironment::new("superseded");
        env.executor.set_delay_ms(15);
        let coordinator = env.coordinator.clone();
        let request = env.request("superseded");
        let task = tokio::spawn(async move { coordinator.request_switch(request).await });
        wait_for_executor_call(&env.executor, "revalidate_prepared").await;
        let mut newer_selection = env.selection.clone();
        newer_selection.model_id = "model-newer".to_string();
        AgentSessionRuntimeRepository::set_desired_selection(
            &env.connection(),
            &env.session_id,
            0,
            &newer_selection,
            SessionRuntimeSelectionStatus::Preparing,
        )
        .unwrap();
        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.code, "runtime_switch_superseded");
        let record = env.switch_by_key("superseded");
        assert_eq!(record.status, RuntimeSwitchStatus::Superseded);
        assert_eq!(
            env.runtime_state().current_binding_id.as_ref(),
            Some(&env.source_binding.binding_id)
        );
        assert_eq!(
            RuntimeBindingRepository::get(
                &env.connection(),
                record.target_binding_id.as_ref().unwrap()
            )
            .unwrap()
            .unwrap()
            .binding_state,
            BindingState::Failed
        );
        assert_eq!(env.gate.prompt_gate_events().last(), Some(&false));
    }

    #[tokio::test]
    async fn repeated_live_mutations_persist_binding_generation_and_reactivate_latest_only() {
        let env = TestEnvironment::new("repeated-live");
        env.executor.set_assessment(live_assessment());
        let mut first = env.request("repeated-live-1");
        first.requested_policy = RuntimeSwitchPolicy::PreferLiveMutation;
        env.coordinator.request_switch(first).await.unwrap();

        let mut second = env.request("repeated-live-2");
        second.expected_revision = 1;
        second.requested_policy = RuntimeSwitchPolicy::PreferLiveMutation;
        env.coordinator.request_switch(second).await.unwrap();
        let state = env.runtime_state();
        assert_eq!((state.revision, state.activation_generation), (2, 2));
        let binding =
            RuntimeBindingRepository::get(&env.connection(), &env.source_binding.binding_id)
                .unwrap()
                .unwrap();
        assert_eq!(binding.activation_generation, 2);

        env.executor.clear_calls();
        let report = env.coordinator.reconcile_on_startup().await.unwrap();
        assert!(report.errors.is_empty());
        assert_eq!(
            report
                .outcomes
                .iter()
                .filter(|outcome| outcome.status == RuntimeSwitchStatus::Committed)
                .count(),
            1
        );
        let calls = env.executor.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("activate:"))
                .count(),
            1
        );
        assert!(!calls.iter().any(|call| call == "cleanup_source"));
    }
}

use std::collections::HashSet;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio::time::sleep;
use vibex_core::{
    AcpAdapterId, AgentSessionRuntimeSelectionEvent, AgentSessionRuntimeSelectionState,
    AgentSessionState, BusyDisposition, CancelAgentSessionRuntimeSwitchRequest,
    MAX_RUNTIME_SWITCH_WAIT_DEADLINE_MS, RuntimeBinding, RuntimeBindingId,
    RuntimeSelectionActionableError, RuntimeSelectionInteraction, RuntimeSwitchActiveWorkPolicy,
    RuntimeSwitchId, RuntimeSwitchPolicy, RuntimeSwitchStatus, SessionRuntimeSelection,
    SessionRuntimeSelectionStatus, SetDesiredAgentSessionRuntimeRequest,
    SwitchAgentSessionRuntimeRequest, SwitchAgentSessionRuntimeResponse, VibexError, VibexResult,
    VibexSessionId,
};
use vibex_db::{
    AgentSessionRuntimeRepository, DesiredRuntimeSwitchEnqueueRequest,
    DesiredRuntimeSwitchEnqueueResult, MessageSubmissionRepository, RuntimeSwitchEventRepository,
    RuntimeSwitchRecord, RuntimeSwitchRepository, SessionRepository, apply_migrations,
    open_database,
};

use crate::runtime_switch::{
    RuntimeSwitchCoordinator, RuntimeSwitchReconcileReport, RuntimeSwitchRequest,
};
use crate::{RuntimeLogContext, RuntimeLogLevel, RuntimeMetricName, RuntimeMetricResult};

pub const DEFAULT_SEAMLESS_RUNTIME_SWITCH_WAIT_DEADLINE_MS: u64 = 5 * 60 * 1000;
pub const DEFAULT_RUNTIME_SELECTION_POLL_INTERVAL: Duration = Duration::from_millis(25);
const DEFAULT_RUNTIME_SELECTION_BROADCAST_CAPACITY: usize = 64;

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedRuntimeSelection {
    pub adapter_id: AcpAdapterId,
    pub auth_source_revision: i64,
    pub session_config: Option<serde_json::Value>,
}

impl fmt::Debug for ResolvedRuntimeSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedRuntimeSelection")
            .field("adapter_id", &self.adapter_id)
            .field("auth_source_revision", &self.auth_source_revision)
            .field("has_session_config", &self.session_config.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInitialRuntimeSelection {
    pub binding: RuntimeBinding,
    pub selection: SessionRuntimeSelection,
}

#[async_trait]
pub trait RuntimeSelectionResolver: Send + Sync {
    async fn resolve(
        &self,
        session_id: &VibexSessionId,
        selection: &SessionRuntimeSelection,
        preferred_adapter_id: Option<&AcpAdapterId>,
    ) -> VibexResult<ResolvedRuntimeSelection>;

    async fn resolve_current(
        &self,
        _session_id: &VibexSessionId,
    ) -> VibexResult<ResolvedInitialRuntimeSelection> {
        Err(VibexError::capability(
            "runtime_selection_initialization_unsupported",
            "current runtime selection cannot be materialized by this runtime",
        ))
    }

    async fn materialize_current_runtime(&self, _session_id: &VibexSessionId) -> VibexResult<()> {
        Err(VibexError::capability(
            "runtime_selection_current_materialization_unsupported",
            "current runtime cannot be materialized by this runtime",
        ))
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeSelectionServiceConfig {
    pub seamless_wait_deadline_ms: u64,
    pub poll_interval: Duration,
    pub broadcast_capacity: usize,
}

impl Default for RuntimeSelectionServiceConfig {
    fn default() -> Self {
        Self {
            seamless_wait_deadline_ms: DEFAULT_SEAMLESS_RUNTIME_SWITCH_WAIT_DEADLINE_MS,
            poll_interval: DEFAULT_RUNTIME_SELECTION_POLL_INTERVAL,
            broadcast_capacity: DEFAULT_RUNTIME_SELECTION_BROADCAST_CAPACITY,
        }
    }
}

struct RuntimeSelectionServiceInner {
    coordinator: RuntimeSwitchCoordinator,
    resolver: Arc<dyn RuntimeSelectionResolver>,
    config: RuntimeSelectionServiceConfig,
    events: broadcast::Sender<AgentSessionRuntimeSelectionEvent>,
    watched_switches: Mutex<HashSet<RuntimeSwitchId>>,
}

#[derive(Clone)]
pub struct RuntimeSelectionService {
    inner: Arc<RuntimeSelectionServiceInner>,
}

impl RuntimeSelectionService {
    pub fn new(
        coordinator: RuntimeSwitchCoordinator,
        resolver: Arc<dyn RuntimeSelectionResolver>,
        config: RuntimeSelectionServiceConfig,
    ) -> VibexResult<Self> {
        if config.seamless_wait_deadline_ms == 0
            || config.seamless_wait_deadline_ms > MAX_RUNTIME_SWITCH_WAIT_DEADLINE_MS
            || config.poll_interval.is_zero()
            || config.broadcast_capacity == 0
        {
            return Err(VibexError::validation(
                "runtime_selection_service_config_invalid",
                "runtime selection service durations and capacity must be positive and bounded",
            ));
        }
        let mut conn = open_database(coordinator.database_path())?;
        apply_migrations(&mut conn)?;
        let (events, _) = broadcast::channel(config.broadcast_capacity);
        Ok(Self {
            inner: Arc::new(RuntimeSelectionServiceInner {
                coordinator,
                resolver,
                config,
                events,
                watched_switches: Mutex::new(HashSet::new()),
            }),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentSessionRuntimeSelectionEvent> {
        self.inner.events.subscribe()
    }

    pub fn seamless_active_work_policy(&self) -> RuntimeSwitchActiveWorkPolicy {
        let wait = BusyDisposition::Wait {
            deadline_ms: self.inner.config.seamless_wait_deadline_ms,
        };
        RuntimeSwitchActiveWorkPolicy {
            active_turn: wait,
            pending_permission: wait,
            active_terminal: wait,
            background_work: wait,
        }
    }

    pub fn get_selection_state(
        &self,
        session_id: &VibexSessionId,
    ) -> VibexResult<AgentSessionRuntimeSelectionState> {
        let event = self.authoritative_event(session_id)?;
        self.converge_initial_session_lifecycle(session_id, &event.state)?;
        Ok(event.state)
    }

    /// Materializes the first ACP Runtime through the same durable switch
    /// state machine used by later selections. The Requested row and desired
    /// selection are committed before the executor can spawn or call
    /// `session/new`.
    pub async fn initialize_new_session(
        &self,
        session_id: &VibexSessionId,
        desired: SessionRuntimeSelection,
    ) -> VibexResult<AgentSessionRuntimeSelectionState> {
        let record = self
            .enqueue_initial_runtime_switch(session_id, desired)
            .await?;
        let outcome = self
            .inner
            .coordinator
            .drive_switch(&record.switch_id)
            .await?;
        if outcome.status != RuntimeSwitchStatus::Committed {
            return Err(VibexError::process(
                "runtime_selection_initialization_failed",
                "initial ACP runtime did not commit",
            )
            .with_diagnostic("switchId", outcome.switch_id.as_str())
            .with_diagnostic("status", format!("{:?}", outcome.status)));
        }
        Ok(self.emit_authoritative(session_id)?.state)
    }

    /// Persists the initial ACP runtime intent and lets the normal switch
    /// watcher materialize it in the background. Queued messages can safely
    /// wait on the resulting `Preparing` state before any provider prompt is
    /// dispatched.
    pub async fn initialize_new_session_deferred(
        &self,
        session_id: &VibexSessionId,
        desired: SessionRuntimeSelection,
    ) -> VibexResult<AgentSessionRuntimeSelectionState> {
        let record = self
            .enqueue_initial_runtime_switch(session_id, desired)
            .await?;
        let event = self.emit_authoritative(session_id)?;
        self.start_watcher(&record)?;
        Ok(event.state)
    }

    async fn enqueue_initial_runtime_switch(
        &self,
        session_id: &VibexSessionId,
        desired: SessionRuntimeSelection,
    ) -> VibexResult<RuntimeSwitchRecord> {
        let resolved = self
            .inner
            .resolver
            .resolve(session_id, &desired, None)
            .await?;
        let requested_session_config =
            RuntimeSwitchCoordinator::encode_requested_config(&desired, resolved.session_config)?;
        let record = {
            let mut conn = open_database(self.inner.coordinator.database_path())?;
            AgentSessionRuntimeRepository::enqueue_initial_runtime_switch(
                &mut conn,
                RuntimeSwitchId::new(),
                &DesiredRuntimeSwitchEnqueueRequest {
                    session_id: session_id.clone(),
                    idempotency_key: format!("session-init:{}", session_id.as_str()),
                    expected_revision: 0,
                    expected_selection_revision: 0,
                    target_binding_id: RuntimeBindingId::new(),
                    target_adapter_id: resolved.adapter_id,
                    target_auth_source_revision: resolved.auth_source_revision,
                    desired,
                    requested_policy: RuntimeSwitchPolicy::Automatic,
                    active_work_policy: self.seamless_active_work_policy(),
                    requested_session_config,
                },
            )?
        };
        Ok(record)
    }

    pub async fn initialize_runtime_selection(
        &self,
        session_id: &VibexSessionId,
    ) -> VibexResult<AgentSessionRuntimeSelectionState> {
        let durable = self.required_runtime_state(session_id)?;
        let initialized_fields = [
            durable.current_binding_id.is_some(),
            durable.desired_runtime_selection.is_some(),
            durable.effective_runtime_selection.is_some(),
            durable.runtime_selection_status.is_some(),
        ];
        if initialized_fields.iter().all(|initialized| *initialized) {
            if durable.current_agent_id.is_none() {
                return Err(VibexError::conflict(
                    "session_runtime_selection_partially_initialized",
                    "Agent session runtime selection state is partially initialized",
                ));
            }
            return self.get_selection_state(session_id);
        }
        if initialized_fields.iter().any(|initialized| *initialized) {
            return Err(VibexError::conflict(
                "session_runtime_selection_partially_initialized",
                "Agent session runtime selection state is partially initialized",
            ));
        }
        let resolved = self.inner.resolver.resolve_current(session_id).await?;
        if &resolved.binding.session_id != session_id {
            return Err(VibexError::conflict(
                "runtime_selection_initial_session_mismatch",
                "resolved initial runtime binding belongs to another logical session",
            ));
        }
        {
            let mut conn = open_database(self.inner.coordinator.database_path())?;
            AgentSessionRuntimeRepository::initialize_runtime_selection(
                &mut conn,
                &resolved.binding,
                &resolved.selection,
            )?;
        }
        Ok(self.emit_authoritative(session_id)?.state)
    }

    pub async fn set_desired_runtime(
        &self,
        request: SetDesiredAgentSessionRuntimeRequest,
    ) -> VibexResult<AgentSessionRuntimeSelectionState> {
        if request.interaction != RuntimeSelectionInteraction::Seamless {
            return Err(VibexError::validation(
                "runtime_selection_interaction_unsupported",
                "runtime selection interaction is not supported",
            ));
        }

        if let Some(existing) =
            self.get_by_idempotency_key(&request.session_id, request.idempotency_key.trim())?
        {
            if durable_effective_selection(&existing)? != request.desired {
                return Err(VibexError::conflict(
                    "runtime_selection_idempotency_payload_conflict",
                    "runtime selection idempotency key was already used for a different target",
                ));
            }
            let event = self.emit_authoritative(&request.session_id)?;
            self.start_watcher(&existing)?;
            return Ok(event.state);
        }

        let resolved = self
            .inner
            .resolver
            .resolve(&request.session_id, &request.desired, None)
            .await?;
        let requested_session_config = RuntimeSwitchCoordinator::encode_requested_config(
            &request.desired,
            resolved.session_config,
        )?;
        RuntimeSwitchRepository::validate_requested_config(&requested_session_config)?;
        let result = {
            let mut conn = open_database(self.inner.coordinator.database_path())?;
            AgentSessionRuntimeRepository::enqueue_desired_switch(
                &mut conn,
                RuntimeSwitchId::new(),
                &DesiredRuntimeSwitchEnqueueRequest {
                    session_id: request.session_id.clone(),
                    idempotency_key: request.idempotency_key.trim().to_string(),
                    expected_revision: request.expected_revision,
                    expected_selection_revision: request.expected_selection_revision,
                    target_binding_id: RuntimeBindingId::new(),
                    target_adapter_id: resolved.adapter_id,
                    target_auth_source_revision: resolved.auth_source_revision,
                    desired: request.desired,
                    requested_policy: RuntimeSwitchPolicy::Automatic,
                    active_work_policy: self.seamless_active_work_policy(),
                    requested_session_config,
                },
            )?
        };
        if let DesiredRuntimeSwitchEnqueueResult::NoChange(state) = &result
            && state.runtime_selection_status
                == Some(SessionRuntimeSelectionStatus::FailedUsingPrevious)
        {
            self.inner
                .resolver
                .materialize_current_runtime(&request.session_id)
                .await?;
        }
        let event = self.emit_authoritative(&request.session_id)?;
        if let DesiredRuntimeSwitchEnqueueResult::Enqueued(record) = result {
            self.start_watcher(&record)?;
        }
        Ok(event.state)
    }

    pub async fn switch_runtime(
        &self,
        request: SwitchAgentSessionRuntimeRequest,
    ) -> VibexResult<SwitchAgentSessionRuntimeResponse> {
        request.active_work_policy.validate()?;
        let state = self.required_runtime_state(&request.session_id)?;
        let resolved = self
            .inner
            .resolver
            .resolve(
                &request.session_id,
                &request.target,
                request.target_adapter_id.as_ref(),
            )
            .await?;
        if request
            .target_adapter_id
            .as_ref()
            .is_some_and(|adapter_id| adapter_id != &resolved.adapter_id)
        {
            return Err(VibexError::conflict(
                "runtime_selection_adapter_mismatch",
                "resolved runtime adapter does not match the requested adapter",
            ));
        }
        let outcome = self
            .inner
            .coordinator
            .request_switch(RuntimeSwitchRequest {
                session_id: request.session_id.clone(),
                idempotency_key: request.idempotency_key,
                expected_revision: request.expected_revision,
                expected_current_binding_id: state.current_binding_id,
                desired_selection_revision: state.selection_revision,
                target_adapter_id: resolved.adapter_id,
                target_auth_source_revision: resolved.auth_source_revision,
                target_selection: request.target,
                requested_policy: request.policy,
                active_work_policy: request.active_work_policy,
                requested_session_config: resolved.session_config,
            })
            .await?;
        let record = self.required_switch(&outcome.switch_id)?;
        let current = self.required_runtime_state(&request.session_id)?;
        Ok(SwitchAgentSessionRuntimeResponse {
            switch_id: outcome.switch_id,
            status: outcome.status,
            session_revision: current.revision,
            current_binding_id: current.current_binding_id,
            target_binding_id: record.target_binding_id,
        })
    }

    pub async fn cancel_switch(
        &self,
        request: CancelAgentSessionRuntimeSwitchRequest,
    ) -> VibexResult<AgentSessionRuntimeSelectionState> {
        let state = self.required_runtime_state(&request.session_id)?;
        let record = self.required_switch(&request.switch_id)?;
        if record.session_id != request.session_id
            || record.desired_selection_revision != state.selection_revision
        {
            return Err(VibexError::conflict(
                "runtime_selection_cancel_stale",
                "runtime switch is not the current desired runtime intent",
            ));
        }
        self.inner
            .coordinator
            .cancel_switch(&request.session_id, &request.switch_id)
            .await?;
        {
            let conn = open_database(self.inner.coordinator.database_path())?;
            MessageSubmissionRepository::cancel_for_switch(&conn, &request.switch_id)?;
        }
        Ok(self.emit_authoritative(&request.session_id)?.state)
    }

    pub async fn reconcile_on_startup(&self) -> VibexResult<RuntimeSwitchReconcileReport> {
        let report = self.inner.coordinator.reconcile_on_startup().await?;
        let mut session_ids = HashSet::new();
        for switch_id in report
            .outcomes
            .iter()
            .map(|outcome| &outcome.switch_id)
            .chain(report.errors.iter().map(|error| &error.switch_id))
        {
            if let Some(record) = self.get_switch(switch_id)? {
                session_ids.insert(record.session_id);
            }
        }

        let non_terminal = {
            let conn = open_database(self.inner.coordinator.database_path())?;
            RuntimeSwitchRepository::list_non_terminal(&conn)?
        };
        for record in non_terminal {
            let state = self.required_runtime_state(&record.session_id)?;
            if state.selection_revision == record.desired_selection_revision {
                session_ids.insert(record.session_id.clone());
                self.start_watcher(&record)?;
            }
        }
        for session_id in session_ids {
            self.emit_authoritative(&session_id)?;
        }
        Ok(report)
    }

    fn authoritative_event(
        &self,
        session_id: &VibexSessionId,
    ) -> VibexResult<AgentSessionRuntimeSelectionEvent> {
        let conn = open_database(self.inner.coordinator.database_path())?;
        let state = AgentSessionRuntimeRepository::get_runtime_state(&conn, session_id)?
            .ok_or_else(|| {
                VibexError::validation("session_not_found", "Agent session was not found")
            })?;
        let latest = RuntimeSwitchRepository::get_latest_for_selection_revision(
            &conn,
            session_id,
            state.selection_revision,
            false,
        )?;
        let projects_initial_target = state.current_binding_id.is_none()
            && state.effective_runtime_selection.is_none()
            && state.selection_revision == 1
            && matches!(
                state.runtime_selection_status,
                Some(
                    SessionRuntimeSelectionStatus::Preparing
                        | SessionRuntimeSelectionStatus::FailedUsingPrevious
                )
            )
            && latest
                .as_ref()
                .is_some_and(|record| record.source_binding_id.is_none());
        let desired = match state.desired_runtime_selection.clone() {
            Some(desired) => desired,
            None if projects_initial_target => durable_effective_selection(
                latest
                    .as_ref()
                    .expect("initial target projection requires a durable switch"),
            )?,
            None => {
                return Err(VibexError::conflict(
                    "session_runtime_selection_uninitialized",
                    "Agent session desired runtime selection is not initialized",
                ));
            }
        };
        // The product API has a non-optional effective field. Before the first
        // binding exists, expose the durable initial target for Preparing and
        // terminal failure states; status and binding gates still prohibit
        // provider work, and SQLite retains a null effective selection.
        let effective = state
            .effective_runtime_selection
            .clone()
            .or_else(|| projects_initial_target.then(|| desired.clone()))
            .ok_or_else(|| {
                VibexError::conflict(
                    "session_runtime_selection_uninitialized",
                    "Agent session effective runtime selection is not initialized",
                )
            })?;
        let status = selection_status(&state, latest.as_ref(), &desired, &effective);
        let pending_switch_id = latest
            .as_ref()
            .filter(|record| !record.status.is_terminal())
            .map(|record| record.switch_id.clone())
            .or(state.pending_switch_id);
        let actionable_error = if status == SessionRuntimeSelectionStatus::FailedUsingPrevious {
            state
                .runtime_selection_error_code
                .as_deref()
                .map(actionable_error_for_code)
                .transpose()?
        } else {
            None
        };
        let event = latest
            .as_ref()
            .map(|record| RuntimeSwitchEventRepository::latest_by_switch(&conn, &record.switch_id))
            .transpose()?
            .flatten();
        Ok(AgentSessionRuntimeSelectionEvent {
            session_id: session_id.clone(),
            state: AgentSessionRuntimeSelectionState {
                desired,
                effective,
                status,
                session_revision: state.revision,
                selection_revision: state.selection_revision,
                current_binding_id: state.current_binding_id,
                activation_generation: state.activation_generation,
                pending_switch_id,
                actionable_error,
            },
            event,
        })
    }

    fn emit_authoritative(
        &self,
        session_id: &VibexSessionId,
    ) -> VibexResult<AgentSessionRuntimeSelectionEvent> {
        let event = self.authoritative_event(session_id)?;
        self.converge_initial_session_lifecycle(session_id, &event.state)?;
        let _ = self.inner.events.send(event.clone());
        Ok(event)
    }

    fn converge_initial_session_lifecycle(
        &self,
        session_id: &VibexSessionId,
        state: &AgentSessionRuntimeSelectionState,
    ) -> VibexResult<()> {
        if state.status != SessionRuntimeSelectionStatus::Ready
            || state.pending_switch_id.is_some()
            || state.current_binding_id.is_none()
            || state.desired != state.effective
        {
            return Ok(());
        }
        let conn = open_database(self.inner.coordinator.database_path())?;
        let Some(session) = SessionRepository::get(&conn, session_id)? else {
            return Err(VibexError::validation(
                "session_not_found",
                "Agent session was not found",
            ));
        };
        if session.state == AgentSessionState::Initializing {
            SessionRepository::update_state(&conn, session_id, AgentSessionState::Idle)?;
        }
        Ok(())
    }

    fn start_watcher(&self, record: &RuntimeSwitchRecord) -> VibexResult<()> {
        if record.status.is_terminal() {
            return Ok(());
        }
        {
            let mut watched = self.inner.watched_switches.lock().map_err(|_| {
                VibexError::process(
                    "runtime_selection_watcher_lock_poisoned",
                    "runtime selection watcher registry is unavailable",
                )
            })?;
            if !watched.insert(record.switch_id.clone()) {
                return Ok(());
            }
        }

        let service = self.clone();
        let switch_id = record.switch_id.clone();
        let session_id = record.session_id.clone();
        tokio::spawn(async move {
            let mut driver = Some(spawn_switch_driver(
                service.inner.coordinator.clone(),
                switch_id.clone(),
            ));

            let mut previous = service.authoritative_event(&session_id).ok();
            loop {
                sleep(service.inner.config.poll_interval).await;
                if let Ok(next) = service.authoritative_event(&session_id) {
                    let _ = service.converge_initial_session_lifecycle(&session_id, &next.state);
                    if previous.as_ref() != Some(&next) {
                        let _ = service.inner.events.send(next.clone());
                        previous = Some(next);
                    }
                }
                match service.get_switch(&switch_id) {
                    Ok(Some(current)) if !current.status.is_terminal() => {}
                    Ok(Some(current)) => {
                        let result = selection_metric_result(current.status);
                        let duration_ms = current
                            .committed_at_ms
                            .unwrap_or(current.updated_at_ms)
                            .saturating_sub(current.created_at_ms)
                            .max(0) as u64;
                        service
                            .inner
                            .coordinator
                            .observability()
                            .observe_duration_ms(
                                RuntimeMetricName::DesiredToEffectiveDuration,
                                None,
                                result,
                                duration_ms,
                            );
                        RuntimeLogContext::new("desired_to_effective")
                            .with_logical_session_id(&current.session_id)
                            .with_switch_id(&current.switch_id)
                            .with_agent_id(&current.target_agent_id)
                            .with_adapter_id(&current.target_adapter_id)
                            .with_auth_source(&current.target_auth_source)
                            .emit(
                                if current.status == RuntimeSwitchStatus::Committed {
                                    RuntimeLogLevel::Info
                                } else {
                                    RuntimeLogLevel::Warn
                                },
                                "runtime_desired_to_effective",
                                result,
                                current.error_code.as_deref(),
                                Some(duration_ms),
                            );
                        break;
                    }
                    _ => break,
                }
                if driver
                    .as_ref()
                    .is_some_and(tokio::task::JoinHandle::is_finished)
                {
                    let _ = driver
                        .take()
                        .expect("finished switch driver is present")
                        .await;
                }
                if driver.is_none() {
                    // A driver can lose a lease race, be cancelled with its
                    // caller, or fail before claiming the durable Requested
                    // row. Keep supervising until SQLite records a terminal
                    // outcome instead of leaving the session initializing.
                    driver = Some(spawn_switch_driver(
                        service.inner.coordinator.clone(),
                        switch_id.clone(),
                    ));
                }
            }
            if let Ok(mut watched) = service.inner.watched_switches.lock() {
                watched.remove(&switch_id);
            }
        });
        Ok(())
    }

    fn required_runtime_state(
        &self,
        session_id: &VibexSessionId,
    ) -> VibexResult<vibex_db::AgentSessionRuntimeState> {
        let conn = open_database(self.inner.coordinator.database_path())?;
        AgentSessionRuntimeRepository::get_runtime_state(&conn, session_id)?.ok_or_else(|| {
            VibexError::validation("session_not_found", "Agent session was not found")
        })
    }

    fn get_switch(&self, switch_id: &RuntimeSwitchId) -> VibexResult<Option<RuntimeSwitchRecord>> {
        let conn = open_database(self.inner.coordinator.database_path())?;
        RuntimeSwitchRepository::get(&conn, switch_id)
    }

    fn required_switch(&self, switch_id: &RuntimeSwitchId) -> VibexResult<RuntimeSwitchRecord> {
        self.get_switch(switch_id)?.ok_or_else(|| {
            VibexError::validation("runtime_switch_not_found", "runtime switch was not found")
        })
    }

    fn get_by_idempotency_key(
        &self,
        session_id: &VibexSessionId,
        idempotency_key: &str,
    ) -> VibexResult<Option<RuntimeSwitchRecord>> {
        let conn = open_database(self.inner.coordinator.database_path())?;
        RuntimeSwitchRepository::get_by_idempotency_key(&conn, session_id, idempotency_key)
    }
}

fn spawn_switch_driver(
    coordinator: RuntimeSwitchCoordinator,
    switch_id: RuntimeSwitchId,
) -> tokio::task::JoinHandle<VibexResult<crate::runtime_switch::SwitchOutcome>> {
    tokio::spawn(async move { coordinator.drive_switch(&switch_id).await })
}

fn selection_metric_result(status: RuntimeSwitchStatus) -> RuntimeMetricResult {
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

fn selection_status(
    state: &vibex_db::AgentSessionRuntimeState,
    latest: Option<&RuntimeSwitchRecord>,
    desired: &SessionRuntimeSelection,
    effective: &SessionRuntimeSelection,
) -> SessionRuntimeSelectionStatus {
    if state.runtime_selection_status == Some(SessionRuntimeSelectionStatus::FailedUsingPrevious) {
        return SessionRuntimeSelectionStatus::FailedUsingPrevious;
    }
    if let Some(record) = latest.filter(|record| !record.status.is_terminal()) {
        return match record.status {
            RuntimeSwitchStatus::WaitingForIdle => {
                SessionRuntimeSelectionStatus::WaitingForCurrentWork
            }
            RuntimeSwitchStatus::Requested
            | RuntimeSwitchStatus::Reserved
            | RuntimeSwitchStatus::Preparing
            | RuntimeSwitchStatus::Prepared
            | RuntimeSwitchStatus::Committing => SessionRuntimeSelectionStatus::Preparing,
            RuntimeSwitchStatus::Committed
            | RuntimeSwitchStatus::Failed
            | RuntimeSwitchStatus::Cancelled
            | RuntimeSwitchStatus::Superseded
            | RuntimeSwitchStatus::AmbiguousExternalEffect => unreachable!(),
        };
    }
    if desired == effective {
        SessionRuntimeSelectionStatus::Ready
    } else {
        state
            .runtime_selection_status
            .unwrap_or(SessionRuntimeSelectionStatus::Preparing)
    }
}

fn durable_effective_selection(
    record: &RuntimeSwitchRecord,
) -> VibexResult<SessionRuntimeSelection> {
    record
        .requested_session_config
        .as_ref()
        .and_then(|value| value.get("effectiveSelection"))
        .cloned()
        .ok_or_else(|| {
            VibexError::validation(
                "runtime_switch_requested_config_missing",
                "runtime switch durable requested configuration is missing its selection",
            )
        })
        .and_then(|value| {
            serde_json::from_value(value).map_err(|_| {
                VibexError::validation(
                    "runtime_switch_requested_config_invalid",
                    "runtime switch durable requested selection is invalid",
                )
            })
        })
}

fn actionable_error_for_code(code: &str) -> VibexResult<RuntimeSelectionActionableError> {
    let (message, recovery_hint) = match code {
        "runtime_switch_wait_timeout" => (
            "Current Agent work did not finish before the runtime switch deadline.",
            "Retry the selection after the current work finishes.",
        ),
        "runtime_switch_authentication_required" => (
            "The selected Agent runtime requires authentication.",
            "Configure the selected Agent profile, then retry the selection.",
        ),
        "runtime_switch_configuration_unavailable" => (
            "The selected Agent runtime configuration is unavailable.",
            "Review the selected Agent profile and model, then retry.",
        ),
        "runtime_switch_claim_retry_exhausted" => (
            "Vibex could not reserve local runtime state because the database stayed busy.",
            "Retry initialization shortly; no Agent work was started.",
        ),
        _ => (
            "The selected Agent runtime could not be activated; the previous runtime remains available.",
            "Review the selected runtime configuration and retry.",
        ),
    };
    RuntimeSelectionActionableError::new(code, message, Some(recovery_hint.to_string()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use vibex_core::{
        ActiveWorkKind, AgentId, AgentSession, AgentSessionSafety, AgentSessionState, BindingState,
        NativeStateHomeId, ProviderKind, ProviderProfileId, RequestId, RuntimeBinding,
        SessionRuntimeConfigState, TransportKind, WorkspaceMode, unix_timestamp_ms,
    };
    use vibex_db::{
        RuntimeBindingRepository, SessionRepository, SwitchOperationRecord, WorkspaceRepository,
    };

    use super::*;
    use crate::runtime_switch::{
        ActiveWorkGate, ActiveWorkSnapshot, JournaledOperation, OperationReconcileOutcome,
        PreparedAttachment, PreparedProcess, RestoreAssessment, RuntimeSwitchCoordinatorConfig,
        RuntimeSwitchStrategy, SwitchIntent, SwitchTargetAssessment, SwitchTargetExecutor,
    };

    #[derive(Clone)]
    struct TestResolver {
        adapter_id: AcpAdapterId,
        calls: Arc<Mutex<usize>>,
        initial: Arc<Mutex<Option<ResolvedInitialRuntimeSelection>>>,
        materialize_calls: Arc<Mutex<usize>>,
        db_path: PathBuf,
        session_id: VibexSessionId,
        current_binding_id: RuntimeBindingId,
        activation_generation: i64,
    }

    impl TestResolver {
        fn call_count(&self) -> usize {
            *self.calls.lock().unwrap()
        }

        fn set_initial(&self, initial: Option<ResolvedInitialRuntimeSelection>) {
            *self.initial.lock().unwrap() = initial;
        }

        fn materialize_call_count(&self) -> usize {
            *self.materialize_calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl RuntimeSelectionResolver for TestResolver {
        async fn resolve(
            &self,
            _session_id: &VibexSessionId,
            selection: &SessionRuntimeSelection,
            preferred_adapter_id: Option<&AcpAdapterId>,
        ) -> VibexResult<ResolvedRuntimeSelection> {
            *self.calls.lock().unwrap() += 1;
            if preferred_adapter_id.is_some_and(|adapter| adapter != &self.adapter_id) {
                return Err(VibexError::validation(
                    "runtime_selection_adapter_unavailable",
                    "requested adapter is unavailable",
                ));
            }
            Ok(ResolvedRuntimeSelection {
                adapter_id: self.adapter_id.clone(),
                auth_source_revision: 1,
                session_config: Some(serde_json::json!({
                    "model": selection.model_id(),
                    "reasoningEffort": selection.reasoning_effort,
                    "mode": selection.mode_id,
                })),
            })
        }

        async fn resolve_current(
            &self,
            _session_id: &VibexSessionId,
        ) -> VibexResult<ResolvedInitialRuntimeSelection> {
            self.initial.lock().unwrap().clone().ok_or_else(|| {
                VibexError::capability(
                    "runtime_selection_initialization_unsupported",
                    "test runtime has no current selection",
                )
            })
        }

        async fn materialize_current_runtime(
            &self,
            session_id: &VibexSessionId,
        ) -> VibexResult<()> {
            *self.materialize_calls.lock().unwrap() += 1;
            if session_id != &self.session_id {
                return Err(VibexError::conflict(
                    "runtime_selection_current_session_mismatch",
                    "current runtime belongs to another logical session",
                ));
            }
            let conn = open_database(&self.db_path)?;
            AgentSessionRuntimeRepository::mark_materialized_current_runtime_ready(
                &conn,
                session_id,
                &self.current_binding_id,
                self.activation_generation,
            )?;
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct TestExecutor {
        state: Arc<Mutex<TestExecutorState>>,
    }

    #[derive(Default)]
    struct TestExecutorState {
        calls: Vec<String>,
        create_delay: Duration,
    }

    impl TestExecutor {
        fn set_create_delay(&self, delay: Duration) {
            self.state.lock().unwrap().create_delay = delay;
        }

        fn calls(&self) -> Vec<String> {
            self.state.lock().unwrap().calls.clone()
        }

        async fn record(&self, call: &str) {
            let delay = {
                let mut state = self.state.lock().unwrap();
                state.calls.push(call.to_string());
                (call == "create_session").then_some(state.create_delay)
            };
            if let Some(delay) = delay
                && !delay.is_zero()
            {
                sleep(delay).await;
            }
        }

        fn attachment(intent: &SwitchIntent) -> PreparedAttachment {
            let now = unix_timestamp_ms();
            PreparedAttachment {
                binding: RuntimeBinding {
                    binding_id: intent.target_binding_id.clone().unwrap(),
                    session_id: intent.session_id.clone(),
                    agent_id: intent.target_selection.agent_id.clone(),
                    transport_kind: TransportKind::Acp,
                    auth_source: intent.target_selection.auth_source.clone(),
                    auth_source_revision: intent.target_auth_source_revision,
                    adapter_id: intent.target_adapter_id.clone(),
                    adapter_version: "test-v1".to_string(),
                    adapter_compatibility_identity: "test-compatible-v1".to_string(),
                    native_session_id: Some("test-native-session".to_string()),
                    native_state_home_id: NativeStateHomeId::new(),
                    provider_resume_identity: None,
                    process_spawn_fingerprint: "test-fingerprint".to_string(),
                    session_runtime_config_state: SessionRuntimeConfigState::default(),
                    capability_snapshot: None,
                    restore_compatibility_key: None,
                    last_context_sequence: 0,
                    last_summary_sequence: 0,
                    context_bridge_version: 0,
                    activation_generation: 0,
                    binding_state: BindingState::Preparing,
                    created_by_switch_id: Some(intent.switch_id.clone()),
                    created_at_ms: now,
                    updated_at_ms: now,
                },
                opaque_handle: "test-attachment".to_string(),
                restore_result: None,
            }
        }
    }

    #[async_trait]
    impl SwitchTargetExecutor for TestExecutor {
        async fn assess_target(
            &self,
            _intent: &SwitchIntent,
        ) -> VibexResult<SwitchTargetAssessment> {
            self.record("assess_target").await;
            Ok(SwitchTargetAssessment {
                same_route: true,
                process_config_changed: true,
                session_scoped_changes_only: false,
                live_ops_supported: false,
                exact_descriptor: false,
                runtime_evidence_verified: false,
                projection_fingerprint_matches: false,
                active_turn: false,
                restore: RestoreAssessment::Incompatible,
                resumable_historical_binding: false,
                supports_client_idempotency: true,
            })
        }

        async fn ensure_process(
            &self,
            _intent: &SwitchIntent,
            _operation: &JournaledOperation,
        ) -> VibexResult<PreparedProcess> {
            self.record("spawn_process").await;
            Ok(PreparedProcess {
                opaque_handle: "test-process".to_string(),
            })
        }

        async fn reacquire_process(&self, _intent: &SwitchIntent) -> VibexResult<PreparedProcess> {
            self.record("reacquire_process").await;
            Ok(PreparedProcess {
                opaque_handle: "test-process".to_string(),
            })
        }

        async fn restore_or_create_session(
            &self,
            intent: &SwitchIntent,
            _process: &PreparedProcess,
            _strategy: RuntimeSwitchStrategy,
            _operation: &JournaledOperation,
        ) -> VibexResult<PreparedAttachment> {
            self.record("create_session").await;
            Ok(Self::attachment(intent))
        }

        async fn recover_attachment(
            &self,
            intent: &SwitchIntent,
            _operation: &SwitchOperationRecord,
        ) -> VibexResult<PreparedAttachment> {
            self.record("recover_attachment").await;
            Ok(Self::attachment(intent))
        }

        async fn acquire_prepared(
            &self,
            _intent: &SwitchIntent,
            binding: &RuntimeBinding,
        ) -> VibexResult<PreparedAttachment> {
            self.record("acquire_prepared").await;
            Ok(PreparedAttachment {
                binding: binding.clone(),
                opaque_handle: "test-attachment".to_string(),
                restore_result: None,
            })
        }

        async fn apply_session_config(
            &self,
            _intent: &SwitchIntent,
            _attachment: &PreparedAttachment,
            _operation: &JournaledOperation,
        ) -> VibexResult<()> {
            self.record("apply_session_config").await;
            Ok(())
        }

        async fn apply_live_mutation(
            &self,
            _intent: &SwitchIntent,
            _attachment: &PreparedAttachment,
            _operation: &JournaledOperation,
        ) -> VibexResult<()> {
            self.record("apply_live_mutation").await;
            Ok(())
        }

        async fn revalidate_prepared(
            &self,
            _intent: &SwitchIntent,
            _attachment: &PreparedAttachment,
        ) -> VibexResult<()> {
            self.record("revalidate_prepared").await;
            Ok(())
        }

        async fn activate(
            &self,
            _intent: &SwitchIntent,
            _attachment: &PreparedAttachment,
            _activation_generation: i64,
        ) -> VibexResult<()> {
            self.record("activate").await;
            Ok(())
        }

        async fn cleanup_target(
            &self,
            _intent: &SwitchIntent,
            _attachment: Option<&PreparedAttachment>,
        ) -> VibexResult<()> {
            self.record("cleanup_target").await;
            Ok(())
        }

        async fn reconcile_operation(
            &self,
            _intent: &SwitchIntent,
            _operation: &SwitchOperationRecord,
        ) -> VibexResult<OperationReconcileOutcome> {
            Ok(OperationReconcileOutcome::NotFound)
        }
    }

    #[derive(Clone, Default)]
    struct TestGate {
        state: Arc<Mutex<TestGateState>>,
    }

    #[derive(Default)]
    struct TestGateState {
        snapshot: ActiveWorkSnapshot,
        prompt_gate_events: Vec<bool>,
        cancel_calls: Vec<ActiveWorkKind>,
    }

    impl TestGate {
        fn set_active_turn(&self, active: bool) {
            self.state.lock().unwrap().snapshot.active_turn = active;
        }

        fn prompt_gate_events(&self) -> Vec<bool> {
            self.state.lock().unwrap().prompt_gate_events.clone()
        }

        fn cancel_calls(&self) -> Vec<ActiveWorkKind> {
            self.state.lock().unwrap().cancel_calls.clone()
        }
    }

    #[async_trait]
    impl ActiveWorkGate for TestGate {
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
            self.state.lock().unwrap().cancel_calls.push(kind);
            Ok(())
        }
    }

    struct TestEnvironment {
        db_path: PathBuf,
        project_dir: PathBuf,
        session_id: VibexSessionId,
        source_binding: RuntimeBinding,
        effective: SessionRuntimeSelection,
        resolver: TestResolver,
        executor: TestExecutor,
        gate: TestGate,
        service: RuntimeSelectionService,
    }

    impl TestEnvironment {
        fn new(label: &str, wait_deadline_ms: u64) -> Self {
            let db_path = std::env::temp_dir().join(format!(
                "vibex-runtime-selection-{label}-{}.db",
                RequestId::new().as_str()
            ));
            let project_dir = std::env::temp_dir().join(format!(
                "vibex-runtime-selection-project-{label}-{}",
                RequestId::new().as_str()
            ));
            fs::create_dir_all(&project_dir).unwrap();
            let mut conn = open_database(&db_path).unwrap();
            apply_migrations(&mut conn).unwrap();
            let (project, workspace) =
                WorkspaceRepository::ensure(&conn, &project_dir, WorkspaceMode::CurrentCheckout)
                    .unwrap();
            let session_id = VibexSessionId::new();
            let agent_id = AgentId::parse("claude-code").unwrap();
            let profile_id =
                ProviderProfileId::parse(ProviderKind::Acp.local_default_profile_id().to_string())
                    .unwrap();
            let adapter_id = AcpAdapterId::parse("claude-code-acp").unwrap();
            let now = unix_timestamp_ms();
            SessionRepository::insert(
                &conn,
                &AgentSession {
                    id: session_id.clone(),
                    title: format!("runtime selection {label}"),
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
                },
            )
            .unwrap();
            let effective = SessionRuntimeSelection::provider(
                agent_id.clone(),
                profile_id.clone(),
                "model-source",
            );
            let source_binding = RuntimeBinding {
                binding_id: RuntimeBindingId::new(),
                session_id: session_id.clone(),
                agent_id: agent_id.clone(),
                transport_kind: TransportKind::Acp,
                auth_source: effective.auth_source.clone(),
                auth_source_revision: 1,
                adapter_id: adapter_id.clone(),
                adapter_version: "source-v1".to_string(),
                adapter_compatibility_identity: "source-compatible-v1".to_string(),
                native_session_id: Some("source-native-session".to_string()),
                native_state_home_id: NativeStateHomeId::new(),
                provider_resume_identity: None,
                process_spawn_fingerprint: "source-fingerprint".to_string(),
                session_runtime_config_state: SessionRuntimeConfigState::default(),
                capability_snapshot: None,
                restore_compatibility_key: None,
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
            conn.execute(
                "UPDATE agent_sessions
                 SET current_agent_id = ?2, current_binding_id = ?3,
                     desired_runtime_selection_json = ?4,
                     effective_runtime_selection_json = ?4,
                     runtime_selection_status = 'ready'
                 WHERE session_id = ?1",
                (
                    session_id.as_str(),
                    agent_id.as_str(),
                    source_binding.binding_id.as_str(),
                    serde_json::to_string(&effective).unwrap(),
                ),
            )
            .unwrap();
            drop(conn);

            let resolver = TestResolver {
                adapter_id,
                calls: Arc::new(Mutex::new(0)),
                initial: Arc::new(Mutex::new(None)),
                materialize_calls: Arc::new(Mutex::new(0)),
                db_path: db_path.clone(),
                session_id: session_id.clone(),
                current_binding_id: source_binding.binding_id.clone(),
                activation_generation: source_binding.activation_generation,
            };
            let executor = TestExecutor::default();
            let gate = TestGate::default();
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
            let service = RuntimeSelectionService::new(
                coordinator,
                Arc::new(resolver.clone()),
                RuntimeSelectionServiceConfig {
                    seamless_wait_deadline_ms: wait_deadline_ms,
                    poll_interval: Duration::from_millis(2),
                    broadcast_capacity: 16,
                },
            )
            .unwrap();
            Self {
                db_path,
                project_dir,
                session_id,
                source_binding,
                effective,
                resolver,
                executor,
                gate,
                service,
            }
        }

        fn request(
            &self,
            key: &str,
            expected_selection_revision: i64,
            model: &str,
        ) -> SetDesiredAgentSessionRuntimeRequest {
            SetDesiredAgentSessionRuntimeRequest {
                session_id: self.session_id.clone(),
                idempotency_key: key.to_string(),
                expected_revision: 0,
                expected_selection_revision,
                desired: SessionRuntimeSelection {
                    model: vibex_core::RuntimeModelSelection::explicit(model),
                    ..self.effective.clone()
                },
                interaction: RuntimeSelectionInteraction::Seamless,
            }
        }

        fn switch_by_key(&self, key: &str) -> RuntimeSwitchRecord {
            RuntimeSwitchRepository::get_by_idempotency_key(
                &open_database(&self.db_path).unwrap(),
                &self.session_id,
                key,
            )
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

    async fn wait_for_status(
        service: &RuntimeSelectionService,
        session_id: &VibexSessionId,
        expected: SessionRuntimeSelectionStatus,
    ) -> AgentSessionRuntimeSelectionState {
        for _ in 0..2_500 {
            let state = service.get_selection_state(session_id).unwrap();
            if state.status == expected {
                return state;
            }
            sleep(Duration::from_millis(2)).await;
        }
        panic!("runtime selection did not reach {expected:?}");
    }

    #[tokio::test]
    async fn seamless_wait_can_be_cancelled_without_cancelling_active_work() {
        let env = TestEnvironment::new("cancel-wait", 10_000);
        env.gate.set_active_turn(true);
        let mut events = env.service.subscribe();
        let initial = env
            .service
            .set_desired_runtime(env.request("cancel-wait", 0, "model-target"))
            .await
            .unwrap();
        assert_eq!(initial.status, SessionRuntimeSelectionStatus::Preparing);
        let switch_id = initial.pending_switch_id.clone().unwrap();
        let emitted = events.recv().await.unwrap();
        assert_eq!(emitted.state, initial);

        let waiting = wait_for_status(
            &env.service,
            &env.session_id,
            SessionRuntimeSelectionStatus::WaitingForCurrentWork,
        )
        .await;
        assert_eq!(waiting.pending_switch_id.as_ref(), Some(&switch_id));
        let record = env.switch_by_key("cancel-wait");
        let policy = record.active_work_policy.unwrap();
        for kind in [
            ActiveWorkKind::ActiveTurn,
            ActiveWorkKind::PendingPermission,
            ActiveWorkKind::ActiveTerminal,
            ActiveWorkKind::BackgroundWork,
        ] {
            assert_eq!(
                policy.disposition(kind),
                BusyDisposition::Wait {
                    deadline_ms: 10_000
                }
            );
        }

        let cancelled = env
            .service
            .cancel_switch(CancelAgentSessionRuntimeSwitchRequest {
                session_id: env.session_id.clone(),
                switch_id,
            })
            .await
            .unwrap();
        assert_eq!(cancelled.status, SessionRuntimeSelectionStatus::Ready);
        assert_eq!(cancelled.desired, env.effective);
        assert_eq!(cancelled.effective, env.effective);
        assert!(cancelled.actionable_error.is_none());
        assert!(env.gate.cancel_calls().is_empty());
        assert_eq!(env.gate.prompt_gate_events().last(), Some(&false));
    }

    #[tokio::test]
    async fn seamless_timeout_keeps_source_and_projects_actionable_error() {
        let env = TestEnvironment::new("wait-timeout", 10);
        env.gate.set_active_turn(true);
        env.service
            .set_desired_runtime(env.request("wait-timeout", 0, "model-target"))
            .await
            .unwrap();
        let failed = wait_for_status(
            &env.service,
            &env.session_id,
            SessionRuntimeSelectionStatus::FailedUsingPrevious,
        )
        .await;
        assert_eq!(failed.desired, env.effective);
        assert_eq!(failed.effective, env.effective);
        assert_eq!(
            failed.current_binding_id.as_ref(),
            Some(&env.source_binding.binding_id)
        );
        let error = failed.actionable_error.unwrap();
        assert_eq!(error.code, "runtime_switch_wait_timeout");
        assert!(error.message.contains("did not finish"));
        assert!(env.gate.cancel_calls().is_empty());
        assert_eq!(env.gate.prompt_gate_events().last(), Some(&false));
    }

    #[test]
    fn busy_claim_failure_has_a_storage_recovery_message() {
        let error = actionable_error_for_code("runtime_switch_claim_retry_exhausted").unwrap();
        assert!(error.message.contains("database stayed busy"));
        assert!(
            error
                .recovery_hint
                .as_deref()
                .is_some_and(|hint| hint.contains("no Agent work was started"))
        );
    }

    #[tokio::test]
    async fn failed_unchanged_selection_retry_materializes_current_runtime() {
        let env = TestEnvironment::new("retry-current-runtime", 500);
        let unchanged = env
            .service
            .set_desired_runtime(env.request("unchanged-ready", 0, "model-source"))
            .await
            .unwrap();
        assert_eq!(unchanged.status, SessionRuntimeSelectionStatus::Ready);
        assert_eq!(env.resolver.materialize_call_count(), 0);

        let conn = open_database(&env.db_path).unwrap();
        conn.execute(
            "UPDATE agent_sessions
             SET runtime_selection_status = 'failed_using_previous',
                 runtime_selection_error_code = 'acp_restore_fatal_failure'
             WHERE session_id = ?1",
            [env.session_id.as_str()],
        )
        .unwrap();
        drop(conn);

        let recovered = env
            .service
            .set_desired_runtime(env.request("retry-current", 0, "model-source"))
            .await
            .unwrap();
        assert_eq!(env.resolver.materialize_call_count(), 1);
        assert_eq!(recovered.status, SessionRuntimeSelectionStatus::Ready);
        assert_eq!(recovered.desired, env.effective);
        assert_eq!(recovered.effective, env.effective);
        assert!(recovered.actionable_error.is_none());
    }

    #[tokio::test]
    async fn rapid_desired_changes_converge_latest_and_retry_skips_resolution() {
        let env = TestEnvironment::new("rapid-selection", 500);
        env.executor.set_create_delay(Duration::from_millis(25));
        let first = env
            .service
            .set_desired_runtime(env.request("rapid-first", 0, "model-first"))
            .await
            .unwrap();
        assert_eq!(first.selection_revision, 1);
        let latest = env
            .service
            .set_desired_runtime(env.request("rapid-latest", 1, "model-latest"))
            .await
            .unwrap();
        assert_eq!(latest.selection_revision, 2);

        let ready = wait_for_status(
            &env.service,
            &env.session_id,
            SessionRuntimeSelectionStatus::Ready,
        )
        .await;
        assert_eq!(ready.effective.model_id(), Some("model-latest"));
        assert_eq!(ready.desired, ready.effective);
        assert_eq!(
            env.switch_by_key("rapid-first").status,
            RuntimeSwitchStatus::Superseded
        );
        assert_eq!(
            env.switch_by_key("rapid-latest").status,
            RuntimeSwitchStatus::Committed
        );
        assert_eq!(
            env.executor
                .calls()
                .iter()
                .filter(|call| call.as_str() == "create_session")
                .count(),
            1
        );
        assert_eq!(env.resolver.call_count(), 2);

        let retry = env
            .service
            .set_desired_runtime(env.request("rapid-latest", 0, "model-latest"))
            .await
            .unwrap();
        assert_eq!(retry, ready);
        assert_eq!(env.resolver.call_count(), 2);
    }

    #[tokio::test]
    async fn initial_switch_ready_state_converges_session_lifecycle_to_idle() {
        let env = TestEnvironment::new("initial-lifecycle", 10);
        let conn = open_database(&env.db_path).unwrap();
        let mut session = SessionRepository::get(&conn, &env.session_id)
            .unwrap()
            .unwrap();
        session.id = VibexSessionId::new();
        session.title = "durable initial switch".to_string();
        session.state = AgentSessionState::Initializing;
        SessionRepository::insert(&conn, &session).unwrap();
        drop(conn);

        let ready = env
            .service
            .initialize_new_session(&session.id, env.effective.clone())
            .await
            .unwrap();
        assert_eq!(ready.status, SessionRuntimeSelectionStatus::Ready);
        assert_eq!(ready.desired, ready.effective);

        let conn = open_database(&env.db_path).unwrap();
        let stored = SessionRepository::get(&conn, &session.id).unwrap().unwrap();
        assert_eq!(stored.state, AgentSessionState::Idle);
    }

    #[tokio::test]
    async fn deferred_initial_switch_returns_preparing_before_session_becomes_idle() {
        let env = TestEnvironment::new("deferred-initial-lifecycle", 500);
        let conn = open_database(&env.db_path).unwrap();
        let template = SessionRepository::get(&conn, &env.session_id)
            .unwrap()
            .unwrap();
        let session_id = VibexSessionId::new();
        let mut session = template;
        session.id = session_id.clone();
        session.title = "deferred initial switch".to_string();
        session.state = AgentSessionState::Initializing;
        SessionRepository::insert(&conn, &session).unwrap();
        drop(conn);

        let preparing = env
            .service
            .initialize_new_session_deferred(&session_id, env.effective.clone())
            .await
            .unwrap();
        assert_eq!(preparing.status, SessionRuntimeSelectionStatus::Preparing);
        assert_eq!(preparing.desired, env.effective);
        assert_eq!(preparing.effective, env.effective);
        let conn = open_database(&env.db_path).unwrap();
        assert_eq!(
            SessionRepository::get(&conn, &session_id)
                .unwrap()
                .unwrap()
                .state,
            AgentSessionState::Initializing
        );
        drop(conn);

        let ready = wait_for_status(
            &env.service,
            &session_id,
            SessionRuntimeSelectionStatus::Ready,
        )
        .await;
        assert_eq!(ready.desired, ready.effective);
        let conn = open_database(&env.db_path).unwrap();
        assert_eq!(
            SessionRepository::get(&conn, &session_id)
                .unwrap()
                .unwrap()
                .state,
            AgentSessionState::Idle
        );
    }

    #[tokio::test]
    async fn deferred_initial_switch_retries_when_the_first_driver_cannot_claim() {
        let env = TestEnvironment::new("deferred-driver-retry", 500);
        let conn = open_database(&env.db_path).unwrap();
        let template = SessionRepository::get(&conn, &env.session_id)
            .unwrap()
            .unwrap();
        let session_id = VibexSessionId::new();
        let mut session = template;
        session.id = session_id.clone();
        session.title = "deferred driver retry".to_string();
        session.state = AgentSessionState::Initializing;
        SessionRepository::insert(&conn, &session).unwrap();
        drop(conn);

        let record = env
            .service
            .enqueue_initial_runtime_switch(&session_id, env.effective.clone())
            .await
            .unwrap();
        let now = unix_timestamp_ms();
        let conn = open_database(&env.db_path).unwrap();
        assert!(
            RuntimeSwitchRepository::try_acquire_worker_lease(
                &conn,
                &record.switch_id,
                "departed-worker",
                25,
                now,
            )
            .unwrap()
        );
        drop(conn);

        env.service.emit_authoritative(&session_id).unwrap();
        env.service.start_watcher(&record).unwrap();
        let ready = wait_for_status(
            &env.service,
            &session_id,
            SessionRuntimeSelectionStatus::Ready,
        )
        .await;
        assert_eq!(ready.desired, env.effective);
        assert_eq!(ready.effective, env.effective);
        assert_eq!(
            RuntimeSwitchRepository::get(&open_database(&env.db_path).unwrap(), &record.switch_id,)
                .unwrap()
                .unwrap()
                .status,
            RuntimeSwitchStatus::Committed
        );
    }

    #[tokio::test]
    async fn initial_failure_returns_actionable_state_and_recovers_legacy_cleared_desired() {
        let env = TestEnvironment::new("initial-failure-projection", 500);
        let conn = open_database(&env.db_path).unwrap();
        let mut session = SessionRepository::get(&conn, &env.session_id)
            .unwrap()
            .unwrap();
        let session_id = VibexSessionId::new();
        session.id = session_id.clone();
        session.title = "failed initial switch".to_string();
        session.state = AgentSessionState::Initializing;
        SessionRepository::insert(&conn, &session).unwrap();
        drop(conn);

        let record = env
            .service
            .enqueue_initial_runtime_switch(&session_id, env.effective.clone())
            .await
            .unwrap();
        let mut conn = open_database(&env.db_path).unwrap();
        assert_eq!(
            RuntimeSwitchRepository::claim_requested(&mut conn, &record.switch_id).unwrap(),
            vibex_db::RequestedSwitchClaimOutcome::Claimed
        );
        RuntimeSwitchRepository::fail(
            &mut conn,
            &session_id,
            &record.switch_id,
            "runtime_switch_configuration_unavailable",
            Some("redacted configuration failure"),
        )
        .unwrap();
        let stored = AgentSessionRuntimeRepository::get_runtime_state(&conn, &session_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.desired_runtime_selection.as_ref(),
            Some(&env.effective)
        );
        assert!(stored.effective_runtime_selection.is_none());
        drop(conn);

        let failed = env.service.get_selection_state(&session_id).unwrap();
        assert_eq!(
            failed.status,
            SessionRuntimeSelectionStatus::FailedUsingPrevious
        );
        assert_eq!(failed.desired, env.effective);
        assert_eq!(failed.effective, env.effective);
        assert!(failed.current_binding_id.is_none());
        assert_eq!(
            failed
                .actionable_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("runtime_switch_configuration_unavailable")
        );

        // Versions before this fix cleared the initial desired value while
        // retaining the switch journal. The same-revision request remains the
        // durable recovery source for those sessions.
        let conn = open_database(&env.db_path).unwrap();
        conn.execute(
            "UPDATE agent_sessions
             SET desired_runtime_selection_json = NULL
             WHERE session_id = ?1",
            [session_id.as_str()],
        )
        .unwrap();
        drop(conn);
        let recovered = env.service.get_selection_state(&session_id).unwrap();
        assert_eq!(recovered, failed);
    }

    #[tokio::test]
    async fn initialization_uses_current_resolver_once_and_rejects_partial_projection() {
        let env = TestEnvironment::new("initial-selection", 10);
        let conn = open_database(&env.db_path).unwrap();
        let template = SessionRepository::get(&conn, &env.session_id)
            .unwrap()
            .unwrap();
        let session_id = VibexSessionId::new();
        let mut session = template.clone();
        session.id = session_id.clone();
        session.title = "uninitialized runtime selection".to_string();
        SessionRepository::insert(&conn, &session).unwrap();
        let mut binding = env.source_binding.clone();
        binding.binding_id = RuntimeBindingId::new();
        binding.session_id = session_id.clone();
        binding.native_session_id = Some("initial-native-session".to_string());
        env.resolver
            .set_initial(Some(ResolvedInitialRuntimeSelection {
                binding: binding.clone(),
                selection: env.effective.clone(),
            }));
        drop(conn);

        let initialized = env
            .service
            .initialize_runtime_selection(&session_id)
            .await
            .unwrap();
        assert_eq!(initialized.desired, env.effective);
        assert_eq!(initialized.effective, env.effective);
        assert_eq!(initialized.current_binding_id, Some(binding.binding_id));
        env.resolver.set_initial(None);
        assert_eq!(
            env.service
                .initialize_runtime_selection(&session_id)
                .await
                .unwrap(),
            initialized
        );

        let conn = open_database(&env.db_path).unwrap();
        let partial_session_id = VibexSessionId::new();
        let mut partial = template;
        partial.id = partial_session_id.clone();
        partial.title = "partial runtime selection".to_string();
        SessionRepository::insert(&conn, &partial).unwrap();
        conn.execute(
            "UPDATE agent_sessions SET desired_runtime_selection_json = ?2 WHERE session_id = ?1",
            (
                partial_session_id.as_str(),
                serde_json::to_string(&env.effective).unwrap(),
            ),
        )
        .unwrap();
        drop(conn);
        let error = env
            .service
            .initialize_runtime_selection(&partial_session_id)
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            "session_runtime_selection_partially_initialized"
        );
    }

    #[test]
    fn service_rejects_unbounded_seamless_deadline() {
        let env = TestEnvironment::new("config-validation", 10);
        let error = RuntimeSelectionService::new(
            env.inner_coordinator_for_invalid_config(),
            Arc::new(env.resolver.clone()),
            RuntimeSelectionServiceConfig {
                seamless_wait_deadline_ms: MAX_RUNTIME_SWITCH_WAIT_DEADLINE_MS + 1,
                ..RuntimeSelectionServiceConfig::default()
            },
        )
        .err()
        .unwrap();
        assert_eq!(error.code, "runtime_selection_service_config_invalid");
    }

    impl TestEnvironment {
        fn inner_coordinator_for_invalid_config(&self) -> RuntimeSwitchCoordinator {
            RuntimeSwitchCoordinator::new(
                &self.db_path,
                Arc::new(self.executor.clone()),
                Arc::new(self.gate.clone()),
                RuntimeSwitchCoordinatorConfig::default(),
            )
            .unwrap()
        }
    }
}

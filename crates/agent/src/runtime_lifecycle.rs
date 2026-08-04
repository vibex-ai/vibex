//! Provider-neutral runtime attachment lifecycle.
//!
//! The service owns client/internal leases and the reconnect event cursor. A
//! provider backend owns process/attachment handles and implements the small
//! materialization/sweep surface below. Keeping these concerns separate lets
//! Remote and Desktop consume one contract without depending on ACP types.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Mutex as AsyncMutex, broadcast};
use tokio::task::JoinHandle;
use vibex_core::{
    AgentSessionRuntimeSnapshot, AttachRuntimeRequest, AttachRuntimeResponse, DetachRuntimeRequest,
    DetachRuntimeResponse, RuntimeAttachmentSnapshot, RuntimeClientId, RuntimeEventBatch,
    RuntimeEventCursor, RuntimeEventKind, RuntimeLeaseId, RuntimeLeaseRole, RuntimeLeaseRoleCounts,
    RuntimeMaterializationStatus, RuntimeProcessId, RuntimeProcessSnapshot, RuntimeSessionEvent,
    RuntimeStreamId, VibexError, VibexResult, VibexSessionId,
};

pub const DEFAULT_RUNTIME_CLIENT_HEARTBEAT: Duration = Duration::from_secs(30);
pub const DEFAULT_RUNTIME_CLIENT_TTL: Duration = Duration::from_secs(90);
pub const DEFAULT_RUNTIME_EVENT_CAPACITY: usize = 256;
pub const DEFAULT_RUNTIME_EVENT_BATCH_LIMIT: usize = 128;
pub const DEFAULT_RUNTIME_SWEEP_INTERVAL: Duration = Duration::from_secs(15);

/// Internal target identity. A replacement with a new process or generation
/// never inherits a lease for an older target.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RuntimeLeaseTarget {
    Process(RuntimeProcessId),
    Attachment {
        binding_id: vibex_core::RuntimeBindingId,
        activation_generation: i64,
        process_id: RuntimeProcessId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeBackendSnapshot {
    pub materialization_status: RuntimeMaterializationStatus,
    pub attachment: Option<RuntimeAttachmentSnapshot>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeSweepReport {
    pub attachments_removed: usize,
    pub processes_removed: usize,
}

/// Backend seam implemented by ACP (and replaceable by deterministic tests).
#[async_trait]
pub trait RuntimeLifecycleBackend: Send + Sync + 'static {
    fn install_publisher(&self, _publisher: RuntimeLifecyclePublisher) {}

    fn snapshot(&self, session_id: &VibexSessionId) -> VibexResult<RuntimeBackendSnapshot>;

    fn process_snapshot(
        &self,
        process_id: &RuntimeProcessId,
    ) -> VibexResult<RuntimeProcessSnapshot>;

    fn touch(&self, _target: &RuntimeLeaseTarget, _now_ms: i64) -> VibexResult<()> {
        Ok(())
    }

    async fn materialize_owner(
        &self,
        session_id: &VibexSessionId,
    ) -> VibexResult<RuntimeBackendSnapshot>;

    async fn sweep(
        &self,
        now_ms: i64,
        protected_targets: &[RuntimeLeaseTarget],
    ) -> VibexResult<RuntimeSweepReport>;
}

pub trait RuntimeLifecycleClock: Send + Sync + 'static {
    fn now_ms(&self) -> i64;
}

#[derive(Debug, Default)]
pub struct SystemRuntimeLifecycleClock;

impl RuntimeLifecycleClock for SystemRuntimeLifecycleClock {
    fn now_ms(&self) -> i64 {
        vibex_core::unix_timestamp_ms()
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeLifecycleConfig {
    pub heartbeat: Duration,
    pub client_ttl: Duration,
    pub event_capacity: usize,
    pub event_batch_limit: usize,
    pub sweep_interval: Duration,
}

impl Default for RuntimeLifecycleConfig {
    fn default() -> Self {
        Self {
            heartbeat: DEFAULT_RUNTIME_CLIENT_HEARTBEAT,
            client_ttl: DEFAULT_RUNTIME_CLIENT_TTL,
            event_capacity: DEFAULT_RUNTIME_EVENT_CAPACITY,
            event_batch_limit: DEFAULT_RUNTIME_EVENT_BATCH_LIMIT,
            sweep_interval: DEFAULT_RUNTIME_SWEEP_INTERVAL,
        }
    }
}

impl RuntimeLifecycleConfig {
    fn validate(&self) -> VibexResult<()> {
        if self.heartbeat.is_zero()
            || self.client_ttl.is_zero()
            || self.client_ttl < self.heartbeat
            || self.event_capacity == 0
            || self.event_batch_limit == 0
            || self.sweep_interval.is_zero()
        {
            return Err(VibexError::validation(
                "runtime_lifecycle_config_invalid",
                "runtime lifecycle durations and event limits must be positive and ordered",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct LeaseEntry {
    session_id: VibexSessionId,
    scope: String,
    client_id: String,
    role: RuntimeLeaseRole,
    target: RuntimeLeaseTarget,
    expires_at_ms: Option<i64>,
}

#[derive(Default)]
struct LifecycleState {
    stopping: bool,
    next_sequence: HashMap<String, u64>,
    rings: HashMap<String, VecDeque<RuntimeSessionEvent>>,
    leases: HashMap<RuntimeLeaseId, LeaseEntry>,
}

struct RuntimeLifecycleInner {
    backend: Arc<dyn RuntimeLifecycleBackend>,
    clock: Arc<dyn RuntimeLifecycleClock>,
    config: RuntimeLifecycleConfig,
    stream_id: Mutex<RuntimeStreamId>,
    state: Mutex<LifecycleState>,
    mutation_gate: AsyncMutex<()>,
    session_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    events: broadcast::Sender<RuntimeSessionEvent>,
    sweep_task: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct RuntimeLifecycleService {
    inner: Arc<RuntimeLifecycleInner>,
}

#[derive(Clone)]
pub struct RuntimeLifecyclePublisher {
    inner: Weak<RuntimeLifecycleInner>,
}

pub struct RuntimeLeaseGuard {
    service: Weak<RuntimeLifecycleInner>,
    lease_id: Option<RuntimeLeaseId>,
}

impl RuntimeLifecycleService {
    pub fn new(
        backend: Arc<dyn RuntimeLifecycleBackend>,
        config: RuntimeLifecycleConfig,
    ) -> VibexResult<Self> {
        Self::with_clock(backend, config, Arc::new(SystemRuntimeLifecycleClock))
    }

    pub fn with_clock(
        backend: Arc<dyn RuntimeLifecycleBackend>,
        config: RuntimeLifecycleConfig,
        clock: Arc<dyn RuntimeLifecycleClock>,
    ) -> VibexResult<Self> {
        config.validate()?;
        let (events, _) = broadcast::channel(config.event_capacity);
        let inner = Arc::new(RuntimeLifecycleInner {
            backend,
            clock,
            config,
            stream_id: Mutex::new(RuntimeStreamId::new()),
            state: Mutex::new(LifecycleState::default()),
            mutation_gate: AsyncMutex::new(()),
            session_locks: Mutex::new(HashMap::new()),
            events,
            sweep_task: Mutex::new(None),
        });
        inner.backend.install_publisher(RuntimeLifecyclePublisher {
            inner: Arc::downgrade(&inner),
        });
        Ok(Self { inner })
    }

    pub fn config(&self) -> &RuntimeLifecycleConfig {
        &self.inner.config
    }

    pub fn stream_id(&self) -> RuntimeStreamId {
        self.inner
            .stream_id
            .lock()
            .map(|stream_id| stream_id.clone())
            .unwrap_or_default()
    }

    fn current_stream_id(&self) -> RuntimeStreamId {
        self.stream_id()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeSessionEvent> {
        self.inner.events.subscribe()
    }

    pub fn subscribe_runtime_events(&self) -> broadcast::Receiver<RuntimeSessionEvent> {
        self.subscribe()
    }

    pub fn publisher(&self) -> RuntimeLifecyclePublisher {
        RuntimeLifecyclePublisher {
            inner: Arc::downgrade(&self.inner),
        }
    }

    pub fn start(&self, runtime: &tokio::runtime::Handle) -> VibexResult<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| lifecycle_lock_error("state"))?;
        let mut task = self
            .inner
            .sweep_task
            .lock()
            .map_err(|_| lifecycle_lock_error("sweepTask"))?;
        if task.is_some() {
            return Ok(());
        }
        state.stopping = false;
        state.next_sequence.clear();
        state.rings.clear();
        if let Ok(mut stream_id) = self.inner.stream_id.lock() {
            *stream_id = RuntimeStreamId::new();
        }
        let weak = Arc::downgrade(&self.inner);
        let interval = self.inner.config.sweep_interval;
        *task = Some(runtime.spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                if inner.is_stopping() {
                    break;
                }
                let service = RuntimeLifecycleService { inner };
                let _ = service.sweep_once().await;
            }
        }));
        Ok(())
    }

    pub async fn stop(&self) -> VibexResult<()> {
        let _mutation_guard = self.inner.mutation_gate.lock().await;
        let task = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| lifecycle_lock_error("state"))?;
            state.stopping = true;
            state.leases.clear();
            state.next_sequence.clear();
            state.rings.clear();
            let mut task = self
                .inner
                .sweep_task
                .lock()
                .map_err(|_| lifecycle_lock_error("sweepTask"))?;
            task.take()
        };
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        Ok(())
    }

    pub fn snapshot(
        &self,
        session_id: &VibexSessionId,
    ) -> VibexResult<AgentSessionRuntimeSnapshot> {
        let sequence = self.session_sequence(session_id);
        let backend = self.inner.backend.snapshot(session_id)?;
        Ok(self.project_snapshot_at(session_id, backend, sequence))
    }

    pub fn get_session_attachment_snapshot(
        &self,
        session_id: &VibexSessionId,
    ) -> VibexResult<AgentSessionRuntimeSnapshot> {
        self.snapshot(session_id)
    }

    pub fn process_snapshot(
        &self,
        process_id: &RuntimeProcessId,
    ) -> VibexResult<RuntimeProcessSnapshot> {
        let mut snapshot = self.inner.backend.process_snapshot(process_id)?;
        snapshot.lease_counts = self.process_lease_projection(process_id)?;
        Ok(snapshot)
    }

    pub fn get_process_snapshot(
        &self,
        process_id: &RuntimeProcessId,
    ) -> VibexResult<RuntimeProcessSnapshot> {
        self.process_snapshot(process_id)
    }

    pub fn events(
        &self,
        session_id: &VibexSessionId,
        after: Option<&RuntimeEventCursor>,
        limit: Option<usize>,
    ) -> VibexResult<RuntimeEventBatch> {
        let limit = limit
            .unwrap_or(self.inner.config.event_batch_limit)
            .min(self.inner.config.event_batch_limit)
            .max(1);
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| lifecycle_lock_error("state"))?;
        let key = session_id.as_str();
        let ring = state.rings.get(key);
        let head = state.next_sequence.get(key).copied().unwrap_or(0);
        let next_cursor = RuntimeEventCursor {
            stream_id: self.current_stream_id(),
            sequence: head,
        };
        let Some(after) = after else {
            return Ok(RuntimeEventBatch {
                session_id: session_id.clone(),
                events: Vec::new(),
                next_cursor,
                reset_required: false,
            });
        };
        if after.stream_id != self.current_stream_id() {
            return Ok(RuntimeEventBatch {
                session_id: session_id.clone(),
                events: Vec::new(),
                next_cursor,
                reset_required: true,
            });
        }
        if after.sequence > head {
            return Ok(RuntimeEventBatch {
                session_id: session_id.clone(),
                events: Vec::new(),
                next_cursor,
                reset_required: true,
            });
        }
        let reset_required = ring
            .and_then(|events| events.front())
            .is_some_and(|event| after.sequence.saturating_add(1) < event.cursor.sequence);
        if reset_required {
            return Ok(RuntimeEventBatch {
                session_id: session_id.clone(),
                events: Vec::new(),
                next_cursor,
                reset_required: true,
            });
        }
        let events = ring
            .into_iter()
            .flat_map(|events| events.iter())
            .filter(|event| event.cursor.sequence > after.sequence)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let next_cursor = RuntimeEventCursor {
            stream_id: next_cursor.stream_id,
            sequence: events
                .last()
                .map(|event| event.cursor.sequence)
                .unwrap_or(head),
        };
        Ok(RuntimeEventBatch {
            session_id: session_id.clone(),
            events,
            next_cursor,
            reset_required: false,
        })
    }

    pub fn get_runtime_events(
        &self,
        session_id: &VibexSessionId,
        after: Option<&RuntimeEventCursor>,
        limit: Option<usize>,
    ) -> VibexResult<RuntimeEventBatch> {
        self.events(session_id, after, limit)
    }

    pub async fn attach(
        &self,
        request: AttachRuntimeRequest,
        scope: impl Into<String>,
    ) -> VibexResult<AttachRuntimeResponse> {
        if !matches!(
            request.role,
            RuntimeLeaseRole::Owner | RuntimeLeaseRole::Viewer
        ) {
            return Err(VibexError::validation(
                "runtime_lease_internal_role_forbidden",
                "internal runtime lease roles cannot be requested by clients",
            ));
        }
        let scope = bounded_scope(scope.into())?;
        let _mutation_guard = self.inner.mutation_gate.lock().await;
        let session_lock = self.session_lock(&request.session_id)?;
        let _guard = session_lock.lock().await;
        self.ensure_running()?;
        let mut backend = self.inner.backend.snapshot(&request.session_id)?;
        if request.role == RuntimeLeaseRole::Owner
            && (backend.attachment.is_none()
                || backend.materialization_status != RuntimeMaterializationStatus::Available)
        {
            backend = self
                .inner
                .backend
                .materialize_owner(&request.session_id)
                .await?;
        }
        let target = backend
            .attachment
            .as_ref()
            .filter(|attachment| {
                backend.materialization_status == RuntimeMaterializationStatus::Available
                    && matches!(
                        attachment.status,
                        vibex_core::RuntimeAttachmentStatus::Ready
                            | vibex_core::RuntimeAttachmentStatus::Inactive
                    )
            })
            .map(attachment_target);
        let now = self.inner.clock.now_ms();
        let (lease_id, expires_at_ms) = if let Some(target) = target {
            self.inner.backend.touch(&target, now)?;
            let (lease_id, changed) = self.upsert_client_lease(
                &request.session_id,
                &scope,
                &request.client_id,
                request.role,
                target,
                now,
            )?;
            if changed {
                self.publish_for(
                    &request.session_id,
                    RuntimeEventKind::LeaseChanged,
                    backend.attachment.as_ref(),
                );
            }
            (
                Some(lease_id),
                Some(now.saturating_add(duration_ms_i64(self.inner.config.client_ttl))),
            )
        } else {
            (None, None)
        };
        let snapshot = self.project_snapshot(&request.session_id, backend);
        Ok(AttachRuntimeResponse {
            snapshot,
            lease_expires_at_ms: expires_at_ms,
            lease_id,
        })
    }

    pub async fn attach_runtime(
        &self,
        request: AttachRuntimeRequest,
        scope: impl Into<String>,
    ) -> VibexResult<AttachRuntimeResponse> {
        self.attach(request, scope).await
    }

    /// Materializes a runtime for backend work and holds an exact-fence lease
    /// until the returned guard is dropped. This path is intentionally separate
    /// from the public Owner/Viewer attach API so clients cannot mint internal
    /// keep-alives.
    pub async fn materialize_internal(
        self: &Arc<Self>,
        session_id: VibexSessionId,
        role: RuntimeLeaseRole,
        holder: impl Into<String>,
    ) -> VibexResult<RuntimeLeaseGuard> {
        if !matches!(
            role,
            RuntimeLeaseRole::BackgroundWorker | RuntimeLeaseRole::SwitchPreparation
        ) {
            return Err(VibexError::validation(
                "runtime_lease_internal_role_invalid",
                "internal materialization requires a backend worker role",
            ));
        }
        let _mutation_guard = self.inner.mutation_gate.lock().await;
        let session_lock = self.session_lock(&session_id)?;
        let _guard = session_lock.lock().await;
        self.ensure_running()?;
        let mut backend = self.inner.backend.snapshot(&session_id)?;
        if backend.attachment.is_none()
            || backend.materialization_status != RuntimeMaterializationStatus::Available
        {
            backend = self.inner.backend.materialize_owner(&session_id).await?;
        }
        let attachment = backend.attachment.as_ref().ok_or_else(|| {
            VibexError::conflict(
                "runtime_not_materialized",
                "runtime backend did not return an attachment",
            )
        })?;
        let target = attachment_target(attachment);
        self.acquire_internal_locked(session_id, target, role, holder)
    }

    pub async fn detach(
        &self,
        request: DetachRuntimeRequest,
        scope: impl Into<String>,
    ) -> VibexResult<DetachRuntimeResponse> {
        let scope = bounded_scope(scope.into())?;
        let _mutation_guard = self.inner.mutation_gate.lock().await;
        let session_lock = self.session_lock(&request.session_id)?;
        let _guard = session_lock.lock().await;
        let removed = self.remove_client_leases(&request.session_id, &scope, &request.client_id)?;
        if removed {
            self.publish_for(&request.session_id, RuntimeEventKind::LeaseChanged, None);
        }
        Ok(DetachRuntimeResponse { released: removed })
    }

    pub async fn detach_runtime(
        &self,
        request: DetachRuntimeRequest,
        scope: impl Into<String>,
    ) -> VibexResult<DetachRuntimeResponse> {
        self.detach(request, scope).await
    }

    pub async fn acquire_internal(
        self: &Arc<Self>,
        session_id: VibexSessionId,
        target: RuntimeLeaseTarget,
        role: RuntimeLeaseRole,
        holder: impl Into<String>,
    ) -> VibexResult<RuntimeLeaseGuard> {
        let _mutation_guard = self.inner.mutation_gate.lock().await;
        self.acquire_internal_locked(session_id, target, role, holder)
    }

    fn acquire_internal_locked(
        self: &Arc<Self>,
        session_id: VibexSessionId,
        target: RuntimeLeaseTarget,
        role: RuntimeLeaseRole,
        holder: impl Into<String>,
    ) -> VibexResult<RuntimeLeaseGuard> {
        self.ensure_running()?;
        if !matches!(
            role,
            RuntimeLeaseRole::BackgroundWorker | RuntimeLeaseRole::SwitchPreparation
        ) {
            return Err(VibexError::validation(
                "runtime_lease_internal_role_invalid",
                "internal lease guard requires a backend worker role",
            ));
        }
        let holder = bounded_scope(holder.into())?;
        self.inner
            .backend
            .touch(&target, self.inner.clock.now_ms())?;
        let lease_id = RuntimeLeaseId::new();
        let entry = LeaseEntry {
            session_id: session_id.clone(),
            scope: "internal".to_string(),
            client_id: holder,
            role,
            target,
            expires_at_ms: None,
        };
        self.inner
            .state
            .lock()
            .map_err(|_| lifecycle_lock_error("state"))?
            .leases
            .insert(lease_id.clone(), entry);
        self.publish_for(&session_id, RuntimeEventKind::LeaseChanged, None);
        Ok(RuntimeLeaseGuard {
            service: Arc::downgrade(&self.inner),
            lease_id: Some(lease_id),
        })
    }

    pub async fn sweep_once(&self) -> VibexResult<RuntimeSweepReport> {
        let _mutation_guard = self.inner.mutation_gate.lock().await;
        self.ensure_running()?;
        let now = self.inner.clock.now_ms();
        let expired = self.expire_client_leases(now)?;
        for session_id in expired {
            self.publish_for(&session_id, RuntimeEventKind::LeaseChanged, None);
        }
        let protected = self
            .inner
            .state
            .lock()
            .map_err(|_| lifecycle_lock_error("state"))?
            .leases
            .values()
            .map(|lease| lease.target.clone())
            .collect::<Vec<_>>();
        self.inner.backend.sweep(now, &protected).await
    }

    fn project_snapshot(
        &self,
        session_id: &VibexSessionId,
        backend: RuntimeBackendSnapshot,
    ) -> AgentSessionRuntimeSnapshot {
        let sequence = self.session_sequence(session_id);
        self.project_snapshot_at(session_id, backend, sequence)
    }

    fn project_snapshot_at(
        &self,
        session_id: &VibexSessionId,
        mut backend: RuntimeBackendSnapshot,
        sequence: u64,
    ) -> AgentSessionRuntimeSnapshot {
        let (_, counts) = self.lease_projection(session_id, backend.attachment.as_ref());
        if let Some(attachment) = backend.attachment.as_mut() {
            attachment.lease_counts = counts;
            attachment.last_event_sequence = sequence;
        }
        AgentSessionRuntimeSnapshot {
            session_id: session_id.clone(),
            cursor: RuntimeEventCursor {
                stream_id: self.current_stream_id(),
                sequence,
            },
            materialization_status: backend.materialization_status,
            attachment: backend.attachment,
        }
    }

    fn session_sequence(&self, session_id: &VibexSessionId) -> u64 {
        self.inner
            .state
            .lock()
            .ok()
            .and_then(|state| state.next_sequence.get(session_id.as_str()).copied())
            .unwrap_or(0)
    }

    fn lease_projection(
        &self,
        session_id: &VibexSessionId,
        attachment: Option<&RuntimeAttachmentSnapshot>,
    ) -> (u64, RuntimeLeaseRoleCounts) {
        let Ok(state) = self.inner.state.lock() else {
            return (0, RuntimeLeaseRoleCounts::default());
        };
        let sequence = state
            .next_sequence
            .get(session_id.as_str())
            .copied()
            .unwrap_or(0);
        let Some(attachment) = attachment else {
            return (sequence, RuntimeLeaseRoleCounts::default());
        };
        let target = attachment_target(attachment);
        let mut counts = RuntimeLeaseRoleCounts::default();
        for lease in state
            .leases
            .values()
            .filter(|lease| lease.session_id == *session_id && lease.target == target)
        {
            increment_role_count(&mut counts, lease.role);
        }
        (sequence, counts)
    }

    fn session_lock(&self, session_id: &VibexSessionId) -> VibexResult<Arc<AsyncMutex<()>>> {
        let mut locks = self
            .inner
            .session_locks
            .lock()
            .map_err(|_| lifecycle_lock_error("sessionLocks"))?;
        Ok(Arc::clone(
            locks
                .entry(session_id.as_str().to_string())
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        ))
    }

    fn ensure_running(&self) -> VibexResult<()> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| lifecycle_lock_error("state"))?;
        if state.stopping {
            return Err(VibexError::conflict(
                "runtime_lifecycle_stopping",
                "runtime lifecycle is stopping",
            ));
        }
        Ok(())
    }

    fn upsert_client_lease(
        &self,
        session_id: &VibexSessionId,
        scope: &str,
        client_id: &RuntimeClientId,
        role: RuntimeLeaseRole,
        target: RuntimeLeaseTarget,
        now: i64,
    ) -> VibexResult<(RuntimeLeaseId, bool)> {
        let expires_at_ms = now.saturating_add(duration_ms_i64(self.inner.config.client_ttl));
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| lifecycle_lock_error("state"))?;
        if let Some((id, lease)) = state.leases.iter_mut().find(|(_, lease)| {
            lease.session_id == *session_id
                && lease.scope == scope
                && lease.client_id == client_id.as_str()
                && lease.expires_at_ms.is_some()
        }) {
            let changed = lease.role != role || lease.target != target;
            lease.role = role;
            lease.target = target;
            lease.expires_at_ms = Some(expires_at_ms);
            return Ok((id.clone(), changed));
        }
        let id = RuntimeLeaseId::new();
        state.leases.insert(
            id.clone(),
            LeaseEntry {
                session_id: session_id.clone(),
                scope: scope.to_string(),
                client_id: client_id.as_str().to_string(),
                role,
                target,
                expires_at_ms: Some(expires_at_ms),
            },
        );
        Ok((id, true))
    }

    fn remove_client_leases(
        &self,
        session_id: &VibexSessionId,
        scope: &str,
        client_id: &RuntimeClientId,
    ) -> VibexResult<bool> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| lifecycle_lock_error("state"))?;
        let before = state.leases.len();
        state.leases.retain(|_, lease| {
            !(lease.session_id == *session_id
                && lease.scope == scope
                && lease.client_id == client_id.as_str())
        });
        Ok(state.leases.len() != before)
    }

    fn process_lease_projection(
        &self,
        process_id: &RuntimeProcessId,
    ) -> VibexResult<RuntimeLeaseRoleCounts> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| lifecycle_lock_error("state"))?;
        let mut counts = RuntimeLeaseRoleCounts::default();
        for lease in state.leases.values() {
            let matches_process = match &lease.target {
                RuntimeLeaseTarget::Process(candidate) => candidate == process_id,
                RuntimeLeaseTarget::Attachment {
                    process_id: candidate,
                    ..
                } => candidate == process_id,
            };
            if matches_process {
                increment_role_count(&mut counts, lease.role);
            }
        }
        Ok(counts)
    }

    fn expire_client_leases(&self, now: i64) -> VibexResult<Vec<VibexSessionId>> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| lifecycle_lock_error("state"))?;
        let mut sessions = Vec::new();
        state.leases.retain(|_, lease| {
            let expired = lease.expires_at_ms.is_some_and(|deadline| deadline <= now);
            if expired && !sessions.contains(&lease.session_id) {
                sessions.push(lease.session_id.clone());
            }
            !expired
        });
        Ok(sessions)
    }

    fn publish_for(
        &self,
        session_id: &VibexSessionId,
        kind: RuntimeEventKind,
        attachment: Option<&RuntimeAttachmentSnapshot>,
    ) {
        let _ = self.publisher().publish(
            session_id,
            kind,
            attachment.map(|attachment| attachment.binding_id.clone()),
            attachment.map(|attachment| attachment.process_id.clone()),
        );
    }
}

impl RuntimeLifecyclePublisher {
    pub fn publish(
        &self,
        session_id: &VibexSessionId,
        kind: RuntimeEventKind,
        binding_id: Option<vibex_core::RuntimeBindingId>,
        process_id: Option<RuntimeProcessId>,
    ) -> Option<RuntimeEventCursor> {
        let inner = self.inner.upgrade()?;
        let mut state = inner.state.lock().ok()?;
        if state.stopping {
            return None;
        }
        let key = session_id.as_str().to_string();
        let sequence = state
            .next_sequence
            .entry(key.clone())
            .and_modify(|value| *value = value.saturating_add(1))
            .or_insert(1);
        let event = RuntimeSessionEvent {
            session_id: session_id.clone(),
            cursor: RuntimeEventCursor {
                stream_id: inner
                    .stream_id
                    .lock()
                    .ok()
                    .map(|stream_id| stream_id.clone())
                    .unwrap_or_default(),
                sequence: *sequence,
            },
            kind,
            binding_id,
            process_id,
            emitted_at_ms: inner.clock.now_ms(),
        };
        let ring = state.rings.entry(key).or_default();
        ring.push_back(event.clone());
        while ring.len() > inner.config.event_capacity {
            ring.pop_front();
        }
        let _ = inner.events.send(event.clone());
        Some(event.cursor)
    }
}

impl RuntimeLeaseGuard {
    pub fn lease_id(&self) -> Option<&RuntimeLeaseId> {
        self.lease_id.as_ref()
    }

    pub fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        let Some(lease_id) = self.lease_id.take() else {
            return;
        };
        let Some(inner) = self.service.upgrade() else {
            return;
        };
        let removed = inner.state.lock().ok().and_then(|mut state| {
            state
                .leases
                .remove(&lease_id)
                .map(|lease| (lease.session_id, lease.target))
        });
        if let Some((session_id, target)) = removed {
            let (binding_id, process_id) = match target {
                RuntimeLeaseTarget::Process(process_id) => (None, Some(process_id)),
                RuntimeLeaseTarget::Attachment {
                    binding_id,
                    process_id,
                    ..
                } => (Some(binding_id), Some(process_id)),
            };
            let publisher = RuntimeLifecyclePublisher {
                inner: Arc::downgrade(&inner),
            };
            let _ = publisher.publish(
                &session_id,
                RuntimeEventKind::LeaseChanged,
                binding_id,
                process_id,
            );
        }
    }
}

impl Drop for RuntimeLeaseGuard {
    fn drop(&mut self) {
        self.release_inner();
    }
}

fn attachment_target(attachment: &RuntimeAttachmentSnapshot) -> RuntimeLeaseTarget {
    RuntimeLeaseTarget::Attachment {
        binding_id: attachment.binding_id.clone(),
        activation_generation: attachment.activation_generation,
        process_id: attachment.process_id.clone(),
    }
}

fn increment_role_count(counts: &mut RuntimeLeaseRoleCounts, role: RuntimeLeaseRole) {
    match role {
        RuntimeLeaseRole::Owner => counts.owner = counts.owner.saturating_add(1),
        RuntimeLeaseRole::Viewer => counts.viewer = counts.viewer.saturating_add(1),
        RuntimeLeaseRole::BackgroundWorker => {
            counts.background_worker = counts.background_worker.saturating_add(1)
        }
        RuntimeLeaseRole::SwitchPreparation => {
            counts.switch_preparation = counts.switch_preparation.saturating_add(1)
        }
    }
}

fn bounded_scope(scope: String) -> VibexResult<String> {
    let scope = scope.trim();
    if scope.is_empty() || scope.len() > 256 || scope.chars().any(|ch| ch.is_control()) {
        return Err(VibexError::validation(
            "runtime_client_scope_invalid",
            "runtime client scope must be non-empty and bounded",
        ));
    }
    Ok(scope.to_string())
}

fn duration_ms_i64(duration: Duration) -> i64 {
    duration.as_millis().min(i64::MAX as u128) as i64
}

fn lifecycle_lock_error(scope: &'static str) -> VibexError {
    VibexError::process(
        "runtime_lifecycle_lock_poisoned",
        format!("runtime lifecycle lock is poisoned: {scope}"),
    )
}

impl RuntimeLifecycleInner {
    fn is_stopping(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.stopping)
            .unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;

    #[derive(Default)]
    struct ManualClock(AtomicI64);

    impl RuntimeLifecycleClock for ManualClock {
        fn now_ms(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct FakeBackend {
        snapshot: Mutex<RuntimeBackendSnapshot>,
        publisher: Mutex<Option<RuntimeLifecyclePublisher>>,
        materialize_calls: Mutex<usize>,
        touches: Mutex<Vec<(RuntimeLeaseTarget, i64)>>,
    }

    impl FakeBackend {
        fn new(_session: &VibexSessionId) -> Self {
            Self {
                snapshot: Mutex::new(RuntimeBackendSnapshot {
                    materialization_status: RuntimeMaterializationStatus::NotMaterialized,
                    attachment: None,
                }),
                publisher: Mutex::new(None),
                materialize_calls: Mutex::new(0),
                touches: Mutex::new(Vec::new()),
            }
        }

        fn set_attachment(&self, session: &VibexSessionId) {
            let attachment = RuntimeAttachmentSnapshot {
                binding_id: vibex_core::RuntimeBindingId::new(),
                process_id: RuntimeProcessId::new(),
                activation_generation: 0,
                status: vibex_core::RuntimeAttachmentStatus::Ready,
                last_event_sequence: 0,
                current_model: None,
                current_mode: None,
                config_options: Vec::new(),
                active_message: None,
                active_tool_calls: Vec::new(),
                pending_permissions: Vec::new(),
                active_terminal_count: 0,
                active_background_work_count: 0,
                lease_counts: RuntimeLeaseRoleCounts::default(),
                usage: None,
            };
            *self.snapshot.lock().unwrap() = RuntimeBackendSnapshot {
                materialization_status: RuntimeMaterializationStatus::Available,
                attachment: Some(attachment),
            };
            if let Some(publisher) = self.publisher.lock().unwrap().as_ref() {
                publisher.publish(session, RuntimeEventKind::AttachmentActivated, None, None);
            }
        }
    }

    #[async_trait]
    impl RuntimeLifecycleBackend for FakeBackend {
        fn install_publisher(&self, publisher: RuntimeLifecyclePublisher) {
            *self.publisher.lock().unwrap() = Some(publisher);
        }

        fn snapshot(&self, _session_id: &VibexSessionId) -> VibexResult<RuntimeBackendSnapshot> {
            Ok(self.snapshot.lock().unwrap().clone())
        }

        fn process_snapshot(
            &self,
            _process_id: &RuntimeProcessId,
        ) -> VibexResult<RuntimeProcessSnapshot> {
            Err(VibexError::process(
                "runtime_process_snapshot_missing",
                "fake process snapshot missing",
            ))
        }

        fn touch(&self, target: &RuntimeLeaseTarget, now_ms: i64) -> VibexResult<()> {
            self.touches.lock().unwrap().push((target.clone(), now_ms));
            Ok(())
        }

        async fn materialize_owner(
            &self,
            session_id: &VibexSessionId,
        ) -> VibexResult<RuntimeBackendSnapshot> {
            *self.materialize_calls.lock().unwrap() += 1;
            self.set_attachment(session_id);
            self.snapshot(session_id)
        }

        async fn sweep(
            &self,
            _now_ms: i64,
            _protected_targets: &[RuntimeLeaseTarget],
        ) -> VibexResult<RuntimeSweepReport> {
            Ok(RuntimeSweepReport::default())
        }
    }

    fn request(session_id: VibexSessionId, role: RuntimeLeaseRole) -> AttachRuntimeRequest {
        AttachRuntimeRequest {
            session_id,
            client_id: RuntimeClientId::new(),
            role,
        }
    }

    #[tokio::test]
    async fn owner_materializes_viewer_does_not() {
        let session = VibexSessionId::new();
        let backend = Arc::new(FakeBackend::new(&session));
        let clock = Arc::new(ManualClock::default());
        let service = RuntimeLifecycleService::with_clock(
            backend.clone(),
            RuntimeLifecycleConfig {
                heartbeat: Duration::from_millis(10),
                client_ttl: Duration::from_millis(30),
                event_capacity: 8,
                event_batch_limit: 4,
                sweep_interval: Duration::from_secs(1),
            },
            clock,
        )
        .unwrap();
        let viewer = service
            .attach(request(session.clone(), RuntimeLeaseRole::Viewer), "local")
            .await
            .unwrap();
        assert_eq!(viewer.lease_id, None);
        assert_eq!(*backend.materialize_calls.lock().unwrap(), 0);
        let owner = service
            .attach(request(session.clone(), RuntimeLeaseRole::Owner), "local")
            .await
            .unwrap();
        assert!(owner.lease_id.is_some());
        assert_eq!(*backend.materialize_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn client_attach_is_idempotent_and_expiry_removes_lease() {
        let session = VibexSessionId::new();
        let backend = Arc::new(FakeBackend::new(&session));
        backend.set_attachment(&session);
        let clock = Arc::new(ManualClock::default());
        let service = RuntimeLifecycleService::with_clock(
            backend.clone(),
            RuntimeLifecycleConfig {
                heartbeat: Duration::from_millis(10),
                client_ttl: Duration::from_millis(30),
                event_capacity: 8,
                event_batch_limit: 4,
                sweep_interval: Duration::from_secs(1),
            },
            clock.clone(),
        )
        .unwrap();
        let client = RuntimeClientId::new();
        let first = service
            .attach(
                AttachRuntimeRequest {
                    session_id: session.clone(),
                    client_id: client.clone(),
                    role: RuntimeLeaseRole::Owner,
                },
                "local",
            )
            .await
            .unwrap();
        let second = service
            .attach(
                AttachRuntimeRequest {
                    session_id: session.clone(),
                    client_id: client,
                    role: RuntimeLeaseRole::Owner,
                },
                "local",
            )
            .await
            .unwrap();
        assert_eq!(first.lease_id, second.lease_id);
        assert_eq!(second.snapshot.attachment.unwrap().lease_counts.owner, 1);
        assert_eq!(backend.touches.lock().unwrap().len(), 2);
        clock.0.store(31, Ordering::SeqCst);
        service.sweep_once().await.unwrap();
        assert_eq!(
            service
                .snapshot(&session)
                .unwrap()
                .attachment
                .unwrap()
                .lease_counts
                .owner,
            0
        );
    }

    #[tokio::test]
    async fn event_cursor_reports_reset_after_ring_lag() {
        let session = VibexSessionId::new();
        let backend = Arc::new(FakeBackend::new(&session));
        let clock = Arc::new(ManualClock::default());
        let service = RuntimeLifecycleService::with_clock(
            backend.clone(),
            RuntimeLifecycleConfig {
                heartbeat: Duration::from_millis(10),
                client_ttl: Duration::from_millis(30),
                event_capacity: 2,
                event_batch_limit: 4,
                sweep_interval: Duration::from_secs(1),
            },
            clock,
        )
        .unwrap();
        for _ in 0..4 {
            service
                .publisher()
                .publish(&session, RuntimeEventKind::AttachmentUpdated, None, None);
        }
        let old = RuntimeEventCursor {
            stream_id: service.stream_id().clone(),
            sequence: 1,
        };
        assert!(
            service
                .events(&session, Some(&old), None)
                .unwrap()
                .reset_required
        );
        let wrong = RuntimeEventCursor {
            stream_id: RuntimeStreamId::new(),
            sequence: 4,
        };
        assert!(
            service
                .events(&session, Some(&wrong), None)
                .unwrap()
                .reset_required
        );
        let _ = backend;
    }

    #[tokio::test]
    async fn event_catch_up_cursor_advances_only_through_returned_page() {
        let session = VibexSessionId::new();
        let backend = Arc::new(FakeBackend::new(&session));
        let service = RuntimeLifecycleService::with_clock(
            backend,
            RuntimeLifecycleConfig {
                heartbeat: Duration::from_millis(10),
                client_ttl: Duration::from_millis(30),
                event_capacity: 8,
                event_batch_limit: 2,
                sweep_interval: Duration::from_secs(1),
            },
            Arc::new(ManualClock::default()),
        )
        .unwrap();
        let stream_id = service.stream_id();
        for _ in 0..4 {
            service
                .publisher()
                .publish(&session, RuntimeEventKind::AttachmentUpdated, None, None);
        }

        let first = service
            .events(
                &session,
                Some(&RuntimeEventCursor {
                    stream_id,
                    sequence: 0,
                }),
                None,
            )
            .unwrap();
        assert_eq!(first.events.len(), 2);
        assert_eq!(first.next_cursor.sequence, 2);
        let second = service
            .events(&session, Some(&first.next_cursor), None)
            .unwrap();
        assert_eq!(
            second
                .events
                .iter()
                .map(|event| event.cursor.sequence)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert_eq!(second.next_cursor.sequence, 4);
    }

    #[tokio::test]
    async fn role_change_scope_isolation_and_internal_guard_drop_are_exact() {
        let session = VibexSessionId::new();
        let backend = Arc::new(FakeBackend::new(&session));
        backend.set_attachment(&session);
        let service = Arc::new(
            RuntimeLifecycleService::with_clock(
                backend,
                RuntimeLifecycleConfig {
                    heartbeat: Duration::from_millis(10),
                    client_ttl: Duration::from_millis(30),
                    event_capacity: 16,
                    event_batch_limit: 8,
                    sweep_interval: Duration::from_secs(1),
                },
                Arc::new(ManualClock::default()),
            )
            .unwrap(),
        );
        let client = RuntimeClientId::new();
        service
            .attach(
                AttachRuntimeRequest {
                    session_id: session.clone(),
                    client_id: client.clone(),
                    role: RuntimeLeaseRole::Viewer,
                },
                "remote:one",
            )
            .await
            .unwrap();
        let owner = service
            .attach(
                AttachRuntimeRequest {
                    session_id: session.clone(),
                    client_id: client.clone(),
                    role: RuntimeLeaseRole::Owner,
                },
                "remote:one",
            )
            .await
            .unwrap();
        let attachment = owner.snapshot.attachment.unwrap();
        assert_eq!(attachment.lease_counts.viewer, 0);
        assert_eq!(attachment.lease_counts.owner, 1);

        let guard = service
            .acquire_internal(
                session.clone(),
                attachment_target(&attachment),
                RuntimeLeaseRole::BackgroundWorker,
                "test-worker",
            )
            .await
            .unwrap();
        assert_eq!(
            service
                .snapshot(&session)
                .unwrap()
                .attachment
                .unwrap()
                .lease_counts
                .background_worker,
            1
        );
        drop(guard);
        assert_eq!(
            service
                .snapshot(&session)
                .unwrap()
                .attachment
                .unwrap()
                .lease_counts
                .background_worker,
            0
        );

        assert!(
            !service
                .detach(
                    DetachRuntimeRequest {
                        session_id: session.clone(),
                        client_id: client.clone(),
                    },
                    "remote:two",
                )
                .await
                .unwrap()
                .released
        );
        assert_eq!(
            service
                .snapshot(&session)
                .unwrap()
                .attachment
                .unwrap()
                .lease_counts
                .owner,
            1
        );
        assert!(
            service
                .detach(
                    DetachRuntimeRequest {
                        session_id: session,
                        client_id: client,
                    },
                    "remote:one",
                )
                .await
                .unwrap()
                .released
        );
    }

    #[tokio::test]
    async fn stop_rejects_new_leases_and_restart_changes_stream_epoch() {
        let session = VibexSessionId::new();
        let backend = Arc::new(FakeBackend::new(&session));
        backend.set_attachment(&session);
        let service = Arc::new(
            RuntimeLifecycleService::with_clock(
                backend,
                RuntimeLifecycleConfig {
                    heartbeat: Duration::from_millis(10),
                    client_ttl: Duration::from_millis(30),
                    event_capacity: 8,
                    event_batch_limit: 4,
                    sweep_interval: Duration::from_secs(1),
                },
                Arc::new(ManualClock::default()),
            )
            .unwrap(),
        );
        service.start(&tokio::runtime::Handle::current()).unwrap();
        let first_stream = service.stream_id();
        service.stop().await.unwrap();
        let error = service
            .attach(request(session.clone(), RuntimeLeaseRole::Viewer), "local")
            .await
            .unwrap_err();
        assert_eq!(error.code, "runtime_lifecycle_stopping");
        let target = RuntimeLeaseTarget::Process(RuntimeProcessId::new());
        let error = service
            .acquire_internal(
                session,
                target,
                RuntimeLeaseRole::SwitchPreparation,
                "test-switch",
            )
            .await
            .err()
            .expect("stopped lifecycle must reject an internal lease");
        assert_eq!(error.code, "runtime_lifecycle_stopping");
        service.start(&tokio::runtime::Handle::current()).unwrap();
        assert_ne!(service.stream_id(), first_stream);
        service.stop().await.unwrap();
    }

    #[test]
    fn start_uses_explicit_runtime_handle_outside_runtime_context() {
        let session = VibexSessionId::new();
        let backend = Arc::new(FakeBackend::new(&session));
        let service = RuntimeLifecycleService::new(
            backend,
            RuntimeLifecycleConfig {
                sweep_interval: Duration::from_secs(1),
                ..RuntimeLifecycleConfig::default()
            },
        )
        .unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();

        service.start(runtime.handle()).unwrap();
        runtime.block_on(service.stop()).unwrap();
    }
}

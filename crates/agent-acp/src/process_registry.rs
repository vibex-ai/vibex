//! ACP process ownership and lifecycle registry.
//!
//! A process is a reusable transport resource, not a session or attachment.
//! This module deliberately does not know about native ACP session ids or
//! binding ids; those belong to the attachment layer that will consume the
//! registry in P2-04.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use tokio::sync::{Mutex as AsyncMutex, broadcast};
use uuid::Uuid;
use vibex_agent::{
    RuntimeMetricName, RuntimeMetricOperation, RuntimeMetricResult, RuntimeObservability,
};
use vibex_core::{
    AgentRuntimeRouteKey, ProviderProfileId, RuntimeAuthSource, VibexError, VibexResult,
    unix_timestamp_ms,
};

use crate::registry::CapabilitySupport;
use crate::spawn_config::{
    ProcessConfigStatus, ProcessConfigStatusEvent, ProcessSpawnConfigSnapshot,
};

const CRASH_BROADCAST_CAPACITY: usize = 32;
const PROCESS_INSTANCE_ID_PREFIX: &str = "acp-process-";

/// Canonical workspace identity used by process reuse decisions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkspaceScope(PathBuf);

impl WorkspaceScope {
    /// Canonicalizes an existing absolute workspace path.
    pub fn new(path: impl AsRef<Path>) -> VibexResult<Self> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(VibexError::validation(
                "acp_workspace_scope_relative",
                "ACP process workspace scope must be an absolute path",
            ));
        }
        let canonical = path.canonicalize().map_err(|error| {
            VibexError::validation(
                "acp_workspace_scope_missing",
                "ACP process workspace scope must exist",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        Ok(Self(canonical))
    }

    /// Builds a scope from a path already canonicalized by the runtime.
    ///
    /// This is kept crate-visible so tests and the runtime can avoid a second
    /// filesystem lookup after `resolve_workspace_cwd` has completed.
    pub(crate) fn from_canonical(path: PathBuf) -> VibexResult<Self> {
        if !path.is_absolute() {
            return Err(VibexError::validation(
                "acp_workspace_scope_relative",
                "ACP process workspace scope must be an absolute path",
            ));
        }
        Ok(Self(path))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// Process-level identity. Session, binding, and native session ids are
/// intentionally absent: those are attachment-level concerns.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProcessAcquireKey {
    pub route_key: AgentRuntimeRouteKey,
    pub auth_source: RuntimeAuthSource,
    pub auth_source_revision: i64,
    pub process_spawn_fingerprint: String,
    pub workspace_scope: WorkspaceScope,
}

impl ProcessAcquireKey {
    pub fn new(
        route_key: AgentRuntimeRouteKey,
        auth_source: RuntimeAuthSource,
        auth_source_revision: i64,
        process_spawn_fingerprint: impl Into<String>,
        workspace_scope: WorkspaceScope,
    ) -> VibexResult<Self> {
        let fingerprint = process_spawn_fingerprint.into();
        if auth_source_revision < 0 || fingerprint.trim().is_empty() {
            return Err(VibexError::validation(
                "acp_process_spawn_fingerprint_empty",
                "ACP process acquire keys require a spawn fingerprint",
            ));
        }
        Ok(Self {
            route_key,
            auth_source,
            auth_source_revision,
            process_spawn_fingerprint: fingerprint,
            workspace_scope,
        })
    }
}

/// Stable identity for one OS process instance. It is not a PID and must not
/// be used to infer the current durable binding after a restart.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AcpProcessInstanceId(String);

impl AcpProcessInstanceId {
    pub fn new() -> Self {
        Self(format!("{PROCESS_INSTANCE_ID_PREFIX}{}", Uuid::new_v4()))
    }

    pub(crate) fn from_opaque(value: impl Into<String>) -> VibexResult<Self> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > 256 || value.chars().any(|ch| ch.is_control()) {
            return Err(VibexError::validation(
                "acp_process_instance_id_invalid",
                "ACP process instance id must be non-empty and bounded",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for AcpProcessInstanceId {
    fn default() -> Self {
        Self::new()
    }
}

/// Runtime process state visible to snapshots and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpProcessStatus {
    Starting,
    Ready,
    Closing,
    Closed,
    Crashed,
}

/// Evidence kind for the second half of the safe multi-session gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiSessionEvidenceKind {
    RealManagedAdapter,
    Fixture,
    Mock,
    Unknown,
}

/// Evidence produced by an exact-version multi-session contract run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiSessionContractEvidence {
    pub compatibility_identity: String,
    pub evidence_kind: MultiSessionEvidenceKind,
    pub session_routing_verified: bool,
    pub crash_isolation_verified: bool,
}

impl MultiSessionContractEvidence {
    pub fn real(
        compatibility_identity: impl Into<String>,
        session_routing_verified: bool,
        crash_isolation_verified: bool,
    ) -> Self {
        Self {
            compatibility_identity: compatibility_identity.into(),
            evidence_kind: MultiSessionEvidenceKind::RealManagedAdapter,
            session_routing_verified,
            crash_isolation_verified,
        }
    }

    fn permits_reuse(&self, expected_identity: &str) -> bool {
        self.evidence_kind == MultiSessionEvidenceKind::RealManagedAdapter
            && self.compatibility_identity == expected_identity
            && self.session_routing_verified
            && self.crash_isolation_verified
    }
}

/// Explicit decision returned by the two-factor multi-session gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessReuseDecision {
    Dedicated { fallback_reason: Option<String> },
    Shared { compatibility_identity: String },
}

impl ProcessReuseDecision {
    pub fn allows_shared_process(&self) -> bool {
        matches!(self, Self::Shared { .. })
    }

    pub fn fallback_reason(&self) -> Option<&str> {
        match self {
            Self::Dedicated { fallback_reason } => fallback_reason.as_deref(),
            Self::Shared { .. } => None,
        }
    }
}

/// Applies the descriptor + real contract double gate. A profile feature or a
/// fixture report cannot make this return `Shared`.
pub fn decide_process_reuse(
    requested: bool,
    descriptor_support: Option<CapabilitySupport>,
    expected_compatibility_identity: Option<&str>,
    evidence: Option<&MultiSessionContractEvidence>,
) -> ProcessReuseDecision {
    if !requested {
        return ProcessReuseDecision::Dedicated {
            fallback_reason: None,
        };
    }
    let Some(CapabilitySupport::Supported) = descriptor_support else {
        return ProcessReuseDecision::Dedicated {
            fallback_reason: Some("acp_multi_session_descriptor_not_verified".to_string()),
        };
    };
    let Some(expected_identity) = expected_compatibility_identity else {
        return ProcessReuseDecision::Dedicated {
            fallback_reason: Some("acp_multi_session_identity_missing".to_string()),
        };
    };
    let Some(evidence) = evidence else {
        return ProcessReuseDecision::Dedicated {
            fallback_reason: Some("acp_multi_session_contract_missing".to_string()),
        };
    };
    if !evidence.permits_reuse(expected_identity) {
        return ProcessReuseDecision::Dedicated {
            fallback_reason: Some("acp_multi_session_contract_not_verified".to_string()),
        };
    }
    ProcessReuseDecision::Shared {
        compatibility_identity: expected_identity.to_string(),
    }
}

/// A bounded, provider-neutral crash notification. Native session ids and
/// payloads are deliberately not included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpProcessCrash {
    pub process_instance_id: AcpProcessInstanceId,
    pub code: String,
    pub recoverable: bool,
}

impl AcpProcessCrash {
    fn exited(process_instance_id: AcpProcessInstanceId) -> Self {
        Self {
            process_instance_id,
            code: "acp_process_exited".to_string(),
            recoverable: true,
        }
    }
}

/// Snapshot of process-owned state. It intentionally contains no PID, native
/// session id, prompt, environment value, or secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpProcessSnapshot {
    pub process_instance_id: AcpProcessInstanceId,
    pub process_spawn_fingerprint: String,
    pub process_config_status: Option<ProcessConfigStatus>,
    pub status: AcpProcessStatus,
    pub protocol_version: Option<i64>,
    pub attached_session_count: usize,
    pub pending_request_count: usize,
    pub last_used_at_ms: i64,
    pub reusable: bool,
}

/// Minimal contract implemented by the concrete runtime process. Keeping this
/// trait small lets the registry be tested without spawning a provider.
#[async_trait]
pub trait AcpProcessHandle: Send + Sync + 'static {
    fn is_closed(&self) -> bool;
    fn protocol_version(&self) -> Option<i64> {
        None
    }
    fn pending_request_count(&self) -> usize {
        0
    }
    async fn shutdown(&self);
}

struct RegisteredProcess<P> {
    key: ProcessAcquireKey,
    process: Arc<P>,
    spawn_config: Option<ProcessSpawnConfigSnapshot>,
    last_observed_fingerprint: Option<String>,
    config_status: Option<ProcessConfigStatus>,
    shutdown_lock: Arc<AsyncMutex<()>>,
    status: AcpProcessStatus,
    reusable: bool,
    attached_session_count: usize,
    crash_sender: broadcast::Sender<AcpProcessCrash>,
    crash_reported: bool,
    last_used_at_ms: i64,
}

struct RegistryState<P> {
    instances: HashMap<AcpProcessInstanceId, RegisteredProcess<P>>,
    reusable_ready: HashMap<ProcessAcquireKey, AcpProcessInstanceId>,
}

struct RegistryInner<P> {
    state: Mutex<RegistryState<P>>,
    acquire_locks: Mutex<HashMap<ProcessAcquireKey, Arc<AsyncMutex<()>>>>,
    config_status_sender: broadcast::Sender<ProcessConfigStatusEvent>,
    observability: Arc<RuntimeObservability>,
}

/// Process registry shared by the ACP runtime and the future attachment
/// registry.
pub struct AcpProcessRegistry<P: AcpProcessHandle> {
    inner: Arc<RegistryInner<P>>,
}

impl<P: AcpProcessHandle> Clone for AcpProcessRegistry<P> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<P: AcpProcessHandle> Default for AcpProcessRegistry<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: AcpProcessHandle> AcpProcessRegistry<P> {
    pub fn new() -> Self {
        Self::with_observability(Arc::new(RuntimeObservability::new()))
    }

    pub fn with_observability(observability: Arc<RuntimeObservability>) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                state: Mutex::new(RegistryState {
                    instances: HashMap::new(),
                    reusable_ready: HashMap::new(),
                }),
                acquire_locks: Mutex::new(HashMap::new()),
                config_status_sender: broadcast::channel(CRASH_BROADCAST_CAPACITY).0,
                observability,
            }),
        }
    }

    fn key_lock(&self, key: &ProcessAcquireKey) -> VibexResult<Arc<AsyncMutex<()>>> {
        let mut locks = self
            .inner
            .acquire_locks
            .lock()
            .map_err(|_| lock_poisoned("acquireLocks"))?;
        Ok(Arc::clone(
            locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        ))
    }

    fn maybe_drop_key_lock(&self, key: &ProcessAcquireKey, lock: &Arc<AsyncMutex<()>>) {
        let Ok(mut locks) = self.inner.acquire_locks.lock() else {
            return;
        };
        // One reference lives in the map and one is held by this caller. Any
        // additional strong reference is an active/waiting acquire.
        if Arc::strong_count(lock) == 2 {
            locks.remove(key);
        }
    }

    fn evict_reusable_ready(
        state: &mut RegistryState<P>,
        key: &ProcessAcquireKey,
        instance_id: &AcpProcessInstanceId,
    ) {
        if state.reusable_ready.get(key) == Some(instance_id) {
            state.reusable_ready.remove(key);
        }
    }

    fn validate_spawn_config(
        key: &ProcessAcquireKey,
        spawn_config: Option<&ProcessSpawnConfigSnapshot>,
    ) -> VibexResult<()> {
        if let Some(spawn_config) = spawn_config
            && spawn_config.process_spawn_fingerprint() != key.process_spawn_fingerprint
        {
            return Err(VibexError::validation(
                "acp_process_spawn_snapshot_mismatch",
                "ACP process snapshot does not match its acquire fingerprint",
            ));
        }
        Ok(())
    }

    /// Acquires a reusable process. The supplied callbacks run under the
    /// per-key lock, so initialization is part of the de-duplication window.
    pub async fn acquire_reusable<S, SFut, I, IFut>(
        &self,
        key: ProcessAcquireKey,
        spawn: S,
        initialize: I,
    ) -> VibexResult<ProcessLease<P>>
    where
        S: FnOnce(AcpProcessInstanceId) -> SFut,
        SFut: Future<Output = VibexResult<Arc<P>>>,
        I: FnOnce(Arc<P>) -> IFut,
        IFut: Future<Output = VibexResult<()>>,
    {
        self.acquire_reusable_with_snapshot(key, None, spawn, initialize)
            .await
    }

    /// Acquires a reusable process and records its immutable launch snapshot.
    pub async fn acquire_reusable_with_snapshot<S, SFut, I, IFut>(
        &self,
        key: ProcessAcquireKey,
        spawn_config: Option<ProcessSpawnConfigSnapshot>,
        spawn: S,
        initialize: I,
    ) -> VibexResult<ProcessLease<P>>
    where
        S: FnOnce(AcpProcessInstanceId) -> SFut,
        SFut: Future<Output = VibexResult<Arc<P>>>,
        I: FnOnce(Arc<P>) -> IFut,
        IFut: Future<Output = VibexResult<()>>,
    {
        Self::validate_spawn_config(&key, spawn_config.as_ref())?;
        let lock = self.key_lock(&key)?;
        let guard = lock.lock().await;

        if let Some(lease) = self.ready_for_key(&key)? {
            self.record_acquire(RuntimeMetricResult::Reused);
            self.inner.observability.increment(
                RuntimeMetricName::DuplicateAcquirePrevented,
                Some(RuntimeMetricOperation::Process),
                RuntimeMetricResult::Prevented,
            );
            drop(guard);
            self.maybe_drop_key_lock(&key, &lock);
            return Ok(lease);
        }

        let instance_id = AcpProcessInstanceId::new();
        let process = match spawn(instance_id.clone()).await {
            Ok(process) => process,
            Err(error) => {
                self.record_acquire(RuntimeMetricResult::Failure);
                drop(guard);
                self.maybe_drop_key_lock(&key, &lock);
                return Err(error);
            }
        };
        if let Err(error) = self.register_starting(
            key.clone(),
            instance_id.clone(),
            Arc::clone(&process),
            true,
            spawn_config,
        ) {
            self.cleanup_failed_start(&instance_id, &process).await;
            self.record_acquire(RuntimeMetricResult::Failure);
            drop(guard);
            self.maybe_drop_key_lock(&key, &lock);
            return Err(error);
        }
        if let Err(error) = initialize(Arc::clone(&process)).await {
            self.cleanup_failed_start(&instance_id, &process).await;
            self.record_acquire(RuntimeMetricResult::Failure);
            drop(guard);
            self.maybe_drop_key_lock(&key, &lock);
            return Err(error);
        }
        let lease = match self.mark_ready(&instance_id) {
            Ok(lease) => lease,
            Err(error) => {
                self.cleanup_failed_start(&instance_id, &process).await;
                self.record_acquire(RuntimeMetricResult::Failure);
                drop(guard);
                self.maybe_drop_key_lock(&key, &lock);
                return Err(error);
            }
        };
        drop(guard);
        self.maybe_drop_key_lock(&lease.key, &lock);
        self.record_acquire(RuntimeMetricResult::Created);
        Ok(lease)
    }

    /// Starts a dedicated process. It still uses the key lock to prevent two
    /// callers from executing spawn/initialize simultaneously for the same
    /// process boundary, but it never inserts the process into reusable lookup.
    pub async fn acquire_dedicated<S, SFut, I, IFut>(
        &self,
        key: ProcessAcquireKey,
        spawn: S,
        initialize: I,
    ) -> VibexResult<ProcessLease<P>>
    where
        S: FnOnce(AcpProcessInstanceId) -> SFut,
        SFut: Future<Output = VibexResult<Arc<P>>>,
        I: FnOnce(Arc<P>) -> IFut,
        IFut: Future<Output = VibexResult<()>>,
    {
        self.acquire_dedicated_with_snapshot(key, None, spawn, initialize)
            .await
    }

    /// Starts a dedicated process and records its immutable launch snapshot.
    pub async fn acquire_dedicated_with_snapshot<S, SFut, I, IFut>(
        &self,
        key: ProcessAcquireKey,
        spawn_config: Option<ProcessSpawnConfigSnapshot>,
        spawn: S,
        initialize: I,
    ) -> VibexResult<ProcessLease<P>>
    where
        S: FnOnce(AcpProcessInstanceId) -> SFut,
        SFut: Future<Output = VibexResult<Arc<P>>>,
        I: FnOnce(Arc<P>) -> IFut,
        IFut: Future<Output = VibexResult<()>>,
    {
        Self::validate_spawn_config(&key, spawn_config.as_ref())?;
        let lock = self.key_lock(&key)?;
        let guard = lock.lock().await;
        let instance_id = AcpProcessInstanceId::new();
        let process = match spawn(instance_id.clone()).await {
            Ok(process) => process,
            Err(error) => {
                self.record_acquire(RuntimeMetricResult::Failure);
                drop(guard);
                self.maybe_drop_key_lock(&key, &lock);
                return Err(error);
            }
        };
        if let Err(error) = self.register_starting(
            key.clone(),
            instance_id.clone(),
            Arc::clone(&process),
            false,
            spawn_config,
        ) {
            self.cleanup_failed_start(&instance_id, &process).await;
            self.record_acquire(RuntimeMetricResult::Failure);
            drop(guard);
            self.maybe_drop_key_lock(&key, &lock);
            return Err(error);
        }
        if let Err(error) = initialize(Arc::clone(&process)).await {
            self.cleanup_failed_start(&instance_id, &process).await;
            self.record_acquire(RuntimeMetricResult::Failure);
            drop(guard);
            self.maybe_drop_key_lock(&key, &lock);
            return Err(error);
        }
        let lease = match self.mark_ready(&instance_id) {
            Ok(lease) => lease,
            Err(error) => {
                self.cleanup_failed_start(&instance_id, &process).await;
                self.record_acquire(RuntimeMetricResult::Failure);
                drop(guard);
                self.maybe_drop_key_lock(&key, &lock);
                return Err(error);
            }
        };
        drop(guard);
        self.maybe_drop_key_lock(&lease.key, &lock);
        self.record_acquire(RuntimeMetricResult::Created);
        Ok(lease)
    }

    fn ready_for_key(&self, key: &ProcessAcquireKey) -> VibexResult<Option<ProcessLease<P>>> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| lock_poisoned("state"))?;
        let Some(instance_id) = state.reusable_ready.get(key).cloned() else {
            return Ok(None);
        };
        let Some(registered) = state.instances.get_mut(&instance_id) else {
            state.reusable_ready.remove(key);
            return Ok(None);
        };
        if registered.status != AcpProcessStatus::Ready || registered.process.is_closed() {
            let _ = registered;
            state.reusable_ready.remove(key);
            return Ok(None);
        }
        Ok(Some(ProcessLease {
            process_instance_id: instance_id,
            key: key.clone(),
            process: Arc::clone(&registered.process),
            registry: Arc::downgrade(&self.inner),
        }))
    }

    fn register_starting(
        &self,
        key: ProcessAcquireKey,
        instance_id: AcpProcessInstanceId,
        process: Arc<P>,
        reusable: bool,
        spawn_config: Option<ProcessSpawnConfigSnapshot>,
    ) -> VibexResult<()> {
        if process.is_closed() {
            return Err(VibexError::process(
                "acp_process_closed_before_ready",
                "ACP process closed before it became ready",
            ));
        }
        let (crash_sender, _) = broadcast::channel(CRASH_BROADCAST_CAPACITY);
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| lock_poisoned("state"))?;
        state.instances.insert(
            instance_id.clone(),
            RegisteredProcess {
                key: key.clone(),
                process,
                last_observed_fingerprint: spawn_config
                    .as_ref()
                    .map(ProcessSpawnConfigSnapshot::process_spawn_fingerprint),
                config_status: spawn_config.as_ref().map(|_| ProcessConfigStatus::Current),
                spawn_config,
                shutdown_lock: Arc::new(AsyncMutex::new(())),
                status: AcpProcessStatus::Starting,
                reusable,
                attached_session_count: 0,
                crash_sender,
                crash_reported: false,
                last_used_at_ms: unix_timestamp_ms(),
            },
        );
        Ok(())
    }

    async fn cleanup_failed_start(&self, instance_id: &AcpProcessInstanceId, process: &Arc<P>) {
        process.shutdown().await;
        let _ = self.remove(instance_id);
    }

    fn mark_ready(&self, instance_id: &AcpProcessInstanceId) -> VibexResult<ProcessLease<P>> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| lock_poisoned("state"))?;
        let (key, process, reusable) = {
            let registered = state.instances.get_mut(instance_id).ok_or_else(|| {
                VibexError::process(
                    "acp_process_instance_missing",
                    "ACP process instance disappeared during initialization",
                )
            })?;
            if registered.status == AcpProcessStatus::Crashed || registered.process.is_closed() {
                return Err(VibexError::process(
                    "acp_process_closed_before_ready",
                    "ACP process closed before it became ready",
                ));
            }
            registered.status = AcpProcessStatus::Ready;
            registered.last_used_at_ms = unix_timestamp_ms();
            (
                registered.key.clone(),
                Arc::clone(&registered.process),
                registered.reusable,
            )
        };
        if reusable {
            state
                .reusable_ready
                .insert(key.clone(), instance_id.clone());
        }
        Ok(ProcessLease {
            process_instance_id: instance_id.clone(),
            key,
            process,
            registry: Arc::downgrade(&self.inner),
        })
    }

    /// Marks an instance as attached to one session/attachment.
    pub fn attach(&self, instance_id: &AcpProcessInstanceId) -> VibexResult<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| lock_poisoned("state"))?;
        let registered = state.instances.get_mut(instance_id).ok_or_else(|| {
            VibexError::process(
                "acp_process_instance_missing",
                "ACP process instance is no longer registered",
            )
        })?;
        if registered.status != AcpProcessStatus::Ready || registered.process.is_closed() {
            return Err(VibexError::process(
                "acp_process_not_ready",
                "ACP process instance is not ready for an attachment",
            ));
        }
        if registered
            .config_status
            .is_some_and(|status| status != ProcessConfigStatus::Current)
        {
            return Err(VibexError::process(
                "acp_process_config_stale",
                "ACP process configuration is stale and cannot accept a new attachment",
            ));
        }
        registered.attached_session_count = registered.attached_session_count.saturating_add(1);
        registered.last_used_at_ms = unix_timestamp_ms();
        Ok(())
    }

    pub(crate) fn exit_reporter(
        &self,
        instance_id: AcpProcessInstanceId,
    ) -> ProcessExitReporter<P> {
        ProcessExitReporter {
            process_instance_id: instance_id,
            registry: Arc::downgrade(&self.inner),
        }
    }

    /// Removes one attachment without implicitly killing a reusable process.
    pub fn detach(&self, instance_id: &AcpProcessInstanceId) -> VibexResult<usize> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| lock_poisoned("state"))?;
        let registered = state.instances.get_mut(instance_id).ok_or_else(|| {
            VibexError::process(
                "acp_process_instance_missing",
                "ACP process instance is no longer registered",
            )
        })?;
        registered.attached_session_count = registered.attached_session_count.saturating_sub(1);
        registered.last_used_at_ms = unix_timestamp_ms();
        Ok(registered.attached_session_count)
    }

    /// Touches one exact live process for warm-cache accounting.
    pub fn touch(&self, instance_id: &AcpProcessInstanceId, now_ms: i64) -> VibexResult<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| lock_poisoned("state"))?;
        let registered = state.instances.get_mut(instance_id).ok_or_else(|| {
            VibexError::process(
                "acp_process_instance_missing",
                "ACP process instance is no longer registered",
            )
        })?;
        if !matches!(
            registered.status,
            AcpProcessStatus::Starting | AcpProcessStatus::Ready
        ) || registered.process.is_closed()
        {
            return Err(VibexError::process(
                "acp_process_not_ready",
                "ACP process instance is not live for a cache touch",
            ));
        }
        registered.last_used_at_ms = now_ms;
        Ok(())
    }

    /// Subscribes to crash events for an instance. P2-04 consumes this channel
    /// to fan structured failures into each attachment.
    pub fn subscribe_crashes(
        &self,
        instance_id: &AcpProcessInstanceId,
    ) -> VibexResult<broadcast::Receiver<AcpProcessCrash>> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| lock_poisoned("state"))?;
        let registered = state.instances.get(instance_id).ok_or_else(|| {
            VibexError::process(
                "acp_process_instance_missing",
                "ACP process instance is no longer registered",
            )
        })?;
        Ok(registered.crash_sender.subscribe())
    }

    /// Reports an unexpected process exit. Returns false for duplicate reports
    /// or an instance that has already reached a terminal state.
    pub fn report_crash(&self, instance_id: &AcpProcessInstanceId) -> VibexResult<bool> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| lock_poisoned("state"))?;
        let reusable_key = {
            let registered = state.instances.get_mut(instance_id).ok_or_else(|| {
                VibexError::process(
                    "acp_process_instance_missing",
                    "ACP process instance is no longer registered",
                )
            })?;
            if registered.crash_reported
                || matches!(
                    registered.status,
                    AcpProcessStatus::Closing
                        | AcpProcessStatus::Closed
                        | AcpProcessStatus::Crashed
                )
            {
                return Ok(false);
            }
            registered.crash_reported = true;
            registered.status = AcpProcessStatus::Crashed;
            let reusable_key = registered.reusable.then(|| registered.key.clone());
            let event = AcpProcessCrash::exited(instance_id.clone());
            let _ = registered.crash_sender.send(event);
            reusable_key
        };
        if let Some(key) = reusable_key {
            Self::evict_reusable_ready(&mut state, &key, instance_id);
        }
        self.inner.observability.increment(
            RuntimeMetricName::AdapterCrash,
            None,
            RuntimeMetricResult::Crashed,
        );
        Ok(true)
    }

    /// Returns a bounded process snapshot.
    pub fn snapshot(&self, instance_id: &AcpProcessInstanceId) -> VibexResult<AcpProcessSnapshot> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| lock_poisoned("state"))?;
        let registered = state.instances.get(instance_id).ok_or_else(|| {
            VibexError::process(
                "acp_process_instance_missing",
                "ACP process instance is no longer registered",
            )
        })?;
        Ok(snapshot_for(registered, instance_id))
    }

    /// Returns bounded snapshots for all registered process instances. The
    /// caller must still re-check a chosen instance with `claim_idle_close`.
    pub fn snapshots(&self) -> VibexResult<Vec<AcpProcessSnapshot>> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| lock_poisoned("state"))?;
        Ok(state
            .instances
            .iter()
            .map(|(instance_id, registered)| snapshot_for(registered, instance_id))
            .collect())
    }

    /// Resolves process ids for an exact credential source without exposing
    /// the process key or native session details outside the runtime layer.
    pub fn process_ids_for_auth_source(
        &self,
        auth_source: &RuntimeAuthSource,
    ) -> VibexResult<Vec<AcpProcessInstanceId>> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| lock_poisoned("state"))?;
        Ok(state
            .instances
            .iter()
            .filter(|(_, registered)| &registered.key.auth_source == auth_source)
            .map(|(instance_id, _)| instance_id.clone())
            .collect())
    }

    /// Atomically claims an idle process for shutdown. The claim removes the
    /// reusable lookup before any await, so a concurrent acquire cannot reuse
    /// a process that is already closing.
    pub fn claim_idle_close(
        &self,
        instance_id: &AcpProcessInstanceId,
        expected_last_used_at_ms: i64,
        now_ms: i64,
        idle_timeout_ms: i64,
        quota_victim: bool,
    ) -> VibexResult<Option<ProcessLease<P>>> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| lock_poisoned("state"))?;
        let (key, process) = {
            let Some(registered) = state.instances.get_mut(instance_id) else {
                return Ok(None);
            };
            if registered.last_used_at_ms != expected_last_used_at_ms {
                return Ok(None);
            }
            let pending = registered.process.pending_request_count();
            let idle = now_ms.saturating_sub(registered.last_used_at_ms) >= idle_timeout_ms;
            let stale = registered
                .config_status
                .is_some_and(|status| status != ProcessConfigStatus::Current);
            let should_close = registered.status == AcpProcessStatus::Ready
                && registered.attached_session_count == 0
                && pending == 0
                && !registered.process.is_closed()
                && (quota_victim || idle || stale);
            if !should_close {
                return Ok(None);
            }
            registered.status = AcpProcessStatus::Closing;
            (registered.key.clone(), Arc::clone(&registered.process))
        };
        Self::evict_reusable_ready(&mut state, &key, instance_id);
        Ok(Some(ProcessLease {
            process_instance_id: instance_id.clone(),
            key,
            process,
            registry: Arc::downgrade(&self.inner),
        }))
    }

    /// Subscribes to bounded process configuration status changes.
    pub fn subscribe_config_status(&self) -> broadcast::Receiver<ProcessConfigStatusEvent> {
        self.inner.config_status_sender.subscribe()
    }

    /// Returns immutable launch snapshots for live processes belonging to a
    /// profile. Runtime refresh uses these as the comparison baseline; the
    /// current observed snapshot is intentionally not persisted as identity.
    pub fn process_configs_for_profile(
        &self,
        provider_profile_id: &ProviderProfileId,
    ) -> VibexResult<
        Vec<(
            AcpProcessInstanceId,
            WorkspaceScope,
            ProcessSpawnConfigSnapshot,
        )>,
    > {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| lock_poisoned("state"))?;
        Ok(state
            .instances
            .iter()
            .filter_map(|(instance_id, registered)| {
                (registered.key.auth_source.provider_profile_id() == Some(provider_profile_id)
                    && matches!(
                        registered.status,
                        AcpProcessStatus::Starting | AcpProcessStatus::Ready
                    ))
                .then(|| {
                    registered.spawn_config.clone().map(|snapshot| {
                        (
                            instance_id.clone(),
                            registered.key.workspace_scope.clone(),
                            snapshot,
                        )
                    })
                })
                .flatten()
            })
            .collect())
    }

    /// Compares a newly rebuilt launch snapshot with the immutable process
    /// snapshot and emits at most one event for each observed fingerprint.
    /// Staleness never shuts down or mutates the concrete process.
    pub fn refresh_config_status(
        &self,
        instance_id: &AcpProcessInstanceId,
        current: ProcessSpawnConfigSnapshot,
    ) -> VibexResult<Option<ProcessConfigStatusEvent>> {
        let event = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| lock_poisoned("state"))?;
            let registered = state.instances.get_mut(instance_id).ok_or_else(|| {
                VibexError::process(
                    "acp_process_instance_missing",
                    "ACP process instance is no longer registered",
                )
            })?;
            let Some(launch) = registered.spawn_config.as_ref() else {
                // Legacy registry callers did not provide a snapshot. Adopt
                // the first observed value as the baseline without emitting a
                // misleading stale event.
                registered.spawn_config = Some(current.clone());
                registered.last_observed_fingerprint = Some(current.process_spawn_fingerprint());
                registered.config_status = Some(ProcessConfigStatus::Current);
                return Ok(None);
            };
            let launch_fingerprint = launch.process_spawn_fingerprint();
            let current_fingerprint = current.process_spawn_fingerprint();
            let changed = launch.diff(&current);
            let status = ProcessConfigStatus::from_diff(&changed, &BTreeSet::new());
            let previous_fingerprint = registered
                .last_observed_fingerprint
                .clone()
                .unwrap_or_else(|| launch_fingerprint.clone());
            let previous_status = registered
                .config_status
                .unwrap_or(ProcessConfigStatus::Current);
            if previous_fingerprint == current_fingerprint && previous_status == status {
                return Ok(None);
            }
            registered.last_observed_fingerprint = Some(current_fingerprint.clone());
            registered.config_status = Some(status);
            Some(ProcessConfigStatusEvent {
                process_instance_id: instance_id.as_str().to_string(),
                auth_source: registered.key.auth_source.clone(),
                auth_source_revision: registered.key.auth_source_revision,
                previous_status,
                status,
                previous_fingerprint,
                current_fingerprint,
                changed_fields: changed
                    .into_iter()
                    .map(|field| field.as_str().to_string())
                    .collect(),
            })
        };
        if let Some(event) = event.as_ref() {
            let _ = self.inner.config_status_sender.send(event.clone());
            if event.status != ProcessConfigStatus::Current {
                self.inner.observability.increment(
                    RuntimeMetricName::ConfigStale,
                    None,
                    RuntimeMetricResult::Stale,
                );
            }
        }
        Ok(event)
    }

    fn record_acquire(&self, result: RuntimeMetricResult) {
        self.inner.observability.increment(
            RuntimeMetricName::Acquire,
            Some(RuntimeMetricOperation::Process),
            result,
        );
    }

    /// Marks an instance as closed and removes it from future reusable lookup.
    /// The concrete process shutdown is awaited outside the registry state lock.
    pub async fn shutdown(&self, instance_id: &AcpProcessInstanceId) -> VibexResult<()> {
        let shutdown_lock = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| lock_poisoned("state"))?;
            let Some(registered) = state.instances.get(instance_id) else {
                return Ok(());
            };
            Arc::clone(&registered.shutdown_lock)
        };
        let _shutdown_guard = shutdown_lock.lock().await;
        let process = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| lock_poisoned("state"))?;
            let (key, process) = {
                let Some(registered) = state.instances.get_mut(instance_id) else {
                    return Ok(());
                };
                if registered.status == AcpProcessStatus::Closed {
                    return Ok(());
                }
                if registered.status != AcpProcessStatus::Crashed {
                    registered.status = AcpProcessStatus::Closing;
                }
                (registered.key.clone(), Arc::clone(&registered.process))
            };
            Self::evict_reusable_ready(&mut state, &key, instance_id);
            process
        };
        process.shutdown().await;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| lock_poisoned("state"))?;
        let reusable_key = state.instances.get_mut(instance_id).and_then(|registered| {
            if !registered.crash_reported {
                registered.status = AcpProcessStatus::Closed;
            }
            registered.reusable.then(|| registered.key.clone())
        });
        if let Some(key) = reusable_key {
            Self::evict_reusable_ready(&mut state, &key, instance_id);
        }
        Ok(())
    }

    /// Removes terminal entries after their process has been shut down. This
    /// is intentionally explicit so crash diagnostics can be consumed first.
    pub fn remove(&self, instance_id: &AcpProcessInstanceId) -> VibexResult<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| lock_poisoned("state"))?;
        let Some(registered) = state.instances.remove(instance_id) else {
            return Ok(());
        };
        Self::evict_reusable_ready(&mut state, &registered.key, instance_id);
        Ok(())
    }

    #[cfg(test)]
    fn ready_instance_for_key(&self, key: &ProcessAcquireKey) -> Option<AcpProcessInstanceId> {
        self.inner
            .state
            .lock()
            .ok()
            .and_then(|state| state.reusable_ready.get(key).cloned())
    }

    #[cfg(test)]
    fn acquire_lock_count(&self) -> usize {
        self.inner
            .acquire_locks
            .lock()
            .map(|locks| locks.len())
            .unwrap_or_default()
    }
}

/// Weak process-to-registry callback. A process may own this reporter without
/// creating an Arc cycle through the registry's instance map.
pub(crate) struct ProcessExitReporter<P: AcpProcessHandle> {
    process_instance_id: AcpProcessInstanceId,
    registry: Weak<RegistryInner<P>>,
}

impl<P: AcpProcessHandle> Clone for ProcessExitReporter<P> {
    fn clone(&self) -> Self {
        Self {
            process_instance_id: self.process_instance_id.clone(),
            registry: self.registry.clone(),
        }
    }
}

impl<P: AcpProcessHandle> ProcessExitReporter<P> {
    pub(crate) fn report_crash(&self) -> VibexResult<bool> {
        let Some(inner) = self.registry.upgrade() else {
            return Ok(false);
        };
        AcpProcessRegistry { inner }.report_crash(&self.process_instance_id)
    }
}

fn snapshot_for<P: AcpProcessHandle>(
    registered: &RegisteredProcess<P>,
    instance_id: &AcpProcessInstanceId,
) -> AcpProcessSnapshot {
    AcpProcessSnapshot {
        process_instance_id: instance_id.clone(),
        process_spawn_fingerprint: registered.key.process_spawn_fingerprint.clone(),
        process_config_status: registered.config_status,
        status: registered.status,
        protocol_version: registered.process.protocol_version(),
        attached_session_count: registered.attached_session_count,
        pending_request_count: registered.process.pending_request_count(),
        last_used_at_ms: registered.last_used_at_ms,
        reusable: registered.reusable,
    }
}

/// Handle returned by a successful process acquire.
pub struct ProcessLease<P: AcpProcessHandle> {
    process_instance_id: AcpProcessInstanceId,
    key: ProcessAcquireKey,
    process: Arc<P>,
    registry: Weak<RegistryInner<P>>,
}

impl<P: AcpProcessHandle> Clone for ProcessLease<P> {
    fn clone(&self) -> Self {
        Self {
            process_instance_id: self.process_instance_id.clone(),
            key: self.key.clone(),
            process: Arc::clone(&self.process),
            registry: self.registry.clone(),
        }
    }
}

impl<P: AcpProcessHandle> ProcessLease<P> {
    pub fn process(&self) -> Arc<P> {
        Arc::clone(&self.process)
    }

    pub fn process_instance_id(&self) -> &AcpProcessInstanceId {
        &self.process_instance_id
    }

    pub fn key(&self) -> &ProcessAcquireKey {
        &self.key
    }

    pub fn attach(&self) -> VibexResult<()> {
        let registry = AcpProcessRegistry {
            inner: self.registry.upgrade().ok_or_else(|| {
                VibexError::process(
                    "acp_process_registry_closed",
                    "ACP process registry is no longer available",
                )
            })?,
        };
        registry.attach(&self.process_instance_id)
    }

    pub fn detach(&self) -> VibexResult<usize> {
        let registry = AcpProcessRegistry {
            inner: self.registry.upgrade().ok_or_else(|| {
                VibexError::process(
                    "acp_process_registry_closed",
                    "ACP process registry is no longer available",
                )
            })?,
        };
        registry.detach(&self.process_instance_id)
    }

    pub fn subscribe_crashes(&self) -> VibexResult<broadcast::Receiver<AcpProcessCrash>> {
        let registry = AcpProcessRegistry {
            inner: self.registry.upgrade().ok_or_else(|| {
                VibexError::process(
                    "acp_process_registry_closed",
                    "ACP process registry is no longer available",
                )
            })?,
        };
        registry.subscribe_crashes(&self.process_instance_id)
    }

    pub fn snapshot(&self) -> VibexResult<AcpProcessSnapshot> {
        let registry = AcpProcessRegistry {
            inner: self.registry.upgrade().ok_or_else(|| {
                VibexError::process(
                    "acp_process_registry_closed",
                    "ACP process registry is no longer available",
                )
            })?,
        };
        registry.snapshot(&self.process_instance_id)
    }
}

fn lock_poisoned(scope: &str) -> VibexError {
    VibexError::process(
        "acp_process_registry_lock_poisoned",
        "ACP process registry state is unavailable",
    )
    .with_diagnostic("scope", scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use tempfile::TempDir;
    use tokio::time::{Duration, sleep};
    use vibex_core::{AcpAdapterId, AgentId, NativeStateHomeId, ProviderProfileId, TransportKind};

    #[derive(Default)]
    struct FakeProcess {
        closed: AtomicBool,
        shutdowns: AtomicUsize,
        shutdown_delay_ms: AtomicU64,
        pending: AtomicUsize,
    }

    #[async_trait]
    impl AcpProcessHandle for FakeProcess {
        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::SeqCst)
        }

        fn protocol_version(&self) -> Option<i64> {
            Some(1)
        }

        fn pending_request_count(&self) -> usize {
            self.pending.load(Ordering::SeqCst)
        }

        async fn shutdown(&self) {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
            let delay_ms = self.shutdown_delay_ms.load(Ordering::SeqCst);
            if delay_ms > 0 {
                sleep(Duration::from_millis(delay_ms)).await;
            }
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    fn key(temp: &TempDir, fingerprint: &str) -> ProcessAcquireKey {
        ProcessAcquireKey::new(
            AgentRuntimeRouteKey {
                agent_id: AgentId::parse("opencode").unwrap(),
                transport_kind: TransportKind::Acp,
                adapter_id: AcpAdapterId::parse("opencode-acp").unwrap(),
            },
            RuntimeAuthSource::provider_profile(
                ProviderProfileId::parse("provider_profile").unwrap(),
            ),
            1,
            fingerprint,
            WorkspaceScope::new(temp.path()).unwrap(),
        )
        .unwrap()
    }

    fn spawn_snapshot() -> ProcessSpawnConfigSnapshot {
        ProcessSpawnConfigSnapshot {
            agent_id: AgentId::parse("opencode").unwrap(),
            adapter_id: AcpAdapterId::parse("opencode-acp").unwrap(),
            adapter_version: "1.0.0".to_string(),
            adapter_binary_identity: "command:opencode".to_string(),
            auth_source: RuntimeAuthSource::provider_profile(
                ProviderProfileId::parse("provider_profile").unwrap(),
            ),
            auth_source_revision: 1,
            process_config_revision: 0,
            command: "opencode".to_string(),
            args: Vec::new(),
            cwd_policy: "{workspaceRoot}".to_string(),
            base_url: Some("https://one.example.test".to_string()),
            model_provider_id: None,
            non_secret_env: BTreeMap::new(),
            env_unsets: BTreeSet::new(),
            secret_reference_versions: BTreeMap::new(),
            mcp_revision: None,
            skills_revision: None,
            native_state_home_id: NativeStateHomeId::parse("statehome_registry").unwrap(),
        }
        .with_content_revision()
    }

    #[test]
    fn key_contains_only_process_scope_identity() {
        let temp = tempfile::tempdir().unwrap();
        let other_workspace = tempfile::tempdir().unwrap();
        let one = key(&temp, "fp-a");
        let same = key(&temp, "fp-a");
        assert_eq!(one, same);
        assert_ne!(one, key(&temp, "fp-b"));
        assert_ne!(one, key(&other_workspace, "fp-a"));

        let mut other_route = one.clone();
        other_route.route_key.adapter_id = AcpAdapterId::parse("other-acp").unwrap();
        assert_ne!(one, other_route);

        let mut other_source = one.clone();
        other_source.auth_source = RuntimeAuthSource::provider_profile(
            ProviderProfileId::parse("provider_other_profile").unwrap(),
        );
        assert_ne!(one, other_source);
        assert!(!format!("{one:?}").contains("session_id"));
        assert!(!format!("{one:?}").contains("binding_id"));
        assert!(!format!("{one:?}").contains("native_session_id"));
    }

    #[test]
    fn reuse_requires_descriptor_and_real_contract_evidence() {
        let identity = "adapter=test@1.0.0";
        assert_eq!(
            decide_process_reuse(
                false,
                Some(CapabilitySupport::Supported),
                Some(identity),
                None,
            ),
            ProcessReuseDecision::Dedicated {
                fallback_reason: None
            }
        );
        for descriptor_support in [
            None,
            Some(CapabilitySupport::Unknown),
            Some(CapabilitySupport::Unsupported),
        ] {
            assert_eq!(
                decide_process_reuse(
                    true,
                    descriptor_support,
                    Some(identity),
                    Some(&MultiSessionContractEvidence::real(identity, true, true)),
                )
                .fallback_reason(),
                Some("acp_multi_session_descriptor_not_verified")
            );
        }
        assert_eq!(
            decide_process_reuse(true, Some(CapabilitySupport::Supported), None, None,)
                .fallback_reason(),
            Some("acp_multi_session_identity_missing")
        );
        assert_eq!(
            decide_process_reuse(
                true,
                Some(CapabilitySupport::Supported),
                Some(identity),
                None
            )
            .fallback_reason(),
            Some("acp_multi_session_contract_missing")
        );
        for evidence in [
            MultiSessionContractEvidence {
                compatibility_identity: identity.to_string(),
                evidence_kind: MultiSessionEvidenceKind::Fixture,
                session_routing_verified: true,
                crash_isolation_verified: true,
            },
            MultiSessionContractEvidence {
                compatibility_identity: identity.to_string(),
                evidence_kind: MultiSessionEvidenceKind::Mock,
                session_routing_verified: true,
                crash_isolation_verified: true,
            },
            MultiSessionContractEvidence::real("adapter=other@1.0.0", true, true),
            MultiSessionContractEvidence::real(identity, false, true),
            MultiSessionContractEvidence::real(identity, true, false),
        ] {
            assert_eq!(
                decide_process_reuse(
                    true,
                    Some(CapabilitySupport::Supported),
                    Some(identity),
                    Some(&evidence),
                )
                .fallback_reason(),
                Some("acp_multi_session_contract_not_verified")
            );
        }
        assert!(
            decide_process_reuse(
                true,
                Some(CapabilitySupport::Supported),
                Some(identity),
                Some(&MultiSessionContractEvidence::real(identity, true, true)),
            )
            .allows_shared_process()
        );
    }

    #[tokio::test]
    async fn same_reusable_key_spawns_and_initializes_once() {
        let temp = tempfile::tempdir().unwrap();
        let key = key(&temp, "fp");
        let registry = AcpProcessRegistry::<FakeProcess>::new();
        let spawns = Arc::new(AtomicUsize::new(0));
        let initializes = Arc::new(AtomicUsize::new(0));
        let spawn = |spawns: Arc<AtomicUsize>| async move {
            spawns.fetch_add(1, Ordering::SeqCst);
            sleep(Duration::from_millis(10)).await;
            Ok(Arc::new(FakeProcess::default()))
        };
        let left = {
            let registry = registry.clone();
            let key = key.clone();
            let spawns = Arc::clone(&spawns);
            let initializes = Arc::clone(&initializes);
            tokio::spawn(async move {
                registry
                    .acquire_reusable(
                        key,
                        move |_| spawn(spawns),
                        move |_| {
                            initializes.fetch_add(1, Ordering::SeqCst);
                            async { Ok(()) }
                        },
                    )
                    .await
                    .unwrap()
            })
        };
        let right = {
            let registry = registry.clone();
            let key = key.clone();
            let spawns = Arc::clone(&spawns);
            let initializes = Arc::clone(&initializes);
            tokio::spawn(async move {
                registry
                    .acquire_reusable(
                        key,
                        move |_| spawn(spawns),
                        move |_| {
                            initializes.fetch_add(1, Ordering::SeqCst);
                            async { Ok(()) }
                        },
                    )
                    .await
                    .unwrap()
            })
        };
        let left = left.await.unwrap();
        let right = right.await.unwrap();
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        assert_eq!(initializes.load(Ordering::SeqCst), 1);
        assert_eq!(left.process_instance_id(), right.process_instance_id());
        assert_eq!(
            registry.ready_instance_for_key(&key),
            Some(left.process_instance_id().clone())
        );
    }

    #[tokio::test]
    async fn different_keys_initialize_concurrently() {
        let temp = tempfile::tempdir().unwrap();
        let registry = AcpProcessRegistry::<FakeProcess>::new();
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let acquire = |key: ProcessAcquireKey| {
            let registry = registry.clone();
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            tokio::spawn(async move {
                registry
                    .acquire_reusable(
                        key,
                        |_| async { Ok(Arc::new(FakeProcess::default())) },
                        move |_| async move {
                            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                            maximum.fetch_max(current, Ordering::SeqCst);
                            sleep(Duration::from_millis(25)).await;
                            active.fetch_sub(1, Ordering::SeqCst);
                            Ok(())
                        },
                    )
                    .await
                    .unwrap()
            })
        };
        let left = acquire(key(&temp, "fp-a"));
        let right = acquire(key(&temp, "fp-b"));
        left.await.unwrap();
        right.await.unwrap();
        assert_eq!(maximum.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn shutdown_is_idempotent_and_snapshot_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let registry = AcpProcessRegistry::<FakeProcess>::new();
        let lease = registry
            .acquire_dedicated(
                key(&temp, "fp-safe"),
                |_| async { Ok(Arc::new(FakeProcess::default())) },
                |_| async { Ok(()) },
            )
            .await
            .unwrap();
        lease.attach().unwrap();
        let process = lease.process();
        process.shutdown_delay_ms.store(25, Ordering::SeqCst);
        let instance_id = lease.process_instance_id().clone();
        let (left, right) = tokio::join!(
            registry.shutdown(&instance_id),
            registry.shutdown(&instance_id)
        );
        left.unwrap();
        right.unwrap();
        registry.shutdown(&instance_id).await.unwrap();
        let snapshot = registry.snapshot(&instance_id).unwrap();
        assert_eq!(snapshot.status, AcpProcessStatus::Closed);
        assert_eq!(snapshot.process_spawn_fingerprint, "fp-safe");
        assert_eq!(snapshot.attached_session_count, 1);
        assert_eq!(snapshot.protocol_version, Some(1));
        assert_eq!(process.shutdowns.load(Ordering::SeqCst), 1);
        registry.remove(&instance_id).unwrap();
        registry.shutdown(&instance_id).await.unwrap();
        let rendered = format!("{snapshot:?}");
        assert!(!rendered.contains("native_session"));
        assert!(!rendered.contains("prompt"));
        assert!(!rendered.contains("secret"));
    }

    #[tokio::test]
    async fn stale_refresh_is_idempotent_revertible_and_does_not_shutdown() {
        let temp = tempfile::tempdir().unwrap();
        let registry = AcpProcessRegistry::<FakeProcess>::new();
        let launch = spawn_snapshot();
        let key = ProcessAcquireKey::new(
            AgentRuntimeRouteKey {
                agent_id: AgentId::parse("opencode").unwrap(),
                transport_kind: TransportKind::Acp,
                adapter_id: AcpAdapterId::parse("opencode-acp").unwrap(),
            },
            RuntimeAuthSource::provider_profile(
                ProviderProfileId::parse("provider_profile").unwrap(),
            ),
            1,
            launch.process_spawn_fingerprint(),
            WorkspaceScope::new(temp.path()).unwrap(),
        )
        .unwrap();
        let lease = registry
            .acquire_dedicated_with_snapshot(
                key,
                Some(launch.clone()),
                |_| async { Ok(Arc::new(FakeProcess::default())) },
                |_| async { Ok(()) },
            )
            .await
            .unwrap();
        lease.attach().unwrap();
        let process = lease.process();
        let mut events = registry.subscribe_config_status();

        let mut changed = launch.clone();
        changed.base_url = Some("https://two.example.test".to_string());
        changed = changed.with_content_revision();
        let event = registry
            .refresh_config_status(lease.process_instance_id(), changed.clone())
            .unwrap()
            .unwrap();
        assert_eq!(event.status, ProcessConfigStatus::StaleRestartRequired);
        assert_eq!(events.try_recv().unwrap(), event);
        assert_eq!(process.shutdowns.load(Ordering::SeqCst), 0);
        assert_eq!(
            lease.snapshot().unwrap().process_config_status,
            Some(ProcessConfigStatus::StaleRestartRequired)
        );
        assert_eq!(lease.attach().unwrap_err().code, "acp_process_config_stale");
        assert!(
            registry
                .refresh_config_status(lease.process_instance_id(), changed)
                .unwrap()
                .is_none()
        );

        assert_eq!(lease.detach().unwrap(), 0);
        let stale_candidate = lease.snapshot().unwrap();
        let reverted = registry
            .refresh_config_status(lease.process_instance_id(), launch)
            .unwrap()
            .unwrap();
        assert_eq!(reverted.status, ProcessConfigStatus::Current);
        assert_eq!(events.try_recv().unwrap(), reverted);
        assert_eq!(process.shutdowns.load(Ordering::SeqCst), 0);
        assert!(
            registry
                .claim_idle_close(
                    lease.process_instance_id(),
                    stale_candidate.last_used_at_ms,
                    stale_candidate.last_used_at_ms.saturating_add(1),
                    1_000,
                    false,
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(lease.snapshot().unwrap().attached_session_count, 0);
    }

    #[tokio::test]
    async fn acquire_rejects_snapshot_and_key_fingerprint_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        let registry = AcpProcessRegistry::<FakeProcess>::new();
        let snapshot = spawn_snapshot();
        let error = registry
            .acquire_dedicated_with_snapshot(
                key(&temp, "different-fingerprint"),
                Some(snapshot),
                |_| async { Ok(Arc::new(FakeProcess::default())) },
                |_| async { Ok(()) },
            )
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, "acp_process_spawn_snapshot_mismatch");
    }

    #[tokio::test]
    async fn dedicated_acquires_never_share_ready_instance() {
        let temp = tempfile::tempdir().unwrap();
        let key = key(&temp, "fp");
        let registry = AcpProcessRegistry::<FakeProcess>::new();
        let one = registry
            .acquire_dedicated(
                key.clone(),
                |_| async { Ok(Arc::new(FakeProcess::default())) },
                |_| async { Ok(()) },
            )
            .await
            .unwrap();
        let two = registry
            .acquire_dedicated(
                key,
                |_| async { Ok(Arc::new(FakeProcess::default())) },
                |_| async { Ok(()) },
            )
            .await
            .unwrap();
        assert_ne!(one.process_instance_id(), two.process_instance_id());
    }

    #[tokio::test]
    async fn spawn_failure_leaves_no_state_and_allows_retry() {
        let temp = tempfile::tempdir().unwrap();
        let key = key(&temp, "fp");
        let registry = AcpProcessRegistry::<FakeProcess>::new();
        let result = registry
            .acquire_reusable(
                key.clone(),
                |_| async { Err(VibexError::process("test_spawn_failed", "spawn failed")) },
                |_| async { Ok(()) },
            )
            .await;
        assert!(result.is_err());
        assert_eq!(registry.ready_instance_for_key(&key), None);
        assert_eq!(registry.acquire_lock_count(), 0);

        let retry = registry
            .acquire_reusable(
                key,
                |_| async { Ok(Arc::new(FakeProcess::default())) },
                |_| async { Ok(()) },
            )
            .await
            .unwrap();
        assert_eq!(retry.snapshot().unwrap().status, AcpProcessStatus::Ready);
    }

    #[tokio::test]
    async fn initialize_failure_shuts_process_and_allows_retry() {
        let temp = tempfile::tempdir().unwrap();
        let key = key(&temp, "fp");
        let registry = AcpProcessRegistry::<FakeProcess>::new();
        let process = Arc::new(FakeProcess::default());
        let first = Arc::clone(&process);
        let result = registry
            .acquire_reusable(
                key.clone(),
                move |_| {
                    let process = Arc::clone(&first);
                    async move { Ok(process) }
                },
                |_| async {
                    Err(VibexError::provider(
                        "test_initialize_failed",
                        "initialize failed",
                    ))
                },
            )
            .await;
        assert!(result.is_err());
        assert_eq!(process.shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(registry.ready_instance_for_key(&key), None);
        assert_eq!(registry.acquire_lock_count(), 0);
        let lease = registry
            .acquire_reusable(
                key,
                |_| async { Ok(Arc::new(FakeProcess::default())) },
                |_| async { Ok(()) },
            )
            .await
            .unwrap();
        assert_eq!(lease.snapshot().unwrap().status, AcpProcessStatus::Ready);
    }

    #[tokio::test]
    async fn late_old_instance_crash_and_close_do_not_evict_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let key = key(&temp, "fp");
        let registry = AcpProcessRegistry::<FakeProcess>::new();
        let old = registry
            .acquire_reusable(
                key.clone(),
                |_| async { Ok(Arc::new(FakeProcess::default())) },
                |_| async { Ok(()) },
            )
            .await
            .unwrap();
        let old_process = old.process();
        old_process.closed.store(true, Ordering::SeqCst);
        let mut crash = old.subscribe_crashes().unwrap();

        let replacement = registry
            .acquire_reusable(
                key.clone(),
                |_| async { Ok(Arc::new(FakeProcess::default())) },
                |_| async { Ok(()) },
            )
            .await
            .unwrap();
        assert_ne!(old.process_instance_id(), replacement.process_instance_id());
        assert!(registry.report_crash(old.process_instance_id()).unwrap());
        assert_eq!(
            crash.recv().await.unwrap().process_instance_id,
            old.process_instance_id().clone()
        );
        assert_eq!(
            registry.ready_instance_for_key(&key),
            Some(replacement.process_instance_id().clone())
        );

        registry.shutdown(old.process_instance_id()).await.unwrap();
        registry.remove(old.process_instance_id()).unwrap();
        assert_eq!(
            registry.ready_instance_for_key(&key),
            Some(replacement.process_instance_id().clone())
        );
    }

    #[tokio::test]
    async fn closed_process_during_registration_or_ready_is_cleaned_up() {
        let temp = tempfile::tempdir().unwrap();
        let key = key(&temp, "fp");
        let registry = AcpProcessRegistry::<FakeProcess>::new();

        let closed_before_registration = Arc::new(FakeProcess::default());
        closed_before_registration
            .closed
            .store(true, Ordering::SeqCst);
        let first = Arc::clone(&closed_before_registration);
        assert!(
            registry
                .acquire_reusable(
                    key.clone(),
                    move |_| async move { Ok(first) },
                    |_| async { Ok(()) },
                )
                .await
                .is_err()
        );
        assert_eq!(
            closed_before_registration.shutdowns.load(Ordering::SeqCst),
            1
        );

        let closed_before_ready = Arc::new(FakeProcess::default());
        let spawned = Arc::clone(&closed_before_ready);
        let initialized = Arc::clone(&closed_before_ready);
        assert!(
            registry
                .acquire_reusable(
                    key.clone(),
                    move |_| async move { Ok(spawned) },
                    move |_| async move {
                        initialized.closed.store(true, Ordering::SeqCst);
                        Ok(())
                    },
                )
                .await
                .is_err()
        );
        assert_eq!(closed_before_ready.shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(registry.ready_instance_for_key(&key), None);
        assert_eq!(registry.acquire_lock_count(), 0);

        let retry = registry
            .acquire_reusable(
                key,
                |_| async { Ok(Arc::new(FakeProcess::default())) },
                |_| async { Ok(()) },
            )
            .await
            .unwrap();
        assert_eq!(retry.snapshot().unwrap().status, AcpProcessStatus::Ready);
    }

    #[tokio::test]
    async fn idle_close_claim_rechecks_attachments_pending_requests_and_exact_instance() {
        let temp = tempfile::tempdir().unwrap();
        let key = key(&temp, "fp-idle-close");
        let registry = AcpProcessRegistry::<FakeProcess>::new();
        let lease = registry
            .acquire_reusable(
                key.clone(),
                |_| async { Ok(Arc::new(FakeProcess::default())) },
                |_| async { Ok(()) },
            )
            .await
            .unwrap();
        lease.attach().unwrap();
        let snapshot = lease.snapshot().unwrap();
        let now = snapshot.last_used_at_ms.saturating_add(100);
        assert!(
            registry
                .claim_idle_close(
                    lease.process_instance_id(),
                    snapshot.last_used_at_ms,
                    now,
                    50,
                    true,
                )
                .unwrap()
                .is_none()
        );

        assert_eq!(lease.detach().unwrap(), 0);
        let detached = lease.snapshot().unwrap();
        let process = lease.process();
        process.pending.store(1, Ordering::SeqCst);
        assert!(
            registry
                .claim_idle_close(
                    lease.process_instance_id(),
                    detached.last_used_at_ms,
                    now,
                    50,
                    true,
                )
                .unwrap()
                .is_none()
        );
        process.pending.store(0, Ordering::SeqCst);
        registry
            .touch(lease.process_instance_id(), now.saturating_sub(50))
            .unwrap();
        let candidate = lease.snapshot().unwrap();
        registry
            .touch(lease.process_instance_id(), now.saturating_sub(49))
            .unwrap();
        assert!(
            registry
                .claim_idle_close(
                    lease.process_instance_id(),
                    candidate.last_used_at_ms,
                    now,
                    50,
                    true,
                )
                .unwrap()
                .is_none()
        );
        let refreshed = lease.snapshot().unwrap();
        let claimed = registry
            .claim_idle_close(
                lease.process_instance_id(),
                refreshed.last_used_at_ms,
                now.saturating_add(1),
                50,
                false,
            )
            .unwrap()
            .expect("idle reusable process should be claimed exactly once");
        assert_eq!(
            registry
                .snapshot(lease.process_instance_id())
                .unwrap()
                .status,
            AcpProcessStatus::Closing
        );
        assert_eq!(registry.ready_instance_for_key(&key), None);
        assert!(
            registry
                .claim_idle_close(
                    lease.process_instance_id(),
                    refreshed.last_used_at_ms,
                    now.saturating_add(1),
                    50,
                    true,
                )
                .unwrap()
                .is_none()
        );
        claimed.process().shutdown().await;
        registry.remove(lease.process_instance_id()).unwrap();
        assert_eq!(process.shutdowns.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn crash_is_broadcast_once_and_evicts_ready_instance() {
        let temp = tempfile::tempdir().unwrap();
        let key = key(&temp, "fp");
        let registry = AcpProcessRegistry::<FakeProcess>::new();
        let lease = registry
            .acquire_reusable(
                key.clone(),
                |_| async { Ok(Arc::new(FakeProcess::default())) },
                |_| async { Ok(()) },
            )
            .await
            .unwrap();
        lease.attach().unwrap();
        lease.attach().unwrap();
        let mut left = lease.subscribe_crashes().unwrap();
        let mut right = lease.subscribe_crashes().unwrap();
        assert!(registry.report_crash(lease.process_instance_id()).unwrap());
        assert!(!registry.report_crash(lease.process_instance_id()).unwrap());
        let left = left.recv().await.unwrap();
        let right = right.recv().await.unwrap();
        assert_eq!(left, right);
        assert_eq!(left.code, "acp_process_exited");
        assert_eq!(lease.snapshot().unwrap().status, AcpProcessStatus::Crashed);
        assert_eq!(registry.ready_instance_for_key(&key), None);
        assert_eq!(lease.detach().unwrap(), 1);
        assert_eq!(lease.detach().unwrap(), 0);

        let replacement = registry
            .acquire_reusable(
                key,
                |_| async { Ok(Arc::new(FakeProcess::default())) },
                |_| async { Ok(()) },
            )
            .await
            .unwrap();
        assert_ne!(
            replacement.process_instance_id(),
            lease.process_instance_id()
        );
    }
}

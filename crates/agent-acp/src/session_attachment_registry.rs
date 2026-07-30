//! ACP session-attachment ownership and native-session routing.
//!
//! A process is a transport resource; an attachment is the session-scoped
//! resource that may write timeline, permission, or config state.  This module
//! keeps those identities separate and makes the event fence explicit before
//! the concrete ACP event normalizer runs.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, Weak};

use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;
use vibex_agent::{
    RuntimeMetricName, RuntimeMetricOperation, RuntimeMetricResult, RuntimeObservability,
};
use vibex_core::{RuntimeBindingId, VibexError, VibexResult, VibexSessionId, unix_timestamp_ms};

use crate::process_registry::AcpProcessInstanceId;

const NATIVE_ID_HASH_BYTES: usize = 8;
const DIAGNOSTIC_METHOD_LIMIT: usize = 80;

/// The identity used to de-duplicate one attachment operation.
///
/// The native id is optional while `session/new` is in flight.  It is still
/// retained in the key when a caller is restoring an existing native session.
/// Native ids are intentionally redacted from `Debug` output.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SessionAttachmentAcquireKey {
    pub binding_id: RuntimeBindingId,
    pub native_session_id: Option<String>,
    pub process_instance_id: AcpProcessInstanceId,
    pub activation_generation: u64,
}

impl fmt::Debug for SessionAttachmentAcquireKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionAttachmentAcquireKey")
            .field("binding_id", &self.binding_id)
            .field(
                "native_session_id",
                &self.native_session_id.as_deref().map(redact_native_id),
            )
            .field("process_instance_id", &self.process_instance_id)
            .field("activation_generation", &self.activation_generation)
            .finish()
    }
}

impl SessionAttachmentAcquireKey {
    pub fn new(
        binding_id: RuntimeBindingId,
        native_session_id: Option<String>,
        process_instance_id: AcpProcessInstanceId,
        activation_generation: u64,
    ) -> VibexResult<Self> {
        let native_session_id = normalize_native_session_id(native_session_id)?;
        Ok(Self {
            binding_id,
            native_session_id,
            process_instance_id,
            activation_generation,
        })
    }

    fn validate(&self) -> VibexResult<()> {
        if let Some(native_session_id) = self.native_session_id.as_deref()
            && native_session_id.trim().is_empty()
        {
            return Err(VibexError::validation(
                "acp_attachment_native_session_id_empty",
                "ACP attachment native session id must not be empty",
            ));
        }
        Ok(())
    }
}

/// The exact four-field fence attached to a routed event or operation.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SessionAttachmentEventFence {
    pub binding_id: RuntimeBindingId,
    pub activation_generation: u64,
    pub process_instance_id: AcpProcessInstanceId,
    pub native_session_id: String,
}

impl fmt::Debug for SessionAttachmentEventFence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionAttachmentEventFence")
            .field("binding_id", &self.binding_id)
            .field("activation_generation", &self.activation_generation)
            .field("process_instance_id", &self.process_instance_id)
            .field(
                "native_session_id",
                &redact_native_id(&self.native_session_id),
            )
            .finish()
    }
}

impl SessionAttachmentEventFence {
    fn from_key(key: &SessionAttachmentAcquireKey, native_session_id: String) -> Self {
        Self {
            binding_id: key.binding_id.clone(),
            activation_generation: key.activation_generation,
            process_instance_id: key.process_instance_id.clone(),
            native_session_id,
        }
    }
}

/// Lifecycle state used by the routing fence.  Snapshot/lease projections are
/// deliberately left to the later runtime lifecycle task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAttachmentState {
    Preparing,
    Prepared,
    Committed,
    Inactive,
    Crashed,
    Closing,
    Closed,
}

impl SessionAttachmentState {
    fn is_live(self) -> bool {
        matches!(self, Self::Preparing | Self::Prepared | Self::Committed)
    }
}

/// Why an inbound event was not delivered to a live timeline attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAttachmentRouteRejection {
    MissingNativeSessionId,
    EmptyNativeSessionId,
    UnknownNativeSessionId,
    NativeSessionRouteConflict,
    FenceMismatch,
    AttachmentNotCurrent,
    AttachmentPrepared,
    AttachmentInactive,
}

impl SessionAttachmentRouteRejection {
    pub fn code(self) -> &'static str {
        match self {
            Self::MissingNativeSessionId => "acp_event_session_id_missing",
            Self::EmptyNativeSessionId => "acp_event_session_id_empty",
            Self::UnknownNativeSessionId => "acp_event_session_route_unknown",
            Self::NativeSessionRouteConflict => "acp_native_session_route_conflict",
            Self::FenceMismatch => "acp_event_fence_stale",
            Self::AttachmentNotCurrent => "acp_attachment_not_current",
            Self::AttachmentPrepared => "acp_event_attachment_prepared",
            Self::AttachmentInactive => "acp_event_attachment_inactive",
        }
    }
}

/// Bounded process-level diagnostic returned for an event that cannot be
/// safely associated with a Logical Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAttachmentRouteDiagnostic {
    pub code: String,
    pub rejection: SessionAttachmentRouteRejection,
    pub process_instance_id: AcpProcessInstanceId,
    pub native_session_hash: Option<String>,
    pub method: Option<String>,
}

impl SessionAttachmentRouteDiagnostic {
    fn new(
        process_instance_id: AcpProcessInstanceId,
        rejection: SessionAttachmentRouteRejection,
        native_session_id: Option<&str>,
        method: Option<&str>,
    ) -> Self {
        Self {
            code: rejection.code().to_string(),
            rejection,
            process_instance_id,
            native_session_hash: native_session_id.map(hash_native_id),
            method: method.map(|method| method.chars().take(DIAGNOSTIC_METHOD_LIMIT).collect()),
        }
    }
}

/// Payload returned by an attachment side-effect closure.
pub struct SessionAttachmentAcquireOutput<A> {
    pub native_session_id: String,
    pub payload: A,
}

/// Result of a de-duplicated attachment acquire.
pub enum SessionAttachmentAcquireResult<A> {
    Existing(SessionAttachmentHandle<A>),
    Created(SessionAttachmentHandle<A>),
}

impl<A> fmt::Debug for SessionAttachmentAcquireResult<A> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Existing(handle) => formatter.debug_tuple("Existing").field(handle).finish(),
            Self::Created(handle) => formatter.debug_tuple("Created").field(handle).finish(),
        }
    }
}

impl<A> SessionAttachmentAcquireResult<A> {
    pub fn handle(&self) -> &SessionAttachmentHandle<A> {
        match self {
            Self::Existing(handle) | Self::Created(handle) => handle,
        }
    }

    pub fn is_existing(&self) -> bool {
        matches!(self, Self::Existing(_))
    }
}

/// Result of routing an ACP event before event normalization.
pub enum SessionAttachmentRoute<A> {
    Deliver(SessionAttachmentHandle<A>),
    Quarantine(SessionAttachmentHandle<A>),
    Diagnostic(SessionAttachmentRouteDiagnostic),
}

impl<A> SessionAttachmentRoute<A> {
    pub fn diagnostic(&self) -> Option<&SessionAttachmentRouteDiagnostic> {
        match self {
            Self::Diagnostic(diagnostic) => Some(diagnostic),
            Self::Deliver(_) | Self::Quarantine(_) => None,
        }
    }
}

struct AttachmentRecord<A> {
    session_id: VibexSessionId,
    acquire_key: SessionAttachmentAcquireKey,
    fence: SessionAttachmentEventFence,
    payload: Arc<A>,
    prompt_lock: Arc<AsyncMutex<()>>,
    prompt_active: bool,
    prompt_closed: bool,
    state: SessionAttachmentState,
    created_at_ms: i64,
    last_used_at_ms: i64,
}

struct NativeRouteKey {
    process_instance_id: AcpProcessInstanceId,
    native_session_id: String,
}

impl PartialEq for NativeRouteKey {
    fn eq(&self, other: &Self) -> bool {
        self.process_instance_id == other.process_instance_id
            && self.native_session_id == other.native_session_id
    }
}

impl Eq for NativeRouteKey {}

impl std::hash::Hash for NativeRouteKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.process_instance_id.hash(state);
        self.native_session_id.hash(state);
    }
}

struct RegistryState<A> {
    attachments: HashMap<RuntimeBindingId, AttachmentRecord<A>>,
    native_routes: HashMap<NativeRouteKey, RuntimeBindingId>,
    current_by_session: HashMap<VibexSessionId, RuntimeBindingId>,
    last_generation_by_session: HashMap<VibexSessionId, u64>,
}

struct RegistryInner<A> {
    state: Mutex<RegistryState<A>>,
    acquire_locks: Mutex<HashMap<SessionAttachmentAcquireKey, Weak<AsyncMutex<()>>>>,
    observability: Arc<RuntimeObservability>,
}

struct AcquireLockEntryGuard<A> {
    registry: Weak<RegistryInner<A>>,
    key: SessionAttachmentAcquireKey,
    lock: Arc<AsyncMutex<()>>,
}

impl<A> Drop for AcquireLockEntryGuard<A> {
    fn drop(&mut self) {
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        let Ok(mut locks) = registry.acquire_locks.lock() else {
            return;
        };
        let candidate = Arc::downgrade(&self.lock);
        if Arc::strong_count(&self.lock) == 1
            && locks
                .get(&self.key)
                .is_some_and(|current| current.ptr_eq(&candidate))
        {
            locks.remove(&self.key);
        }
    }
}

/// Session-scoped registry layered above [`AcpProcessRegistry`].
pub struct SessionAttachmentRegistry<A> {
    inner: Arc<RegistryInner<A>>,
}

impl<A> Clone for SessionAttachmentRegistry<A> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<A> Default for SessionAttachmentRegistry<A> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A> SessionAttachmentRegistry<A> {
    pub fn new() -> Self {
        Self::with_observability(Arc::new(RuntimeObservability::new()))
    }

    pub fn with_observability(observability: Arc<RuntimeObservability>) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                state: Mutex::new(RegistryState {
                    attachments: HashMap::new(),
                    native_routes: HashMap::new(),
                    current_by_session: HashMap::new(),
                    last_generation_by_session: HashMap::new(),
                }),
                acquire_locks: Mutex::new(HashMap::new()),
                observability,
            }),
        }
    }

    fn key_lock(&self, key: &SessionAttachmentAcquireKey) -> VibexResult<Arc<AsyncMutex<()>>> {
        let mut locks = self
            .inner
            .acquire_locks
            .lock()
            .map_err(|_| registry_lock_poisoned("acquireLocks"))?;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
            return Ok(lock);
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(key.clone(), Arc::downgrade(&lock));
        Ok(lock)
    }

    #[cfg(test)]
    fn acquire_lock_count(&self) -> usize {
        self.inner
            .acquire_locks
            .lock()
            .map(|locks| locks.len())
            .unwrap_or_default()
    }

    /// Runs one attachment side effect under the per-key lock. The closure is
    /// never called when an exact live attachment already exists.
    pub async fn acquire<F, Fut>(
        &self,
        session_id: VibexSessionId,
        key: SessionAttachmentAcquireKey,
        operation: F,
    ) -> VibexResult<SessionAttachmentAcquireResult<A>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = VibexResult<SessionAttachmentAcquireOutput<A>>>,
    {
        key.validate()?;
        let lock_entry = AcquireLockEntryGuard {
            registry: Arc::downgrade(&self.inner),
            key: key.clone(),
            lock: self.key_lock(&key)?,
        };
        let guard = Arc::clone(&lock_entry.lock).lock_owned().await;
        let result = async {
            if let Some(existing) = self.existing_for_key(&session_id, &key)? {
                return Ok(SessionAttachmentAcquireResult::Existing(existing));
            }

            let output = operation().await?;
            let native_session_id = normalize_native_session_id(Some(output.native_session_id))?
                .ok_or_else(|| {
                    VibexError::validation(
                        "acp_attachment_native_session_id_empty",
                        "ACP attachment operation returned an empty native session id",
                    )
                })?;
            if let Some(expected) = key.native_session_id.as_deref()
                && expected != native_session_id
            {
                return Err(VibexError::conflict(
                    "acp_attachment_native_session_mismatch",
                    "ACP attachment operation returned a different native session id",
                ));
            }

            self.register_created(session_id, key.clone(), native_session_id, output.payload)
        }
        .await;
        drop(guard);
        drop(lock_entry);
        let metric_result = match result.as_ref() {
            Ok(result) if result.is_existing() => RuntimeMetricResult::Reused,
            Ok(_) => RuntimeMetricResult::Created,
            Err(_) => RuntimeMetricResult::Failure,
        };
        self.inner.observability.increment(
            RuntimeMetricName::Acquire,
            Some(RuntimeMetricOperation::Attachment),
            metric_result,
        );
        if metric_result == RuntimeMetricResult::Reused {
            self.inner.observability.increment(
                RuntimeMetricName::DuplicateAcquirePrevented,
                Some(RuntimeMetricOperation::Attachment),
                RuntimeMetricResult::Prevented,
            );
        }
        result
    }

    fn existing_for_key(
        &self,
        session_id: &VibexSessionId,
        key: &SessionAttachmentAcquireKey,
    ) -> VibexResult<Option<SessionAttachmentHandle<A>>> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let Some(existing) = state.attachments.get(&key.binding_id) else {
            return Ok(None);
        };
        if existing.session_id != *session_id {
            return Err(VibexError::conflict(
                "acp_attachment_binding_session_conflict",
                "ACP attachment binding is already owned by another session",
            ));
        }
        if existing.acquire_key == *key {
            if existing.state.is_live() {
                return Ok(Some(
                    self.handle_for_record(key.binding_id.clone(), existing),
                ));
            }
            return Ok(None);
        }
        if existing.state.is_live() {
            return Err(VibexError::conflict(
                "acp_attachment_key_conflict",
                "ACP binding already has a live attachment with a different acquire key",
            ));
        }
        remove_record_locked(&mut state, &key.binding_id);
        Ok(None)
    }

    fn register_created(
        &self,
        session_id: VibexSessionId,
        key: SessionAttachmentAcquireKey,
        native_session_id: String,
        payload: A,
    ) -> VibexResult<SessionAttachmentAcquireResult<A>> {
        let fence = SessionAttachmentEventFence::from_key(&key, native_session_id.clone());
        let route_key = NativeRouteKey {
            process_instance_id: key.process_instance_id.clone(),
            native_session_id,
        };
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;

        if let Some(existing_binding) = state.native_routes.get(&route_key)
            && existing_binding != &key.binding_id
        {
            return Err(VibexError::conflict(
                "acp_native_session_route_conflict",
                "ACP native session route is already owned by another binding",
            ));
        }
        if let Some(existing) = state.attachments.get(&key.binding_id) {
            if existing.state.is_live() {
                return Err(VibexError::conflict(
                    "acp_attachment_key_conflict",
                    "ACP binding already has a live attachment",
                ));
            }
            remove_record_locked(&mut state, &key.binding_id);
        }

        state
            .last_generation_by_session
            .entry(session_id.clone())
            .and_modify(|generation| *generation = (*generation).max(key.activation_generation))
            .or_insert(key.activation_generation);
        state
            .native_routes
            .insert(route_key, key.binding_id.clone());
        state.attachments.insert(
            key.binding_id.clone(),
            AttachmentRecord {
                session_id,
                acquire_key: key.clone(),
                fence,
                payload: Arc::new(payload),
                prompt_lock: Arc::new(AsyncMutex::new(())),
                prompt_active: false,
                prompt_closed: false,
                state: SessionAttachmentState::Prepared,
                created_at_ms: unix_timestamp_ms(),
                last_used_at_ms: unix_timestamp_ms(),
            },
        );
        let handle = self.handle_for_binding_locked(&state, key.binding_id)?;
        Ok(SessionAttachmentAcquireResult::Created(handle))
    }

    /// Activates a prepared attachment as the committed current attachment.
    pub fn activate(
        &self,
        expected: &SessionAttachmentEventFence,
    ) -> VibexResult<SessionAttachmentHandle<A>> {
        self.activate_with_prompt_closed(expected, false)
    }

    /// Activates a prepared attachment and applies its initial prompt-gate
    /// state under the same registry lock as current-pointer replacement.
    pub fn activate_with_prompt_closed(
        &self,
        expected: &SessionAttachmentEventFence,
        prompt_closed: bool,
    ) -> VibexResult<SessionAttachmentHandle<A>> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let (session_id, current_state) = {
            let record = state.attachments.get(&expected.binding_id).ok_or_else(|| {
                VibexError::conflict(
                    "acp_attachment_missing",
                    "ACP attachment is no longer registered",
                )
            })?;
            if record.fence != *expected {
                return Err(VibexError::conflict(
                    "acp_attachment_fence_stale",
                    "ACP attachment activation fence is stale",
                ));
            }
            (record.session_id.clone(), record.state)
        };
        if !matches!(
            current_state,
            SessionAttachmentState::Prepared | SessionAttachmentState::Committed
        ) {
            return Err(VibexError::conflict(
                "acp_attachment_not_activatable",
                "ACP attachment is not in a prepared or committed state",
            ));
        }

        if let Some(current_binding_id) = state.current_by_session.get(&session_id).cloned() {
            if current_binding_id == expected.binding_id {
                if let Some(record) = state.attachments.get_mut(&expected.binding_id) {
                    record.state = SessionAttachmentState::Committed;
                    record.prompt_closed = prompt_closed;
                    record.last_used_at_ms = unix_timestamp_ms();
                }
                return self.handle_for_binding_locked(&state, expected.binding_id.clone());
            }
            let current_generation = state
                .attachments
                .get(&current_binding_id)
                .map(|record| record.fence.activation_generation)
                .unwrap_or_default();
            if expected.activation_generation <= current_generation {
                return Err(VibexError::conflict(
                    "acp_attachment_generation_regression",
                    "ACP attachment activation generation is not newer than the current attachment",
                ));
            }
            if let Some(previous) = state.attachments.get_mut(&current_binding_id) {
                previous.state = SessionAttachmentState::Inactive;
                previous.prompt_active = false;
                previous.prompt_closed = false;
            }
        } else if state
            .last_generation_by_session
            .get(&session_id)
            .is_some_and(|generation| *generation > expected.activation_generation)
        {
            return Err(VibexError::conflict(
                "acp_attachment_generation_regression",
                "ACP attachment activation generation is older than a previous activation",
            ));
        }

        state
            .current_by_session
            .insert(session_id.clone(), expected.binding_id.clone());
        state
            .last_generation_by_session
            .entry(session_id)
            .and_modify(|generation| {
                *generation = (*generation).max(expected.activation_generation)
            })
            .or_insert(expected.activation_generation);
        if let Some(record) = state.attachments.get_mut(&expected.binding_id) {
            record.state = SessionAttachmentState::Committed;
            record.prompt_closed = prompt_closed;
            record.last_used_at_ms = unix_timestamp_ms();
        }
        self.handle_for_binding_locked(&state, expected.binding_id.clone())
    }

    /// Advances the fence for a committed in-place mutation without changing
    /// process or native-session ownership. Old handles become stale
    /// immediately and cannot route events or acquire prompts.
    pub fn advance_generation(
        &self,
        expected: &SessionAttachmentEventFence,
        next_generation: u64,
    ) -> VibexResult<SessionAttachmentHandle<A>> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let (session_id, next_fence) = {
            let record = state.attachments.get(&expected.binding_id).ok_or_else(|| {
                VibexError::conflict(
                    "acp_attachment_missing",
                    "ACP attachment is no longer registered",
                )
            })?;
            if record.fence != *expected
                || record.state != SessionAttachmentState::Committed
                || record.prompt_active
                || state.current_by_session.get(&record.session_id) != Some(&expected.binding_id)
            {
                return Err(VibexError::conflict(
                    "acp_attachment_generation_fence_stale",
                    "ACP attachment cannot advance a stale or active generation fence",
                ));
            }
            if expected.activation_generation.checked_add(1) != Some(next_generation) {
                return Err(VibexError::conflict(
                    "acp_attachment_generation_advance_invalid",
                    "ACP attachment generation must advance exactly once",
                ));
            }
            (
                record.session_id.clone(),
                SessionAttachmentEventFence {
                    binding_id: expected.binding_id.clone(),
                    activation_generation: next_generation,
                    process_instance_id: expected.process_instance_id.clone(),
                    native_session_id: expected.native_session_id.clone(),
                },
            )
        };
        let record = state
            .attachments
            .get_mut(&expected.binding_id)
            .expect("attachment checked above");
        record.acquire_key.activation_generation = next_generation;
        record.fence = next_fence;
        record.last_used_at_ms = unix_timestamp_ms();
        state
            .last_generation_by_session
            .insert(session_id, next_generation);
        self.handle_for_binding_locked(&state, expected.binding_id.clone())
    }

    /// Routes an event using the process/native pair from its ACP envelope.
    pub fn route(
        &self,
        process_instance_id: &AcpProcessInstanceId,
        native_session_id: Option<&str>,
        method: Option<&str>,
    ) -> SessionAttachmentRoute<A> {
        let Some(native_session_id) = native_session_id else {
            return SessionAttachmentRoute::Diagnostic(SessionAttachmentRouteDiagnostic::new(
                process_instance_id.clone(),
                SessionAttachmentRouteRejection::MissingNativeSessionId,
                None,
                method,
            ));
        };
        if native_session_id.trim().is_empty() {
            return SessionAttachmentRoute::Diagnostic(SessionAttachmentRouteDiagnostic::new(
                process_instance_id.clone(),
                SessionAttachmentRouteRejection::EmptyNativeSessionId,
                Some(native_session_id),
                method,
            ));
        }
        let Ok(state) = self.inner.state.lock() else {
            return SessionAttachmentRoute::Diagnostic(SessionAttachmentRouteDiagnostic::new(
                process_instance_id.clone(),
                SessionAttachmentRouteRejection::UnknownNativeSessionId,
                Some(native_session_id),
                method,
            ));
        };
        let route_key = NativeRouteKey {
            process_instance_id: process_instance_id.clone(),
            native_session_id: native_session_id.to_string(),
        };
        let Some(binding_id) = state.native_routes.get(&route_key).cloned() else {
            return SessionAttachmentRoute::Diagnostic(SessionAttachmentRouteDiagnostic::new(
                process_instance_id.clone(),
                SessionAttachmentRouteRejection::UnknownNativeSessionId,
                Some(native_session_id),
                method,
            ));
        };
        self.route_binding_locked(&state, binding_id, None, method)
    }

    /// Routes an event while checking an explicitly supplied fence. This is
    /// useful to callers that carry a binding/generation alongside a queued
    /// event and makes each fence component testable independently.
    pub fn route_fenced(
        &self,
        expected: &SessionAttachmentEventFence,
        method: Option<&str>,
    ) -> SessionAttachmentRoute<A> {
        let Ok(state) = self.inner.state.lock() else {
            return SessionAttachmentRoute::Diagnostic(SessionAttachmentRouteDiagnostic::new(
                expected.process_instance_id.clone(),
                SessionAttachmentRouteRejection::FenceMismatch,
                Some(&expected.native_session_id),
                method,
            ));
        };
        let route_key = NativeRouteKey {
            process_instance_id: expected.process_instance_id.clone(),
            native_session_id: expected.native_session_id.clone(),
        };
        let Some(binding_id) = state.native_routes.get(&route_key).cloned() else {
            return SessionAttachmentRoute::Diagnostic(SessionAttachmentRouteDiagnostic::new(
                expected.process_instance_id.clone(),
                SessionAttachmentRouteRejection::UnknownNativeSessionId,
                Some(&expected.native_session_id),
                method,
            ));
        };
        self.route_binding_locked(&state, binding_id, Some(expected), method)
    }

    fn route_binding_locked(
        &self,
        state: &RegistryState<A>,
        binding_id: RuntimeBindingId,
        expected: Option<&SessionAttachmentEventFence>,
        method: Option<&str>,
    ) -> SessionAttachmentRoute<A> {
        let Some(record) = state.attachments.get(&binding_id) else {
            return SessionAttachmentRoute::Diagnostic(SessionAttachmentRouteDiagnostic::new(
                expected
                    .map(|fence| fence.process_instance_id.clone())
                    .unwrap_or_default(),
                SessionAttachmentRouteRejection::UnknownNativeSessionId,
                expected.map(|fence| fence.native_session_id.as_str()),
                method,
            ));
        };
        if expected.is_some_and(|expected| record.fence != *expected) {
            return SessionAttachmentRoute::Diagnostic(SessionAttachmentRouteDiagnostic::new(
                record.fence.process_instance_id.clone(),
                SessionAttachmentRouteRejection::FenceMismatch,
                Some(&record.fence.native_session_id),
                method,
            ));
        }
        if matches!(
            record.state,
            SessionAttachmentState::Prepared | SessionAttachmentState::Preparing
        ) {
            return SessionAttachmentRoute::Quarantine(self.handle_for_record(binding_id, record));
        }
        if state.current_by_session.get(&record.session_id) != Some(&binding_id) {
            return SessionAttachmentRoute::Diagnostic(SessionAttachmentRouteDiagnostic::new(
                record.fence.process_instance_id.clone(),
                SessionAttachmentRouteRejection::AttachmentNotCurrent,
                Some(&record.fence.native_session_id),
                method,
            ));
        }
        let handle = self.handle_for_record(binding_id, record);
        match record.state {
            SessionAttachmentState::Committed => SessionAttachmentRoute::Deliver(handle),
            SessionAttachmentState::Prepared | SessionAttachmentState::Preparing => {
                unreachable!("prepared attachments return before the current-attachment check")
            }
            SessionAttachmentState::Inactive
            | SessionAttachmentState::Crashed
            | SessionAttachmentState::Closing
            | SessionAttachmentState::Closed => {
                SessionAttachmentRoute::Diagnostic(SessionAttachmentRouteDiagnostic::new(
                    record.fence.process_instance_id.clone(),
                    SessionAttachmentRouteRejection::AttachmentInactive,
                    Some(&record.fence.native_session_id),
                    method,
                ))
            }
        }
    }

    /// Returns the committed attachment for a Logical Session, if one exists.
    pub fn current(
        &self,
        session_id: &VibexSessionId,
    ) -> VibexResult<Option<SessionAttachmentHandle<A>>> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let Some(binding_id) = state.current_by_session.get(session_id).cloned() else {
            return Ok(None);
        };
        let handle = self.handle_for_binding_locked(&state, binding_id)?;
        if let Some(record) = state.attachments.get_mut(&handle.binding_id) {
            record.last_used_at_ms = unix_timestamp_ms();
        }
        Ok(Some(handle))
    }

    /// Returns a registered attachment by binding regardless of lifecycle
    /// state. Runtime cleanup uses this to detach a crashed attachment before
    /// rebuilding the same durable binding on a new process instance.
    pub fn attachment(
        &self,
        binding_id: &RuntimeBindingId,
    ) -> VibexResult<Option<SessionAttachmentHandle<A>>> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        Ok(state
            .attachments
            .get(binding_id)
            .map(|record| self.handle_for_record(binding_id.clone(), record)))
    }

    /// Returns lightweight handles for all registered attachments. Callers
    /// must treat the result as a candidate list: every handle needs an exact
    /// fence/state recheck before cleanup because replacement may race the
    /// enumeration.
    pub fn attachments(&self) -> VibexResult<Vec<SessionAttachmentHandle<A>>> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        Ok(state
            .attachments
            .iter()
            .map(|(binding_id, record)| self.handle_for_record(binding_id.clone(), record))
            .collect())
    }

    /// Touches an exact fence for warm-cache accounting. A stale fence is a
    /// no-op error rather than a reason to update a replacement attachment.
    pub fn touch(&self, expected: &SessionAttachmentEventFence, now_ms: i64) -> VibexResult<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let record = state
            .attachments
            .get_mut(&expected.binding_id)
            .ok_or_else(|| {
                VibexError::conflict(
                    "acp_attachment_missing",
                    "ACP attachment is no longer registered",
                )
            })?;
        if record.fence != *expected {
            return Err(VibexError::conflict(
                "acp_attachment_fence_stale",
                "ACP attachment touch fence is stale",
            ));
        }
        record.last_used_at_ms = now_ms;
        Ok(())
    }

    /// Returns the registry-owned usage timestamps for an exact fence.
    pub fn usage_timestamps(
        &self,
        expected: &SessionAttachmentEventFence,
    ) -> VibexResult<(i64, i64)> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let record = state.attachments.get(&expected.binding_id).ok_or_else(|| {
            VibexError::conflict(
                "acp_attachment_missing",
                "ACP attachment is no longer registered",
            )
        })?;
        if record.fence != *expected {
            return Err(VibexError::conflict(
                "acp_attachment_fence_stale",
                "ACP attachment timestamp fence is stale",
            ));
        }
        Ok((record.created_at_ms, record.last_used_at_ms))
    }

    /// Atomically claims an exact attachment for cleanup. The route and
    /// current pointer are removed while the record remains Closing until the
    /// caller finishes transport cleanup. A replacement with a different
    /// fence cannot be affected by the returned handle.
    pub fn claim_close(
        &self,
        expected: &SessionAttachmentEventFence,
    ) -> VibexResult<Option<SessionAttachmentHandle<A>>> {
        self.claim_close_if(expected, |_| true)
    }

    /// Variant of [`claim_close`] that re-checks provider-owned state while
    /// the exact registry fence is still held. Returning false leaves the
    /// attachment untouched and lets the next sweep retry.
    pub fn claim_close_if<F>(
        &self,
        expected: &SessionAttachmentEventFence,
        predicate: F,
    ) -> VibexResult<Option<SessionAttachmentHandle<A>>>
    where
        F: FnOnce(&A) -> bool,
    {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let Some(record) = state.attachments.get(&expected.binding_id) else {
            return Ok(None);
        };
        if record.fence != *expected {
            return Err(VibexError::conflict(
                "acp_attachment_fence_stale",
                "ACP attachment close fence is stale",
            ));
        }
        if matches!(
            record.state,
            SessionAttachmentState::Closing | SessionAttachmentState::Closed
        ) {
            return Ok(None);
        }
        if !predicate(record.payload.as_ref()) {
            return Ok(None);
        }
        let session_id = record.session_id.clone();
        let handle = self.handle_for_record(expected.binding_id.clone(), record);
        if let Some(record) = state.attachments.get_mut(&expected.binding_id) {
            record.state = SessionAttachmentState::Closing;
            record.prompt_active = false;
            record.prompt_closed = false;
        }
        if state.current_by_session.get(&session_id) == Some(&expected.binding_id) {
            state.current_by_session.remove(&session_id);
        }
        let route_key = NativeRouteKey {
            process_instance_id: expected.process_instance_id.clone(),
            native_session_id: expected.native_session_id.clone(),
        };
        if state.native_routes.get(&route_key) == Some(&expected.binding_id) {
            state.native_routes.remove(&route_key);
        }
        Ok(Some(handle))
    }

    /// Atomically revalidates an idle-sweep candidate and claims it for
    /// cleanup. `predicate` runs under the short registry lock with the exact
    /// payload, current-pointer state and freshly computed idle result.
    pub fn claim_idle_close_if<F>(
        &self,
        expected: &SessionAttachmentEventFence,
        expected_last_used_at_ms: i64,
        now_ms: i64,
        idle_timeout_ms: i64,
        predicate: F,
    ) -> VibexResult<Option<SessionAttachmentHandle<A>>>
    where
        F: FnOnce(&A, bool, bool) -> bool,
    {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let Some(record) = state.attachments.get(&expected.binding_id) else {
            return Ok(None);
        };
        if record.fence != *expected {
            return Err(VibexError::conflict(
                "acp_attachment_fence_stale",
                "ACP attachment idle-close fence is stale",
            ));
        }
        if record.last_used_at_ms != expected_last_used_at_ms {
            return Ok(None);
        }
        let is_current =
            state.current_by_session.get(&record.session_id) == Some(&expected.binding_id);
        if !matches!(
            (record.state, is_current),
            (SessionAttachmentState::Committed, true) | (SessionAttachmentState::Inactive, false)
        ) {
            return Ok(None);
        }
        let idle = now_ms.saturating_sub(record.last_used_at_ms) >= idle_timeout_ms;
        if !predicate(record.payload.as_ref(), is_current, idle) {
            return Ok(None);
        }
        let session_id = record.session_id.clone();
        let handle = self.handle_for_record(expected.binding_id.clone(), record);
        if let Some(record) = state.attachments.get_mut(&expected.binding_id) {
            record.state = SessionAttachmentState::Closing;
            record.prompt_active = false;
            record.prompt_closed = false;
        }
        if is_current {
            state.current_by_session.remove(&session_id);
        }
        let route_key = NativeRouteKey {
            process_instance_id: expected.process_instance_id.clone(),
            native_session_id: expected.native_session_id.clone(),
        };
        if state.native_routes.get(&route_key) == Some(&expected.binding_id) {
            state.native_routes.remove(&route_key);
        }
        Ok(Some(handle))
    }

    /// Runs a synchronous attachment-local update only while the supplied
    /// fence is still the committed current attachment. The closure executes
    /// under the short registry state lock and therefore must not block or
    /// perform async I/O.
    pub fn apply_current<R, F>(
        &self,
        expected: &SessionAttachmentEventFence,
        operation: F,
    ) -> VibexResult<R>
    where
        F: FnOnce(&A) -> R,
    {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let record = state.attachments.get(&expected.binding_id).ok_or_else(|| {
            VibexError::conflict(
                "acp_attachment_missing",
                "ACP attachment is no longer registered",
            )
        })?;
        if record.fence != *expected
            || record.state != SessionAttachmentState::Committed
            || state.current_by_session.get(&record.session_id) != Some(&expected.binding_id)
        {
            return Err(VibexError::conflict(
                "acp_attachment_fence_stale",
                "ACP attachment is no longer the committed current attachment",
            ));
        }
        Ok(operation(record.payload.as_ref()))
    }

    /// Runs a synchronous update against one exact live fence. Prepared
    /// attachments are allowed, but inactive/crashed/replaced attachments are
    /// rejected. This is the mutation boundary used during two-phase prepare.
    pub fn apply_fenced<R, F>(
        &self,
        expected: &SessionAttachmentEventFence,
        operation: F,
    ) -> VibexResult<R>
    where
        F: FnOnce(&A) -> R,
    {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let record = state.attachments.get(&expected.binding_id).ok_or_else(|| {
            VibexError::conflict(
                "acp_attachment_missing",
                "ACP attachment is no longer registered",
            )
        })?;
        if record.fence != *expected
            || !matches!(
                record.state,
                SessionAttachmentState::Prepared | SessionAttachmentState::Committed
            )
        {
            return Err(VibexError::conflict(
                "acp_attachment_fence_stale",
                "ACP attachment is no longer live at the supplied fence",
            ));
        }
        Ok(operation(record.payload.as_ref()))
    }

    pub fn set_prompt_closed(
        &self,
        expected: &SessionAttachmentEventFence,
        closed: bool,
    ) -> VibexResult<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let (session_id, record_state) = state
            .attachments
            .get(&expected.binding_id)
            .map(|record| (record.session_id.clone(), record.state))
            .ok_or_else(|| {
                VibexError::conflict(
                    "acp_attachment_missing",
                    "ACP attachment is no longer registered",
                )
            })?;
        if state.current_by_session.get(&session_id) != Some(&expected.binding_id)
            || record_state != SessionAttachmentState::Committed
        {
            return Err(VibexError::conflict(
                "acp_attachment_fence_stale",
                "ACP prompt gate fence is no longer committed and current",
            ));
        }
        let record = state
            .attachments
            .get_mut(&expected.binding_id)
            .expect("attachment existence checked above");
        if record.fence != *expected {
            return Err(VibexError::conflict(
                "acp_attachment_fence_stale",
                "ACP prompt gate fence is stale",
            ));
        }
        record.prompt_closed = closed;
        Ok(())
    }

    /// Marks one exact attachment fence as crashed. This is used by each
    /// attachment's process-crash subscription and is idempotent.
    pub fn mark_crashed(
        &self,
        expected: &SessionAttachmentEventFence,
    ) -> VibexResult<Option<SessionAttachmentHandle<A>>> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let Some(record) = state.attachments.get(&expected.binding_id) else {
            return Ok(None);
        };
        if record.fence != *expected || !record.state.is_live() {
            return Ok(None);
        }
        let (session_id, handle) = {
            let record = state
                .attachments
                .get_mut(&expected.binding_id)
                .expect("attachment existence checked above");
            record.state = SessionAttachmentState::Crashed;
            record.prompt_active = false;
            record.prompt_closed = false;
            (
                record.session_id.clone(),
                self.handle_for_record(expected.binding_id.clone(), record),
            )
        };
        if state.current_by_session.get(&session_id) == Some(&expected.binding_id) {
            state.current_by_session.remove(&session_id);
        }
        Ok(Some(handle))
    }

    /// Marks all attachments on one process as crashed exactly once.
    pub fn mark_process_crashed(
        &self,
        process_instance_id: &AcpProcessInstanceId,
    ) -> VibexResult<Vec<SessionAttachmentHandle<A>>> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let binding_ids = state
            .attachments
            .iter()
            .filter(|(_, record)| {
                record.fence.process_instance_id == *process_instance_id && record.state.is_live()
            })
            .map(|(binding_id, _)| binding_id.clone())
            .collect::<Vec<_>>();
        let mut handles = Vec::with_capacity(binding_ids.len());
        for binding_id in binding_ids {
            let (session_id, handle) = {
                let Some(record) = state.attachments.get_mut(&binding_id) else {
                    continue;
                };
                record.state = SessionAttachmentState::Crashed;
                record.prompt_active = false;
                record.prompt_closed = false;
                let session_id = record.session_id.clone();
                let handle = self.handle_for_record(binding_id.clone(), record);
                (session_id, handle)
            };
            if state.current_by_session.get(&session_id) == Some(&binding_id) {
                state.current_by_session.remove(&session_id);
            }
            handles.push(handle);
        }
        Ok(handles)
    }

    /// Removes an attachment and its native route. The payload remains owned
    /// by the returned handle until callers finish transport cleanup.
    pub fn remove(
        &self,
        expected: &SessionAttachmentEventFence,
    ) -> VibexResult<Option<SessionAttachmentHandle<A>>> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let Some(record) = state.attachments.get(&expected.binding_id) else {
            return Ok(None);
        };
        if record.fence != *expected {
            return Err(VibexError::conflict(
                "acp_attachment_fence_stale",
                "ACP attachment removal fence is stale",
            ));
        }
        let handle = self.handle_for_record(expected.binding_id.clone(), record);
        remove_record_locked(&mut state, &expected.binding_id);
        Ok(Some(handle))
    }

    /// Re-checks a handle's fence and current activation without exposing the
    /// registry state lock to the caller.
    pub fn validate_current(
        &self,
        expected: &SessionAttachmentEventFence,
    ) -> VibexResult<SessionAttachmentHandle<A>> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let Some(record) = state.attachments.get(&expected.binding_id) else {
            return Err(VibexError::conflict(
                "acp_attachment_missing",
                "ACP attachment is no longer registered",
            ));
        };
        if record.fence != *expected
            || record.state != SessionAttachmentState::Committed
            || state.current_by_session.get(&record.session_id) != Some(&expected.binding_id)
        {
            return Err(VibexError::conflict(
                "acp_attachment_fence_stale",
                "ACP attachment is no longer the committed current attachment",
            ));
        }
        Ok(self.handle_for_record(expected.binding_id.clone(), record))
    }

    /// Acquires the short prompt admission gate and marks the attachment's
    /// active-turn flag. The async mutex is released before the prompt round
    /// trip; the returned guard clears the flag on every exit path.
    pub async fn acquire_prompt(
        &self,
        expected: &SessionAttachmentEventFence,
    ) -> VibexResult<SessionAttachmentPromptGuard<A>> {
        let prompt_lock = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| registry_lock_poisoned("state"))?;
            let record = state.attachments.get(&expected.binding_id).ok_or_else(|| {
                VibexError::conflict(
                    "acp_attachment_missing",
                    "ACP attachment is no longer registered",
                )
            })?;
            if record.fence != *expected
                || record.state != SessionAttachmentState::Committed
                || state.current_by_session.get(&record.session_id) != Some(&expected.binding_id)
            {
                return Err(VibexError::conflict(
                    "acp_attachment_fence_stale",
                    "ACP attachment prompt fence is stale",
                ));
            }
            if record.prompt_closed {
                return Err(VibexError::conflict(
                    "acp_prompt_gate_closed",
                    "ACP session is temporarily closed to new prompts",
                ));
            }
            Arc::clone(&record.prompt_lock)
        };
        let lock_guard = prompt_lock.lock_owned().await;
        let result = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| registry_lock_poisoned("state"))?;
            let (session_id, fence, record_state, prompt_active, prompt_closed) = {
                let record = state.attachments.get(&expected.binding_id).ok_or_else(|| {
                    VibexError::conflict(
                        "acp_attachment_missing",
                        "ACP attachment disappeared while waiting for prompt admission",
                    )
                })?;
                (
                    record.session_id.clone(),
                    record.fence.clone(),
                    record.state,
                    record.prompt_active,
                    record.prompt_closed,
                )
            };
            if fence != *expected
                || record_state != SessionAttachmentState::Committed
                || state.current_by_session.get(&session_id) != Some(&expected.binding_id)
            {
                Err(VibexError::conflict(
                    "acp_attachment_fence_stale",
                    "ACP attachment prompt fence is stale",
                ))
            } else if prompt_closed {
                Err(VibexError::conflict(
                    "acp_prompt_gate_closed",
                    "ACP session is temporarily closed to new prompts",
                ))
            } else if prompt_active {
                Err(VibexError::conflict(
                    "acp_turn_already_running",
                    "ACP session already has an active turn",
                ))
            } else {
                let record = state
                    .attachments
                    .get_mut(&expected.binding_id)
                    .ok_or_else(|| {
                        VibexError::conflict(
                            "acp_attachment_missing",
                            "ACP attachment disappeared while waiting for prompt admission",
                        )
                    })?;
                record.prompt_active = true;
                record.last_used_at_ms = unix_timestamp_ms();
                Ok(())
            }
        };
        drop(lock_guard);
        result.map(|()| SessionAttachmentPromptGuard {
            registry: Arc::downgrade(&self.inner),
            binding_id: expected.binding_id.clone(),
            fence: expected.clone(),
            released: false,
            _marker: PhantomData,
        })
    }

    fn release_prompt(&self, binding_id: &RuntimeBindingId, fence: &SessionAttachmentEventFence) {
        let Ok(mut state) = self.inner.state.lock() else {
            return;
        };
        if let Some(record) = state.attachments.get_mut(binding_id)
            && record.fence == *fence
        {
            record.prompt_active = false;
        }
    }

    fn handle_for_record(
        &self,
        binding_id: RuntimeBindingId,
        record: &AttachmentRecord<A>,
    ) -> SessionAttachmentHandle<A> {
        SessionAttachmentHandle {
            registry: Arc::downgrade(&self.inner),
            binding_id,
            session_id: record.session_id.clone(),
            fence: record.fence.clone(),
            payload: Arc::clone(&record.payload),
        }
    }

    fn handle_for_binding_locked(
        &self,
        state: &RegistryState<A>,
        binding_id: RuntimeBindingId,
    ) -> VibexResult<SessionAttachmentHandle<A>> {
        let record = state.attachments.get(&binding_id).ok_or_else(|| {
            VibexError::conflict(
                "acp_attachment_missing",
                "ACP attachment is no longer registered",
            )
        })?;
        Ok(self.handle_for_record(binding_id, record))
    }
}

/// A lightweight handle to an attachment. The handle does not keep the
/// registry alive; callers must keep the owning registry/client alive.
pub struct SessionAttachmentHandle<A> {
    registry: Weak<RegistryInner<A>>,
    binding_id: RuntimeBindingId,
    session_id: VibexSessionId,
    fence: SessionAttachmentEventFence,
    payload: Arc<A>,
}

impl<A> Clone for SessionAttachmentHandle<A> {
    fn clone(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            binding_id: self.binding_id.clone(),
            session_id: self.session_id.clone(),
            fence: self.fence.clone(),
            payload: Arc::clone(&self.payload),
        }
    }
}

impl<A> fmt::Debug for SessionAttachmentHandle<A> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionAttachmentHandle")
            .field("binding_id", &self.binding_id)
            .field("session_id", &self.session_id)
            .field("fence", &self.fence)
            .finish_non_exhaustive()
    }
}

impl<A> SessionAttachmentHandle<A> {
    pub fn binding_id(&self) -> &RuntimeBindingId {
        &self.binding_id
    }

    pub fn session_id(&self) -> &VibexSessionId {
        &self.session_id
    }

    pub fn fence(&self) -> &SessionAttachmentEventFence {
        &self.fence
    }

    pub fn payload(&self) -> Arc<A> {
        Arc::clone(&self.payload)
    }

    pub fn state(&self) -> VibexResult<SessionAttachmentState> {
        let inner = self.registry.upgrade().ok_or_else(|| {
            VibexError::process(
                "acp_session_attachment_registry_closed",
                "ACP session attachment registry is no longer available",
            )
        })?;
        let state = inner
            .state
            .lock()
            .map_err(|_| registry_lock_poisoned("state"))?;
        let record = state.attachments.get(&self.binding_id).ok_or_else(|| {
            VibexError::conflict(
                "acp_attachment_missing",
                "ACP attachment is no longer registered",
            )
        })?;
        if record.fence != self.fence {
            return Err(VibexError::conflict(
                "acp_attachment_fence_stale",
                "ACP attachment handle fence is stale",
            ));
        }
        Ok(record.state)
    }
}

/// RAII guard for one prompt admission. Dropping it always clears the active
/// turn flag, including timeout and caller-cancellation paths.
pub struct SessionAttachmentPromptGuard<A> {
    registry: Weak<RegistryInner<A>>,
    binding_id: RuntimeBindingId,
    fence: SessionAttachmentEventFence,
    released: bool,
    _marker: PhantomData<Arc<A>>,
}

impl<A> fmt::Debug for SessionAttachmentPromptGuard<A> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionAttachmentPromptGuard")
            .field("binding_id", &self.binding_id)
            .field("fence", &self.fence)
            .field("released", &self.released)
            .finish_non_exhaustive()
    }
}

impl<A> SessionAttachmentPromptGuard<A> {
    pub fn fence(&self) -> &SessionAttachmentEventFence {
        &self.fence
    }

    pub fn release(mut self) {
        self.released = true;
        self.release_inner();
    }

    fn release_inner(&self) {
        let Some(inner) = self.registry.upgrade() else {
            return;
        };
        SessionAttachmentRegistry { inner }.release_prompt(&self.binding_id, &self.fence);
    }
}

impl<A> Drop for SessionAttachmentPromptGuard<A> {
    fn drop(&mut self) {
        if !self.released {
            self.release_inner();
        }
    }
}

fn remove_record_locked<A>(state: &mut RegistryState<A>, binding_id: &RuntimeBindingId) {
    let Some(record) = state.attachments.remove(binding_id) else {
        return;
    };
    let route_key = NativeRouteKey {
        process_instance_id: record.fence.process_instance_id.clone(),
        native_session_id: record.fence.native_session_id.clone(),
    };
    if state.native_routes.get(&route_key) == Some(binding_id) {
        state.native_routes.remove(&route_key);
    }
    if state.current_by_session.get(&record.session_id) == Some(binding_id) {
        state.current_by_session.remove(&record.session_id);
    }
}

fn normalize_native_session_id(native_session_id: Option<String>) -> VibexResult<Option<String>> {
    let Some(native_session_id) = native_session_id else {
        return Ok(None);
    };
    let native_session_id = native_session_id.trim().to_string();
    if native_session_id.is_empty() {
        return Err(VibexError::validation(
            "acp_attachment_native_session_id_empty",
            "ACP attachment native session id must not be empty",
        ));
    }
    Ok(Some(native_session_id))
}

fn hash_native_id(native_session_id: &str) -> String {
    let digest = Sha256::digest(native_session_id.as_bytes());
    digest[..NATIVE_ID_HASH_BYTES]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn redact_native_id(native_session_id: &str) -> String {
    format!("sha256:{}", hash_native_id(native_session_id))
}

fn registry_lock_poisoned(scope: &str) -> VibexError {
    VibexError::process(
        "acp_session_attachment_registry_lock_poisoned",
        "ACP session attachment registry state is unavailable",
    )
    .with_diagnostic("scope", scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Barrier;
    use tokio::time::sleep;

    fn ids() -> (VibexSessionId, RuntimeBindingId, AcpProcessInstanceId) {
        (
            VibexSessionId::new(),
            RuntimeBindingId::new(),
            AcpProcessInstanceId::new(),
        )
    }

    fn key(
        binding_id: RuntimeBindingId,
        process_instance_id: AcpProcessInstanceId,
        generation: u64,
        native_session_id: Option<&str>,
    ) -> SessionAttachmentAcquireKey {
        SessionAttachmentAcquireKey::new(
            binding_id,
            native_session_id.map(str::to_string),
            process_instance_id,
            generation,
        )
        .unwrap()
    }

    async fn create(
        registry: &SessionAttachmentRegistry<usize>,
        session_id: VibexSessionId,
        key: SessionAttachmentAcquireKey,
        value: usize,
    ) -> SessionAttachmentHandle<usize> {
        match registry
            .acquire(session_id, key, || async move {
                Ok(SessionAttachmentAcquireOutput {
                    native_session_id: format!("native-{value}"),
                    payload: value,
                })
            })
            .await
            .unwrap()
        {
            SessionAttachmentAcquireResult::Existing(handle)
            | SessionAttachmentAcquireResult::Created(handle) => handle,
        }
    }

    #[tokio::test]
    async fn same_key_runs_side_effect_once() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_id, binding_id, process_id) = ids();
        let acquire_key = key(binding_id, process_id.clone(), 0, None);
        let calls = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));

        let first_registry = registry.clone();
        let first_calls = Arc::clone(&calls);
        let first_barrier = Arc::clone(&barrier);
        let first_session = session_id.clone();
        let first_key = acquire_key.clone();
        let first = tokio::spawn(async move {
            first_registry
                .acquire(first_session, first_key, || async move {
                    first_calls.fetch_add(1, Ordering::SeqCst);
                    first_barrier.wait().await;
                    Ok(SessionAttachmentAcquireOutput {
                        native_session_id: "native-same".to_string(),
                        payload: 1,
                    })
                })
                .await
        });
        // Ensure the first operation is inside the key lock before starting
        // the duplicate. The barrier is released by the duplicate only after
        // it has entered the lock wait path.
        sleep(Duration::from_millis(10)).await;
        let duplicate_registry = registry.clone();
        let duplicate_calls = Arc::clone(&calls);
        let duplicate_session = session_id.clone();
        let duplicate_key = acquire_key.clone();
        let duplicate = tokio::spawn(async move {
            duplicate_registry
                .acquire(duplicate_session, duplicate_key, || async move {
                    duplicate_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(SessionAttachmentAcquireOutput {
                        native_session_id: "native-duplicate".to_string(),
                        payload: 2,
                    })
                })
                .await
        });
        // The duplicate cannot enter its closure until the first completes;
        // release the first operation after a bounded wait instead of relying
        // on task scheduling details.
        sleep(Duration::from_millis(10)).await;
        barrier.wait().await;
        let first_result = first.await.unwrap().unwrap();
        let duplicate_result = duplicate.await.unwrap().unwrap();
        assert!(!first_result.is_existing());
        assert!(duplicate_result.is_existing());
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let second_binding = RuntimeBindingId::new();
        let second_key = key(second_binding, process_id, 0, None);
        let second = create(&registry, session_id, second_key, 3).await;
        assert_eq!(*second.payload(), 3);
    }

    #[tokio::test]
    async fn failed_operation_can_retry_without_route_or_lock_leak() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_id, binding_id, process_id) = ids();
        let acquire_key = key(binding_id, process_id.clone(), 0, None);
        let error = registry
            .acquire(session_id.clone(), acquire_key.clone(), || async {
                Err(VibexError::provider("test_failure", "load failed"))
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "test_failure");
        assert_eq!(registry.acquire_lock_count(), 0);
        assert!(matches!(
            registry.route(&process_id, Some("native-retry"), Some("session/update")),
            SessionAttachmentRoute::Diagnostic(_)
        ));
        let handle = create(&registry, session_id, acquire_key, 4).await;
        assert_eq!(*handle.payload(), 4);
        assert_eq!(registry.acquire_lock_count(), 0);
    }

    #[tokio::test]
    async fn cancelled_operation_releases_key_lock_and_can_retry() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_id, binding_id, process_id) = ids();
        let acquire_key = key(binding_id, process_id, 0, None);
        let entered = Arc::new(Barrier::new(2));

        let cancelled_registry = registry.clone();
        let cancelled_session = session_id.clone();
        let cancelled_key = acquire_key.clone();
        let operation_entered = Arc::clone(&entered);
        let cancelled = tokio::spawn(async move {
            cancelled_registry
                .acquire(cancelled_session, cancelled_key, || async move {
                    operation_entered.wait().await;
                    std::future::pending::<VibexResult<SessionAttachmentAcquireOutput<usize>>>()
                        .await
                })
                .await
        });
        entered.wait().await;
        cancelled.abort();
        assert!(cancelled.await.unwrap_err().is_cancelled());
        assert_eq!(registry.acquire_lock_count(), 0);

        let handle = create(&registry, session_id, acquire_key, 5).await;
        assert_eq!(*handle.payload(), 5);
        assert_eq!(registry.acquire_lock_count(), 0);
    }

    #[tokio::test]
    async fn returned_native_id_mismatch_registers_nothing_and_can_retry() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_id, binding_id, process_id) = ids();
        let acquire_key = key(binding_id, process_id.clone(), 0, Some("native-expected"));
        let error = registry
            .acquire(session_id.clone(), acquire_key.clone(), || async {
                Ok(SessionAttachmentAcquireOutput {
                    native_session_id: "native-other".to_string(),
                    payload: 1,
                })
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "acp_attachment_native_session_mismatch");
        assert!(matches!(
            registry.route(&process_id, Some("native-other"), None),
            SessionAttachmentRoute::Diagnostic(_)
        ));
        assert_eq!(registry.acquire_lock_count(), 0);

        let created = registry
            .acquire(session_id, acquire_key, || async {
                Ok(SessionAttachmentAcquireOutput {
                    native_session_id: "native-expected".to_string(),
                    payload: 2,
                })
            })
            .await
            .unwrap();
        assert!(matches!(
            created,
            SessionAttachmentAcquireResult::Created(_)
        ));
    }

    #[tokio::test]
    async fn different_keys_run_attachment_operations_concurrently() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let process_id = AcpProcessInstanceId::new();
        let barrier = Arc::new(Barrier::new(3));
        let mut tasks = Vec::new();
        for value in [1usize, 2] {
            let registry = registry.clone();
            let barrier = Arc::clone(&barrier);
            let acquire_key = key(RuntimeBindingId::new(), process_id.clone(), 0, None);
            tasks.push(tokio::spawn(async move {
                registry
                    .acquire(VibexSessionId::new(), acquire_key, || async move {
                        barrier.wait().await;
                        Ok(SessionAttachmentAcquireOutput {
                            native_session_id: format!("native-parallel-{value}"),
                            payload: value,
                        })
                    })
                    .await
            }));
        }
        tokio::time::timeout(Duration::from_secs(1), barrier.wait())
            .await
            .expect("different attachment operations must both reach their closures");
        for task in tasks {
            assert!(matches!(
                task.await.unwrap().unwrap(),
                SessionAttachmentAcquireResult::Created(_)
            ));
        }
        assert_eq!(registry.acquire_lock_count(), 0);
    }

    #[tokio::test]
    async fn live_binding_rejects_a_different_attachment_key_before_side_effect() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_id, binding_id, process_id) = ids();
        let first = create(
            &registry,
            session_id.clone(),
            key(binding_id.clone(), process_id, 0, None),
            1,
        )
        .await;
        registry.activate(first.fence()).unwrap();
        let calls = AtomicUsize::new(0);
        let error = registry
            .acquire(
                session_id,
                key(binding_id, AcpProcessInstanceId::new(), 1, None),
                || async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(SessionAttachmentAcquireOutput {
                        native_session_id: "native-conflict".to_string(),
                        payload: 2,
                    })
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "acp_attachment_key_conflict");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn live_binding_rejects_a_different_expected_native_id_before_side_effect() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_id, binding_id, process_id) = ids();
        let first_key = key(
            binding_id.clone(),
            process_id.clone(),
            0,
            Some("native-original"),
        );
        let first = registry
            .acquire(session_id.clone(), first_key.clone(), || async {
                Ok(SessionAttachmentAcquireOutput {
                    native_session_id: "native-original".to_string(),
                    payload: 1,
                })
            })
            .await
            .unwrap();
        assert!(matches!(first, SessionAttachmentAcquireResult::Created(_)));

        let exact_calls = AtomicUsize::new(0);
        let exact = registry
            .acquire(session_id.clone(), first_key, || async {
                exact_calls.fetch_add(1, Ordering::SeqCst);
                Ok(SessionAttachmentAcquireOutput {
                    native_session_id: "native-original".to_string(),
                    payload: 2,
                })
            })
            .await
            .unwrap();
        assert!(matches!(exact, SessionAttachmentAcquireResult::Existing(_)));
        assert_eq!(exact_calls.load(Ordering::SeqCst), 0);

        let conflicting_calls = AtomicUsize::new(0);
        let error = registry
            .acquire(
                session_id,
                key(binding_id, process_id, 0, Some("native-conflicting")),
                || async {
                    conflicting_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(SessionAttachmentAcquireOutput {
                        native_session_id: "native-conflicting".to_string(),
                        payload: 3,
                    })
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "acp_attachment_key_conflict");
        assert_eq!(conflicting_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn route_requires_current_committed_fence_and_never_falls_back() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_id, binding_id, process_id) = ids();
        let acquire_key = key(binding_id, process_id.clone(), 0, None);
        let handle = create(&registry, session_id.clone(), acquire_key, 7).await;
        assert!(matches!(
            registry.route(&process_id, Some("native-7"), Some("session/update")),
            SessionAttachmentRoute::Quarantine(_)
        ));
        registry.activate(handle.fence()).unwrap();
        assert_eq!(
            registry.activate(handle.fence()).unwrap().state().unwrap(),
            SessionAttachmentState::Committed
        );
        assert!(matches!(
            registry.route(&process_id, Some("native-7"), Some("session/update")),
            SessionAttachmentRoute::Deliver(_)
        ));
        assert!(matches!(
            registry.route(&process_id, None, Some("session/update")),
            SessionAttachmentRoute::Diagnostic(diagnostic)
                if diagnostic.rejection == SessionAttachmentRouteRejection::MissingNativeSessionId
        ));
        assert!(matches!(
            registry.route(&process_id, Some("   "), Some("session/update")),
            SessionAttachmentRoute::Diagnostic(diagnostic)
                if diagnostic.rejection == SessionAttachmentRouteRejection::EmptyNativeSessionId
        ));
        assert!(matches!(
            registry.route(&process_id, Some("unknown"), Some("session/update")),
            SessionAttachmentRoute::Diagnostic(diagnostic)
                if diagnostic.rejection == SessionAttachmentRouteRejection::UnknownNativeSessionId
        ));
    }

    #[tokio::test]
    async fn prepared_events_are_quarantined_and_old_generation_is_stale() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_id, binding_id, process_id) = ids();
        let handle = create(
            &registry,
            session_id.clone(),
            key(binding_id.clone(), process_id.clone(), 0, None),
            1,
        )
        .await;
        assert!(matches!(
            registry.route(&process_id, Some("native-1"), None),
            SessionAttachmentRoute::Quarantine(_)
        ));
        registry.activate(handle.fence()).unwrap();
        let replacement_binding = RuntimeBindingId::new();
        let replacement_process = AcpProcessInstanceId::new();
        let replacement = create(
            &registry,
            session_id,
            key(replacement_binding, replacement_process.clone(), 1, None),
            2,
        )
        .await;
        assert!(matches!(
            registry.route(&replacement_process, Some("native-2"), None),
            SessionAttachmentRoute::Quarantine(_)
        ));
        registry.activate(replacement.fence()).unwrap();
        assert!(matches!(
            registry.route(&process_id, Some("native-1"), None),
            SessionAttachmentRoute::Diagnostic(diagnostic)
                if diagnostic.rejection == SessionAttachmentRouteRejection::AttachmentNotCurrent
        ));
        assert!(matches!(
            registry.route(&replacement_process, Some("native-2"), None),
            SessionAttachmentRoute::Deliver(_)
        ));
        let regressed = create(
            &registry,
            replacement.session_id().clone(),
            key(
                RuntimeBindingId::new(),
                AcpProcessInstanceId::new(),
                0,
                None,
            ),
            4,
        )
        .await;
        let error = registry.activate(regressed.fence()).unwrap_err();
        assert_eq!(error.code, "acp_attachment_generation_regression");
    }

    #[tokio::test]
    async fn every_event_fence_component_is_checked_before_side_effect() {
        let registry = SessionAttachmentRegistry::<AtomicUsize>::new();
        let (session_id, binding_id, process_id) = ids();
        let handle = match registry
            .acquire(session_id, key(binding_id, process_id, 4, None), || async {
                Ok(SessionAttachmentAcquireOutput {
                    native_session_id: "native-fence".to_string(),
                    payload: AtomicUsize::new(0),
                })
            })
            .await
            .unwrap()
        {
            SessionAttachmentAcquireResult::Created(handle)
            | SessionAttachmentAcquireResult::Existing(handle) => handle,
        };
        registry.activate(handle.fence()).unwrap();
        registry
            .apply_current(handle.fence(), |counter| {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();

        let mut wrong_binding = handle.fence().clone();
        wrong_binding.binding_id = RuntimeBindingId::new();
        let mut wrong_generation = handle.fence().clone();
        wrong_generation.activation_generation += 1;
        let mut wrong_process = handle.fence().clone();
        wrong_process.process_instance_id = AcpProcessInstanceId::new();
        let mut wrong_native = handle.fence().clone();
        wrong_native.native_session_id = "native-other".to_string();

        for fence in [&wrong_binding, &wrong_generation] {
            assert!(matches!(
                registry.route_fenced(fence, Some("session/update")),
                SessionAttachmentRoute::Diagnostic(diagnostic)
                    if diagnostic.rejection == SessionAttachmentRouteRejection::FenceMismatch
            ));
            assert!(
                registry
                    .apply_current(fence, |counter| {
                        counter.fetch_add(1, Ordering::SeqCst);
                    })
                    .is_err()
            );
        }
        for fence in [&wrong_process, &wrong_native] {
            assert!(matches!(
                registry.route_fenced(fence, Some("session/update")),
                SessionAttachmentRoute::Diagnostic(diagnostic)
                    if diagnostic.rejection
                        == SessionAttachmentRouteRejection::UnknownNativeSessionId
            ));
        }
        assert_eq!(handle.payload().load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn route_conflict_keeps_original_owner() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_a, binding_a, process_id) = ids();
        let first = create(
            &registry,
            session_a,
            key(binding_a, process_id.clone(), 0, None),
            1,
        )
        .await;
        let (session_b, binding_b, _) = ids();
        let error = registry
            .acquire(
                session_b,
                key(binding_b, process_id.clone(), 0, None),
                || async {
                    Ok(SessionAttachmentAcquireOutput {
                        native_session_id: "native-1".to_string(),
                        payload: 2,
                    })
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "acp_native_session_route_conflict");
        registry.activate(first.fence()).unwrap();
        assert!(matches!(
            registry.route(&process_id, Some("native-1"), None),
            SessionAttachmentRoute::Deliver(_)
        ));
    }

    #[tokio::test]
    async fn prompt_gate_rejects_active_turn_and_clears_on_drop() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_id, binding_id, process_id) = ids();
        let handle = create(
            &registry,
            session_id,
            key(binding_id, process_id, 0, None),
            9,
        )
        .await;
        registry.activate(handle.fence()).unwrap();
        let first = registry.acquire_prompt(handle.fence()).await.unwrap();
        let second = registry.acquire_prompt(handle.fence()).await.unwrap_err();
        assert_eq!(second.code, "acp_turn_already_running");
        drop(first);
        let _again = registry.acquire_prompt(handle.fence()).await.unwrap();
    }

    #[tokio::test]
    async fn explicit_prompt_close_rejects_before_and_after_waiting() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_id, binding_id, process_id) = ids();
        let handle = create(
            &registry,
            session_id,
            key(binding_id, process_id, 0, None),
            9,
        )
        .await;
        registry.activate(handle.fence()).unwrap();
        registry.set_prompt_closed(handle.fence(), true).unwrap();
        let closed = registry.acquire_prompt(handle.fence()).await.unwrap_err();
        assert_eq!(closed.code, "acp_prompt_gate_closed");
        registry.set_prompt_closed(handle.fence(), false).unwrap();

        let prompt_lock = {
            let state = registry.inner.state.lock().unwrap();
            Arc::clone(
                &state
                    .attachments
                    .get(handle.binding_id())
                    .unwrap()
                    .prompt_lock,
            )
        };
        let held = prompt_lock.lock_owned().await;
        let waiting_registry = registry.clone();
        let waiting_fence = handle.fence().clone();
        let waiter =
            tokio::spawn(async move { waiting_registry.acquire_prompt(&waiting_fence).await });
        sleep(Duration::from_millis(10)).await;
        registry.set_prompt_closed(handle.fence(), true).unwrap();
        drop(held);
        let closed_after_wait = waiter.await.unwrap().unwrap_err();
        assert_eq!(closed_after_wait.code, "acp_prompt_gate_closed");
    }

    #[tokio::test]
    async fn prepared_fenced_updates_never_target_current_attachment() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_id, binding_id, process_id) = ids();
        let prepared = create(
            &registry,
            session_id.clone(),
            key(binding_id, process_id, 0, None),
            7,
        )
        .await;
        assert_eq!(
            registry
                .apply_fenced(prepared.fence(), |value| *value)
                .unwrap(),
            7
        );
        assert_eq!(
            registry
                .apply_current(prepared.fence(), |value| *value)
                .unwrap_err()
                .code,
            "acp_attachment_fence_stale"
        );

        registry.activate(prepared.fence()).unwrap();
        let replacement = create(
            &registry,
            session_id,
            key(
                RuntimeBindingId::new(),
                AcpProcessInstanceId::new(),
                1,
                None,
            ),
            8,
        )
        .await;
        registry.activate(replacement.fence()).unwrap();
        assert_eq!(
            registry
                .apply_fenced(prepared.fence(), |value| *value)
                .unwrap_err()
                .code,
            "acp_attachment_fence_stale"
        );
    }

    #[tokio::test]
    async fn prompt_gates_are_attachment_local_and_waiters_revalidate_replacement() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_a, binding_a, process_a) = ids();
        let first = create(
            &registry,
            session_a.clone(),
            key(binding_a, process_a, 0, None),
            1,
        )
        .await;
        registry.activate(first.fence()).unwrap();
        let (session_b, binding_b, process_b) = ids();
        let second = create(&registry, session_b, key(binding_b, process_b, 0, None), 2).await;
        registry.activate(second.fence()).unwrap();

        let first_guard = registry.acquire_prompt(first.fence()).await.unwrap();
        let second_guard = tokio::time::timeout(
            Duration::from_millis(100),
            registry.acquire_prompt(second.fence()),
        )
        .await
        .expect("another attachment must not share the prompt lock")
        .unwrap();
        drop(first_guard);
        drop(second_guard);

        let prompt_lock = {
            let state = registry.inner.state.lock().unwrap();
            Arc::clone(
                &state
                    .attachments
                    .get(first.binding_id())
                    .unwrap()
                    .prompt_lock,
            )
        };
        let held = prompt_lock.lock_owned().await;
        let waiting_registry = registry.clone();
        let waiting_fence = first.fence().clone();
        let waiter =
            tokio::spawn(async move { waiting_registry.acquire_prompt(&waiting_fence).await });
        sleep(Duration::from_millis(10)).await;

        let replacement = create(
            &registry,
            session_a,
            key(
                RuntimeBindingId::new(),
                AcpProcessInstanceId::new(),
                1,
                None,
            ),
            3,
        )
        .await;
        registry.activate(replacement.fence()).unwrap();
        drop(held);
        let error = waiter.await.unwrap().unwrap_err();
        assert_eq!(error.code, "acp_attachment_fence_stale");
    }

    #[tokio::test]
    async fn exact_close_claim_is_predicated_fenced_and_removes_routes_before_cleanup() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_id, binding_id, process_id) = ids();
        let handle = create(
            &registry,
            session_id.clone(),
            key(binding_id, process_id.clone(), 0, None),
            7,
        )
        .await;
        registry.activate(handle.fence()).unwrap();
        registry.touch(handle.fence(), 42).unwrap();
        assert_eq!(registry.usage_timestamps(handle.fence()).unwrap().1, 42);
        assert!(
            registry
                .claim_close_if(handle.fence(), |_| false)
                .unwrap()
                .is_none()
        );
        assert!(registry.current(&session_id).unwrap().is_some());

        let mut stale = handle.fence().clone();
        stale.activation_generation = 1;
        assert_eq!(
            registry.claim_close(&stale).unwrap_err().code,
            "acp_attachment_fence_stale"
        );
        let claimed = registry
            .claim_close_if(handle.fence(), |payload| *payload == 7)
            .unwrap()
            .expect("exact idle fence should be claimed");
        assert_eq!(claimed.state().unwrap(), SessionAttachmentState::Closing);
        assert!(registry.current(&session_id).unwrap().is_none());
        assert!(matches!(
            registry.route(
                &process_id,
                Some(&handle.fence().native_session_id),
                Some("session/update")
            ),
            SessionAttachmentRoute::Diagnostic(_)
        ));
        assert!(registry.remove(handle.fence()).unwrap().is_some());
        assert!(registry.attachment(handle.binding_id()).unwrap().is_none());
    }

    #[tokio::test]
    async fn idle_close_claim_rechecks_current_state_and_latest_touch_atomically() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_id, binding_id, process_id) = ids();
        let handle = create(
            &registry,
            session_id.clone(),
            key(binding_id, process_id, 0, None),
            7,
        )
        .await;
        registry.activate(handle.fence()).unwrap();
        registry.touch(handle.fence(), 100).unwrap();

        assert!(
            registry
                .claim_idle_close_if(handle.fence(), 100, 109, 10, |_, is_current, idle| {
                    is_current && idle
                })
                .unwrap()
                .is_none()
        );

        registry.touch(handle.fence(), 105).unwrap();
        assert!(
            registry
                .claim_idle_close_if(handle.fence(), 100, 110, 10, |_, is_current, idle| {
                    is_current && idle
                })
                .unwrap()
                .is_none()
        );

        let claimed = registry
            .claim_idle_close_if(handle.fence(), 105, 115, 10, |_, is_current, idle| {
                is_current && idle
            })
            .unwrap()
            .expect("the exact current fence should become idle at the deadline");
        assert_eq!(claimed.state().unwrap(), SessionAttachmentState::Closing);
        assert!(registry.current(&session_id).unwrap().is_none());
    }

    #[tokio::test]
    async fn crash_marks_each_attachment_once_and_removes_current_selection() {
        let registry = SessionAttachmentRegistry::<usize>::new();
        let (session_a, binding_a, process_id) = ids();
        let first = create(
            &registry,
            session_a.clone(),
            key(binding_a, process_id.clone(), 0, None),
            1,
        )
        .await;
        registry.activate(first.fence()).unwrap();
        let (session_b, binding_b, _) = ids();
        let second = create(
            &registry,
            session_b,
            key(binding_b, process_id.clone(), 0, None),
            2,
        )
        .await;
        registry.activate(second.fence()).unwrap();
        let crashed = registry.mark_process_crashed(&process_id).unwrap();
        assert_eq!(crashed.len(), 2);
        assert!(registry.current(&session_a).unwrap().is_none());
        assert_eq!(first.state().unwrap(), SessionAttachmentState::Crashed);
        assert_eq!(second.state().unwrap(), SessionAttachmentState::Crashed);
        assert!(
            registry
                .mark_process_crashed(&process_id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn debug_and_diagnostics_do_not_contain_raw_native_id() {
        let native_id = "native-secret-session-id";
        let (_, binding_id, process_id) = ids();
        let key = key(binding_id, process_id.clone(), 0, Some(native_id));
        let debug = format!("{key:?}");
        assert!(!debug.contains(native_id));
        let registry = SessionAttachmentRegistry::<usize>::new();
        let diagnostic = registry.route(&process_id, Some(native_id), Some("session/update"));
        let SessionAttachmentRoute::Diagnostic(diagnostic) = diagnostic else {
            panic!("expected unknown route");
        };
        let debug = format!("{diagnostic:?}");
        assert!(!debug.contains(native_id));
        assert!(diagnostic.native_session_hash.is_some());
    }
}

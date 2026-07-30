use std::collections::BTreeMap;
use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use tracing::field;

pub const RUNTIME_METRIC_SERIES_LIMIT: usize = 256;

const FINGERPRINT_PREFIX_LENGTH: usize = 16;
const NATIVE_SESSION_HASH_DOMAIN: &[u8] = b"vibex/runtime-native-session/v1";
const STABLE_CODE_LIMIT: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeMetricName {
    SpawnDuration,
    InitializeDuration,
    SessionOpenDuration,
    PromptLatency,
    SwitchPhaseDuration,
    DesiredToEffectiveDuration,
    SwitchReconciliation,
    SwitchActiveWork,
    QueuedMessageWait,
    DuplicateSubmissionPrevented,
    AmbiguousPromptDispatch,
    Restore,
    FreshBridge,
    ConfigStale,
    DuplicateAcquirePrevented,
    Acquire,
    AdapterCrash,
    UnknownAcpEvent,
    UnroutableNativeEvent,
    PreparedEventQuarantined,
    EventEnricherFallback,
    TranscriptWatcherLag,
    ProcessTreeCleanupFailure,
}

impl RuntimeMetricName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SpawnDuration => "runtime_spawn_duration_ms",
            Self::InitializeDuration => "runtime_initialize_duration_ms",
            Self::SessionOpenDuration => "runtime_session_open_duration_ms",
            Self::PromptLatency => "runtime_prompt_latency_ms",
            Self::SwitchPhaseDuration => "runtime_switch_phase_duration_ms",
            Self::DesiredToEffectiveDuration => "runtime_desired_to_effective_duration_ms",
            Self::SwitchReconciliation => "runtime_switch_reconciliation_total",
            Self::SwitchActiveWork => "runtime_switch_active_work_total",
            Self::QueuedMessageWait => "runtime_queued_message_wait_ms",
            Self::DuplicateSubmissionPrevented => "runtime_duplicate_submission_prevented_total",
            Self::AmbiguousPromptDispatch => "runtime_ambiguous_prompt_dispatch_total",
            Self::Restore => "runtime_restore_total",
            Self::FreshBridge => "runtime_fresh_bridge_total",
            Self::ConfigStale => "runtime_config_stale_total",
            Self::DuplicateAcquirePrevented => "runtime_duplicate_acquire_prevented_total",
            Self::Acquire => "runtime_acquire_total",
            Self::AdapterCrash => "runtime_adapter_crash_total",
            Self::UnknownAcpEvent => "runtime_unknown_acp_event_total",
            Self::UnroutableNativeEvent => "runtime_unroutable_native_event_total",
            Self::PreparedEventQuarantined => "runtime_prepared_event_quarantined_total",
            Self::EventEnricherFallback => "runtime_event_enricher_fallback_total",
            Self::TranscriptWatcherLag => "runtime_transcript_watcher_lag_ms",
            Self::ProcessTreeCleanupFailure => "runtime_process_tree_cleanup_failure_total",
        }
    }
}

impl fmt::Display for RuntimeMetricName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeMetricOperation {
    Resume,
    Load,
    New,
    Prepare,
    Commit,
    ActiveTurn,
    PendingPermission,
    ActiveTerminal,
    BackgroundWork,
    Process,
    Attachment,
}

impl RuntimeMetricOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resume => "resume",
            Self::Load => "load",
            Self::New => "new",
            Self::Prepare => "prepare",
            Self::Commit => "commit",
            Self::ActiveTurn => "active_turn",
            Self::PendingPermission => "pending_permission",
            Self::ActiveTerminal => "active_terminal",
            Self::BackgroundWork => "background_work",
            Self::Process => "process",
            Self::Attachment => "attachment",
        }
    }
}

impl fmt::Display for RuntimeMetricOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeMetricResult {
    Success,
    Failure,
    Created,
    Reused,
    Prevented,
    Committed,
    Cancelled,
    Superseded,
    Ambiguous,
    Rejected,
    Waited,
    TimedOut,
    Resumed,
    Loaded,
    NotFound,
    AuthenticationRequired,
    Unsupported,
    TransientFailure,
    FatalFailure,
    Fresh,
    Current,
    Stale,
    Quarantined,
    Unroutable,
    Fallback,
    Crashed,
}

impl RuntimeMetricResult {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Created => "created",
            Self::Reused => "reused",
            Self::Prevented => "prevented",
            Self::Committed => "committed",
            Self::Cancelled => "cancelled",
            Self::Superseded => "superseded",
            Self::Ambiguous => "ambiguous",
            Self::Rejected => "rejected",
            Self::Waited => "waited",
            Self::TimedOut => "timed_out",
            Self::Resumed => "resumed",
            Self::Loaded => "loaded",
            Self::NotFound => "not_found",
            Self::AuthenticationRequired => "authentication_required",
            Self::Unsupported => "unsupported",
            Self::TransientFailure => "transient_failure",
            Self::FatalFailure => "fatal_failure",
            Self::Fresh => "fresh",
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Quarantined => "quarantined",
            Self::Unroutable => "unroutable",
            Self::Fallback => "fallback",
            Self::Crashed => "crashed",
        }
    }
}

impl fmt::Display for RuntimeMetricResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RuntimeMetricKey {
    name: RuntimeMetricName,
    operation: Option<RuntimeMetricOperation>,
    result: RuntimeMetricResult,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RuntimeMetricAggregate {
    count: u64,
    duration_total_ms: Option<u64>,
    duration_min_ms: Option<u64>,
    duration_max_ms: Option<u64>,
    duration_last_ms: Option<u64>,
}

impl RuntimeMetricAggregate {
    fn increment(&mut self) {
        self.count = self.count.saturating_add(1);
    }

    fn observe_duration(&mut self, duration_ms: u64) {
        self.increment();
        self.duration_total_ms = Some(
            self.duration_total_ms
                .unwrap_or_default()
                .saturating_add(duration_ms),
        );
        self.duration_min_ms = Some(
            self.duration_min_ms
                .map_or(duration_ms, |current| current.min(duration_ms)),
        );
        self.duration_max_ms = Some(
            self.duration_max_ms
                .map_or(duration_ms, |current| current.max(duration_ms)),
        );
        self.duration_last_ms = Some(duration_ms);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMetricSeries {
    pub name: RuntimeMetricName,
    pub operation: Option<RuntimeMetricOperation>,
    pub result: RuntimeMetricResult,
    pub count: u64,
    pub duration_total_ms: Option<u64>,
    pub duration_min_ms: Option<u64>,
    pub duration_max_ms: Option<u64>,
    pub duration_last_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMetricSnapshot {
    pub process_started_at_ms: i64,
    pub snapshot_at_ms: i64,
    pub series_limit: usize,
    pub series: Vec<RuntimeMetricSeries>,
}

pub struct RuntimeObservability {
    process_started_at_ms: i64,
    metrics: Mutex<BTreeMap<RuntimeMetricKey, RuntimeMetricAggregate>>,
}

impl fmt::Debug for RuntimeObservability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeObservability")
            .field("process_started_at_ms", &self.process_started_at_ms)
            .field("series_limit", &RUNTIME_METRIC_SERIES_LIMIT)
            .finish_non_exhaustive()
    }
}

impl Default for RuntimeObservability {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeObservability {
    pub fn new() -> Self {
        Self {
            process_started_at_ms: unix_timestamp_ms(),
            metrics: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn increment(
        &self,
        name: RuntimeMetricName,
        operation: Option<RuntimeMetricOperation>,
        result: RuntimeMetricResult,
    ) {
        self.record(name, operation, result, None);
    }

    pub fn observe_duration(
        &self,
        name: RuntimeMetricName,
        operation: Option<RuntimeMetricOperation>,
        result: RuntimeMetricResult,
        duration: Duration,
    ) {
        self.observe_duration_ms(
            name,
            operation,
            result,
            duration.as_millis().min(u128::from(u64::MAX)) as u64,
        );
    }

    pub fn observe_duration_ms(
        &self,
        name: RuntimeMetricName,
        operation: Option<RuntimeMetricOperation>,
        result: RuntimeMetricResult,
        duration_ms: u64,
    ) {
        self.record(name, operation, result, Some(duration_ms));
    }

    pub fn snapshot(&self) -> RuntimeMetricSnapshot {
        let series = self
            .metrics
            .lock()
            .map(|metrics| {
                metrics
                    .iter()
                    .map(|(key, aggregate)| RuntimeMetricSeries {
                        name: key.name,
                        operation: key.operation,
                        result: key.result,
                        count: aggregate.count,
                        duration_total_ms: aggregate.duration_total_ms,
                        duration_min_ms: aggregate.duration_min_ms,
                        duration_max_ms: aggregate.duration_max_ms,
                        duration_last_ms: aggregate.duration_last_ms,
                    })
                    .collect()
            })
            .unwrap_or_default();
        RuntimeMetricSnapshot {
            process_started_at_ms: self.process_started_at_ms,
            snapshot_at_ms: unix_timestamp_ms(),
            series_limit: RUNTIME_METRIC_SERIES_LIMIT,
            series,
        }
    }

    fn record(
        &self,
        name: RuntimeMetricName,
        operation: Option<RuntimeMetricOperation>,
        result: RuntimeMetricResult,
        duration_ms: Option<u64>,
    ) {
        let key = RuntimeMetricKey {
            name,
            operation,
            result,
        };
        let Ok(mut metrics) = self.metrics.lock() else {
            return;
        };
        if !metrics.contains_key(&key) && metrics.len() >= RUNTIME_METRIC_SERIES_LIMIT {
            return;
        }
        let aggregate = metrics.entry(key).or_default();
        if let Some(duration_ms) = duration_ms {
            aggregate.observe_duration(duration_ms);
        } else {
            aggregate.increment();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeLogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct RuntimeLogContext {
    logical_session_id: Option<String>,
    binding_id: Option<String>,
    process_instance_id: Option<String>,
    activation_generation: Option<i64>,
    switch_id: Option<String>,
    agent_id: Option<String>,
    adapter_id: Option<String>,
    adapter_version: Option<String>,
    provider_profile_id: Option<String>,
    process_spawn_fingerprint_prefix: Option<String>,
    native_session_id_hash: Option<String>,
    operation: &'static str,
    restore_outcome: Option<RuntimeMetricResult>,
}

impl fmt::Debug for RuntimeLogContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeLogContext")
            .field("logical_session_id", &self.logical_session_id)
            .field("binding_id", &self.binding_id)
            .field("process_instance_id", &self.process_instance_id)
            .field("activation_generation", &self.activation_generation)
            .field("switch_id", &self.switch_id)
            .field("agent_id", &self.agent_id)
            .field("adapter_id", &self.adapter_id)
            .field("adapter_version", &self.adapter_version)
            .field("provider_profile_id", &self.provider_profile_id)
            .field(
                "process_spawn_fingerprint_prefix",
                &self.process_spawn_fingerprint_prefix,
            )
            .field("native_session_id_hash", &self.native_session_id_hash)
            .field("operation", &self.operation)
            .field("restore_outcome", &self.restore_outcome)
            .finish()
    }
}

impl RuntimeLogContext {
    pub fn new(operation: &'static str) -> Self {
        Self {
            operation,
            ..Self::default()
        }
    }

    pub fn for_operation(&self, operation: &'static str) -> Self {
        let mut context = self.clone();
        context.operation = operation;
        context
    }

    pub fn with_logical_session_id(mut self, value: impl fmt::Display) -> Self {
        self.logical_session_id = Some(value.to_string());
        self
    }

    pub fn with_binding_id(mut self, value: impl fmt::Display) -> Self {
        self.binding_id = Some(value.to_string());
        self
    }

    pub fn with_process_instance_id(mut self, value: impl fmt::Display) -> Self {
        self.process_instance_id = Some(value.to_string());
        self
    }

    pub fn with_activation_generation(mut self, value: i64) -> Self {
        self.activation_generation = Some(value);
        self
    }

    pub fn with_switch_id(mut self, value: impl fmt::Display) -> Self {
        self.switch_id = Some(value.to_string());
        self
    }

    pub fn with_agent_id(mut self, value: impl fmt::Display) -> Self {
        self.agent_id = Some(value.to_string());
        self
    }

    pub fn with_adapter_id(mut self, value: impl fmt::Display) -> Self {
        self.adapter_id = Some(value.to_string());
        self
    }

    pub fn with_adapter_version(mut self, value: impl fmt::Display) -> Self {
        self.adapter_version = Some(value.to_string());
        self
    }

    pub fn with_provider_profile_id(mut self, value: impl fmt::Display) -> Self {
        self.provider_profile_id = Some(value.to_string());
        self
    }

    pub fn with_process_spawn_fingerprint(mut self, value: &str) -> Self {
        self.process_spawn_fingerprint_prefix = Some(fingerprint_prefix(value));
        self
    }

    pub fn with_native_session_id(mut self, value: &str) -> Self {
        self.native_session_id_hash = Some(native_session_id_hash(value));
        self
    }

    pub fn with_restore_outcome(mut self, value: RuntimeMetricResult) -> Self {
        self.restore_outcome = Some(value);
        self
    }

    pub fn emit(
        &self,
        level: RuntimeLogLevel,
        event_code: &'static str,
        result: RuntimeMetricResult,
        error_code: Option<&str>,
        duration_ms: Option<u64>,
    ) {
        let span = tracing::info_span!(
            target: "vibex_runtime",
            "runtime_operation",
            logical_session_id = field::Empty,
            binding_id = field::Empty,
            process_instance_id = field::Empty,
            activation_generation = field::Empty,
            switch_id = field::Empty,
            agent_id = field::Empty,
            adapter_id = field::Empty,
            adapter_version = field::Empty,
            provider_profile_id = field::Empty,
            process_spawn_fingerprint_prefix = field::Empty,
            native_session_id_hash = field::Empty,
            operation = self.operation,
            restore_outcome = field::Empty,
        );
        record_optional(
            &span,
            "logical_session_id",
            self.logical_session_id.as_deref(),
        );
        record_optional(&span, "binding_id", self.binding_id.as_deref());
        record_optional(
            &span,
            "process_instance_id",
            self.process_instance_id.as_deref(),
        );
        if let Some(value) = self.activation_generation {
            span.record("activation_generation", value);
        }
        record_optional(&span, "switch_id", self.switch_id.as_deref());
        record_optional(&span, "agent_id", self.agent_id.as_deref());
        record_optional(&span, "adapter_id", self.adapter_id.as_deref());
        record_optional(&span, "adapter_version", self.adapter_version.as_deref());
        record_optional(
            &span,
            "provider_profile_id",
            self.provider_profile_id.as_deref(),
        );
        record_optional(
            &span,
            "process_spawn_fingerprint_prefix",
            self.process_spawn_fingerprint_prefix.as_deref(),
        );
        record_optional(
            &span,
            "native_session_id_hash",
            self.native_session_id_hash.as_deref(),
        );
        if let Some(value) = self.restore_outcome {
            span.record("restore_outcome", value.as_str());
        }
        let error_code = error_code.map(stable_code);
        let _entered = span.enter();
        match level {
            RuntimeLogLevel::Debug => tracing::debug!(
                target: "vibex_runtime",
                event_code,
                result = result.as_str(),
                error_code = error_code.as_deref(),
                duration_ms,
                "runtime operation"
            ),
            RuntimeLogLevel::Info => tracing::info!(
                target: "vibex_runtime",
                event_code,
                result = result.as_str(),
                error_code = error_code.as_deref(),
                duration_ms,
                "runtime operation"
            ),
            RuntimeLogLevel::Warn => tracing::warn!(
                target: "vibex_runtime",
                event_code,
                result = result.as_str(),
                error_code = error_code.as_deref(),
                duration_ms,
                "runtime operation"
            ),
            RuntimeLogLevel::Error => tracing::error!(
                target: "vibex_runtime",
                event_code,
                result = result.as_str(),
                error_code = error_code.as_deref(),
                duration_ms,
                "runtime operation"
            ),
        }
    }
}

fn record_optional(span: &tracing::Span, field_name: &'static str, value: Option<&str>) {
    if let Some(value) = value {
        span.record(field_name, value);
    }
}

fn fingerprint_prefix(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, ':' | '-'))
        .take(FINGERPRINT_PREFIX_LENGTH)
        .collect()
}

fn native_session_id_hash(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(NATIVE_SESSION_HASH_DOMAIN);
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn stable_code(value: &str) -> String {
    let value = value.trim();
    if value.is_empty()
        || value.len() > STABLE_CODE_LIMIT
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
    {
        "invalid_code".to_string()
    } else {
        value.to_string()
    }
}

fn unix_timestamp_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    use super::*;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    #[derive(Clone, Default)]
    struct RecordingSubscriber {
        next_id: Arc<AtomicU64>,
        output: Arc<Mutex<String>>,
    }

    impl RecordingSubscriber {
        fn capture(&self) -> String {
            self.output.lock().unwrap().clone()
        }
    }

    impl Subscriber for RecordingSubscriber {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, attributes: &Attributes<'_>) -> Id {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
            let mut visitor = RecordingVisitor(&self.output);
            attributes.record(&mut visitor);
            Id::from_u64(id)
        }

        fn record(&self, _span: &Id, values: &Record<'_>) {
            values.record(&mut RecordingVisitor(&self.output));
        }

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            event.record(&mut RecordingVisitor(&self.output));
        }

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}
    }

    struct RecordingVisitor<'a>(&'a Mutex<String>);

    impl Visit for RecordingVisitor<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            let mut output = self.0.lock().unwrap();
            output.push_str(field.name());
            output.push('=');
            output.push_str(&format!("{value:?}"));
            output.push('\n');
        }
    }

    #[test]
    fn aggregates_durations_and_counters_in_stable_series() {
        let observability = RuntimeObservability::new();
        observability.observe_duration_ms(
            RuntimeMetricName::SessionOpenDuration,
            Some(RuntimeMetricOperation::Resume),
            RuntimeMetricResult::Success,
            12,
        );
        observability.observe_duration_ms(
            RuntimeMetricName::SessionOpenDuration,
            Some(RuntimeMetricOperation::Resume),
            RuntimeMetricResult::Success,
            20,
        );
        observability.increment(
            RuntimeMetricName::DuplicateSubmissionPrevented,
            None,
            RuntimeMetricResult::Prevented,
        );

        let snapshot = observability.snapshot();
        assert_eq!(snapshot.series.len(), 2);
        let duration = snapshot
            .series
            .iter()
            .find(|series| series.name == RuntimeMetricName::SessionOpenDuration)
            .unwrap();
        assert_eq!(duration.count, 2);
        assert_eq!(duration.duration_total_ms, Some(32));
        assert_eq!(duration.duration_min_ms, Some(12));
        assert_eq!(duration.duration_max_ms, Some(20));
        assert_eq!(duration.duration_last_ms, Some(20));
    }

    #[test]
    fn concurrent_recording_is_lossless_and_bounded() {
        let observability = Arc::new(RuntimeObservability::new());
        let workers = (0..8)
            .map(|_| {
                let observability = observability.clone();
                thread::spawn(move || {
                    for _ in 0..1_000 {
                        observability.increment(
                            RuntimeMetricName::Acquire,
                            Some(RuntimeMetricOperation::Process),
                            RuntimeMetricResult::Reused,
                        );
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        let snapshot = observability.snapshot();
        assert_eq!(snapshot.series.len(), 1);
        assert_eq!(snapshot.series[0].count, 8_000);
        assert!(snapshot.series.len() <= snapshot.series_limit);
    }

    #[test]
    fn log_context_keeps_only_fingerprint_prefix_and_native_hash() {
        let full_fingerprint = "sha256:0123456789abcdefSUPER_SECRET_FINGERPRINT";
        let native_session = "native-session-secret-sentinel";
        let context = RuntimeLogContext::new("restore")
            .with_process_spawn_fingerprint(full_fingerprint)
            .with_native_session_id(native_session)
            .with_restore_outcome(RuntimeMetricResult::AuthenticationRequired);
        let rendered = format!("{context:?}");
        assert!(rendered.contains("sha256:012345678"));
        assert!(!rendered.contains(full_fingerprint));
        assert!(!rendered.contains(native_session));
        assert!(rendered.contains("AuthenticationRequired"));
    }

    #[test]
    fn stable_code_drops_payload_characters_and_bounds_length() {
        let code = stable_code(&format!("bad code token=secret {}", "x".repeat(200)));
        assert_eq!(code, "invalid_code");
        assert_eq!(
            stable_code("runtime_switch_failed"),
            "runtime_switch_failed"
        );
    }

    #[test]
    fn tracing_capture_contains_only_safe_runtime_context() {
        let subscriber = RecordingSubscriber::default();
        let full_fingerprint = "sha256:0123456789abcdefFULL_FINGERPRINT_SENTINEL";
        let native_session_id = "native-session-id-sentinel";
        tracing::subscriber::with_default(subscriber.clone(), || {
            RuntimeLogContext::new("restore")
                .with_logical_session_id("logical-session-safe")
                .with_process_spawn_fingerprint(full_fingerprint)
                .with_native_session_id(native_session_id)
                .with_restore_outcome(RuntimeMetricResult::AuthenticationRequired)
                .emit(
                    RuntimeLogLevel::Warn,
                    "runtime_restore_failed",
                    RuntimeMetricResult::AuthenticationRequired,
                    Some("provider failed bearer secret-token prompt payload"),
                    Some(7),
                );
        });

        let output = subscriber.capture();
        assert!(output.contains("process_spawn_fingerprint_prefix"));
        assert!(output.contains("sha256:012345678"));
        assert!(output.contains("native_session_id_hash"));
        assert!(output.contains("restore_outcome"));
        assert!(output.contains("invalid_code"));
        for forbidden in [
            full_fingerprint,
            native_session_id,
            "secret-token",
            "prompt payload",
            "provider failed",
        ] {
            assert!(
                !output.contains(forbidden),
                "tracing leaked {forbidden}: {output}"
            );
        }
    }
}

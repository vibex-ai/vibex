use serde::{Deserialize, Serialize};

use crate::{
    AgentId, MessageSubmissionId, ProjectId, ProviderProfileId, RuntimeAuthSource,
    RuntimeBindingId, UsageExecutionId, VibexSessionId, WorkspaceId,
};

pub const MAX_AGENT_USAGE_TOKEN_VALUE: u64 = i64::MAX as u64;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AgentUsageTokenValues {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub thought_tokens: Option<u64>,
    pub cached_read_tokens: Option<u64>,
    pub cached_write_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

impl AgentUsageTokenValues {
    pub fn any_reported(&self) -> bool {
        self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.thought_tokens.is_some()
            || self.cached_read_tokens.is_some()
            || self.cached_write_tokens.is_some()
            || self.total_tokens.is_some()
    }

    pub fn values(&self) -> [Option<u64>; 6] {
        [
            self.input_tokens,
            self.output_tokens,
            self.thought_tokens,
            self.cached_read_tokens,
            self.cached_write_tokens,
            self.total_tokens,
        ]
    }

    pub fn validate(&self) -> bool {
        self.values()
            .into_iter()
            .flatten()
            .all(|value| value <= MAX_AGENT_USAGE_TOKEN_VALUE)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AgentUsageReportedFields {
    pub input_tokens: bool,
    pub output_tokens: bool,
    pub thought_tokens: bool,
    pub cached_read_tokens: bool,
    pub cached_write_tokens: bool,
    pub total_tokens: bool,
}

impl AgentUsageReportedFields {
    pub fn from_tokens(tokens: &AgentUsageTokenValues) -> Self {
        Self {
            input_tokens: tokens.input_tokens.is_some(),
            output_tokens: tokens.output_tokens.is_some(),
            thought_tokens: tokens.thought_tokens.is_some(),
            cached_read_tokens: tokens.cached_read_tokens.is_some(),
            cached_write_tokens: tokens.cached_write_tokens.is_some(),
            total_tokens: tokens.total_tokens.is_some(),
        }
    }

    pub fn any(self) -> bool {
        self.input_tokens
            || self.output_tokens
            || self.thought_tokens
            || self.cached_read_tokens
            || self.cached_write_tokens
            || self.total_tokens
    }

    pub fn all(self) -> bool {
        self.input_tokens
            && self.output_tokens
            && self.thought_tokens
            && self.cached_read_tokens
            && self.cached_write_tokens
            && self.total_tokens
    }

    pub fn merge(&mut self, other: Self) {
        self.input_tokens |= other.input_tokens;
        self.output_tokens |= other.output_tokens;
        self.thought_tokens |= other.thought_tokens;
        self.cached_read_tokens |= other.cached_read_tokens;
        self.cached_write_tokens |= other.cached_write_tokens;
        self.total_tokens |= other.total_tokens;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUsageExecutionStatus {
    Dispatched,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUsageObservationSource {
    PromptResponse,
    SessionUsageUpdate,
    /// One API request's own total, taken from an adapter whose `usage_update`
    /// reports per-request totals instead of context occupancy.
    RequestSample,
}

/// What an adapter's reported `usage` counters actually cover.
///
/// The ACP schema documents `Usage` as session-cumulative ("Sum of all token
/// types across session", "Total input tokens across all turns"), so
/// [`Session`](Self::Session) stays the contract for unknown adapters. Shipped
/// adapters disagree with the schema in two different directions, and accounting
/// is wrong by orders of magnitude when the difference is ignored:
///
/// - `claude-agent-acp` sums every API request of a turn but resets the tally on
///   each turn activation, so its numbers are [`Turn`](Self::Turn)-scoped.
/// - `codex-acp` forwards Codex's `last_token_usage` and never its
///   `total_token_usage`, so its numbers cover a single
///   [`Request`](Self::Request) — the last one of the turn.
///
/// Only [`Session`](Self::Session) counters may be differenced against a
/// checkpoint. The others are absolute per-turn readings: differencing them
/// cancels out most of the usage, and their natural decreases look like counter
/// resets, which silently drops whole turns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUsageCounterScope {
    /// Monotonic session-cumulative counters, per the ACP schema.
    #[default]
    Session,
    /// Absolute per-turn totals that reset when the next turn starts.
    Turn,
    /// A single API request's counters — a lower bound on the turn's usage.
    Request,
}

impl AgentUsageCounterScope {
    /// Whether observations may be differenced against the stream checkpoint.
    pub fn is_cumulative(self) -> bool {
        matches!(self, Self::Session)
    }

    /// Whether a turn's recorded usage understates what the provider bills.
    pub fn is_lower_bound(self) -> bool {
        matches!(self, Self::Request)
    }
}

/// What an agent's ACP adapter actually reports, as read from its shipped
/// source rather than assumed from the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentUsageReportingContract {
    pub counter_scope: AgentUsageCounterScope,
    /// Whether each `usage_update` carries one API request's total tokens.
    ///
    /// The ACP schema defines `usage_update.used` as "Tokens currently in
    /// context", which must never be summed. An adapter that instead forwards
    /// its backend's per-request counter turns that stream into the only
    /// per-request signal available, and summing it is then the difference
    /// between counting one request per turn and counting all of them.
    pub usage_update_is_request_total: bool,
}

/// Resolves the usage contract an agent's ACP adapter actually implements.
///
/// Entries are only added once an adapter's reporting has been read directly
/// from its shipped source. Everything else keeps the schema-mandated
/// [`AgentUsageCounterScope::Session`] with no per-request stream, and an
/// adapter that contradicts that at runtime has the contradicting turn counted
/// from its own reading rather than dropped.
pub fn agent_usage_reporting_contract(agent_id: &AgentId) -> AgentUsageReportingContract {
    match agent_id.as_str() {
        // claude-agent-acp: `session.accumulatedUsage` is reset on turn
        // activation and summed across the turn's API requests. Its
        // `usage_update.used` is context occupancy, per the schema.
        "claude" => AgentUsageReportingContract {
            counter_scope: AgentUsageCounterScope::Turn,
            usage_update_is_request_total: false,
        },
        // codex-acp: `buildPromptUsage(sessionState.lastTokenUsage)` — the last
        // request only; `totalTokenUsage` is tracked but never sent. Its
        // `usage_update.used` is `lastTokenUsage.totalTokens`, so every model
        // response emits that request's own total.
        "codex" => AgentUsageReportingContract {
            counter_scope: AgentUsageCounterScope::Request,
            usage_update_is_request_total: true,
        },
        _ => AgentUsageReportingContract {
            counter_scope: AgentUsageCounterScope::Session,
            usage_update_is_request_total: false,
        },
    }
}

/// Convenience accessor for the contract's counter scope.
pub fn agent_usage_counter_scope(agent_id: &AgentId) -> AgentUsageCounterScope {
    agent_usage_reporting_contract(agent_id).counter_scope
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUsageCounterOrigin {
    KnownZero,
    Resumed,
    RestoredCheckpoint,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUsageCoverage {
    Complete,
    Partial,
    BaselineOnly,
    Unreported,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageStreamAttribution {
    pub session_id: VibexSessionId,
    pub binding_id: RuntimeBindingId,
    pub activation_generation: i64,
    pub agent_id: AgentId,
    pub auth_source: RuntimeAuthSource,
    pub auth_source_revision: i64,
    /// Concrete model reported for the stream. `None` means the Agent used its
    /// own default and did not disclose an effective model.
    pub model_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageExecutionContext {
    pub usage_execution_id: UsageExecutionId,
    pub message_submission_id: Option<MessageSubmissionId>,
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub stream: AgentUsageStreamAttribution,
}

impl AgentUsageExecutionContext {
    pub fn dispatched_at(&self, dispatched_at_ms: i64) -> AgentUsageExecution {
        AgentUsageExecution {
            usage_execution_id: self.usage_execution_id.clone(),
            message_submission_id: self.message_submission_id.clone(),
            project_id: self.project_id.clone(),
            workspace_id: self.workspace_id.clone(),
            stream: self.stream.clone(),
            dispatched_at_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageExecution {
    pub usage_execution_id: UsageExecutionId,
    pub message_submission_id: Option<MessageSubmissionId>,
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub stream: AgentUsageStreamAttribution,
    pub dispatched_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageExecutionStatusUpdate {
    pub execution: AgentUsageExecution,
    pub status: AgentUsageExecutionStatus,
    pub completed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageObservation {
    pub stream: AgentUsageStreamAttribution,
    pub execution: Option<AgentUsageExecution>,
    pub counter_origin: AgentUsageCounterOrigin,
    /// What the reported counters cover. Defaults to the schema contract so
    /// replayed backups and older payloads keep their original meaning.
    #[serde(default)]
    pub counter_scope: AgentUsageCounterScope,
    pub observation_sequence: u64,
    pub cumulative: AgentUsageTokenValues,
    pub context_window_used_tokens: Option<u64>,
    pub context_window_size_tokens: Option<u64>,
    pub source: AgentUsageObservationSource,
    pub observed_at_ms: i64,
}

impl AgentUsageObservation {
    pub fn validate(&self) -> bool {
        self.stream.activation_generation >= 0
            && self.observation_sequence <= i64::MAX as u64
            && self.cumulative.validate()
            && self
                .context_window_used_tokens
                .is_none_or(|value| value <= MAX_AGENT_USAGE_TOKEN_VALUE)
            && self
                .context_window_size_tokens
                .is_none_or(|value| value > 0 && value <= MAX_AGENT_USAGE_TOKEN_VALUE)
            && self.execution.as_ref().is_none_or(|execution| {
                execution.stream == self.stream && execution.dispatched_at_ms >= 0
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTurnUsageFact {
    pub usage_execution_id: UsageExecutionId,
    pub message_submission_id: Option<MessageSubmissionId>,
    pub session_id: VibexSessionId,
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub binding_id: RuntimeBindingId,
    pub activation_generation: i64,
    pub reset_epoch: i64,
    pub agent_id: AgentId,
    pub auth_source: RuntimeAuthSource,
    pub auth_source_revision: i64,
    pub model_id: Option<String>,
    pub execution_status: AgentUsageExecutionStatus,
    pub delta: AgentUsageTokenValues,
    pub cumulative_after: AgentUsageTokenValues,
    /// Contract the delta was derived under. Legacy rows written before the
    /// contract was tracked stay `Session`, matching how they were computed.
    #[serde(default)]
    pub counter_scope: AgentUsageCounterScope,
    /// API requests observed inside this turn, when the adapter reports them.
    /// `None` means the adapter gives no per-request signal, so the turn is the
    /// finest unit that can honestly be counted.
    #[serde(default)]
    pub api_requests: Option<u64>,
    pub context_window_used_tokens: Option<u64>,
    pub context_window_size_tokens: Option<u64>,
    pub reported_fields: AgentUsageReportedFields,
    pub coverage: AgentUsageCoverage,
    pub last_source: Option<AgentUsageObservationSource>,
    pub reset_reason: Option<String>,
    pub dispatched_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub last_observed_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUsageRange {
    Today,
    #[default]
    Last7Days,
    Last30Days,
    AllTime,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUsageDimension {
    #[default]
    Time,
    Agent,
    Project,
    ModelProvider,
    Model,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUsageTrendMetric {
    Requests,
    #[default]
    TotalTokens,
    InputTokens,
    OutputTokens,
    CachedTokens,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUsageSortMetric {
    Requests,
    #[default]
    TotalTokens,
    InputTokens,
    OutputTokens,
    CachedTokens,
    CacheHitRate,
    LastActivity,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUsageSortDirection {
    Ascending,
    #[default]
    Descending,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentUsageTimeZone {
    #[default]
    System,
    FixedOffset {
        offset_minutes: i32,
    },
}

impl AgentUsageTimeZone {
    pub fn validate(&self) -> bool {
        match self {
            Self::System => true,
            Self::FixedOffset { offset_minutes } => (-1_439..=1_439).contains(offset_minutes),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AgentUsageStatisticsRequest {
    pub range: AgentUsageRange,
    pub agent_ids: Vec<AgentId>,
    pub project_ids: Vec<ProjectId>,
    pub provider_profile_ids: Vec<ProviderProfileId>,
    pub model_ids: Vec<String>,
    pub session_ids: Vec<VibexSessionId>,
    pub dimension: AgentUsageDimension,
    pub trend_metric: AgentUsageTrendMetric,
    pub sort_metric: AgentUsageSortMetric,
    pub sort_direction: AgentUsageSortDirection,
    pub time_zone: AgentUsageTimeZone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUsageMetricCoverage {
    Complete,
    Derived,
    Partial,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageMetricValue {
    pub value: Option<u64>,
    pub coverage: AgentUsageMetricCoverage,
    pub known_requests: u64,
    #[serde(default)]
    pub derived_requests: u64,
    pub total_requests: u64,
}

impl AgentUsageMetricValue {
    pub fn unknown(total_requests: u64) -> Self {
        Self {
            value: None,
            coverage: AgentUsageMetricCoverage::Unknown,
            known_requests: 0,
            derived_requests: 0,
            total_requests,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageCacheHitRate {
    pub basis_points: Option<u32>,
    pub cached_read_tokens: u64,
    pub denominator_tokens: u64,
    pub eligible_requests: u64,
    pub total_requests: u64,
    pub coverage: AgentUsageMetricCoverage,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageCoverageSummary {
    pub complete_requests: u64,
    pub partial_requests: u64,
    pub baseline_only_requests: u64,
    pub unreported_requests: u64,
    pub unsupported_requests: u64,
    pub total_requests: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageAggregate {
    pub requests: u64,
    /// API requests behind those turns, when the adapters involved report them.
    /// Absent when no turn in the aggregate carries a per-request signal.
    #[serde(default)]
    pub api_requests: Option<u64>,
    pub total_tokens: AgentUsageMetricValue,
    pub input_tokens: AgentUsageMetricValue,
    pub output_tokens: AgentUsageMetricValue,
    pub cached_tokens: AgentUsageMetricValue,
    pub thought_tokens: AgentUsageMetricValue,
    pub cached_write_tokens: AgentUsageMetricValue,
    pub cache_hit_rate: AgentUsageCacheHitRate,
    pub coverage: AgentUsageCoverageSummary,
    pub last_activity_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageEffectiveRange {
    pub start_at_ms: i64,
    pub end_at_ms: i64,
    pub bucket_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageTrendBucket {
    pub id: String,
    pub label: String,
    pub start_at_ms: i64,
    pub end_at_ms: i64,
    pub aggregate: AgentUsageAggregate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageDailyModelUsage {
    pub model_id: Option<String>,
    pub label: String,
    pub requests: u64,
    pub total_tokens: AgentUsageMetricValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageAnnualDay {
    pub id: String,
    pub label: String,
    pub start_at_ms: i64,
    pub end_at_ms: i64,
    pub requests: u64,
    pub total_tokens: AgentUsageMetricValue,
    pub models: Vec<AgentUsageDailyModelUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageAnnualProjection {
    pub effective_range: AgentUsageEffectiveRange,
    pub days: Vec<AgentUsageAnnualDay>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageDimensionRow {
    pub id: String,
    pub label: String,
    pub aggregate: AgentUsageAggregate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageFilterOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageFilterOptions {
    pub agents: Vec<AgentUsageFilterOption>,
    pub projects: Vec<AgentUsageFilterOption>,
    pub provider_profiles: Vec<AgentUsageFilterOption>,
    pub models: Vec<AgentUsageFilterOption>,
    pub sessions: Vec<AgentUsageFilterOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsageStatistics {
    pub generated_at_ms: i64,
    pub effective_range: AgentUsageEffectiveRange,
    pub totals: AgentUsageAggregate,
    pub trend_buckets: Vec<AgentUsageTrendBucket>,
    pub dimension_rows: Vec<AgentUsageDimensionRow>,
    pub filter_options: AgentUsageFilterOptions,
    #[serde(default)]
    pub annual: Option<AgentUsageAnnualProjection>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_id_is_stable_for_message_submission() {
        let submission = MessageSubmissionId::parse("submission_abc123").unwrap();
        let first = UsageExecutionId::from_message_submission(&submission);
        let second = UsageExecutionId::from_message_submission(&submission);
        assert_eq!(first, second);
        assert_eq!(first.as_str(), "usage_execution_abc123");
    }

    #[test]
    fn usage_values_preserve_missing_and_reject_sqlite_overflow() {
        let partial = AgentUsageTokenValues {
            input_tokens: Some(0),
            output_tokens: None,
            ..AgentUsageTokenValues::default()
        };
        assert!(partial.any_reported());
        assert!(partial.validate());
        assert!(AgentUsageReportedFields::from_tokens(&partial).input_tokens);
        assert!(!AgentUsageReportedFields::from_tokens(&partial).output_tokens);

        let overflow = AgentUsageTokenValues {
            total_tokens: Some(i64::MAX as u64 + 1),
            ..AgentUsageTokenValues::default()
        };
        assert!(!overflow.validate());
    }

    #[test]
    fn request_defaults_to_honest_seven_day_total_view() {
        let request = AgentUsageStatisticsRequest::default();
        assert_eq!(request.range, AgentUsageRange::Last7Days);
        assert_eq!(request.dimension, AgentUsageDimension::Time);
        assert_eq!(request.trend_metric, AgentUsageTrendMetric::TotalTokens);
        assert!(request.time_zone.validate());
    }

    #[test]
    fn contracts_have_no_pricing_fields() {
        let serialized = serde_json::to_string(&AgentUsageTokenValues::default()).unwrap();
        for forbidden in ["cost", "price", "currency", "amount"] {
            assert!(!serialized.to_ascii_lowercase().contains(forbidden));
        }
    }

    #[test]
    fn statistics_without_annual_projection_remain_readable() {
        let metric = AgentUsageMetricValue::unknown(0);
        let statistics = AgentUsageStatistics {
            generated_at_ms: 1,
            effective_range: AgentUsageEffectiveRange {
                start_at_ms: 0,
                end_at_ms: 1,
                bucket_kind: "day".to_string(),
            },
            totals: AgentUsageAggregate {
                requests: 0,
                api_requests: None,
                total_tokens: metric.clone(),
                input_tokens: metric.clone(),
                output_tokens: metric.clone(),
                cached_tokens: metric.clone(),
                thought_tokens: metric.clone(),
                cached_write_tokens: metric,
                cache_hit_rate: AgentUsageCacheHitRate {
                    basis_points: None,
                    cached_read_tokens: 0,
                    denominator_tokens: 0,
                    eligible_requests: 0,
                    total_requests: 0,
                    coverage: AgentUsageMetricCoverage::Unknown,
                },
                coverage: AgentUsageCoverageSummary::default(),
                last_activity_at_ms: None,
            },
            trend_buckets: Vec::new(),
            dimension_rows: Vec::new(),
            filter_options: AgentUsageFilterOptions::default(),
            annual: None,
        };
        let mut serialized = serde_json::to_value(statistics).unwrap();
        serialized.as_object_mut().unwrap().remove("annual");
        let decoded: AgentUsageStatistics = serde_json::from_value(serialized).unwrap();
        assert!(decoded.annual.is_none());
    }
}

use serde::{Deserialize, Serialize};

use crate::{
    AgentId, MessageSubmissionId, ProjectId, ProviderProfileId, RuntimeBindingId, UsageExecutionId,
    VibexSessionId, WorkspaceId,
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
    pub provider_profile_id: ProviderProfileId,
    pub model_id: String,
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
    pub provider_profile_id: ProviderProfileId,
    pub model_id: String,
    pub execution_status: AgentUsageExecutionStatus,
    pub delta: AgentUsageTokenValues,
    pub cumulative_after: AgentUsageTokenValues,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

impl Default for AgentUsageStatisticsRequest {
    fn default() -> Self {
        Self {
            range: AgentUsageRange::default(),
            agent_ids: Vec::new(),
            project_ids: Vec::new(),
            provider_profile_ids: Vec::new(),
            model_ids: Vec::new(),
            session_ids: Vec::new(),
            dimension: AgentUsageDimension::default(),
            trend_metric: AgentUsageTrendMetric::default(),
            sort_metric: AgentUsageSortMetric::default(),
            sort_direction: AgentUsageSortDirection::default(),
            time_zone: AgentUsageTimeZone::default(),
        }
    }
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
}

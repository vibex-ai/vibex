use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{
    Datelike, Duration, FixedOffset, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone,
};
use vibex_agent::AgentUsageTelemetryEvent;
use vibex_core::{
    AgentTokenUsage, AgentTurnUsageFact, AgentUsageAggregate, AgentUsageAnnualDay,
    AgentUsageAnnualProjection, AgentUsageCacheHitRate, AgentUsageCounterOrigin,
    AgentUsageCoverage, AgentUsageCoverageSummary, AgentUsageDailyModelUsage, AgentUsageDimension,
    AgentUsageDimensionRow, AgentUsageEffectiveRange, AgentUsageFilterOption,
    AgentUsageFilterOptions, AgentUsageMetricCoverage, AgentUsageMetricValue, AgentUsageRange,
    AgentUsageSortDirection, AgentUsageSortMetric, AgentUsageStatistics,
    AgentUsageStatisticsRequest, AgentUsageTimeZone, AgentUsageTrendBucket, RuntimeBindingId,
    VibexError, VibexResult, VibexSessionId, unix_timestamp_ms,
};
use vibex_db::{
    AgentSessionRuntimeRepository, AgentUsageFactProjection, AgentUsageRepository,
    MAX_AGENT_USAGE_QUERY_ROWS, RuntimeBindingRepository, apply_migrations, open_database,
};

#[derive(Clone)]
pub struct AgentUsageService {
    db_path: PathBuf,
    token_usage_cache: Arc<Mutex<BTreeMap<(String, String), Option<AgentTokenUsage>>>>,
}

impl AgentUsageService {
    pub fn new(db_path: impl Into<PathBuf>) -> VibexResult<Self> {
        let db_path = db_path.into();
        let mut connection = open_database(&db_path)?;
        apply_migrations(&mut connection)?;
        Ok(Self {
            db_path,
            token_usage_cache: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    pub fn apply_telemetry_event(&self, event: AgentUsageTelemetryEvent) -> VibexResult<bool> {
        let mut connection = open_database(&self.db_path)?;
        apply_migrations(&mut connection)?;
        match event {
            AgentUsageTelemetryEvent::ExecutionDispatched {
                execution,
                counter_origin,
            } => {
                if counter_origin == AgentUsageCounterOrigin::KnownZero
                    && let Err(error) = RuntimeBindingRepository::claim_usage_zero_baseline(
                        &connection,
                        &execution.stream.binding_id,
                        execution.stream.activation_generation,
                        &execution.usage_execution_id,
                    )
                {
                    tracing::warn!(
                        target: "vibex_desktop",
                        error_code = %error.code,
                        "Agent usage zero-baseline claim failed after prompt dispatch"
                    );
                }
                AgentUsageRepository::record_execution(&connection, &execution)
            }
            AgentUsageTelemetryEvent::Observation(observation) => {
                let cache_key = (
                    observation.stream.session_id.as_str().to_string(),
                    observation.stream.binding_id.as_str().to_string(),
                );
                let changed =
                    AgentUsageRepository::apply_observation(&mut connection, &observation)?
                        .changed();
                if changed && let Ok(mut cache) = self.token_usage_cache.lock() {
                    cache.remove(&cache_key);
                }
                Ok(changed)
            }
            AgentUsageTelemetryEvent::ExecutionStatus(update) => {
                AgentUsageRepository::record_execution_status(&connection, &update)
            }
        }
    }

    pub fn latest_token_usage(
        &self,
        session_id: &VibexSessionId,
        binding_id: Option<&RuntimeBindingId>,
    ) -> VibexResult<Option<AgentTokenUsage>> {
        let mut connection = None;
        let binding_id = match binding_id {
            Some(binding_id) => Some(binding_id.clone()),
            None => {
                let mut opened = open_database(&self.db_path)?;
                apply_migrations(&mut opened)?;
                let binding_id =
                    AgentSessionRuntimeRepository::get_runtime_state(&opened, session_id)?
                        .and_then(|state| state.current_binding_id);
                connection = Some(opened);
                binding_id
            }
        };
        let Some(binding_id) = binding_id else {
            return Ok(None);
        };
        let cache_key = (
            session_id.as_str().to_string(),
            binding_id.as_str().to_string(),
        );
        if let Ok(cache) = self.token_usage_cache.lock()
            && let Some(usage) = cache.get(&cache_key)
        {
            return Ok(usage.clone());
        }
        let connection = match connection {
            Some(connection) => connection,
            None => {
                let mut connection = open_database(&self.db_path)?;
                apply_migrations(&mut connection)?;
                connection
            }
        };
        let usage = AgentUsageRepository::latest_observed_fact_for_binding(
            &connection,
            session_id,
            &binding_id,
        )
        .map(|fact| fact.map(token_usage_from_fact))?;
        if let Ok(mut cache) = self.token_usage_cache.lock() {
            cache.insert(cache_key, usage.clone());
        }
        Ok(usage)
    }

    pub fn query_statistics(
        &self,
        request: AgentUsageStatisticsRequest,
    ) -> VibexResult<AgentUsageStatistics> {
        self.query_statistics_at(request, unix_timestamp_ms())
    }

    fn query_statistics_at(
        &self,
        request: AgentUsageStatisticsRequest,
        generated_at_ms: i64,
    ) -> VibexResult<AgentUsageStatistics> {
        validate_request(&request)?;
        let time_zone = ResolvedUsageTimeZone::resolve(&request.time_zone)?;
        let bounds = query_bounds(request.range, &time_zone, generated_at_ms)?;
        let annual_buckets = build_annual_time_buckets(&time_zone, generated_at_ms)?;
        let annual_start_at_ms = annual_buckets
            .first()
            .map(|bucket| bucket.start_at_ms)
            .unwrap_or(bounds.start_at_ms);
        let mut connection = open_database(&self.db_path)?;
        apply_migrations(&mut connection)?;
        let all_facts = AgentUsageRepository::list_facts_in_range(
            &connection,
            bounds.start_at_ms.min(annual_start_at_ms),
            bounds.end_at_ms,
            MAX_AGENT_USAGE_QUERY_ROWS.saturating_add(1),
        )?;
        if all_facts.len() > MAX_AGENT_USAGE_QUERY_ROWS {
            return Err(VibexError::validation(
                "agent_usage_query_too_large",
                "Agent usage query exceeded the supported row limit",
            ));
        }
        let filter_options = build_filter_options(&all_facts);
        let facts = all_facts
            .iter()
            .filter(|projection| fact_is_in_bounds(projection, &bounds))
            .filter(|projection| matches_filters(projection, &request))
            .collect::<Vec<_>>();
        let annual_facts = all_facts
            .iter()
            .filter(|projection| {
                projection.fact.dispatched_at_ms >= annual_start_at_ms
                    && projection.fact.dispatched_at_ms < bounds.end_at_ms
            })
            .filter(|projection| matches_filters(projection, &request))
            .collect::<Vec<_>>();
        let buckets =
            build_time_buckets(request.range, &time_zone, generated_at_ms, facts.as_slice())?;
        let trend_buckets = buckets
            .iter()
            .map(|bucket| {
                let bucket_facts = facts
                    .iter()
                    .copied()
                    .filter(|projection| {
                        projection.fact.dispatched_at_ms >= bucket.start_at_ms
                            && projection.fact.dispatched_at_ms < bucket.end_at_ms
                    })
                    .collect::<Vec<_>>();
                Ok(AgentUsageTrendBucket {
                    id: bucket.start_at_ms.to_string(),
                    label: bucket.label.clone(),
                    start_at_ms: bucket.start_at_ms,
                    end_at_ms: bucket.end_at_ms,
                    aggregate: aggregate(bucket_facts.as_slice())?,
                })
            })
            .collect::<VibexResult<Vec<_>>>()?;
        let mut dimension_rows = build_dimension_rows(
            request.dimension,
            facts.as_slice(),
            trend_buckets.as_slice(),
        )?;
        sort_dimension_rows(
            dimension_rows.as_mut_slice(),
            request.sort_metric,
            request.sort_direction,
        );
        let annual =
            build_annual_projection(annual_buckets, annual_facts.as_slice(), bounds.end_at_ms)?;

        Ok(AgentUsageStatistics {
            generated_at_ms,
            effective_range: AgentUsageEffectiveRange {
                start_at_ms: bounds.start_at_ms,
                end_at_ms: bounds.end_at_ms,
                bucket_kind: bounds.bucket_kind.to_string(),
            },
            totals: aggregate(facts.as_slice())?,
            trend_buckets,
            dimension_rows,
            filter_options,
            annual: Some(annual),
        })
    }
}

fn token_usage_from_fact(fact: AgentTurnUsageFact) -> AgentTokenUsage {
    AgentTokenUsage {
        input_tokens: fact.cumulative_after.input_tokens,
        output_tokens: fact.cumulative_after.output_tokens,
        thought_tokens: fact.cumulative_after.thought_tokens,
        cached_read_tokens: fact.cumulative_after.cached_read_tokens,
        cached_write_tokens: fact.cumulative_after.cached_write_tokens,
        total_tokens: fact.cumulative_after.total_tokens,
        context_window_used_tokens: fact.context_window_used_tokens,
        context_window_size_tokens: fact.context_window_size_tokens,
    }
}

fn validate_request(request: &AgentUsageStatisticsRequest) -> VibexResult<()> {
    if !request.time_zone.validate() {
        return Err(VibexError::validation(
            "agent_usage_time_zone_invalid",
            "Agent usage time zone offset is invalid",
        ));
    }
    if request.model_ids.iter().any(|model| {
        model.trim().is_empty() || model.len() > 512 || model.chars().any(char::is_control)
    }) {
        return Err(VibexError::validation(
            "agent_usage_model_filter_invalid",
            "Agent usage model filters must be non-empty and bounded",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ResolvedUsageTimeZone {
    System,
    Fixed(FixedOffset),
}

impl ResolvedUsageTimeZone {
    fn resolve(value: &AgentUsageTimeZone) -> VibexResult<Self> {
        match value {
            AgentUsageTimeZone::System => Ok(Self::System),
            AgentUsageTimeZone::FixedOffset { offset_minutes } => {
                FixedOffset::east_opt(offset_minutes.checked_mul(60).ok_or_else(|| {
                    VibexError::validation(
                        "agent_usage_time_zone_invalid",
                        "Agent usage time zone offset is invalid",
                    )
                })?)
                .map(Self::Fixed)
                .ok_or_else(|| {
                    VibexError::validation(
                        "agent_usage_time_zone_invalid",
                        "Agent usage time zone offset is invalid",
                    )
                })
            }
        }
    }

    fn local_datetime(self, timestamp_ms: i64) -> VibexResult<NaiveDateTime> {
        match self {
            Self::System => Local
                .timestamp_millis_opt(timestamp_ms)
                .single()
                .map(|value| value.naive_local()),
            Self::Fixed(offset) => offset
                .timestamp_millis_opt(timestamp_ms)
                .single()
                .map(|value| value.naive_local()),
        }
        .ok_or_else(|| {
            VibexError::validation(
                "agent_usage_timestamp_invalid",
                "Agent usage timestamp could not be represented",
            )
        })
    }

    fn timestamp_ms(self, local: NaiveDateTime) -> VibexResult<i64> {
        let direct = match self {
            Self::System => timestamp_from_local_result(Local.from_local_datetime(&local)),
            Self::Fixed(offset) => timestamp_from_local_result(offset.from_local_datetime(&local)),
        };
        if let Some(timestamp) = direct {
            return Ok(timestamp);
        }
        for minutes in 1..=180 {
            let candidate = local + Duration::minutes(minutes);
            let resolved = match self {
                Self::System => timestamp_from_local_result(Local.from_local_datetime(&candidate)),
                Self::Fixed(offset) => {
                    timestamp_from_local_result(offset.from_local_datetime(&candidate))
                }
            };
            if let Some(timestamp) = resolved {
                return Ok(timestamp);
            }
        }
        Err(VibexError::validation(
            "agent_usage_local_boundary_invalid",
            "Agent usage local time boundary could not be represented",
        ))
    }
}

fn timestamp_from_local_result<Tz: TimeZone>(
    result: LocalResult<chrono::DateTime<Tz>>,
) -> Option<i64> {
    match result {
        LocalResult::Single(value) => Some(value.timestamp_millis()),
        LocalResult::Ambiguous(first, second) => {
            Some(first.timestamp_millis().min(second.timestamp_millis()))
        }
        LocalResult::None => None,
    }
}

struct QueryBounds {
    start_at_ms: i64,
    end_at_ms: i64,
    bucket_kind: &'static str,
}

fn fact_is_in_bounds(projection: &AgentUsageFactProjection, bounds: &QueryBounds) -> bool {
    projection.fact.dispatched_at_ms >= bounds.start_at_ms
        && projection.fact.dispatched_at_ms < bounds.end_at_ms
}

fn query_bounds(
    range: AgentUsageRange,
    time_zone: &ResolvedUsageTimeZone,
    now_ms: i64,
) -> VibexResult<QueryBounds> {
    if now_ms < 0 {
        return Err(VibexError::validation(
            "agent_usage_clock_invalid",
            "Agent usage query clock is invalid",
        ));
    }
    let today = time_zone.local_datetime(now_ms)?.date();
    let (start_at_ms, bucket_kind) = match range {
        AgentUsageRange::Today => (local_midnight(*time_zone, today)?, "hour"),
        AgentUsageRange::Last7Days => (
            local_midnight(*time_zone, today - Duration::days(6))?,
            "day",
        ),
        AgentUsageRange::Last30Days => (
            local_midnight(*time_zone, today - Duration::days(29))?,
            "day",
        ),
        AgentUsageRange::AllTime => (0, "month"),
    };
    Ok(QueryBounds {
        start_at_ms,
        end_at_ms: now_ms.saturating_add(1),
        bucket_kind,
    })
}

struct TimeBucket {
    label: String,
    start_at_ms: i64,
    end_at_ms: i64,
}

fn build_time_buckets(
    range: AgentUsageRange,
    time_zone: &ResolvedUsageTimeZone,
    now_ms: i64,
    facts: &[&AgentUsageFactProjection],
) -> VibexResult<Vec<TimeBucket>> {
    let today = time_zone.local_datetime(now_ms)?.date();
    match range {
        AgentUsageRange::Today => {
            let start = local_midnight(*time_zone, today)?;
            let end = local_midnight(*time_zone, next_date(today)?)?;
            let mut buckets = Vec::new();
            let mut cursor = start;
            while cursor < end {
                let next = cursor.saturating_add(3_600_000).min(end);
                buckets.push(TimeBucket {
                    label: time_zone
                        .local_datetime(cursor)?
                        .format("%H:00")
                        .to_string(),
                    start_at_ms: cursor,
                    end_at_ms: next,
                });
                cursor = next;
            }
            Ok(buckets)
        }
        AgentUsageRange::Last7Days | AgentUsageRange::Last30Days => {
            let day_count = if range == AgentUsageRange::Last7Days {
                7
            } else {
                30
            };
            let first = today - Duration::days(day_count - 1);
            (0..day_count)
                .map(|index| {
                    let date = first + Duration::days(index);
                    Ok(TimeBucket {
                        label: date.format("%Y-%m-%d").to_string(),
                        start_at_ms: local_midnight(*time_zone, date)?,
                        end_at_ms: local_midnight(*time_zone, next_date(date)?)?,
                    })
                })
                .collect()
        }
        AgentUsageRange::AllTime => {
            let first_date = facts
                .iter()
                .map(|projection| projection.fact.dispatched_at_ms)
                .min()
                .map(|timestamp| {
                    time_zone
                        .local_datetime(timestamp)
                        .map(|value| value.date())
                })
                .transpose()?
                .unwrap_or(today);
            let mut month = first_date.with_day(1).ok_or_else(|| {
                VibexError::validation(
                    "agent_usage_month_invalid",
                    "Agent usage month boundary is invalid",
                )
            })?;
            let final_month = today.with_day(1).ok_or_else(|| {
                VibexError::validation(
                    "agent_usage_month_invalid",
                    "Agent usage month boundary is invalid",
                )
            })?;
            let mut buckets = Vec::new();
            while month <= final_month {
                let next = next_month(month)?;
                buckets.push(TimeBucket {
                    label: month.format("%Y-%m").to_string(),
                    start_at_ms: local_midnight(*time_zone, month)?,
                    end_at_ms: local_midnight(*time_zone, next)?,
                });
                month = next;
            }
            Ok(buckets)
        }
    }
}

fn build_annual_time_buckets(
    time_zone: &ResolvedUsageTimeZone,
    now_ms: i64,
) -> VibexResult<Vec<TimeBucket>> {
    let today = time_zone.local_datetime(now_ms)?.date();
    let first = today - Duration::days(364);
    (0..365)
        .map(|index| {
            let date = first + Duration::days(index);
            Ok(TimeBucket {
                label: date.format("%Y-%m-%d").to_string(),
                start_at_ms: local_midnight(*time_zone, date)?,
                end_at_ms: local_midnight(*time_zone, next_date(date)?)?,
            })
        })
        .collect()
}

fn build_annual_projection(
    buckets: Vec<TimeBucket>,
    facts: &[&AgentUsageFactProjection],
    query_end_at_ms: i64,
) -> VibexResult<AgentUsageAnnualProjection> {
    let start_at_ms = buckets
        .first()
        .map(|bucket| bucket.start_at_ms)
        .unwrap_or(query_end_at_ms);
    let mut facts_by_day = vec![Vec::new(); buckets.len()];
    for projection in facts {
        let timestamp = projection.fact.dispatched_at_ms;
        let index = buckets.partition_point(|bucket| bucket.end_at_ms <= timestamp);
        if let Some(bucket) = buckets.get(index)
            && timestamp >= bucket.start_at_ms
            && timestamp < bucket.end_at_ms
        {
            facts_by_day[index].push(*projection);
        }
    }

    let days = buckets
        .into_iter()
        .zip(facts_by_day)
        .map(|(bucket, day_facts)| {
            let mut model_groups: BTreeMap<Option<String>, Vec<&AgentUsageFactProjection>> =
                BTreeMap::new();
            for projection in &day_facts {
                model_groups
                    .entry(projection.fact.model_id.clone())
                    .or_default()
                    .push(*projection);
            }
            let models = model_groups
                .into_iter()
                .map(|(model_id, model_facts)| {
                    Ok(AgentUsageDailyModelUsage {
                        label: model_id
                            .clone()
                            .unwrap_or_else(|| "Agent default".to_string()),
                        model_id,
                        requests: model_facts.len() as u64,
                        total_tokens: total_metric(model_facts.as_slice())?,
                    })
                })
                .collect::<VibexResult<Vec<_>>>()?;
            Ok(AgentUsageAnnualDay {
                id: bucket.start_at_ms.to_string(),
                label: bucket.label,
                start_at_ms: bucket.start_at_ms,
                end_at_ms: bucket.end_at_ms,
                requests: day_facts.len() as u64,
                total_tokens: total_metric(day_facts.as_slice())?,
                models,
            })
        })
        .collect::<VibexResult<Vec<_>>>()?;

    Ok(AgentUsageAnnualProjection {
        effective_range: AgentUsageEffectiveRange {
            start_at_ms,
            end_at_ms: query_end_at_ms,
            bucket_kind: "day".to_string(),
        },
        days,
    })
}

fn local_midnight(time_zone: ResolvedUsageTimeZone, date: NaiveDate) -> VibexResult<i64> {
    time_zone.timestamp_ms(date.and_hms_opt(0, 0, 0).ok_or_else(|| {
        VibexError::validation(
            "agent_usage_date_invalid",
            "Agent usage local date is invalid",
        )
    })?)
}

fn next_date(date: NaiveDate) -> VibexResult<NaiveDate> {
    date.succ_opt().ok_or_else(|| {
        VibexError::validation(
            "agent_usage_date_overflow",
            "Agent usage local date exceeded the supported range",
        )
    })
}

fn next_month(date: NaiveDate) -> VibexResult<NaiveDate> {
    let (year, month) = if date.month() == 12 {
        (date.year().saturating_add(1), 1)
    } else {
        (date.year(), date.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1).ok_or_else(|| {
        VibexError::validation(
            "agent_usage_month_overflow",
            "Agent usage month exceeded the supported range",
        )
    })
}

fn matches_filters(
    projection: &AgentUsageFactProjection,
    request: &AgentUsageStatisticsRequest,
) -> bool {
    let fact = &projection.fact;
    (request.agent_ids.is_empty() || request.agent_ids.contains(&fact.agent_id))
        && (request.project_ids.is_empty() || request.project_ids.contains(&fact.project_id))
        && (request.provider_profile_ids.is_empty()
            || fact
                .auth_source
                .provider_profile_id()
                .is_some_and(|profile_id| request.provider_profile_ids.contains(profile_id)))
        && (request.model_ids.is_empty()
            || fact
                .model_id
                .as_ref()
                .is_some_and(|model_id| request.model_ids.contains(model_id)))
        && (request.session_ids.is_empty() || request.session_ids.contains(&fact.session_id))
}

fn build_filter_options(facts: &[AgentUsageFactProjection]) -> AgentUsageFilterOptions {
    let mut agents = BTreeMap::new();
    let mut projects = BTreeMap::new();
    let mut provider_profiles = BTreeMap::new();
    let mut models = BTreeMap::new();
    let mut sessions = BTreeMap::new();
    for projection in facts {
        let fact = &projection.fact;
        agents.insert(
            fact.agent_id.as_str().to_string(),
            projection.agent_label.clone(),
        );
        projects.insert(
            fact.project_id.as_str().to_string(),
            projection.project_label.clone(),
        );
        provider_profiles.insert(
            fact.auth_source.id().to_string(),
            projection.auth_source_label.clone(),
        );
        if let Some(model_id) = fact.model_id.as_ref() {
            models.insert(model_id.clone(), model_id.clone());
        }
        sessions.insert(
            fact.session_id.as_str().to_string(),
            projection.session_label.clone(),
        );
    }
    AgentUsageFilterOptions {
        agents: filter_options(agents),
        projects: filter_options(projects),
        provider_profiles: filter_options(provider_profiles),
        models: filter_options(models),
        sessions: filter_options(sessions),
    }
}

fn filter_options(values: BTreeMap<String, String>) -> Vec<AgentUsageFilterOption> {
    values
        .into_iter()
        .map(|(id, label)| AgentUsageFilterOption { id, label })
        .collect()
}

fn build_dimension_rows(
    dimension: AgentUsageDimension,
    facts: &[&AgentUsageFactProjection],
    trend_buckets: &[AgentUsageTrendBucket],
) -> VibexResult<Vec<AgentUsageDimensionRow>> {
    if dimension == AgentUsageDimension::Time {
        return Ok(trend_buckets
            .iter()
            .map(|bucket| AgentUsageDimensionRow {
                id: bucket.id.clone(),
                label: bucket.label.clone(),
                aggregate: bucket.aggregate.clone(),
            })
            .collect());
    }
    let mut groups: BTreeMap<(String, String), Vec<&AgentUsageFactProjection>> = BTreeMap::new();
    for projection in facts {
        let fact = &projection.fact;
        let (id, label) = match dimension {
            AgentUsageDimension::Time => unreachable!(),
            AgentUsageDimension::Agent => (
                fact.agent_id.as_str().to_string(),
                projection.agent_label.clone(),
            ),
            AgentUsageDimension::Project => (
                fact.project_id.as_str().to_string(),
                projection.project_label.clone(),
            ),
            AgentUsageDimension::ModelProvider => (
                fact.auth_source.id().to_string(),
                projection.auth_source_label.clone(),
            ),
            AgentUsageDimension::Model => fact.model_id.as_ref().map_or_else(
                || {
                    (
                        "model-resolution:agent-default".to_string(),
                        "Agent default".to_string(),
                    )
                },
                |model_id| (model_id.clone(), model_id.clone()),
            ),
        };
        groups.entry((id, label)).or_default().push(*projection);
    }
    groups
        .into_iter()
        .map(|((id, label), facts)| {
            Ok(AgentUsageDimensionRow {
                id,
                label,
                aggregate: aggregate(facts.as_slice())?,
            })
        })
        .collect()
}

fn aggregate(facts: &[&AgentUsageFactProjection]) -> VibexResult<AgentUsageAggregate> {
    let total_requests = facts.len() as u64;
    // Turns are always countable; API requests only when an adapter reports
    // them. A turn that reports none contributes nothing rather than one, so a
    // mixed selection never presents a turn count as a request count.
    let api_requests = facts
        .iter()
        .filter_map(|projection| projection.fact.api_requests)
        .try_fold(None::<u64>, |total, requests| {
            checked_add(total.unwrap_or(0), requests).map(Some)
        })?;
    // A request-scoped adapter breaks its turn total down for one request only,
    // so the breakdown is a floor under a turn that made several requests even
    // though every turn reported it. Saying "complete" there would be a lie.
    let breakdown_is_partial = facts.iter().any(|projection| {
        projection.fact.counter_scope.is_lower_bound()
            && projection
                .fact
                .api_requests
                .is_some_and(|requests| requests > 1)
    });
    let input_tokens =
        breakdown_metric(facts, breakdown_is_partial, |fact| fact.delta.input_tokens)?;
    let output_tokens =
        breakdown_metric(facts, breakdown_is_partial, |fact| fact.delta.output_tokens)?;
    let cached_tokens = breakdown_metric(facts, breakdown_is_partial, |fact| {
        fact.delta.cached_read_tokens
    })?;
    let thought_tokens = breakdown_metric(facts, breakdown_is_partial, |fact| {
        fact.delta.thought_tokens
    })?;
    let cached_write_tokens = breakdown_metric(facts, breakdown_is_partial, |fact| {
        fact.delta.cached_write_tokens
    })?;
    let total_tokens = total_metric(facts)?;

    let mut cached_read = 0_u64;
    let mut denominator = 0_u64;
    let mut eligible_requests = 0_u64;
    for projection in facts {
        let fact = &projection.fact;
        if let (Some(input), Some(cached)) =
            (fact.delta.input_tokens, fact.delta.cached_read_tokens)
        {
            cached_read = checked_add(cached_read, cached)?;
            denominator = checked_add(denominator, checked_add(input, cached)?)?;
            eligible_requests = eligible_requests.saturating_add(1);
        }
    }
    let cache_coverage = if breakdown_is_partial && eligible_requests > 0 {
        AgentUsageMetricCoverage::Partial
    } else {
        metric_coverage(eligible_requests, total_requests, false)
    };
    let basis_points = if eligible_requests == 0 {
        None
    } else if denominator == 0 {
        Some(0)
    } else {
        Some(((u128::from(cached_read) * 10_000) / u128::from(denominator)) as u32)
    };
    let mut coverage = AgentUsageCoverageSummary::default();
    for projection in facts {
        match projection.fact.coverage {
            AgentUsageCoverage::Complete => coverage.complete_requests += 1,
            AgentUsageCoverage::Partial => coverage.partial_requests += 1,
            AgentUsageCoverage::BaselineOnly => coverage.baseline_only_requests += 1,
            AgentUsageCoverage::Unreported => coverage.unreported_requests += 1,
            AgentUsageCoverage::Unsupported => coverage.unsupported_requests += 1,
        }
    }
    coverage.total_requests = total_requests;
    let last_activity_at_ms = facts
        .iter()
        .map(|projection| {
            projection
                .fact
                .last_observed_at_ms
                .or(projection.fact.completed_at_ms)
                .unwrap_or(projection.fact.dispatched_at_ms)
        })
        .max();

    Ok(AgentUsageAggregate {
        requests: total_requests,
        api_requests,
        total_tokens,
        input_tokens,
        output_tokens,
        cached_tokens,
        thought_tokens,
        cached_write_tokens,
        cache_hit_rate: AgentUsageCacheHitRate {
            basis_points,
            cached_read_tokens: cached_read,
            denominator_tokens: denominator,
            eligible_requests,
            total_requests,
            coverage: cache_coverage,
        },
        coverage,
        last_activity_at_ms,
    })
}

/// A per-token-kind metric, downgraded to partial when the underlying readings
/// cover fewer API requests than the turns they were recorded for.
fn breakdown_metric(
    facts: &[&AgentUsageFactProjection],
    is_partial: bool,
    value: impl Fn(&AgentTurnUsageFact) -> Option<u64>,
) -> VibexResult<AgentUsageMetricValue> {
    let mut computed = metric(facts, value)?;
    if is_partial && computed.value.is_some() {
        computed.coverage = AgentUsageMetricCoverage::Partial;
    }
    Ok(computed)
}

fn metric(
    facts: &[&AgentUsageFactProjection],
    value: impl Fn(&AgentTurnUsageFact) -> Option<u64>,
) -> VibexResult<AgentUsageMetricValue> {
    let mut sum = 0_u64;
    let mut known_requests = 0_u64;
    for projection in facts {
        if let Some(current) = value(&projection.fact) {
            sum = checked_add(sum, current)?;
            known_requests = known_requests.saturating_add(1);
        }
    }
    let total_requests = facts.len() as u64;
    Ok(AgentUsageMetricValue {
        value: (known_requests > 0).then_some(sum),
        coverage: metric_coverage(known_requests, total_requests, false),
        known_requests,
        derived_requests: 0,
        total_requests,
    })
}

fn total_metric(facts: &[&AgentUsageFactProjection]) -> VibexResult<AgentUsageMetricValue> {
    let mut sum = 0_u64;
    let mut known_requests = 0_u64;
    let mut derived_requests = 0_u64;
    for projection in facts {
        let fact = &projection.fact;
        let value = if let Some(total) = fact.delta.total_tokens {
            Some(total)
        } else if let (Some(input), Some(output)) =
            (fact.delta.input_tokens, fact.delta.output_tokens)
        {
            derived_requests = derived_requests.saturating_add(1);
            Some(checked_add(input, output)?)
        } else {
            None
        };
        if let Some(value) = value {
            sum = checked_add(sum, value)?;
            known_requests = known_requests.saturating_add(1);
        }
    }
    let total_requests = facts.len() as u64;
    Ok(AgentUsageMetricValue {
        value: (known_requests > 0).then_some(sum),
        coverage: metric_coverage(known_requests, total_requests, derived_requests > 0),
        known_requests,
        derived_requests,
        total_requests,
    })
}

fn metric_coverage(
    known_requests: u64,
    total_requests: u64,
    contains_derived: bool,
) -> AgentUsageMetricCoverage {
    if known_requests == 0 {
        AgentUsageMetricCoverage::Unknown
    } else if known_requests < total_requests {
        AgentUsageMetricCoverage::Partial
    } else if contains_derived {
        AgentUsageMetricCoverage::Derived
    } else {
        AgentUsageMetricCoverage::Complete
    }
}

fn checked_add(left: u64, right: u64) -> VibexResult<u64> {
    left.checked_add(right).ok_or_else(|| {
        VibexError::validation(
            "agent_usage_aggregate_overflow",
            "Agent usage aggregate exceeded the supported range",
        )
    })
}

fn sort_dimension_rows(
    rows: &mut [AgentUsageDimensionRow],
    metric: AgentUsageSortMetric,
    direction: AgentUsageSortDirection,
) {
    rows.sort_by(|left, right| {
        let left_value = sort_value(&left.aggregate, metric);
        let right_value = sort_value(&right.aggregate, metric);
        let value_order = match (left_value, right_value) {
            (Some(left), Some(right)) => {
                let order = left.cmp(&right);
                if direction == AgentUsageSortDirection::Descending {
                    order.reverse()
                } else {
                    order
                }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        value_order
            .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn sort_value(aggregate: &AgentUsageAggregate, metric: AgentUsageSortMetric) -> Option<u128> {
    match metric {
        AgentUsageSortMetric::Requests => Some(u128::from(aggregate.requests)),
        AgentUsageSortMetric::TotalTokens => aggregate.total_tokens.value.map(u128::from),
        AgentUsageSortMetric::InputTokens => aggregate.input_tokens.value.map(u128::from),
        AgentUsageSortMetric::OutputTokens => aggregate.output_tokens.value.map(u128::from),
        AgentUsageSortMetric::CachedTokens => aggregate.cached_tokens.value.map(u128::from),
        AgentUsageSortMetric::CacheHitRate => aggregate.cache_hit_rate.basis_points.map(u128::from),
        AgentUsageSortMetric::LastActivity => aggregate
            .last_activity_at_ms
            .and_then(|value| u128::try_from(value).ok()),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};
    use tempfile::tempdir;
    use vibex_core::{
        AgentId, AgentSession, AgentSessionSafety, AgentSessionState, AgentUsageCounterOrigin,
        AgentUsageCounterScope, AgentUsageDimension, AgentUsageExecution,
        AgentUsageExecutionStatus, AgentUsageExecutionStatusUpdate, AgentUsageObservation,
        AgentUsageObservationSource, AgentUsageStreamAttribution, AgentUsageTokenValues,
        ProviderProfileId, RuntimeAuthSource, RuntimeBindingId, UsageExecutionId, VibexSessionId,
        WorkspaceMode,
    };
    use vibex_db::{AgentAuthContextRepository, SessionRepository, WorkspaceRepository};

    use super::*;

    fn seeded_service() -> (tempfile::TempDir, AgentUsageService, AgentSession) {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("usage.db");
        let workspace_root = directory.path().join("workspace");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let mut connection = open_database(&database_path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let (project, workspace) = WorkspaceRepository::ensure(
            &connection,
            &workspace_root,
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: "Usage test".to_string(),
            project_id: project.id,
            workspace_id: workspace.id,
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("opencode").unwrap(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1,
            updated_at_ms: 1,
            last_message_at_ms: 1,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&connection, &session).unwrap();
        (
            directory,
            AgentUsageService::new(database_path).unwrap(),
            session,
        )
    }

    fn dispatched_at(hour: u32) -> i64 {
        timestamp(2026, 7, 31, hour, 0)
    }

    fn timestamp(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .unwrap()
            .timestamp_millis()
    }

    fn insert_session(
        directory: &tempfile::TempDir,
        service: &AgentUsageService,
        name: &str,
        agent_id: &str,
    ) -> AgentSession {
        let workspace_root = directory.path().join(name);
        std::fs::create_dir_all(&workspace_root).unwrap();
        let connection = open_database(service.database_path()).unwrap();
        let (project, workspace) = WorkspaceRepository::ensure(
            &connection,
            &workspace_root,
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: format!("Usage {name}"),
            project_id: project.id,
            workspace_id: workspace.id,
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse(agent_id).unwrap(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1,
            updated_at_ms: 1,
            last_message_at_ms: 1,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&connection, &session).unwrap();
        session
    }

    fn stream(
        service: &AgentUsageService,
        session: &AgentSession,
        agent_id: &str,
        provider_profile_id: ProviderProfileId,
        model_id: &str,
    ) -> AgentUsageStreamAttribution {
        let stream = AgentUsageStreamAttribution {
            session_id: session.id.clone(),
            binding_id: RuntimeBindingId::new(),
            activation_generation: 1,
            agent_id: AgentId::parse(agent_id).unwrap(),
            auth_source: vibex_core::RuntimeAuthSource::provider_profile(
                provider_profile_id.clone(),
            ),
            auth_source_revision: 1,
            model_id: Some(model_id.to_string()),
        };
        let connection = open_database(service.database_path()).unwrap();
        let migration_applied_at_ms = connection
            .query_row(
                "SELECT applied_at_ms FROM schema_migrations WHERE version = 31",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        connection
            .execute(
                "
                INSERT INTO session_runtime_bindings (
                    binding_id, session_id, agent_id, transport_kind, adapter_id, adapter_version,
                    adapter_compatibility_identity, provider_profile_id, profile_revision,
                    auth_source_kind, auth_source_id, auth_source_revision, native_state_home_id,
                    process_spawn_fingerprint, session_runtime_config_state_json, binding_state,
                    activation_generation, created_at_ms, updated_at_ms,
                    usage_zero_baseline_state
                ) VALUES (
                    ?1, ?2, ?3, 'acp', 'usage-test-adapter', '1.0.0',
                    'usage-test-compatibility', ?4, 1, 'provider_profile', ?4, 1,
                    'usage-test-home',
                    'usage-test-fingerprint', '{}', 'current', 1, ?5, ?5, 'available'
                )
                ",
                rusqlite::params![
                    stream.binding_id.as_str(),
                    stream.session_id.as_str(),
                    stream.agent_id.as_str(),
                    provider_profile_id.as_str(),
                    migration_applied_at_ms.saturating_add(1),
                ],
            )
            .unwrap();
        stream
    }

    fn agent_default_stream(
        service: &AgentUsageService,
        session: &AgentSession,
    ) -> AgentUsageStreamAttribution {
        let connection = open_database(service.database_path()).unwrap();
        let auth_context =
            AgentAuthContextRepository::ensure_default(&connection, &session.agent_id).unwrap();
        let stream = AgentUsageStreamAttribution {
            session_id: session.id.clone(),
            binding_id: RuntimeBindingId::new(),
            activation_generation: 1,
            agent_id: session.agent_id.clone(),
            auth_source: RuntimeAuthSource::agent_account(auth_context.id.clone()),
            auth_source_revision: auth_context.revision,
            model_id: None,
        };
        let migration_applied_at_ms = connection
            .query_row(
                "SELECT applied_at_ms FROM schema_migrations WHERE version = 31",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        connection
            .execute(
                "
                INSERT INTO session_runtime_bindings (
                    binding_id, session_id, agent_id, transport_kind, adapter_id, adapter_version,
                    adapter_compatibility_identity, provider_profile_id, profile_revision,
                    auth_source_kind, auth_source_id, auth_source_revision, native_state_home_id,
                    process_spawn_fingerprint, session_runtime_config_state_json, binding_state,
                    activation_generation, created_at_ms, updated_at_ms,
                    usage_zero_baseline_state
                ) VALUES (
                    ?1, ?2, ?3, 'acp', 'usage-test-adapter', '1.0.0',
                    'usage-test-compatibility', NULL, NULL, 'agent_account', ?4, ?5,
                    'usage-test-agent-default-home', 'usage-test-agent-default-fingerprint', '{}',
                    'current', 1, ?6, ?6, 'available'
                )
                ",
                rusqlite::params![
                    stream.binding_id.as_str(),
                    stream.session_id.as_str(),
                    stream.agent_id.as_str(),
                    auth_context.id.as_str(),
                    auth_context.revision,
                    migration_applied_at_ms.saturating_add(1),
                ],
            )
            .unwrap();
        stream
    }

    fn execution(
        service: &AgentUsageService,
        session: &AgentSession,
        stream: &AgentUsageStreamAttribution,
        hour: u32,
    ) -> AgentUsageExecution {
        execution_at(service, session, stream, dispatched_at(hour))
    }

    fn execution_at(
        _service: &AgentUsageService,
        session: &AgentSession,
        stream: &AgentUsageStreamAttribution,
        dispatched_at_ms: i64,
    ) -> AgentUsageExecution {
        AgentUsageExecution {
            usage_execution_id: UsageExecutionId::new(),
            message_submission_id: None,
            project_id: session.project_id.clone(),
            workspace_id: session.workspace_id.clone(),
            stream: stream.clone(),
            dispatched_at_ms,
        }
    }

    fn dispatched_event(execution: AgentUsageExecution) -> AgentUsageTelemetryEvent {
        AgentUsageTelemetryEvent::ExecutionDispatched {
            execution,
            counter_origin: AgentUsageCounterOrigin::KnownZero,
        }
    }

    fn zero_baseline_state(
        service: &AgentUsageService,
        stream: &AgentUsageStreamAttribution,
    ) -> (String, Option<String>, Option<i64>) {
        let connection = open_database(service.database_path()).unwrap();
        connection
            .query_row(
                "SELECT usage_zero_baseline_state, usage_zero_baseline_execution_id,
                        usage_zero_baseline_activation_generation
                 FROM session_runtime_bindings WHERE binding_id = ?1",
                rusqlite::params![stream.binding_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    fn observation(
        execution: AgentUsageExecution,
        sequence: u64,
        input: u64,
        output: u64,
        cached: u64,
        total: u64,
    ) -> AgentUsageObservation {
        AgentUsageObservation {
            stream: execution.stream.clone(),
            execution: Some(execution.clone()),
            counter_origin: AgentUsageCounterOrigin::KnownZero,
            counter_scope: AgentUsageCounterScope::Session,
            observation_sequence: sequence,
            cumulative: AgentUsageTokenValues {
                input_tokens: Some(input),
                output_tokens: Some(output),
                cached_read_tokens: Some(cached),
                total_tokens: Some(total),
                ..AgentUsageTokenValues::default()
            },
            context_window_used_tokens: Some(input + output),
            context_window_size_tokens: Some(200_000),
            source: AgentUsageObservationSource::PromptResponse,
            observed_at_ms: execution.dispatched_at_ms + 100,
        }
    }

    fn fixed_request(range: AgentUsageRange) -> AgentUsageStatisticsRequest {
        AgentUsageStatisticsRequest {
            range,
            time_zone: AgentUsageTimeZone::FixedOffset { offset_minutes: 0 },
            ..AgentUsageStatisticsRequest::default()
        }
    }

    #[test]
    fn latest_token_usage_restores_current_binding_snapshot() {
        let (_directory, service, session) = seeded_service();
        let stream = stream(
            &service,
            &session,
            session.agent_id.as_str(),
            ProviderProfileId::new(),
            "test-model",
        );
        let observed_execution = execution(&service, &session, &stream, 9);
        service
            .apply_telemetry_event(dispatched_event(observed_execution.clone()))
            .unwrap();
        service
            .apply_telemetry_event(AgentUsageTelemetryEvent::Observation(observation(
                observed_execution,
                1,
                600,
                400,
                300,
                1_000,
            )))
            .unwrap();

        let later_unobserved = execution(&service, &session, &stream, 10);
        service
            .apply_telemetry_event(dispatched_event(later_unobserved.clone()))
            .unwrap();
        let connection = open_database(service.database_path()).unwrap();
        connection
            .execute(
                "UPDATE agent_sessions SET current_binding_id = ?2 WHERE session_id = ?1",
                rusqlite::params![session.id.as_str(), stream.binding_id.as_str()],
            )
            .unwrap();

        let restored = service
            .latest_token_usage(&session.id, None)
            .unwrap()
            .unwrap();
        assert_eq!(restored.input_tokens, Some(600));
        assert_eq!(restored.output_tokens, Some(400));
        assert_eq!(restored.cached_read_tokens, Some(300));
        assert_eq!(restored.total_tokens, Some(1_000));
        assert_eq!(restored.context_window_used_tokens, Some(1_000));
        assert_eq!(restored.context_window_size_tokens, Some(200_000));

        service
            .apply_telemetry_event(AgentUsageTelemetryEvent::Observation(observation(
                later_unobserved,
                2,
                700,
                450,
                325,
                1_150,
            )))
            .unwrap();
        assert_eq!(
            service
                .latest_token_usage(&session.id, Some(&stream.binding_id))
                .unwrap()
                .unwrap()
                .input_tokens,
            Some(700)
        );
        assert_eq!(
            service
                .latest_token_usage(&session.id, Some(&RuntimeBindingId::new()))
                .unwrap(),
            None
        );
    }

    #[test]
    fn cumulative_telemetry_queries_exact_deltas_and_local_hour_buckets() {
        let (_directory, service, session) = seeded_service();
        let stream = stream(
            &service,
            &session,
            session.agent_id.as_str(),
            ProviderProfileId::new(),
            "test-model",
        );
        let samples = [
            (9, 1_000, 600, 400, 300),
            (10, 1_800, 1_000, 800, 500),
            (11, 2_500, 1_400, 1_100, 700),
        ];
        for (index, (hour, total, input, output, cached)) in samples.into_iter().enumerate() {
            let execution = execution(&service, &session, &stream, hour);
            let completed_at_ms = execution.dispatched_at_ms + 200;
            assert!(
                service
                    .apply_telemetry_event(dispatched_event(execution.clone()))
                    .unwrap()
            );
            assert!(
                service
                    .apply_telemetry_event(AgentUsageTelemetryEvent::Observation(observation(
                        execution.clone(),
                        index as u64 + 1,
                        input,
                        output,
                        cached,
                        total,
                    )))
                    .unwrap()
            );
            assert!(
                service
                    .apply_telemetry_event(AgentUsageTelemetryEvent::ExecutionStatus(
                        AgentUsageExecutionStatusUpdate {
                            execution,
                            status: AgentUsageExecutionStatus::Completed,
                            completed_at_ms,
                        }
                    ))
                    .unwrap()
            );
        }

        let request = AgentUsageStatisticsRequest {
            range: AgentUsageRange::Today,
            session_ids: vec![session.id.clone()],
            time_zone: AgentUsageTimeZone::FixedOffset { offset_minutes: 0 },
            ..AgentUsageStatisticsRequest::default()
        };
        let statistics = service
            .query_statistics_at(request, dispatched_at(12))
            .unwrap();
        assert_eq!(statistics.totals.requests, 3);
        assert_eq!(statistics.totals.total_tokens.value, Some(2_500));
        assert_eq!(statistics.totals.input_tokens.value, Some(1_400));
        assert_eq!(statistics.totals.output_tokens.value, Some(1_100));
        assert_eq!(statistics.totals.cached_tokens.value, Some(700));
        assert_eq!(statistics.totals.cache_hit_rate.basis_points, Some(3_333));
        assert_eq!(statistics.trend_buckets.len(), 24);
        assert_eq!(
            statistics
                .trend_buckets
                .iter()
                .map(|bucket| bucket.aggregate.requests)
                .sum::<u64>(),
            3
        );
        assert_eq!(statistics.filter_options.sessions.len(), 1);
    }

    #[test]
    fn agent_default_usage_has_no_synthetic_model_but_remains_visible() {
        let (_directory, service, session) = seeded_service();
        let stream = agent_default_stream(&service, &session);
        let execution = execution(&service, &session, &stream, 9);
        service
            .apply_telemetry_event(dispatched_event(execution.clone()))
            .unwrap();
        service
            .apply_telemetry_event(AgentUsageTelemetryEvent::Observation(observation(
                execution.clone(),
                1,
                60,
                40,
                20,
                100,
            )))
            .unwrap();

        let connection = open_database(service.database_path()).unwrap();
        let fact = AgentUsageRepository::get_fact(&connection, &execution.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(fact.model_id, None);
        let checkpoint_model: Option<String> = connection
            .query_row(
                "SELECT last_model_id FROM agent_usage_checkpoints WHERE binding_id = ?1",
                rusqlite::params![stream.binding_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(checkpoint_model, None);

        let statistics = service
            .query_statistics_at(
                AgentUsageStatisticsRequest {
                    dimension: AgentUsageDimension::Model,
                    ..fixed_request(AgentUsageRange::Today)
                },
                dispatched_at(12),
            )
            .unwrap();
        assert_eq!(statistics.totals.requests, 1);
        assert_eq!(statistics.totals.total_tokens.value, Some(100));
        assert!(statistics.filter_options.models.is_empty());
        assert_eq!(statistics.dimension_rows.len(), 1);
        assert_eq!(
            statistics.dimension_rows[0].id,
            "model-resolution:agent-default"
        );
        assert_eq!(statistics.dimension_rows[0].label, "Agent default");
        assert_eq!(statistics.dimension_rows[0].aggregate.requests, 1);
        assert_eq!(
            statistics.dimension_rows[0].aggregate.total_tokens.value,
            Some(100)
        );

        let annual = statistics.annual.unwrap();
        let day = annual
            .days
            .iter()
            .find(|day| day.label == "2026-07-31")
            .unwrap();
        assert_eq!(day.models.len(), 1);
        assert_eq!(day.models[0].model_id, None);
        assert_eq!(day.models[0].label, "Agent default");
        assert_eq!(day.models[0].requests, 1);
        assert_eq!(day.models[0].total_tokens.value, Some(100));
    }

    #[test]
    fn zero_baseline_is_claimed_only_after_execution_dispatch() {
        let (_directory, service, session) = seeded_service();
        let stream = stream(
            &service,
            &session,
            session.agent_id.as_str(),
            ProviderProfileId::new(),
            "dispatch-claim-model",
        );
        let execution = execution(&service, &session, &stream, 9);

        assert_eq!(
            zero_baseline_state(&service, &stream),
            ("available".to_string(), None, None)
        );
        assert!(
            service
                .apply_telemetry_event(dispatched_event(execution.clone()))
                .unwrap()
        );
        assert_eq!(
            zero_baseline_state(&service, &stream),
            (
                "claimed".to_string(),
                Some(execution.usage_execution_id.to_string()),
                Some(stream.activation_generation),
            )
        );
    }

    #[test]
    fn total_fallback_is_derived_only_when_input_and_output_are_known() {
        let (_directory, service, session) = seeded_service();
        let stream = stream(
            &service,
            &session,
            session.agent_id.as_str(),
            ProviderProfileId::new(),
            "partial-model",
        );
        let execution = execution(&service, &session, &stream, 9);
        service
            .apply_telemetry_event(dispatched_event(execution.clone()))
            .unwrap();
        let mut partial = observation(execution, 1, 700, 300, 200, 1_000);
        partial.cumulative.total_tokens = None;
        service
            .apply_telemetry_event(AgentUsageTelemetryEvent::Observation(partial))
            .unwrap();

        let statistics = service
            .query_statistics_at(
                AgentUsageStatisticsRequest {
                    range: AgentUsageRange::Today,
                    time_zone: AgentUsageTimeZone::FixedOffset { offset_minutes: 0 },
                    ..AgentUsageStatisticsRequest::default()
                },
                dispatched_at(12),
            )
            .unwrap();
        assert_eq!(statistics.totals.total_tokens.value, Some(1_000));
        assert_eq!(statistics.totals.total_tokens.derived_requests, 1);
        assert_eq!(
            statistics.totals.total_tokens.coverage,
            AgentUsageMetricCoverage::Derived
        );
    }

    #[test]
    fn dispatched_request_without_tokens_remains_counted_and_unknown() {
        let (_directory, service, session) = seeded_service();
        let stream = stream(
            &service,
            &session,
            session.agent_id.as_str(),
            ProviderProfileId::new(),
            "silent-model",
        );
        let execution = execution(&service, &session, &stream, 9);
        service
            .apply_telemetry_event(dispatched_event(execution.clone()))
            .unwrap();
        service
            .apply_telemetry_event(AgentUsageTelemetryEvent::ExecutionStatus(
                AgentUsageExecutionStatusUpdate {
                    execution,
                    status: AgentUsageExecutionStatus::Completed,
                    completed_at_ms: dispatched_at(9) + 200,
                },
            ))
            .unwrap();

        let statistics = service
            .query_statistics_at(fixed_request(AgentUsageRange::Today), dispatched_at(12))
            .unwrap();
        assert_eq!(statistics.totals.requests, 1);
        for metric in [
            &statistics.totals.total_tokens,
            &statistics.totals.input_tokens,
            &statistics.totals.output_tokens,
            &statistics.totals.cached_tokens,
            &statistics.totals.thought_tokens,
            &statistics.totals.cached_write_tokens,
        ] {
            assert_eq!(metric.value, None);
            assert_eq!(metric.coverage, AgentUsageMetricCoverage::Unknown);
            assert_eq!(metric.known_requests, 0);
            assert_eq!(metric.derived_requests, 0);
            assert_eq!(metric.total_requests, 1);
        }
        assert_eq!(statistics.totals.cache_hit_rate.basis_points, None);
        assert_eq!(
            statistics.totals.cache_hit_rate.coverage,
            AgentUsageMetricCoverage::Unknown
        );
        assert_eq!(statistics.totals.coverage.unreported_requests, 1);
        let annual = statistics.annual.expect("annual projection");
        let today = annual
            .days
            .iter()
            .find(|day| day.label == "2026-07-31")
            .expect("today annual bucket");
        assert_eq!(today.requests, 1);
        assert_eq!(today.total_tokens.value, None);
        assert_eq!(today.models.len(), 1);
        assert_eq!(today.models[0].total_tokens.value, None);
        let empty = annual
            .days
            .iter()
            .find(|day| day.label == "2026-07-30")
            .expect("empty annual bucket");
        assert_eq!(empty.requests, 0);
        assert_eq!(empty.total_tokens.value, None);
    }

    #[test]
    fn ranges_use_inclusive_local_starts_and_exclusive_query_ends() {
        let (_directory, service, session) = seeded_service();
        let stream = stream(
            &service,
            &session,
            session.agent_id.as_str(),
            ProviderProfileId::new(),
            "range-model",
        );
        let timestamps = [
            timestamp(2026, 6, 15, 12, 0),
            timestamp(2026, 7, 1, 23, 59),
            timestamp(2026, 7, 2, 0, 0),
            timestamp(2026, 7, 24, 23, 59),
            timestamp(2026, 7, 25, 0, 0),
            timestamp(2026, 7, 30, 23, 59),
            timestamp(2026, 7, 31, 0, 0),
        ];
        for dispatched_at_ms in timestamps {
            let execution = execution_at(&service, &session, &stream, dispatched_at_ms);
            service
                .apply_telemetry_event(dispatched_event(execution))
                .unwrap();
        }
        let now = dispatched_at(12);
        let cases = [
            (AgentUsageRange::Today, 1, 24, "hour"),
            (AgentUsageRange::Last7Days, 3, 7, "day"),
            (AgentUsageRange::Last30Days, 5, 30, "day"),
            (AgentUsageRange::AllTime, 7, 2, "month"),
        ];
        for (range, requests, buckets, bucket_kind) in cases {
            let statistics = service
                .query_statistics_at(fixed_request(range), now)
                .unwrap();
            assert_eq!(statistics.totals.requests, requests, "range {range:?}");
            assert_eq!(statistics.trend_buckets.len(), buckets, "range {range:?}");
            assert_eq!(statistics.effective_range.bucket_kind, bucket_kind);
            assert_eq!(
                statistics
                    .trend_buckets
                    .iter()
                    .map(|bucket| bucket.aggregate.requests)
                    .sum::<u64>(),
                requests,
                "range {range:?}"
            );
        }
    }

    #[test]
    fn annual_projection_is_fixed_to_365_days_independent_of_selected_range() {
        let (_directory, service, session) = seeded_service();
        let stream = stream(
            &service,
            &session,
            session.agent_id.as_str(),
            ProviderProfileId::new(),
            "annual-model",
        );
        let execution = execution_at(&service, &session, &stream, timestamp(2025, 8, 15, 9, 0));
        service
            .apply_telemetry_event(dispatched_event(execution.clone()))
            .unwrap();
        service
            .apply_telemetry_event(AgentUsageTelemetryEvent::Observation(observation(
                execution, 1, 60, 40, 20, 100,
            )))
            .unwrap();

        let now = dispatched_at(12);
        for (range, selected_requests) in [
            (AgentUsageRange::Today, 0),
            (AgentUsageRange::Last7Days, 0),
            (AgentUsageRange::AllTime, 1),
        ] {
            let statistics = service
                .query_statistics_at(fixed_request(range), now)
                .unwrap();
            assert_eq!(statistics.totals.requests, selected_requests);
            assert_eq!(statistics.filter_options.models[0].id, "annual-model");
            let annual = statistics.annual.expect("annual projection");
            assert_eq!(annual.effective_range.bucket_kind, "day");
            assert_eq!(annual.days.len(), 365);
            assert_eq!(annual.days.first().unwrap().label, "2025-08-01");
            assert_eq!(annual.days.last().unwrap().label, "2026-07-31");
            assert_eq!(annual.days.iter().map(|day| day.requests).sum::<u64>(), 1);
            let used_day = annual
                .days
                .iter()
                .find(|day| day.label == "2025-08-15")
                .unwrap();
            assert_eq!(used_day.total_tokens.value, Some(100));
            assert_eq!(used_day.models[0].model_id.as_deref(), Some("annual-model"));
            assert_eq!(used_day.models[0].requests, 1);
        }
    }

    #[test]
    fn five_dimensions_and_cross_filters_use_exact_attribution() {
        let (directory, service, first_session) = seeded_service();
        let second_session = insert_session(&directory, &service, "second-workspace", "claude");
        let first_profile = ProviderProfileId::new();
        let second_profile = ProviderProfileId::new();
        let first_stream = stream(
            &service,
            &first_session,
            "opencode",
            first_profile.clone(),
            "model-one",
        );
        let second_stream = stream(
            &service,
            &second_session,
            "claude",
            second_profile.clone(),
            "model-two",
        );
        let first_execution = execution(&service, &first_session, &first_stream, 9);
        let second_execution = execution(&service, &second_session, &second_stream, 10);
        service
            .apply_telemetry_event(dispatched_event(first_execution.clone()))
            .unwrap();
        service
            .apply_telemetry_event(dispatched_event(second_execution.clone()))
            .unwrap();
        service
            .apply_telemetry_event(AgentUsageTelemetryEvent::Observation(observation(
                first_execution,
                1,
                60,
                40,
                20,
                100,
            )))
            .unwrap();
        service
            .apply_telemetry_event(AgentUsageTelemetryEvent::Observation(observation(
                second_execution,
                1,
                120,
                80,
                40,
                200,
            )))
            .unwrap();

        let now = dispatched_at(12);
        for (dimension, expected_rows) in [
            (AgentUsageDimension::Time, 24),
            (AgentUsageDimension::Agent, 2),
            (AgentUsageDimension::Project, 2),
            (AgentUsageDimension::ModelProvider, 2),
            (AgentUsageDimension::Model, 2),
        ] {
            let statistics = service
                .query_statistics_at(
                    AgentUsageStatisticsRequest {
                        dimension,
                        ..fixed_request(AgentUsageRange::Today)
                    },
                    now,
                )
                .unwrap();
            assert_eq!(statistics.dimension_rows.len(), expected_rows);
            assert_eq!(statistics.totals.requests, 2);
        }

        let filtered = service
            .query_statistics_at(
                AgentUsageStatisticsRequest {
                    agent_ids: vec![first_stream.agent_id.clone()],
                    project_ids: vec![first_session.project_id.clone()],
                    provider_profile_ids: vec![first_profile],
                    model_ids: vec![first_stream.model_id.clone().unwrap()],
                    session_ids: vec![first_session.id.clone()],
                    ..fixed_request(AgentUsageRange::Today)
                },
                now,
            )
            .unwrap();
        assert_eq!(filtered.totals.requests, 1);
        assert_eq!(filtered.totals.total_tokens.value, Some(100));
        assert_eq!(
            filtered
                .annual
                .as_ref()
                .unwrap()
                .days
                .iter()
                .map(|day| day.requests)
                .sum::<u64>(),
            1
        );
        assert_eq!(filtered.filter_options.agents.len(), 2);
        assert_eq!(filtered.filter_options.projects.len(), 2);
        assert_eq!(filtered.filter_options.provider_profiles.len(), 2);
        assert_eq!(filtered.filter_options.models.len(), 2);
        assert_eq!(filtered.filter_options.sessions.len(), 2);

        let mismatch = service
            .query_statistics_at(
                AgentUsageStatisticsRequest {
                    agent_ids: vec![first_stream.agent_id],
                    project_ids: vec![second_session.project_id],
                    ..fixed_request(AgentUsageRange::Today)
                },
                now,
            )
            .unwrap();
        assert_eq!(mismatch.totals.requests, 0);
    }

    #[test]
    fn token_sorting_is_directional_deterministic_and_keeps_unknown_last() {
        let (_directory, service, session) = seeded_service();
        for (agent_id, total) in [("agent-b", 200), ("agent-aa", 100), ("agent-a", 100)] {
            let stream = stream(
                &service,
                &session,
                agent_id,
                ProviderProfileId::new(),
                "sort-model",
            );
            let execution = execution(&service, &session, &stream, 9);
            service
                .apply_telemetry_event(dispatched_event(execution.clone()))
                .unwrap();
            service
                .apply_telemetry_event(AgentUsageTelemetryEvent::Observation(observation(
                    execution,
                    1,
                    total / 2,
                    total / 2,
                    total / 4,
                    total,
                )))
                .unwrap();
        }
        let unknown_stream = stream(
            &service,
            &session,
            "agent-c",
            ProviderProfileId::new(),
            "sort-model",
        );
        let unknown_execution = execution(&service, &session, &unknown_stream, 10);
        service
            .apply_telemetry_event(dispatched_event(unknown_execution))
            .unwrap();

        let query = |direction| {
            service
                .query_statistics_at(
                    AgentUsageStatisticsRequest {
                        dimension: AgentUsageDimension::Agent,
                        sort_metric: AgentUsageSortMetric::TotalTokens,
                        sort_direction: direction,
                        ..fixed_request(AgentUsageRange::Today)
                    },
                    dispatched_at(12),
                )
                .unwrap()
                .dimension_rows
                .into_iter()
                .map(|row| row.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            query(AgentUsageSortDirection::Descending),
            ["agent-b", "agent-a", "agent-aa", "agent-c"]
        );
        assert_eq!(
            query(AgentUsageSortDirection::Ascending),
            ["agent-a", "agent-aa", "agent-b", "agent-c"]
        );
    }

    #[test]
    fn partial_reporting_and_cache_hit_rate_keep_eligible_request_coverage() {
        let (_directory, service, session) = seeded_service();
        for (index, (input, output, cached, thought, cached_write, total)) in
            [(100, 50, 50, 10, 20, 150), (200, 100, 100, 20, 40, 300)]
                .into_iter()
                .enumerate()
        {
            let stream = stream(
                &service,
                &session,
                session.agent_id.as_str(),
                ProviderProfileId::new(),
                &format!("coverage-model-{index}"),
            );
            let execution = execution(&service, &session, &stream, 9 + index as u32);
            service
                .apply_telemetry_event(dispatched_event(execution.clone()))
                .unwrap();
            let mut complete = observation(execution, 1, input, output, cached, total);
            complete.cumulative.thought_tokens = Some(thought);
            complete.cumulative.cached_write_tokens = Some(cached_write);
            service
                .apply_telemetry_event(AgentUsageTelemetryEvent::Observation(complete))
                .unwrap();
        }

        let partial_stream = stream(
            &service,
            &session,
            session.agent_id.as_str(),
            ProviderProfileId::new(),
            "coverage-model-partial",
        );
        let partial_execution = execution(&service, &session, &partial_stream, 11);
        service
            .apply_telemetry_event(dispatched_event(partial_execution.clone()))
            .unwrap();
        let mut partial = observation(partial_execution, 1, 300, 0, 0, 0);
        partial.cumulative = AgentUsageTokenValues {
            input_tokens: Some(300),
            ..AgentUsageTokenValues::default()
        };
        service
            .apply_telemetry_event(AgentUsageTelemetryEvent::Observation(partial))
            .unwrap();

        let statistics = service
            .query_statistics_at(fixed_request(AgentUsageRange::Today), dispatched_at(12))
            .unwrap();
        assert_eq!(statistics.totals.requests, 3);
        assert_eq!(statistics.totals.input_tokens.value, Some(600));
        assert_eq!(
            statistics.totals.input_tokens.coverage,
            AgentUsageMetricCoverage::Complete
        );
        assert_eq!(statistics.totals.total_tokens.value, Some(450));
        assert_eq!(
            statistics.totals.total_tokens.coverage,
            AgentUsageMetricCoverage::Partial
        );
        assert_eq!(statistics.totals.cached_tokens.value, Some(150));
        assert_eq!(statistics.totals.thought_tokens.value, Some(30));
        assert_eq!(statistics.totals.cached_write_tokens.value, Some(60));
        assert_eq!(statistics.totals.cache_hit_rate.basis_points, Some(3_333));
        assert_eq!(statistics.totals.cache_hit_rate.eligible_requests, 2);
        assert_eq!(statistics.totals.cache_hit_rate.total_requests, 3);
        assert_eq!(
            statistics.totals.cache_hit_rate.coverage,
            AgentUsageMetricCoverage::Partial
        );
        assert_eq!(statistics.totals.coverage.complete_requests, 2);
        assert_eq!(statistics.totals.coverage.partial_requests, 1);
    }

    #[test]
    fn per_request_totals_are_counted_while_their_breakdown_stays_a_floor() {
        let (_directory, service, session) = seeded_service();
        let stream = stream(
            &service,
            &session,
            session.agent_id.as_str(),
            ProviderProfileId::new(),
            "request-scoped-model",
        );
        let execution = execution(&service, &session, &stream, 9);
        service
            .apply_telemetry_event(dispatched_event(execution.clone()))
            .unwrap();

        // Three API requests inside the turn, each reporting its own total.
        for (index, request_total) in [40_000_u64, 60_000, 80_000].into_iter().enumerate() {
            let mut sample = observation(execution.clone(), index as u64 + 1, 0, 0, 0, 0);
            sample.source = AgentUsageObservationSource::RequestSample;
            sample.counter_scope = AgentUsageCounterScope::Request;
            sample.cumulative = AgentUsageTokenValues {
                total_tokens: Some(request_total),
                ..AgentUsageTokenValues::default()
            };
            service
                .apply_telemetry_event(AgentUsageTelemetryEvent::Observation(sample))
                .unwrap();
        }

        // The turn-level reading breaks down that last request alone.
        let mut turn_end = observation(execution, 4, 5_000, 1_000, 74_000, 80_000);
        turn_end.counter_scope = AgentUsageCounterScope::Request;
        service
            .apply_telemetry_event(AgentUsageTelemetryEvent::Observation(turn_end))
            .unwrap();

        let statistics = service
            .query_statistics_at(fixed_request(AgentUsageRange::Today), dispatched_at(12))
            .unwrap();
        assert_eq!(statistics.totals.requests, 1);
        assert_eq!(statistics.totals.api_requests, Some(3));
        assert_eq!(statistics.totals.total_tokens.value, Some(180_000));
        assert_eq!(statistics.totals.input_tokens.value, Some(5_000));
        assert_eq!(
            statistics.totals.input_tokens.coverage,
            AgentUsageMetricCoverage::Partial
        );
        assert_eq!(
            statistics.totals.cached_tokens.coverage,
            AgentUsageMetricCoverage::Partial
        );
        assert_eq!(
            statistics.totals.cache_hit_rate.coverage,
            AgentUsageMetricCoverage::Partial
        );
    }
}

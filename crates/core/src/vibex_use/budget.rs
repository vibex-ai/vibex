use serde::{Deserialize, Serialize};

use super::{VIBEX_USE_MAX_DEPTH, VIBEX_USE_ROOT_EXECUTION_BUDGET};
use crate::{VibexError, VibexResult};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VibexUseBudgetPreset {
    Conservative,
    #[default]
    Balanced,
    Expanded,
}

/// Runtime-owned limits, independent from the number of visible conversation
/// panes. They affect new admissions; changing them never kills existing work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VibexUseBudgetPolicy {
    pub max_depth: u32,
    pub root_execution_limit: u32,
    pub per_agent_execution_limit: u32,
    pub task_timeout_ms: u64,
    pub idle_worker_retention_ms: u64,
    pub warm_worker_limit: usize,
    /// A limit on reported token usage, not a price or an estimated token count.
    pub max_reported_tokens: Option<u64>,
}

impl Default for VibexUseBudgetPolicy {
    fn default() -> Self {
        Self::for_preset(VibexUseBudgetPreset::Balanced)
    }
}

impl VibexUseBudgetPolicy {
    pub fn for_preset(preset: VibexUseBudgetPreset) -> Self {
        match preset {
            VibexUseBudgetPreset::Conservative => Self {
                max_depth: 1,
                root_execution_limit: 4,
                per_agent_execution_limit: 2,
                task_timeout_ms: 30 * 60 * 1_000,
                idle_worker_retention_ms: 2 * 60 * 1_000,
                warm_worker_limit: 1,
                max_reported_tokens: None,
            },
            VibexUseBudgetPreset::Balanced => Self {
                max_depth: VIBEX_USE_MAX_DEPTH,
                root_execution_limit: VIBEX_USE_ROOT_EXECUTION_BUDGET,
                per_agent_execution_limit: 8,
                task_timeout_ms: 60 * 60 * 1_000,
                idle_worker_retention_ms: 5 * 60 * 1_000,
                warm_worker_limit: 2,
                max_reported_tokens: None,
            },
            VibexUseBudgetPreset::Expanded => Self {
                max_depth: 4,
                root_execution_limit: 16,
                per_agent_execution_limit: 8,
                task_timeout_ms: 4 * 60 * 60 * 1_000,
                idle_worker_retention_ms: 15 * 60 * 1_000,
                warm_worker_limit: 4,
                max_reported_tokens: None,
            },
        }
    }

    pub fn validate(&self) -> VibexResult<()> {
        if !(1..=8).contains(&self.max_depth)
            || !(1..=64).contains(&self.root_execution_limit)
            || !(1..=64).contains(&self.per_agent_execution_limit)
            || !(1_000..=24 * 60 * 60 * 1_000).contains(&self.task_timeout_ms)
            || !(1_000..=24 * 60 * 60 * 1_000).contains(&self.idle_worker_retention_ms)
            || !(1..=64).contains(&self.warm_worker_limit)
            || self
                .max_reported_tokens
                .is_some_and(|limit| limit == 0 || limit > i64::MAX as u64)
        {
            return Err(VibexError::validation(
                "vibex_use_budget_invalid",
                "team budget limits are outside the supported range",
            ));
        }
        Ok(())
    }
}

/// An optional local configuration file selects a preset and overrides only
/// the limits the user specifies. Preset defaults therefore stay meaningful.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct VibexUseBudgetSettings {
    pub preset: VibexUseBudgetPreset,
    pub max_depth: Option<u32>,
    pub root_execution_limit: Option<u32>,
    pub per_agent_execution_limit: Option<u32>,
    pub task_timeout_ms: Option<u64>,
    pub idle_worker_retention_ms: Option<u64>,
    pub warm_worker_limit: Option<usize>,
    pub max_reported_tokens: Option<u64>,
}

impl VibexUseBudgetSettings {
    pub fn resolve(&self) -> VibexResult<VibexUseBudgetPolicy> {
        let mut policy = VibexUseBudgetPolicy::for_preset(self.preset);
        if let Some(value) = self.max_depth {
            policy.max_depth = value;
        }
        if let Some(value) = self.root_execution_limit {
            policy.root_execution_limit = value;
        }
        if let Some(value) = self.per_agent_execution_limit {
            policy.per_agent_execution_limit = value;
        }
        if let Some(value) = self.task_timeout_ms {
            policy.task_timeout_ms = value;
        }
        if let Some(value) = self.idle_worker_retention_ms {
            policy.idle_worker_retention_ms = value;
        }
        if let Some(value) = self.warm_worker_limit {
            policy.warm_worker_limit = value;
        }
        policy.max_reported_tokens = self.max_reported_tokens;
        policy.validate()?;
        Ok(policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_overrides_keep_unmodified_preset_limits() {
        let settings: VibexUseBudgetSettings =
            serde_json::from_str(r#"{"preset":"expanded","perAgentExecutionLimit":3}"#).unwrap();
        let policy = settings.resolve().unwrap();
        assert_eq!(policy.root_execution_limit, 16);
        assert_eq!(policy.per_agent_execution_limit, 3);
        assert!(
            serde_json::from_str::<VibexUseBudgetSettings>(r#"{"maxDepth":0}"#)
                .unwrap()
                .resolve()
                .is_err()
        );
    }
}

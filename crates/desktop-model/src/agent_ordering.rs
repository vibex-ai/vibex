//! Shared Agent list ordering for the new-session selector and the
//! Management Center Agent sidebar.
//!
//! Both surfaces derive their order from the same [`AgentOrdering`] inputs so a
//! user's sort strategy and manual adjustments stay consistent. A non-empty
//! manual order (drag-adjusted on the new-session selector) wins over the
//! persisted sort strategy.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Persisted strategy used when the user has not manually reordered Agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentSortStrategy {
    /// Order by display label, case-insensitive.
    #[default]
    Alphabetical,
    /// Order by usage count, most used first, ties broken alphabetically.
    UsageFrequency,
}

/// One Agent identity the ordering operates on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOrderEntry {
    pub id: String,
    pub label: String,
}

/// Ordering inputs shared between the new-session selector and the
/// Management Center Agent sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentOrdering {
    #[serde(default)]
    pub strategy: AgentSortStrategy,
    #[serde(default)]
    pub manual_order: Vec<String>,
    #[serde(default)]
    pub usage_counts: BTreeMap<String, u64>,
}

impl AgentOrdering {
    pub fn new(strategy: AgentSortStrategy, manual_order: Vec<String>) -> Self {
        Self {
            strategy,
            manual_order,
            usage_counts: BTreeMap::new(),
        }
    }

    pub fn with_usage_counts(mut self, usage_counts: BTreeMap<String, u64>) -> Self {
        self.usage_counts = usage_counts;
        self
    }
}

/// Order the given agents with the shared ordering rules and return the
/// agent ids in display order. Every input id appears exactly once; ids
/// missing from a manual order are appended in alphabetical position.
pub fn ordered_agent_ids(agents: &[AgentOrderEntry], ordering: &AgentOrdering) -> Vec<String> {
    let alphabetical = alphabetical_agent_ids(agents);
    let manual_order = effective_manual_order(ordering, &alphabetical);
    if !manual_order.is_empty() {
        let mut order = manual_order;
        complete_string_order(&mut order, alphabetical.clone());
        return order;
    }
    match ordering.strategy {
        AgentSortStrategy::Alphabetical => alphabetical,
        AgentSortStrategy::UsageFrequency => {
            let mut sorted = agents.iter().collect::<Vec<_>>();
            sorted.sort_by(|left, right| {
                let left_usage = ordering.usage_counts.get(&left.id).copied().unwrap_or(0);
                let right_usage = ordering.usage_counts.get(&right.id).copied().unwrap_or(0);
                right_usage
                    .cmp(&left_usage)
                    .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
                    .then_with(|| left.id.cmp(&right.id))
            });
            sorted.into_iter().map(|agent| agent.id.clone()).collect()
        }
    }
}

fn alphabetical_agent_ids(agents: &[AgentOrderEntry]) -> Vec<String> {
    let mut sorted = agents.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    sorted.into_iter().map(|agent| agent.id.clone()).collect()
}

/// The manual order restricted to known agent ids; empty when it carries no
/// known id and therefore must not override the strategy.
fn effective_manual_order(ordering: &AgentOrdering, known_ids: &[String]) -> Vec<String> {
    let known = known_ids
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    ordering
        .manual_order
        .iter()
        .filter(|id| known.contains(*id))
        .cloned()
        .collect()
}

/// Deduplicate `order`, drop ids that are no longer valid, and append valid
/// ids that are missing, keeping every id exactly once.
pub fn complete_string_order(order: &mut Vec<String>, ids: Vec<String>) {
    let valid = ids
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut seen = std::collections::BTreeSet::new();
    order.retain(|id| valid.contains(id) && seen.insert(id.clone()));
    order.extend(ids.into_iter().filter(|id| seen.insert(id.clone())));
}

/// Move `moving_id` relative to `target_id` inside `order`. Returns whether
/// the order changed.
pub fn move_string_relative(
    order: &mut Vec<String>,
    moving_id: &str,
    target_id: &str,
    after: bool,
) -> bool {
    move_strings_relative(
        order,
        std::slice::from_ref(&moving_id.to_string()),
        target_id,
        after,
    )
}

/// Move several ids relative to `target_id`, preserving their mutual order.
pub fn move_strings_relative(
    order: &mut Vec<String>,
    moving_ids: &[String],
    target_id: &str,
    after: bool,
) -> bool {
    let moving_set = moving_ids
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if moving_ids.is_empty()
        || moving_set.len() != moving_ids.len()
        || moving_set.contains(target_id)
        || moving_ids
            .iter()
            .any(|moving_id| !order.contains(moving_id))
    {
        return false;
    }
    let original = order.clone();
    let moving = moving_ids.to_vec();
    order.retain(|id| !moving_set.contains(id));
    let Some(target_index) = order.iter().position(|id| id == target_id) else {
        *order = original;
        return false;
    };
    let insertion_index = target_index + usize::from(after);
    order.splice(insertion_index..insertion_index, moving);
    *order != original
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, label: &str) -> AgentOrderEntry {
        AgentOrderEntry {
            id: id.to_string(),
            label: label.to_string(),
        }
    }

    fn agents() -> Vec<AgentOrderEntry> {
        vec![
            entry("claude", "Claude Code"),
            entry("codex", "Codex"),
            entry("gemini", "Gemini CLI"),
            entry("copilot", "GitHub Copilot"),
        ]
    }

    #[test]
    fn alphabetical_is_the_default_strategy() {
        assert_eq!(
            ordered_agent_ids(&agents(), &AgentOrdering::default()),
            ["claude", "codex", "gemini", "copilot"]
        );
    }

    #[test]
    fn usage_frequency_sorts_most_used_first_and_breaks_ties_alphabetically() {
        let mut usage = BTreeMap::new();
        usage.insert("gemini".to_string(), 3);
        usage.insert("claude".to_string(), 7);
        usage.insert("copilot".to_string(), 1);
        let ordering = AgentOrdering::new(AgentSortStrategy::UsageFrequency, Vec::new())
            .with_usage_counts(usage);

        assert_eq!(
            ordered_agent_ids(&agents(), &ordering),
            ["claude", "gemini", "copilot", "codex"]
        );
    }

    #[test]
    fn usage_frequency_falls_back_to_alphabetical_without_counts() {
        let ordering = AgentOrdering::new(AgentSortStrategy::UsageFrequency, Vec::new());
        assert_eq!(
            ordered_agent_ids(&agents(), &ordering),
            ["claude", "codex", "gemini", "copilot"]
        );
    }

    #[test]
    fn manual_order_wins_over_the_strategy_and_appends_unknown_ids() {
        let mut usage = BTreeMap::new();
        usage.insert("claude".to_string(), 99);
        let ordering = AgentOrdering::new(
            AgentSortStrategy::UsageFrequency,
            vec!["codex".to_string(), "copilot".to_string()],
        )
        .with_usage_counts(usage);

        assert_eq!(
            ordered_agent_ids(&agents(), &ordering),
            ["codex", "copilot", "claude", "gemini"]
        );
    }

    #[test]
    fn stale_manual_order_falls_back_to_the_strategy() {
        let ordering = AgentOrdering::new(
            AgentSortStrategy::Alphabetical,
            vec!["removed-agent".to_string()],
        );
        assert_eq!(
            ordered_agent_ids(&agents(), &ordering),
            ["claude", "codex", "gemini", "copilot"]
        );
    }

    #[test]
    fn complete_and_move_helpers_keep_every_id_once() {
        let mut order = vec![
            "claude".to_string(),
            "stale".to_string(),
            "claude".to_string(),
        ];
        complete_string_order(
            &mut order,
            ["codex", "claude", "gemini"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        );
        assert_eq!(order, ["claude", "codex", "gemini"]);

        assert!(move_string_relative(&mut order, "gemini", "claude", false));
        assert_eq!(order, ["gemini", "claude", "codex"]);
        assert!(!move_string_relative(&mut order, "gemini", "gemini", true));
        assert_eq!(order, ["gemini", "claude", "codex"]);
    }
}

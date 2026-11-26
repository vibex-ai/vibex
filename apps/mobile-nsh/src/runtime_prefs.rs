//! Remembered Agent, provider, model and run-configuration choices per runtime.
//!
//! Mirrors the desktop and TUI runtime preference rules (`default`, `by_agent`,
//! `by_model`) so opening a new session restores the last used Agent, provider,
//! model and options, and switching Agents or models restores each one's last
//! remembered provider, model and options.

use std::collections::BTreeMap;

use nsh_core::storage::Storage;
use serde::{Deserialize, Serialize};
use vibex_core::{
    AgentId, RuntimeOptionAvailability, SessionRuntimeFeatureKind, SessionRuntimeOption,
    SessionRuntimeSelection,
};

/// Maximum number of per-model remembered configurations kept per runtime.
pub const RUNTIME_PREFERENCE_LIMIT: usize = 64;

/// Storage key for per-runtime remembered selections.
const RUNTIME_PREFS_KEY: &str = "vibex.mobile.runtime_prefs.v1";

/// Whether two selections name the same catalogue entry (Agent + provider + model).
pub fn identity_matches(left: &SessionRuntimeSelection, right: &SessionRuntimeSelection) -> bool {
    left.agent_id == right.agent_id
        && left.auth_source == right.auth_source
        && left.model == right.model
}

/// Strips run options that should not be persisted (free-text `String` features
/// and values no longer offered by the matching catalogue entry).
pub fn persisted_selection(
    options: &[SessionRuntimeOption],
    selection: &SessionRuntimeSelection,
) -> SessionRuntimeSelection {
    let mut persisted = selection.clone();
    let matched = options
        .iter()
        .find(|option| identity_matches(&option.selection, selection));

    if let Some(option) = matched {
        if let Some(effort) = persisted.reasoning_effort.as_ref()
            && !option
                .reasoning_efforts
                .iter()
                .any(|candidate| &candidate.value == effort)
        {
            persisted.reasoning_effort = None;
        }
        if let Some(mode) = persisted.mode_id.as_ref()
            && !option
                .modes
                .iter()
                .any(|candidate| &candidate.value == mode)
        {
            persisted.mode_id = None;
        }
        persisted.config_values.retain(|key, value| {
            option
                .features
                .iter()
                .find(|feature| &feature.id == key)
                .is_some_and(|feature| {
                    matches!(
                        feature.kind,
                        SessionRuntimeFeatureKind::Toggle | SessionRuntimeFeatureKind::Select
                    ) && feature.accepts_value(value)
                })
        });
    } else {
        // Without a catalogue entry we cannot distinguish closed-set features
        // from free-text `String` features, so avoid persisting unvalidated
        // config values.
        persisted.config_values.clear();
    }

    persisted
}

/// Folds any still-valid reasoning, mode and Toggle/Select feature values from
/// `source` onto `option.selection`.
pub fn apply_valid_options(
    option: &SessionRuntimeOption,
    source: &SessionRuntimeSelection,
) -> SessionRuntimeSelection {
    let mut selection = option.selection.clone();
    if let Some(effort) = source
        .reasoning_effort
        .as_ref()
        .filter(|effort| {
            option
                .reasoning_efforts
                .iter()
                .any(|candidate| &candidate.value == *effort)
        })
        .cloned()
    {
        selection.reasoning_effort = Some(effort);
    }
    if let Some(mode) = source
        .mode_id
        .as_ref()
        .filter(|mode| {
            option
                .modes
                .iter()
                .any(|candidate| &candidate.value == *mode)
        })
        .cloned()
    {
        selection.mode_id = Some(mode);
    }
    for (key, value) in &source.config_values {
        let accepted = option
            .features
            .iter()
            .find(|feature| &feature.id == key)
            .is_some_and(|feature| {
                matches!(
                    feature.kind,
                    SessionRuntimeFeatureKind::Toggle | SessionRuntimeFeatureKind::Select
                ) && feature.accepts_value(value)
            });
        if accepted {
            selection.config_values.insert(key.clone(), value.clone());
        }
    }
    selection
}

/// The three remembered runtime layers for one paired host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePreferences {
    /// The selection last applied to any session or the new-session composer.
    #[serde(default)]
    pub default: Option<SessionRuntimeSelection>,
    /// The last selection used for each Agent.
    #[serde(default)]
    pub by_agent: BTreeMap<AgentId, SessionRuntimeSelection>,
    /// The last configuration used for each `(agent_id, auth_source, model)`,
    /// ordered oldest-first (most recent last).
    #[serde(default)]
    pub by_model: Vec<SessionRuntimeSelection>,
}

impl RuntimePreferences {
    /// Reads the saved preferences for `host_id` from local storage.
    pub fn load_for_host(host_id: &str) -> Self {
        Storage::get_item(RUNTIME_PREFS_KEY)
            .and_then(|raw| serde_json::from_str::<BTreeMap<String, Self>>(&raw).ok())
            .and_then(|mut map| map.remove(host_id))
            .unwrap_or_default()
    }

    /// Saves this host's preferences into local storage, preserving other hosts.
    pub fn save_for_host(&self, host_id: &str) {
        let mut map = Storage::get_item(RUNTIME_PREFS_KEY)
            .and_then(|raw| serde_json::from_str::<BTreeMap<String, Self>>(&raw).ok())
            .unwrap_or_default();
        map.insert(host_id.to_owned(), self.clone());
        if let Ok(text) = serde_json::to_string(&map) {
            let _ = Storage::set_item(RUNTIME_PREFS_KEY, text);
        }
    }

    /// Records `selection` as the default, its Agent's latest choice, and its
    /// model identity's latest configuration.
    pub fn remember(
        &mut self,
        selection: &SessionRuntimeSelection,
        options: &[SessionRuntimeOption],
    ) {
        let selection = persisted_selection(options, selection);
        self.default = Some(selection.clone());
        self.by_agent
            .insert(selection.agent_id.clone(), selection.clone());
        self.by_model
            .retain(|existing| !identity_matches(existing, &selection));
        while self.by_model.len() >= RUNTIME_PREFERENCE_LIMIT {
            self.by_model.remove(0);
        }
        self.by_model.push(selection);
    }

    /// Restores the remembered reasoning, mode and Toggle/Select options for
    /// `option`'s `(agent_id, auth_source, model)` identity.
    pub fn with_remembered_options(
        &self,
        option: &SessionRuntimeOption,
    ) -> SessionRuntimeSelection {
        let remembered = self
            .by_model
            .iter()
            .rev()
            .find(|remembered| identity_matches(remembered, &option.selection))
            .or_else(|| {
                self.by_agent
                    .get(&option.selection.agent_id)
                    .filter(|remembered| identity_matches(remembered, &option.selection))
            })
            .or_else(|| {
                self.default
                    .as_ref()
                    .filter(|remembered| identity_matches(remembered, &option.selection))
            });
        match remembered {
            Some(remembered) => apply_valid_options(option, remembered),
            None => option.selection.clone(),
        }
    }

    /// Resolves the remembered selection for either the global default
    /// (`agent_id == None`) or a specific Agent (`agent_id == Some(..)`).
    ///
    /// Resolution order matches Desktop:
    /// 1. Exact available `(agent_id, auth_source, model)` with valid remembered options.
    /// 2. First available option for the same `agent_id` + `auth_source` (same provider).
    /// 3. First available option for the same `agent_id`.
    pub fn preferred(
        &self,
        options: &[SessionRuntimeOption],
        agent_id: Option<&AgentId>,
    ) -> Option<SessionRuntimeSelection> {
        let remembered = match agent_id {
            Some(agent_id) => self.by_agent.get(agent_id).or_else(|| {
                self.default
                    .as_ref()
                    .filter(|selection| &selection.agent_id == agent_id)
            }),
            None => self.default.as_ref(),
        }?;

        if let Some(exact) = options.iter().find(|option| {
            option.availability == RuntimeOptionAvailability::Available
                && identity_matches(&option.selection, remembered)
        }) {
            // Prefer `by_model` if present; fall back to `remembered` itself if
            // the entry was evicted from `by_model` while still held in `by_agent`.
            let from_by_model = self
                .by_model
                .iter()
                .rev()
                .find(|entry| identity_matches(entry, &exact.selection));
            return Some(apply_valid_options(
                exact,
                from_by_model.unwrap_or(remembered),
            ));
        }

        let agent = &remembered.agent_id;
        let fallback = options
            .iter()
            .find(|option| {
                option.availability == RuntimeOptionAvailability::Available
                    && &option.selection.agent_id == agent
                    && option.selection.auth_source == remembered.auth_source
            })
            .or_else(|| {
                options.iter().find(|option| {
                    option.availability == RuntimeOptionAvailability::Available
                        && &option.selection.agent_id == agent
                })
            })?;
        Some(self.with_remembered_options(fallback))
    }

    /// Resolves the index in `options` of `agent_id`'s preferred available option,
    /// used to highlight/expand that Agent's last-used provider fold in the picker.
    pub fn preferred_option_index_for_agent(
        &self,
        options: &[SessionRuntimeOption],
        agent_id: &AgentId,
    ) -> Option<usize> {
        let preferred = self.preferred(options, Some(agent_id))?;
        options.iter().position(|option| {
            option.availability == RuntimeOptionAvailability::Available
                && identity_matches(&option.selection, &preferred)
        })
    }

    /// Resolves the initial or switched selection for the New Session composer:
    /// 1. Remembered preference (`self.preferred(options, agent_id)`).
    /// 2. Active session's selection (when available and matching `agent_id` if given).
    /// 3. Desktop-aligned default: first available option for `agent_id` (or first
    ///    available `codex` option, then any first available option).
    pub fn resolve_new_session_selection(
        &self,
        options: &[SessionRuntimeOption],
        active_session: Option<&SessionRuntimeSelection>,
        agent_id: Option<&AgentId>,
    ) -> Option<SessionRuntimeSelection> {
        if let Some(preferred) = self.preferred(options, agent_id) {
            return Some(preferred);
        }

        if let Some(active) = active_session
            && agent_id.is_none_or(|target| target == &active.agent_id)
        {
            if let Some(exact) = options.iter().find(|option| {
                option.availability == RuntimeOptionAvailability::Available
                    && identity_matches(&option.selection, active)
            }) {
                return Some(apply_valid_options(exact, active));
            }
            if let Some(same_provider) = options.iter().find(|option| {
                option.availability == RuntimeOptionAvailability::Available
                    && option.selection.agent_id == active.agent_id
                    && option.selection.auth_source == active.auth_source
            }) {
                return Some(self.with_remembered_options(same_provider));
            }
        }

        let fallback = if let Some(agent_id) = agent_id {
            options.iter().find(|option| {
                option.availability == RuntimeOptionAvailability::Available
                    && &option.selection.agent_id == agent_id
            })
        } else {
            options
                .iter()
                .find(|option| {
                    option.availability == RuntimeOptionAvailability::Available
                        && option.selection.agent_id.as_str() == "codex"
                })
                .or_else(|| {
                    options
                        .iter()
                        .find(|option| option.availability == RuntimeOptionAvailability::Available)
                })
        }?;
        Some(self.with_remembered_options(fallback))
    }

    /// Keeps `existing` if it still matches an available option in `options`
    /// (re-validating its options against the fresh catalogue).
    pub fn revalidate_selection(
        options: &[SessionRuntimeOption],
        existing: &SessionRuntimeSelection,
    ) -> Option<SessionRuntimeSelection> {
        options
            .iter()
            .find(|option| {
                option.availability == RuntimeOptionAvailability::Available
                    && identity_matches(&option.selection, existing)
            })
            .map(|option| apply_valid_options(option, existing))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        ProviderProfileId, RuntimeAuthSource, RuntimeModelSelection, SessionConfigValue,
        SessionRuntimeFeature,
    };

    fn provider(id: &str) -> RuntimeAuthSource {
        RuntimeAuthSource::ProviderProfile {
            provider_profile_id: ProviderProfileId::parse(id).unwrap(),
        }
    }

    fn make_option(
        agent: &str,
        auth_source: RuntimeAuthSource,
        model_id: &str,
        available: bool,
    ) -> SessionRuntimeOption {
        SessionRuntimeOption {
            selection: SessionRuntimeSelection {
                agent_id: AgentId::parse(agent).unwrap(),
                auth_source,
                model: RuntimeModelSelection::Explicit {
                    model_id: model_id.to_owned(),
                },
                reasoning_effort: None,
                mode_id: None,
                config_values: BTreeMap::new(),
            },
            agent_label: agent.to_owned(),
            auth_source_label: "Provider".to_owned(),
            model_label: model_id.to_owned(),
            availability: if available {
                RuntimeOptionAvailability::Available
            } else {
                RuntimeOptionAvailability::TemporarilyUnavailable
            },
            member_count: 1,
            auto_routes: false,
            reasoning_efforts: vec![
                SessionConfigValue {
                    value: "low".to_owned(),
                    label: Some("Low".to_owned()),
                },
                SessionConfigValue {
                    value: "high".to_owned(),
                    label: Some("High".to_owned()),
                },
            ],
            modes: vec![
                SessionConfigValue {
                    value: "default".to_owned(),
                    label: Some("Default".to_owned()),
                },
                SessionConfigValue {
                    value: "plan".to_owned(),
                    label: Some("Plan".to_owned()),
                },
            ],
            features: vec![
                SessionRuntimeFeature {
                    id: "fast_mode".to_owned(),
                    label: "Fast mode".to_owned(),
                    description: None,
                    kind: SessionRuntimeFeatureKind::Toggle,
                    current_value: None,
                    default_value: Some(SessionConfigValue {
                        value: "false".to_owned(),
                        label: None,
                    }),
                    values: Vec::new(),
                },
                SessionRuntimeFeature {
                    id: "tier".to_owned(),
                    label: "Tier".to_owned(),
                    description: None,
                    kind: SessionRuntimeFeatureKind::Select,
                    current_value: None,
                    default_value: Some(SessionConfigValue {
                        value: "standard".to_owned(),
                        label: Some("Standard".to_owned()),
                    }),
                    values: vec![
                        SessionConfigValue {
                            value: "standard".to_owned(),
                            label: Some("Standard".to_owned()),
                        },
                        SessionConfigValue {
                            value: "priority".to_owned(),
                            label: Some("Priority".to_owned()),
                        },
                    ],
                },
                SessionRuntimeFeature {
                    id: "custom_note".to_owned(),
                    label: "Custom note".to_owned(),
                    description: None,
                    kind: SessionRuntimeFeatureKind::String,
                    current_value: None,
                    default_value: None,
                    values: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn remembers_default_agent_provider_model_and_closed_set_configs() {
        let options = vec![
            make_option("codex", provider("provider_openai"), "gpt-5.4", true),
            make_option(
                "claude",
                provider("provider_anthropic"),
                "claude-sonnet",
                true,
            ),
            make_option(
                "claude",
                provider("provider_openrouter"),
                "claude-opus",
                true,
            ),
        ];

        let mut prefs = RuntimePreferences::default();
        let mut chosen = options[2].selection.clone();
        chosen.reasoning_effort = Some("high".to_owned());
        chosen.mode_id = Some("plan".to_owned());
        chosen
            .config_values
            .insert("fast_mode".to_owned(), "true".to_owned());
        chosen
            .config_values
            .insert("tier".to_owned(), "priority".to_owned());
        chosen
            .config_values
            .insert("custom_note".to_owned(), "secret prompt".to_owned());

        prefs.remember(&chosen, &options);

        let restored = prefs
            .resolve_new_session_selection(&options, None, None)
            .expect("should restore remembered default");
        assert_eq!(restored.agent_id.as_str(), "claude");
        assert_eq!(restored.auth_source, provider("provider_openrouter"));
        assert_eq!(restored.model.model_id(), Some("claude-opus"));
        assert_eq!(restored.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(restored.mode_id.as_deref(), Some("plan"));
        assert_eq!(
            restored.config_values.get("fast_mode").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            restored.config_values.get("tier").map(String::as_str),
            Some("priority")
        );
        assert!(
            !restored.config_values.contains_key("custom_note"),
            "free-text String features must never be persisted"
        );
    }

    #[test]
    fn switching_agent_and_model_restores_each_ones_last_provider_and_configs() {
        let options = vec![
            make_option("codex", provider("provider_openai"), "gpt-5.4", true),
            make_option("codex", provider("provider_azure"), "gpt-5.4-mini", true),
            make_option(
                "claude",
                provider("provider_anthropic"),
                "claude-sonnet",
                true,
            ),
            make_option(
                "claude",
                provider("provider_openrouter"),
                "claude-opus",
                true,
            ),
        ];

        let mut prefs = RuntimePreferences::default();

        // 1. User configures codex + azure + gpt-5.4-mini with low reasoning + fast_mode=true
        let mut codex_mini = options[1].selection.clone();
        codex_mini.reasoning_effort = Some("low".to_owned());
        codex_mini
            .config_values
            .insert("fast_mode".to_owned(), "true".to_owned());
        prefs.remember(&codex_mini, &options);

        // 2. User configures claude + openrouter + claude-opus with high reasoning + plan mode
        let mut claude_opus = options[3].selection.clone();
        claude_opus.reasoning_effort = Some("high".to_owned());
        claude_opus.mode_id = Some("plan".to_owned());
        prefs.remember(&claude_opus, &options);

        // 3. Switching back to codex restores azure + gpt-5.4-mini + low + fast_mode=true
        let codex_id = AgentId::parse("codex").unwrap();
        let restored_codex = prefs
            .resolve_new_session_selection(&options, None, Some(&codex_id))
            .unwrap();
        assert_eq!(restored_codex.auth_source, provider("provider_azure"));
        assert_eq!(restored_codex.model.model_id(), Some("gpt-5.4-mini"));
        assert_eq!(restored_codex.reasoning_effort.as_deref(), Some("low"));
        assert_eq!(restored_codex.mode_id, None);
        assert_eq!(
            restored_codex
                .config_values
                .get("fast_mode")
                .map(String::as_str),
            Some("true")
        );

        // 4. Switching to a different model without saved configs starts clean (no leak from mini)
        let fresh_gpt = prefs.with_remembered_options(&options[0]);
        assert_eq!(fresh_gpt.reasoning_effort, None);
        assert!(fresh_gpt.config_values.is_empty());

        // 5. Switching to claude-opus restores its own configs
        let restored_opus = prefs.with_remembered_options(&options[3]);
        assert_eq!(restored_opus.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(restored_opus.mode_id.as_deref(), Some("plan"));
    }

    #[test]
    fn missing_remembered_model_falls_back_to_same_provider_then_same_agent() {
        let initial = vec![
            make_option(
                "claude",
                provider("provider_openrouter"),
                "claude-3-old",
                true,
            ),
            make_option(
                "claude",
                provider("provider_openrouter"),
                "claude-opus",
                true,
            ),
            make_option(
                "claude",
                provider("provider_anthropic"),
                "claude-sonnet",
                true,
            ),
        ];

        let mut prefs = RuntimePreferences::default();
        let mut old_choice = initial[0].selection.clone();
        old_choice.reasoning_effort = Some("high".to_owned());
        prefs.remember(&old_choice, &initial);

        // Old model removed, same provider still has claude-opus available
        let updated = vec![initial[2].clone(), initial[1].clone()];
        let fallback_provider = prefs
            .preferred(&updated, Some(&AgentId::parse("claude").unwrap()))
            .unwrap();
        assert_eq!(
            fallback_provider.auth_source,
            provider("provider_openrouter")
        );
        assert_eq!(fallback_provider.model.model_id(), Some("claude-opus"));

        // Entire provider removed, falls back to same agent's available option
        let anthropic_only = vec![initial[2].clone()];
        let fallback_agent = prefs
            .preferred(&anthropic_only, Some(&AgentId::parse("claude").unwrap()))
            .unwrap();
        assert_eq!(fallback_agent.auth_source, provider("provider_anthropic"));
        assert_eq!(fallback_agent.model.model_id(), Some("claude-sonnet"));
    }

    #[test]
    fn drops_stale_reasoning_or_mode_without_discarding_remembered_model() {
        let mut options = vec![make_option(
            "codex",
            provider("provider_openai"),
            "gpt-5.4",
            true,
        )];
        let mut prefs = RuntimePreferences::default();
        let mut chosen = options[0].selection.clone();
        chosen.reasoning_effort = Some("high".to_owned());
        chosen.mode_id = Some("plan".to_owned());
        prefs.remember(&chosen, &options);

        // Server removes "high" reasoning effort, keeps "plan" mode
        options[0].reasoning_efforts.retain(|c| c.value != "high");
        let restored = prefs.preferred(&options, None).unwrap();
        assert_eq!(restored.model.model_id(), Some("gpt-5.4"));
        assert_eq!(restored.reasoning_effort, None);
        assert_eq!(restored.mode_id.as_deref(), Some("plan"));
    }

    #[test]
    fn fresh_fallback_prefers_active_session_then_codex_then_first_available() {
        let options = vec![
            make_option(
                "claude",
                provider("provider_anthropic"),
                "claude-sonnet",
                true,
            ),
            make_option("codex", provider("provider_openai"), "gpt-5.4", true),
        ];
        let prefs = RuntimePreferences::default();

        // Active session takes priority when no saved preferences exist
        let active = options[0].selection.clone();
        let from_active = prefs
            .resolve_new_session_selection(&options, Some(&active), None)
            .unwrap();
        assert_eq!(from_active.agent_id.as_str(), "claude");

        // Without active session, matches Desktop default (prefer codex)
        let from_default = prefs
            .resolve_new_session_selection(&options, None, None)
            .unwrap();
        assert_eq!(from_default.agent_id.as_str(), "codex");
    }

    #[test]
    fn recency_list_is_bounded_by_limit() {
        let mut prefs = RuntimePreferences::default();
        let mut options = Vec::new();
        for i in 0..(RUNTIME_PREFERENCE_LIMIT + 5) {
            options.push(make_option(
                "codex",
                provider("provider_openai"),
                &format!("model-{i}"),
                true,
            ));
        }
        for option in &options {
            prefs.remember(&option.selection, &options);
        }
        assert_eq!(prefs.by_model.len(), RUNTIME_PREFERENCE_LIMIT);
        assert_eq!(
            prefs.by_model.last().unwrap().model.model_id(),
            Some(format!("model-{}", RUNTIME_PREFERENCE_LIMIT + 4).as_str())
        );
    }
}

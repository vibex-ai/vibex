//! What the runtime switcher remembers between runs.
//!
//! The switcher asks two questions — which Agent and model a message goes
//! through, and how that Agent runs — and a reader who answered them once
//! should not have to answer them again. The desktop keeps the same three
//! answers in its UI state (`default_runtime_selection`,
//! `runtime_selections_by_agent`, `runtime_selections_by_model`); the shapes and
//! the rules are mirrored here, while the storage is not, because the two
//! clients do not share a home.
//!
//! The file sits beside the session-list arrangement and the key overrides
//! under the same home, so a reader who wants to reset the interface clears one
//! directory.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use vibex_core::{
    AgentId, RuntimeModelSelection, RuntimeOptionAvailability, SessionRuntimeFeatureKind,
    SessionRuntimeOption, SessionRuntimeOptionCatalog, SessionRuntimeSelection,
};

/// How many remembered model selections the file keeps.
///
/// The list is a recency record, not a history: past this many entries the
/// oldest is evicted, so a long-lived install cannot grow the file without
/// limit. The desktop's own bound is larger because it also carries every
/// per-model preference the provider menus use; the switcher needs the tail.
pub const RUNTIME_PREFERENCE_LIMIT: usize = 64;

/// How many starred models the file keeps.
pub const RUNTIME_FAVORITE_LIMIT: usize = 32;

/// The key a starred model is filed under.
///
/// Agent plus model rather than the whole selection: a star follows the model
/// across the accounts and provider profiles that advertise it, which is what
/// makes it a shortcut rather than a fourth copy of the catalogue.
pub fn model_key(model: &RuntimeModelSelection) -> String {
    match model {
        RuntimeModelSelection::Explicit { model_id } => format!("model:{model_id}"),
        RuntimeModelSelection::AgentDefault => "agent-default".to_string(),
    }
}

/// One starred model.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeFavorite {
    pub agent_id: AgentId,
    pub model_key: String,
}

/// Whether two selections name the same catalogue entry.
///
/// Identity is Agent, authentication source and model: the run options are how
/// that entry runs, and a reader who changed the thinking depth did not move to
/// a different entry.
pub fn identity_matches(left: &SessionRuntimeSelection, right: &SessionRuntimeSelection) -> bool {
    left.agent_id == right.agent_id
        && left.auth_source == right.auth_source
        && left.model == right.model
}

/// The catalogue entry a selection names, when the catalogue still publishes it
/// as available.
pub fn catalog_entry<'a>(
    catalog: &'a SessionRuntimeOptionCatalog,
    selection: &SessionRuntimeSelection,
) -> Option<&'a SessionRuntimeOption> {
    catalog.options.iter().find(|option| {
        option.availability == RuntimeOptionAvailability::Available
            && identity_matches(&option.selection, selection)
            && selection.reasoning_effort.as_ref().is_none_or(|value| {
                option
                    .reasoning_efforts
                    .iter()
                    .any(|candidate| &candidate.value == value)
            })
            && selection.mode_id.as_ref().is_none_or(|value| {
                option
                    .modes
                    .iter()
                    .any(|candidate| &candidate.value == value)
            })
            && selection.config_values.iter().all(|(key, value)| {
                option
                    .features
                    .iter()
                    .find(|feature| &feature.id == key)
                    .is_some_and(|feature| feature.accepts_value(value))
            })
    })
}

/// The part of a selection worth writing down.
///
/// A free-text feature is not remembered: its value is a word the reader typed
/// for one session, not a setting, and restoring it silently is how a prompt
/// fragment becomes a default. Bounded and closed-set values are the ones the
/// Agent can be asked for again — the desktop's rule, kept identical so a
/// selection moved between the two clients means the same thing.
pub fn persisted(
    catalog: Option<&SessionRuntimeOptionCatalog>,
    selection: &SessionRuntimeSelection,
) -> SessionRuntimeSelection {
    let mut persisted = selection.clone();
    persisted.config_values.retain(|key, value| {
        catalog
            .and_then(|catalog| {
                catalog
                    .options
                    .iter()
                    .find(|option| identity_matches(&option.selection, selection))
            })
            .and_then(|option| option.features.iter().find(|feature| &feature.id == key))
            .is_some_and(|feature| {
                matches!(
                    feature.kind,
                    SessionRuntimeFeatureKind::Toggle | SessionRuntimeFeatureKind::Select
                ) && feature.accepts_value(value)
            })
    });
    persisted
}

/// The three answers the switcher keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePreferences {
    /// The selection last applied to any session or to the composing page.
    #[serde(default)]
    pub default: Option<SessionRuntimeSelection>,
    /// What each Agent was last on.
    ///
    /// Keyed by Agent because that is the question a catalogue that moved
    /// leaves open: the model the reader had may be gone, but the Agent is
    /// still the one they work with, and its authentication source still says
    /// where the next model should come from.
    #[serde(default)]
    pub by_agent: BTreeMap<AgentId, SessionRuntimeSelection>,
    /// How each Agent and model runs, most recently used last.
    ///
    /// Keyed by identity rather than by Agent: thinking depth and conversation
    /// mode are the model's vocabulary, and a reader who set "high" on one
    /// model did not ask for it on the next one.
    #[serde(default)]
    pub by_model: Vec<SessionRuntimeSelection>,
    /// Starred models, oldest first.
    #[serde(default)]
    pub favorites: Vec<RuntimeFavorite>,
}

impl RuntimePreferences {
    /// Read what a previous run wrote, or remember nothing.
    pub fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    /// Persist what has been remembered. A failure is silent: losing a
    /// preference is not worth interrupting the reader, and the next change
    /// tries again.
    pub fn save(&self, path: Option<&Path>) {
        let Some(path) = path else {
            return;
        };
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        if let Ok(body) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, body);
        }
    }

    /// Remember one applied selection: it becomes the default, the Agent's
    /// answer and the model's answer alike.
    pub fn remember(&mut self, selection: &SessionRuntimeSelection) {
        self.default = Some(selection.clone());
        self.by_agent
            .insert(selection.agent_id.clone(), selection.clone());
        // The record is a recency order, so an entry that was already there
        // moves to the end rather than being rewritten where it sits: the
        // pinned "recent" list is only useful if the top of it is the last
        // thing the reader chose.
        self.by_model
            .retain(|existing| !identity_matches(existing, selection));
        while self.by_model.len() >= RUNTIME_PREFERENCE_LIMIT {
            self.by_model.remove(0);
        }
        self.by_model.push(selection.clone());
    }

    /// Fold a model's remembered run options onto a catalogue entry.
    ///
    /// Only values the entry still publishes are kept: an effort or a mode the
    /// Agent stopped offering is dropped rather than sent back at it. A
    /// remembered value that no longer fits is simply not restored — the entry's
    /// own default answers in its place.
    pub fn with_remembered_options(
        &self,
        option: &SessionRuntimeOption,
    ) -> SessionRuntimeSelection {
        let mut selection = option.selection.clone();
        let Some(remembered) = self
            .by_model
            .iter()
            .find(|remembered| identity_matches(remembered, &selection))
        else {
            return selection;
        };
        if let Some(effort) = remembered
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
        if let Some(mode) = remembered
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
        for (key, value) in &remembered.config_values {
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

    /// The selection a page should start from.
    ///
    /// The Agent's own answer is preferred over the global default when the
    /// page names an Agent, and either is only used while the catalogue still
    /// publishes it. When the remembered model is gone the Agent's remembered
    /// authentication source picks the entry instead, which keeps a reader on
    /// the account they were using rather than dropping them on the catalogue's
    /// first line — and when the Agent itself is gone there is no answer here at
    /// all, so the caller's own fallback stands.
    pub fn preferred(
        &self,
        catalog: &SessionRuntimeOptionCatalog,
        agent_id: Option<&AgentId>,
    ) -> Option<SessionRuntimeSelection> {
        // An Agent that was named answers with its own record, or with the
        // default when the default is that same Agent. Falling back to another
        // Agent's default would answer a question about one Agent with a
        // selection for a different one.
        let remembered = match agent_id {
            Some(agent_id) => self.by_agent.get(agent_id).or_else(|| {
                self.default
                    .as_ref()
                    .filter(|selection| &selection.agent_id == agent_id)
            }),
            None => self.default.as_ref(),
        }?;
        if let Some(option) = catalog_entry(catalog, remembered) {
            return Some(self.with_remembered_options(option));
        }
        let agent = &remembered.agent_id;
        let fallback = catalog
            .options
            .iter()
            .find(|option| {
                option.availability == RuntimeOptionAvailability::Available
                    && &option.selection.agent_id == agent
                    && option.selection.auth_source == remembered.auth_source
            })
            .or_else(|| {
                catalog.options.iter().find(|option| {
                    option.availability == RuntimeOptionAvailability::Available
                        && &option.selection.agent_id == agent
                })
            })?;
        Some(self.with_remembered_options(fallback))
    }

    /// The catalogue entries to offer as recent, most recently used first.
    ///
    /// Deduplicated by identity because the record is a list of writes: the
    /// same model applied twice is one row, and the newer copy is the one that
    /// survives the walk.
    pub fn recent<'a>(
        &'a self,
        catalog: &'a SessionRuntimeOptionCatalog,
        limit: usize,
    ) -> Vec<&'a SessionRuntimeOption> {
        let mut rows = Vec::new();
        for selection in self.by_model.iter().rev() {
            if rows.len() >= limit {
                break;
            }
            if rows
                .iter()
                .any(|row: &&SessionRuntimeOption| identity_matches(&row.selection, selection))
            {
                continue;
            }
            if let Some(option) = catalog_entry(catalog, selection) {
                rows.push(option);
            }
        }
        rows
    }

    /// The catalogue entries the reader starred, in the order they starred them.
    pub fn starred<'a>(
        &'a self,
        catalog: &'a SessionRuntimeOptionCatalog,
    ) -> Vec<&'a SessionRuntimeOption> {
        let mut rows = Vec::new();
        for favorite in &self.favorites {
            if let Some(option) = catalog.options.iter().find(|option| {
                option.selection.agent_id == favorite.agent_id
                    && model_key(&option.selection.model) == favorite.model_key
                    && !rows.iter().any(|row: &&SessionRuntimeOption| {
                        identity_matches(&row.selection, &option.selection)
                    })
            }) {
                rows.push(option);
            }
        }
        rows
    }

    /// Whether one catalogue entry's model is starred.
    pub fn is_favorite(&self, option: &SessionRuntimeOption) -> bool {
        let key = model_key(&option.selection.model);
        self.favorites.iter().any(|favorite| {
            favorite.agent_id == option.selection.agent_id && favorite.model_key == key
        })
    }

    /// Star or unstar one catalogue entry's model, answering whether it is
    /// starred afterwards. A star past the limit evicts the oldest, so the list
    /// the menu draws stays bounded.
    pub fn toggle_favorite(&mut self, option: &SessionRuntimeOption) -> bool {
        let agent_id = option.selection.agent_id.clone();
        let key = model_key(&option.selection.model);
        if let Some(index) = self
            .favorites
            .iter()
            .position(|favorite| favorite.agent_id == agent_id && favorite.model_key == key)
        {
            self.favorites.remove(index);
            return false;
        }
        while self.favorites.len() >= RUNTIME_FAVORITE_LIMIT {
            self.favorites.remove(0);
        }
        self.favorites.push(RuntimeFavorite {
            agent_id,
            model_key: key,
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        AgentId, ProviderProfileId, SessionConfigValue, SessionRuntimeFeature,
        SessionRuntimeFeatureKind,
    };

    fn agent(id: &str) -> AgentId {
        AgentId::parse(id).expect("agent id")
    }

    /// One fixed account, so a selection built twice compares equal: identity is
    /// Agent, account and model, and a fresh profile id per call would make
    /// every selection a different entry.
    fn account() -> ProviderProfileId {
        ProviderProfileId::parse("provider_test").expect("profile id")
    }

    fn selection(agent_id: &str, model_id: &str) -> SessionRuntimeSelection {
        SessionRuntimeSelection::provider(agent(agent_id), account(), model_id)
    }

    fn entry(agent_id: &str, model_id: &str) -> SessionRuntimeOption {
        SessionRuntimeOption {
            selection: selection(agent_id, model_id),
            agent_label: agent_id.to_string(),
            auth_source_label: "Default".to_string(),
            model_label: model_id.to_string(),
            reasoning_efforts: vec![SessionConfigValue {
                value: "high".to_string(),
                label: Some("High".to_string()),
            }],
            modes: vec![SessionConfigValue {
                value: "plan".to_string(),
                label: Some("Plan".to_string()),
            }],
            features: vec![SessionRuntimeFeature {
                id: "web_search".to_string(),
                label: "Web search".to_string(),
                description: None,
                kind: SessionRuntimeFeatureKind::Toggle,
                current_value: None,
                default_value: None,
                values: Vec::new(),
            }],
            availability: RuntimeOptionAvailability::Available,
        }
    }

    fn catalog(options: Vec<SessionRuntimeOption>) -> SessionRuntimeOptionCatalog {
        SessionRuntimeOptionCatalog {
            revision: 1,
            agents: Vec::new(),
            auth_sources: Vec::new(),
            options,
        }
    }

    #[test]
    fn preferences_round_trip_through_their_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-runtime.json");
        let mut preferences = RuntimePreferences::default();
        let mut remembered = selection("codex", "gpt-5");
        remembered.reasoning_effort = Some("high".to_string());
        preferences.remember(&remembered);

        preferences.save(Some(&path));
        let loaded = RuntimePreferences::load(Some(&path));
        assert_eq!(loaded, preferences);
        assert_eq!(loaded.default.as_ref(), Some(&remembered));
        assert_eq!(loaded.by_agent.get(&agent("codex")), Some(&remembered));
    }

    #[test]
    fn a_second_write_for_one_model_replaces_rather_than_appends() {
        let mut preferences = RuntimePreferences::default();
        let first = selection("codex", "gpt-5");
        preferences.remember(&first);
        let mut tuned = first.clone();
        tuned.reasoning_effort = Some("high".to_string());
        preferences.remember(&tuned);

        assert_eq!(preferences.by_model.len(), 1);
        assert_eq!(preferences.by_model[0], tuned);
        // The Agent's answer moved with it: the two records are one answer seen
        // from two directions.
        assert_eq!(preferences.by_agent.get(&agent("codex")), Some(&tuned));
    }

    #[test]
    fn the_model_record_is_bounded_and_drops_the_oldest() {
        let mut preferences = RuntimePreferences::default();
        for index in 0..RUNTIME_PREFERENCE_LIMIT + 4 {
            preferences.remember(&selection("codex", &format!("gpt-{index}")));
        }
        assert_eq!(preferences.by_model.len(), RUNTIME_PREFERENCE_LIMIT);
        assert_eq!(
            preferences.by_model[0].model,
            RuntimeModelSelection::explicit("gpt-4")
        );
    }

    #[test]
    fn a_remembered_model_keeps_its_run_options_and_a_new_one_does_not() {
        let mut preferences = RuntimePreferences::default();
        let mut remembered = selection("codex", "gpt-5");
        remembered.reasoning_effort = Some("high".to_string());
        remembered.mode_id = Some("plan".to_string());
        remembered
            .config_values
            .insert("web_search".to_string(), "true".to_string());
        preferences.remember(&remembered);

        let restored = preferences.with_remembered_options(&entry("codex", "gpt-5"));
        assert_eq!(restored, remembered);
        // A different model is a different answer: nothing of gpt-5's depth or
        // mode rides onto it.
        let untouched = preferences.with_remembered_options(&entry("codex", "gpt-6"));
        assert_eq!(untouched, entry("codex", "gpt-6").selection);
    }

    #[test]
    fn free_text_features_are_not_persisted() {
        let mut option = entry("codex", "gpt-5");
        option.features.push(SessionRuntimeFeature {
            id: "system_prompt".to_string(),
            label: "System prompt".to_string(),
            description: None,
            kind: SessionRuntimeFeatureKind::String,
            current_value: None,
            default_value: None,
            values: Vec::new(),
        });
        let catalog = catalog(vec![option]);
        let mut written = selection("codex", "gpt-5");
        written
            .config_values
            .insert("system_prompt".to_string(), "be terse".to_string());
        written
            .config_values
            .insert("web_search".to_string(), "true".to_string());

        let persisted = persisted(Some(&catalog), &written);
        assert_eq!(
            persisted.config_values.get("web_search"),
            Some(&"true".to_string())
        );
        assert!(!persisted.config_values.contains_key("system_prompt"));
    }

    #[test]
    fn the_page_starts_on_the_agent_it_was_using() {
        let catalog = catalog(vec![entry("codex", "gpt-5"), entry("claude", "opus")]);
        let mut preferences = RuntimePreferences::default();
        preferences.remember(&selection("claude", "opus"));

        // No Agent named: the last applied selection answers.
        assert_eq!(
            preferences.preferred(&catalog, None),
            Some(selection("claude", "opus"))
        );
        // An Agent named answers with its own record, even when another Agent
        // was used more recently.
        preferences.remember(&selection("codex", "gpt-5"));
        assert_eq!(
            preferences.preferred(&catalog, Some(&agent("claude"))),
            Some(selection("claude", "opus"))
        );
    }

    #[test]
    fn a_remembered_model_that_is_gone_falls_back_to_its_agent() {
        let catalog = catalog(vec![entry("codex", "gpt-6")]);
        let mut preferences = RuntimePreferences::default();
        preferences.remember(&selection("codex", "gpt-5"));

        // gpt-5 is no longer published: the Agent's account still is, so the
        // replacement comes from the same Agent rather than from line one.
        assert_eq!(
            preferences.preferred(&catalog, None),
            Some(selection("codex", "gpt-6"))
        );
        // An Agent the catalogue does not publish at all has no answer here.
        assert_eq!(
            preferences.preferred(&catalog, Some(&agent("claude"))),
            None
        );
    }

    #[test]
    fn recent_is_most_recent_first_and_deduplicated() {
        let catalog = catalog(vec![entry("codex", "gpt-5"), entry("claude", "opus")]);
        let mut preferences = RuntimePreferences::default();
        preferences.remember(&selection("codex", "gpt-5"));
        preferences.remember(&selection("claude", "opus"));
        preferences.remember(&selection("codex", "gpt-5"));

        let recent = preferences.recent(&catalog, 5);
        let models = recent
            .iter()
            .map(|option| option.model_label.clone())
            .collect::<Vec<_>>();
        assert_eq!(models, ["gpt-5", "opus"]);
    }

    #[test]
    fn starring_a_model_is_keyed_by_agent_and_model_and_stays_bounded() {
        let mut preferences = RuntimePreferences::default();
        let option = entry("codex", "gpt-5");
        assert!(preferences.toggle_favorite(&option));
        assert!(preferences.is_favorite(&option));
        // The same model on another Agent is another star.
        assert!(!preferences.is_favorite(&entry("claude", "gpt-5")));
        assert!(!preferences.toggle_favorite(&option));
        assert!(!preferences.is_favorite(&option));

        for index in 0..RUNTIME_FAVORITE_LIMIT + 3 {
            preferences.toggle_favorite(&entry("codex", &format!("gpt-{index}")));
        }
        assert_eq!(preferences.favorites.len(), RUNTIME_FAVORITE_LIMIT);
        assert_eq!(preferences.favorites[0].model_key, "model:gpt-3");

        // The catalogue is what has the last word: a star for a model the
        // catalogue stopped publishing is a row nobody can draw, and an
        // unstarred one is not a favorite however many times it was used.
        let catalog = catalog(vec![
            entry("codex", "gpt-5"),
            entry("codex", "gpt-10"),
            entry("codex", "gpt-99"),
        ]);
        let starred = preferences.starred(&catalog);
        assert_eq!(
            starred
                .iter()
                .map(|option| option.model_label.clone())
                .collect::<Vec<_>>(),
            ["gpt-5", "gpt-10"]
        );
    }

    #[test]
    fn an_unreadable_file_remembers_nothing_rather_than_failing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-runtime.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(
            RuntimePreferences::load(Some(&path)),
            RuntimePreferences::default()
        );
        assert_eq!(
            RuntimePreferences::load(Some(&directory.path().join("absent.json"))),
            RuntimePreferences::default()
        );
        assert_eq!(
            RuntimePreferences::load(None),
            RuntimePreferences::default()
        );
    }
}

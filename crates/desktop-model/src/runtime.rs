use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use vibex_core::{
    RuntimeOptionAvailability, SessionConfigValue, SessionRuntimeFeature, SessionRuntimeOption,
    SessionRuntimeOptionCatalog, SessionRuntimeSelection,
};

/// One provider-neutral choice in a runtime selector cascade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCascadeChoice {
    pub value: String,
    pub label: String,
    pub selection: SessionRuntimeSelection,
}

/// Deterministic Agent → Profile → Model → Effort → Mode projection.
///
/// The projection only exposes catalog metadata. Selecting a choice returns a
/// complete product-level `SessionRuntimeSelection`; the caller still submits
/// it through the durable runtime-selection service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCascadeProjection {
    pub agents: Vec<RuntimeCascadeChoice>,
    pub profiles: Vec<RuntimeCascadeChoice>,
    pub models: Vec<RuntimeCascadeChoice>,
    pub reasoning_efforts: Vec<RuntimeCascadeChoice>,
    pub modes: Vec<RuntimeCascadeChoice>,
    pub features: Vec<SessionRuntimeFeature>,
}

impl RuntimeCascadeProjection {
    pub fn from_catalog(
        catalog: &SessionRuntimeOptionCatalog,
        desired: &SessionRuntimeSelection,
    ) -> Self {
        let available = catalog
            .options
            .iter()
            .filter(|option| option.availability == RuntimeOptionAvailability::Available)
            .collect::<Vec<_>>();

        let agents = unique_choices(available.iter().copied(), |option| {
            (
                option.selection.agent_id.to_string(),
                option.agent_label.clone(),
            )
        });
        let profiles = unique_choices(
            available
                .iter()
                .copied()
                .filter(|option| option.selection.agent_id == desired.agent_id),
            |option| {
                (
                    option.selection.provider_profile_id.to_string(),
                    option.provider_profile_label.clone(),
                )
            },
        );
        let models = unique_choices(
            available.iter().copied().filter(|option| {
                option.selection.agent_id == desired.agent_id
                    && option.selection.provider_profile_id == desired.provider_profile_id
            }),
            |option| {
                (
                    option.selection.model_id.clone(),
                    option.model_label.clone(),
                )
            },
        );
        let matching = available.iter().copied().filter(|option| {
            option.selection.agent_id == desired.agent_id
                && option.selection.provider_profile_id == desired.provider_profile_id
                && option.selection.model_id == desired.model_id
        });
        let reasoning_efforts = config_choices(
            matching.clone(),
            desired,
            RuntimeConfigDimension::ReasoningEffort,
        );
        let modes = config_choices(matching, desired, RuntimeConfigDimension::Mode);
        let features = available
            .iter()
            .copied()
            .find(|option| {
                option.selection.agent_id == desired.agent_id
                    && option.selection.provider_profile_id == desired.provider_profile_id
                    && option.selection.model_id == desired.model_id
            })
            .map(|option| {
                option
                    .features
                    .iter()
                    .cloned()
                    .map(|mut feature| {
                        feature.current_value = feature.value_for(&desired.config_values);
                        feature
                    })
                    .collect()
            })
            .unwrap_or_default();

        Self {
            agents,
            profiles,
            models,
            reasoning_efforts,
            modes,
            features,
        }
    }
}

fn unique_choices<'a, I, F>(options: I, key: F) -> Vec<RuntimeCascadeChoice>
where
    I: IntoIterator<Item = &'a SessionRuntimeOption>,
    F: Fn(&SessionRuntimeOption) -> (String, String),
{
    let mut values = BTreeMap::new();
    for option in options {
        let (value, label) = key(option);
        values.entry(value).or_insert_with(|| RuntimeCascadeChoice {
            value: option.selection.model_id.clone(),
            label,
            selection: option.selection.clone(),
        });
    }
    values
        .into_iter()
        .map(|(value, mut choice)| {
            choice.value = value;
            choice
        })
        .collect()
}

#[derive(Clone, Copy)]
enum RuntimeConfigDimension {
    ReasoningEffort,
    Mode,
}

fn config_choices<'a, I>(
    options: I,
    desired: &SessionRuntimeSelection,
    dimension: RuntimeConfigDimension,
) -> Vec<RuntimeCascadeChoice>
where
    I: IntoIterator<Item = &'a SessionRuntimeOption>,
{
    let mut values = BTreeMap::<String, String>::new();
    for option in options {
        let candidates: &[SessionConfigValue] = match dimension {
            RuntimeConfigDimension::ReasoningEffort => &option.reasoning_efforts,
            RuntimeConfigDimension::Mode => &option.modes,
        };
        for candidate in candidates {
            let label = match dimension {
                RuntimeConfigDimension::ReasoningEffort => reasoning_effort_label(&candidate.value),
                RuntimeConfigDimension::Mode => candidate
                    .label
                    .clone()
                    .unwrap_or_else(|| candidate.value.clone()),
            };
            values.entry(candidate.value.clone()).or_insert(label);
        }
    }
    if values.is_empty() {
        return Vec::new();
    }

    if matches!(dimension, RuntimeConfigDimension::ReasoningEffort) {
        values.retain(|value, _| !value.eq_ignore_ascii_case("default"));
    }
    let mut values = values.into_iter().collect::<Vec<_>>();
    if matches!(dimension, RuntimeConfigDimension::ReasoningEffort) {
        values.sort_by(|(left, _), (right, _)| {
            reasoning_effort_rank(left)
                .cmp(&reasoning_effort_rank(right))
                .then_with(|| left.cmp(right))
        });
    }

    let choices = values.into_iter().map(|(value, label)| {
        let mut selection = desired.clone();
        match dimension {
            RuntimeConfigDimension::ReasoningEffort => {
                selection.reasoning_effort = Some(value.clone())
            }
            RuntimeConfigDimension::Mode => selection.mode_id = Some(value.clone()),
        }
        RuntimeCascadeChoice {
            value,
            label,
            selection,
        }
    });
    choices.collect()
}

fn reasoning_effort_rank(value: &str) -> (u8, String) {
    let normalized = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    let rank = match normalized.as_str() {
        "none" => 0,
        "minimal" => 1,
        "low" => 2,
        "medium" => 3,
        "high" => 4,
        "xhigh" | "extrahigh" => 5,
        "max" | "maximum" => 6,
        "ultra" => 7,
        _ => u8::MAX,
    };
    (rank, normalized)
}

fn reasoning_effort_label(value: &str) -> String {
    let normalized = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if normalized == "xhigh" {
        return "XHigh".to_string();
    }

    let label = value
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            let Some(first) = characters.next() else {
                return String::new();
            };
            first
                .to_uppercase()
                .chain(characters.flat_map(char::to_lowercase))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ");
    if label.is_empty() {
        value.to_string()
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        AgentId, ProviderProfileId, SessionRuntimeFeature, SessionRuntimeFeatureKind,
    };

    fn option(
        agent: &str,
        profile: &str,
        model: &str,
        effort: &[&str],
        mode: &[&str],
    ) -> SessionRuntimeOption {
        SessionRuntimeOption {
            selection: SessionRuntimeSelection {
                agent_id: AgentId::parse(agent).unwrap(),
                provider_profile_id: ProviderProfileId::parse(profile).unwrap(),
                model_id: model.into(),
                reasoning_effort: None,
                mode_id: None,
                config_values: Default::default(),
            },
            agent_label: agent.into(),
            provider_profile_label: profile.into(),
            model_label: model.into(),
            reasoning_efforts: effort
                .iter()
                .map(|value| SessionConfigValue {
                    value: (*value).into(),
                    label: Some(value.to_uppercase()),
                })
                .collect(),
            modes: mode
                .iter()
                .map(|value| SessionConfigValue {
                    value: (*value).into(),
                    label: None,
                })
                .collect(),
            features: Vec::new(),
            availability: RuntimeOptionAvailability::Available,
        }
    }

    #[test]
    fn cascade_filters_downstream_dimensions_and_preserves_labels() {
        let catalog = SessionRuntimeOptionCatalog {
            revision: 1,
            options: vec![
                option(
                    "claude",
                    "provider_default",
                    "sonnet",
                    &["low", "high"],
                    &["build"],
                ),
                option("claude", "provider_default", "haiku", &["low"], &["chat"]),
                option("codex", "provider_default", "o3", &["high"], &["build"]),
            ],
        };
        let desired = catalog.options[0].selection.clone();
        let projection = RuntimeCascadeProjection::from_catalog(&catalog, &desired);
        assert_eq!(projection.agents.len(), 2);
        assert_eq!(projection.profiles.len(), 1);
        assert_eq!(projection.models.len(), 2);
        assert_eq!(
            projection
                .reasoning_efforts
                .iter()
                .map(|choice| (choice.value.as_str(), choice.label.as_str()))
                .collect::<Vec<_>>(),
            vec![("low", "Low"), ("high", "High")]
        );
        assert_eq!(projection.modes.len(), 1);
        assert_eq!(projection.modes[0].value, "build");
        assert_eq!(
            projection.modes[0].selection.mode_id.as_deref(),
            Some("build")
        );
    }

    #[test]
    fn reasoning_efforts_use_names_without_default_and_sort_by_depth() {
        let mut runtime = option(
            "codex",
            "provider_codex",
            "gpt-5",
            &[
                "ultra", "medium", "xhigh", "default", "custom", "low", "max", "high", "minimal",
            ],
            &[],
        );
        for effort in &mut runtime.reasoning_efforts {
            effort.label = Some(format!("Description for {}", effort.value));
        }
        let desired = runtime.selection.clone();
        let projection = RuntimeCascadeProjection::from_catalog(
            &SessionRuntimeOptionCatalog {
                revision: 2,
                options: vec![runtime],
            },
            &desired,
        );

        assert_eq!(
            projection
                .reasoning_efforts
                .iter()
                .map(|choice| (choice.value.as_str(), choice.label.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("minimal", "Minimal"),
                ("low", "Low"),
                ("medium", "Medium"),
                ("high", "High"),
                ("xhigh", "XHigh"),
                ("max", "Max"),
                ("ultra", "Ultra"),
                ("custom", "Custom"),
            ]
        );
        assert!(
            projection
                .reasoning_efforts
                .iter()
                .all(|choice| choice.value != "default")
        );
    }

    #[test]
    fn modes_preserve_an_advertised_default_without_injecting_one() {
        let with_default = option(
            "claude",
            "provider_claude",
            "sonnet",
            &[],
            &["default", "plan"],
        );
        let without_default = option("codex", "provider_codex", "gpt-5", &[], &["agent"]);

        let with_default_projection = RuntimeCascadeProjection::from_catalog(
            &SessionRuntimeOptionCatalog {
                revision: 3,
                options: vec![with_default.clone()],
            },
            &with_default.selection,
        );
        let without_default_projection = RuntimeCascadeProjection::from_catalog(
            &SessionRuntimeOptionCatalog {
                revision: 4,
                options: vec![without_default.clone()],
            },
            &without_default.selection,
        );

        assert_eq!(
            with_default_projection
                .modes
                .iter()
                .map(|choice| choice.value.as_str())
                .collect::<Vec<_>>(),
            vec!["default", "plan"]
        );
        assert_eq!(without_default_projection.modes.len(), 1);
        assert_eq!(without_default_projection.modes[0].value, "agent");
    }

    #[test]
    fn unavailable_options_do_not_enter_cascade_choices() {
        let mut unavailable = option("claude", "provider_offline", "sonnet", &[], &[]);
        unavailable.availability = RuntimeOptionAvailability::TemporarilyUnavailable;
        let catalog = SessionRuntimeOptionCatalog {
            revision: 2,
            options: vec![unavailable],
        };
        let desired = catalog.options[0].selection.clone();
        let projection = RuntimeCascadeProjection::from_catalog(&catalog, &desired);
        assert!(projection.agents.is_empty());
        assert!(projection.profiles.is_empty());
    }

    #[test]
    fn cascade_projects_only_the_selected_runtime_features_and_overlays_desired_values() {
        let mut selected = option("codex", "provider_codex", "gpt-5", &[], &[]);
        selected.features = vec![SessionRuntimeFeature {
            id: "web_search".into(),
            label: "Web search".into(),
            description: Some("Allow web search".into()),
            kind: SessionRuntimeFeatureKind::Toggle,
            current_value: Some(SessionConfigValue {
                value: "true".into(),
                label: None,
            }),
            default_value: Some(SessionConfigValue {
                value: "false".into(),
                label: None,
            }),
            values: Vec::new(),
        }];
        selected
            .selection
            .config_values
            .insert("web_search".into(), "true".into());
        let other = option("claude", "provider_claude", "sonnet", &[], &[]);
        let catalog = SessionRuntimeOptionCatalog {
            revision: 3,
            options: vec![selected, other],
        };
        let mut desired = catalog.options[0].selection.clone();
        desired
            .config_values
            .insert("web_search".into(), "false".into());

        let projection = RuntimeCascadeProjection::from_catalog(&catalog, &desired);
        assert_eq!(projection.features.len(), 1);
        assert_eq!(projection.features[0].id, "web_search");
        assert_eq!(
            projection.features[0]
                .current_value
                .as_ref()
                .map(|value| value.value.as_str()),
            Some("false")
        );
    }
}

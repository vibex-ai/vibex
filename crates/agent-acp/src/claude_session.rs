//! Claude Code per-session model routing.
//!
//! Claude Code applies the `env` block of `~/.claude/settings.json` *after* the
//! subprocess environment, so a Provider Profile's projected environment is
//! silently ignored for every key the user's own Claude Code settings define —
//! the model catalog included. `claude-agent-acp` forwards
//! `session/new | session/load | session/resume` `_meta.claudeCode.options` to
//! the Claude Agent SDK, and that programmatic settings tier is applied last,
//! so the Profile can win without editing any user file.
//!
//! The adapter resolves an ACP model value against the picker rows Claude Code
//! derives from `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU}_MODEL` and rejects a
//! value that matches no row (`Invalid value for config option model`), which
//! surfaces as a failed model probe and, on the live path, as
//! `runtime_switch_configuration_unavailable` from `apply_session_config`.
//! Every configured model therefore has to own a row: this module assigns each
//! one to a distinct alias slot and projects that mapping into the session
//! settings tier. The alias is only a slot — Claude Code expands it locally, so
//! a slot may point at any model (the user's own settings routinely point
//! `sonnet` at an Opus model).
//!
//! `ANTHROPIC_MODEL` is deliberately not projected when it names an assigned
//! model: it would add a picker row spelling the raw id, and that row wins the
//! adapter's exact-match tier, so `setModel` would receive the raw id and ask
//! the provider to confirm a model spelling it need not recognize instead of
//! the alias Claude Code expands.
//!
//! The `_meta.claudeCode.options` channel exists in `claude-agent-acp` 0.71.0
//! (Vibex's pinned compatibility floor) and later; it is an adapter extension,
//! not standard ACP, so it is only ever built for the `claude` Agent.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};
use vibex_core::{AcpProviderConfig, AcpProviderEnvSource, AgentId};

use crate::registry::CLAUDE_AGENT_ID;

/// Alias slots Claude Code resolves natively, in assignment order.
const CLAUDE_MODEL_ALIAS_SLOTS: [&str; 3] = ["opus", "sonnet", "haiku"];

/// The `_meta` namespace the Claude adapter reads its SDK options from.
const CLAUDE_SESSION_OPTIONS_NAMESPACE: &str = "claudeCode";

/// Only the model-routing surface is projected. Every other Profile key keeps
/// its existing subprocess-environment path so this change cannot silently
/// change proxy, timeout, or telemetry behavior.
const CLAUDE_MODEL_ROUTING_ENV_PREFIX: &str = "ANTHROPIC_";

const CLAUDE_DEFAULT_MODEL_KEY: &str = "ANTHROPIC_MODEL";

/// Builds the `_meta` value attached to a Claude ACP session request.
///
/// `None` means "send no `_meta` at all": the Agent is not Claude, or the
/// Profile declares nothing this tier can carry.
pub(crate) fn session_settings_meta(
    agent_id: &AgentId,
    config: &AcpProviderConfig,
) -> Option<Value> {
    if agent_id.as_str() != CLAUDE_AGENT_ID {
        return None;
    }
    let assignments = model_slot_assignments(&config.models);
    let mut env = Map::new();
    for reference in &config.env {
        // A secret reference is materialized at spawn time and must never be
        // copied into a request payload; only literals are safe here.
        if reference.source != AcpProviderEnvSource::Literal {
            continue;
        }
        let key = reference.key.trim();
        if !key.starts_with(CLAUDE_MODEL_ROUTING_ENV_PREFIX) {
            continue;
        }
        let Some(value) = reference
            .value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if key == CLAUDE_DEFAULT_MODEL_KEY && assignments.values().any(|model| model == value) {
            continue;
        }
        env.insert(key.to_string(), Value::String(value.to_string()));
    }
    for (slot, model) in &assignments {
        env.insert(
            format!(
                "{CLAUDE_MODEL_ROUTING_ENV_PREFIX}DEFAULT_{}_MODEL",
                slot.to_ascii_uppercase()
            ),
            Value::String(model.clone()),
        );
    }
    if env.is_empty() {
        return None;
    }
    Some(json!({
        CLAUDE_SESSION_OPTIONS_NAMESPACE: {
            "options": {
                "settings": {
                    "env": Value::Object(env),
                }
            }
        }
    }))
}

/// Attaches a previously built `_meta` value to a session request payload.
pub(crate) fn attach_session_settings_meta(params: &mut Value, meta: Option<&Value>) {
    let Some(meta) = meta else {
        return;
    };
    let Some(params) = params.as_object_mut() else {
        return;
    };
    params.insert("_meta".to_string(), meta.clone());
}

/// Assigns configured models to distinct Claude Code alias slots.
///
/// A model claims its own family slot when that slot is still free, and the
/// first free slot otherwise; models past the last free slot keep no row and
/// stay unresolvable through this tier. Configured order is the tie-breaker so
/// the mapping is stable across sessions and matches the model list the Profile
/// exposes.
fn model_slot_assignments(models: &[String]) -> BTreeMap<&'static str, String> {
    let mut assignments = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for model in models {
        let model = model.trim();
        if model.is_empty() || !seen.insert(model) {
            continue;
        }
        let slot = model_family(model)
            .filter(|slot| !assignments.contains_key(slot))
            .or_else(|| {
                CLAUDE_MODEL_ALIAS_SLOTS
                    .iter()
                    .copied()
                    .find(|slot| !assignments.contains_key(slot))
            });
        let Some(slot) = slot else {
            break;
        };
        assignments.insert(slot, model.to_string());
    }
    assignments
}

/// The alias family a configured model names, if its id names one.
///
/// Mirrors the alias detection the legacy Provider projection uses to derive a
/// Claude Agent model id, so both layers read `claude-opus-5-5[1m]` as `opus`.
fn model_family(model_id: &str) -> Option<&'static str> {
    model_id
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .find_map(|part| {
            CLAUDE_MODEL_ALIAS_SLOTS
                .iter()
                .copied()
                .find(|slot| *slot == part)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::AcpProviderEnvReference;

    fn literal(key: &str, value: &str) -> AcpProviderEnvReference {
        AcpProviderEnvReference {
            key: key.to_string(),
            source: AcpProviderEnvSource::Literal,
            value: Some(value.to_string()),
            secret_lookup_key: None,
            redacted_hint: "test literal".to_string(),
        }
    }

    fn config(env: Vec<AcpProviderEnvReference>, models: &[&str]) -> AcpProviderConfig {
        AcpProviderConfig {
            command: "claude-agent-acp".to_string(),
            args: Vec::new(),
            env,
            cwd_template: Some("{workspaceRoot}".to_string()),
            process_strategy: Default::default(),
            terminal_tools: false,
            terminal_auth: false,
            models: models.iter().map(|model| model.to_string()).collect(),
            modes: Vec::new(),
            features: Vec::new(),
            disabled_tools: Vec::new(),
        }
    }

    fn claude() -> AgentId {
        AgentId::parse(CLAUDE_AGENT_ID).unwrap()
    }

    fn projected_env(meta: &Value) -> Map<String, Value> {
        meta["claudeCode"]["options"]["settings"]["env"]
            .as_object()
            .cloned()
            .expect("session settings carry an env object")
    }

    #[test]
    fn assigns_configured_models_to_distinct_alias_slots() {
        // Two models of the same family cannot share the `opus` row, so the
        // second one takes the next free slot instead of losing its row.
        let meta = session_settings_meta(
            &claude(),
            &config(Vec::new(), &["claude-opus-5[1m]", "claude-opus-5-5[1m]"]),
        )
        .expect("configured models are projected");

        let env = projected_env(&meta);
        assert_eq!(
            env["ANTHROPIC_DEFAULT_OPUS_MODEL"],
            Value::String("claude-opus-5[1m]".to_string())
        );
        assert_eq!(
            env["ANTHROPIC_DEFAULT_SONNET_MODEL"],
            Value::String("claude-opus-5-5[1m]".to_string())
        );
        assert!(!env.contains_key("ANTHROPIC_DEFAULT_HAIKU_MODEL"));
    }

    #[test]
    fn prefers_the_family_slot_before_overflowing() {
        let meta = session_settings_meta(
            &claude(),
            &config(Vec::new(), &["claude-haiku-4-5", "claude-opus-5[1m]"]),
        )
        .expect("configured models are projected");

        let env = projected_env(&meta);
        assert_eq!(
            env["ANTHROPIC_DEFAULT_HAIKU_MODEL"],
            Value::String("claude-haiku-4-5".to_string())
        );
        assert_eq!(
            env["ANTHROPIC_DEFAULT_OPUS_MODEL"],
            Value::String("claude-opus-5[1m]".to_string())
        );
    }

    #[test]
    fn models_without_a_family_claim_the_first_free_slot() {
        let meta = session_settings_meta(&claude(), &config(Vec::new(), &["claude-fable-5-1[1m]"]))
            .expect("configured models are projected");

        assert_eq!(
            projected_env(&meta)["ANTHROPIC_DEFAULT_OPUS_MODEL"],
            Value::String("claude-fable-5-1[1m]".to_string())
        );
    }

    #[test]
    fn drops_the_default_model_key_when_it_would_shadow_an_alias_row() {
        let meta = session_settings_meta(
            &claude(),
            &config(
                vec![
                    literal("ANTHROPIC_MODEL", "claude-opus-5[1m]"),
                    literal("ANTHROPIC_DEFAULT_SONNET_MODEL", "claude-fable-5[1M]"),
                ],
                &["claude-opus-5[1m]"],
            ),
        )
        .expect("configured models are projected");

        let env = projected_env(&meta);
        assert!(!env.contains_key("ANTHROPIC_MODEL"));
        assert_eq!(
            env["ANTHROPIC_DEFAULT_OPUS_MODEL"],
            Value::String("claude-opus-5[1m]".to_string())
        );
        // A Profile alias that no configured model claims stays authoritative.
        assert_eq!(
            env["ANTHROPIC_DEFAULT_SONNET_MODEL"],
            Value::String("claude-fable-5[1M]".to_string())
        );
    }

    #[test]
    fn keeps_the_default_model_key_for_an_unconfigured_model() {
        let meta = session_settings_meta(
            &claude(),
            &config(vec![literal("ANTHROPIC_MODEL", "claude-sonnet-4-5")], &[]),
        )
        .expect("the profile env is projected");

        assert_eq!(
            projected_env(&meta)["ANTHROPIC_MODEL"],
            Value::String("claude-sonnet-4-5".to_string())
        );
    }

    #[test]
    fn ignores_secrets_and_non_model_routing_keys() {
        let secret = AcpProviderEnvReference {
            key: "ANTHROPIC_AUTH_TOKEN".to_string(),
            source: AcpProviderEnvSource::SecretReference,
            value: None,
            secret_lookup_key: Some("vibex-provider-secret".to_string()),
            redacted_hint: "stored in Vibex OS keychain".to_string(),
        };
        let meta = session_settings_meta(
            &claude(),
            &config(
                vec![
                    secret,
                    literal("ANTHROPIC_BASE_URL", "https://gateway.invalid"),
                    literal("HTTP_PROXY", "http://127.0.0.1:7890"),
                ],
                &[],
            ),
        )
        .expect("the profile env is projected");

        let env = projected_env(&meta);
        assert_eq!(
            env["ANTHROPIC_BASE_URL"],
            Value::String("https://gateway.invalid".to_string())
        );
        assert!(!env.contains_key("ANTHROPIC_AUTH_TOKEN"));
        assert!(!env.contains_key("HTTP_PROXY"));
    }

    #[test]
    fn other_agents_and_empty_profiles_send_no_meta() {
        let codex = AgentId::parse("codex").unwrap();
        assert!(
            session_settings_meta(
                &codex,
                &config(vec![literal("ANTHROPIC_MODEL", "m")], &["m"])
            )
            .is_none()
        );
        assert!(session_settings_meta(&claude(), &config(Vec::new(), &[])).is_none());
    }

    #[test]
    fn attach_meta_leaves_payloads_without_settings_untouched() {
        let mut params = json!({ "cwd": "/tmp/workspace", "mcpServers": [] });
        attach_session_settings_meta(&mut params, None);
        assert!(params.get("_meta").is_none());

        let meta = session_settings_meta(&claude(), &config(Vec::new(), &["claude-opus-5[1m]"]))
            .expect("configured models are projected");
        attach_session_settings_meta(&mut params, Some(&meta));
        assert_eq!(params["_meta"], meta);
    }
}

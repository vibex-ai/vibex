//! Claude Code's command-line settings tier for terminal-mode sessions.
//!
//! Claude Code applies the `env` block of `~/.claude/settings.json` *after* the
//! subprocess environment, so a Provider Profile's projected endpoint and
//! credential lose to whatever the reader's own file defines. The ACP path
//! avoids that with `_meta.claudeCode.options.settings.env`; a terminal session
//! has no ACP request to carry it, so the same environment is restated through
//! `--settings`, which is a command-line argument and therefore applied after
//! the reader's file.
//!
//! The file is written by the projection (see
//! `ResolvedAgentProviderProjection::write_claude_terminal_settings`) and only
//! its path travels in the argv. A JSON string would put the Profile's
//! credential in the process list.

use std::path::Path;

use vibex_core::AgentId;

const CLAUDE_AGENT_ID: &str = "claude";

/// Points a Claude terminal session at the settings file Vibex wrote.
///
/// Any `--settings` the caller already supplied is replaced rather than left
/// beside ours: it names a file Vibex cannot merge, and a later occurrence
/// would shadow the Provider the session was created with. The flag is
/// inserted immediately after the executable so it stays ahead of a positional
/// first prompt.
pub fn apply_claude_settings_argument(
    argv: &mut Vec<String>,
    agent_id: &AgentId,
    settings_path: &Path,
) {
    if agent_id.as_str() != CLAUDE_AGENT_ID || argv.is_empty() {
        return;
    }
    let mut index = 1;
    while index < argv.len() {
        if argv[index] == "--settings" {
            argv.remove(index);
            if index < argv.len() {
                argv.remove(index);
            }
            continue;
        }
        if argv[index].starts_with("--settings=") {
            argv.remove(index);
            continue;
        }
        index += 1;
    }
    let settings_path = settings_path.to_string_lossy().into_owned();
    argv.insert(1, "--settings".to_string());
    argv.insert(2, settings_path);
}

const OPENCODE_AGENT_ID: &str = "opencode";
const OPENCODE_CONFIG_ENV: &str = "OPENCODE_CONFIG_CONTENT";

/// Moves OpenCode's startup model from the command line into its config.
///
/// OpenCode's root command takes no `--model`: the flag belongs to `run`, and
/// the TUI reads its model from the config document. Launching a terminal
/// session as `opencode --model <id> …` therefore made the CLI print help and
/// exit instead of starting. The recorded argv keeps the flag so a restart
/// still knows which model the reader chose; this moves it into
/// `OPENCODE_CONFIG_CONTENT`, which the Provider projection already supplies,
/// on the launch copy only.
pub fn move_opencode_model_to_config(
    argv: &mut Vec<String>,
    env: &mut Vec<(String, String)>,
    projections: &[(String, String)],
    agent_id: &AgentId,
) {
    if agent_id.as_str() != OPENCODE_AGENT_ID {
        return;
    }
    let Some(model) = take_model_argument(argv) else {
        return;
    };
    let projected = projections
        .iter()
        .find(|(product, _)| product == &model)
        .map(|(_, runtime)| runtime.clone())
        .unwrap_or(model);
    let Some(existing) = env
        .iter()
        .rev()
        .find(|(key, _)| key == OPENCODE_CONFIG_ENV)
        .map(|(_, value)| value.clone())
    else {
        // Without a projected document there is no provider entry the model
        // could name; dropping the rejected flag is still the correct launch.
        return;
    };
    let Ok(mut document) = serde_json::from_str::<serde_json::Value>(&existing) else {
        return;
    };
    let Some(document) = document.as_object_mut() else {
        return;
    };
    document.insert("model".to_string(), serde_json::Value::String(projected));
    let Ok(encoded) = serde_json::to_string(&document) else {
        return;
    };
    env.retain(|(key, _)| key != OPENCODE_CONFIG_ENV);
    env.push((OPENCODE_CONFIG_ENV.to_string(), encoded));
}

/// Removes the first `--model` pair and returns its value.
fn take_model_argument(argv: &mut Vec<String>) -> Option<String> {
    let mut index = 1;
    while index < argv.len() {
        if argv[index] == "--model" {
            if index + 1 >= argv.len() {
                argv.remove(index);
                return None;
            }
            let value = argv.remove(index + 1);
            argv.remove(index);
            return Some(value);
        }
        if let Some(value) = argv[index].strip_prefix("--model=") {
            let value = value.to_string();
            argv.remove(index);
            return Some(value);
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str) -> AgentId {
        AgentId::parse(id).expect("test agent ids are valid")
    }

    #[test]
    fn claude_replaces_any_caller_settings_with_the_projections() {
        let mut argv = vec![
            "claude".to_string(),
            "--settings".to_string(),
            "/caller/settings.json".to_string(),
            "--model".to_string(),
            "claude-opus-4-6".to_string(),
        ];
        apply_claude_settings_argument(
            &mut argv,
            &agent("claude"),
            Path::new("/vibex/settings.json"),
        );
        assert_eq!(
            argv,
            vec![
                "claude".to_string(),
                "--settings".to_string(),
                "/vibex/settings.json".to_string(),
                "--model".to_string(),
                "claude-opus-4-6".to_string(),
            ]
        );
    }

    #[test]
    fn the_equals_spelling_is_replaced_too() {
        let mut argv = vec![
            "claude".to_string(),
            "--settings=/caller/settings.json".to_string(),
            "--continue".to_string(),
        ];
        apply_claude_settings_argument(
            &mut argv,
            &agent("claude"),
            Path::new("/vibex/settings.json"),
        );
        assert_eq!(
            argv,
            vec![
                "claude".to_string(),
                "--settings".to_string(),
                "/vibex/settings.json".to_string(),
                "--continue".to_string(),
            ]
        );
    }

    #[test]
    fn a_restart_does_not_duplicate_the_settings_pair() {
        let mut argv = vec![
            "claude".to_string(),
            "--settings".to_string(),
            "/vibex/settings.json".to_string(),
        ];
        apply_claude_settings_argument(
            &mut argv,
            &agent("claude"),
            Path::new("/vibex/settings.json"),
        );
        assert_eq!(
            argv,
            vec![
                "claude".to_string(),
                "--settings".to_string(),
                "/vibex/settings.json".to_string(),
            ]
        );
    }

    #[test]
    fn another_agent_keeps_its_arguments() {
        let mut argv = vec![
            "codex".to_string(),
            "--model".to_string(),
            "gpt-5".to_string(),
        ];
        apply_claude_settings_argument(
            &mut argv,
            &agent("codex"),
            Path::new("/vibex/settings.json"),
        );
        assert_eq!(
            argv,
            vec![
                "codex".to_string(),
                "--model".to_string(),
                "gpt-5".to_string()
            ]
        );
    }

    #[test]
    fn an_empty_argv_is_left_alone() {
        let mut argv = Vec::new();
        apply_claude_settings_argument(
            &mut argv,
            &agent("claude"),
            Path::new("/vibex/settings.json"),
        );
        assert!(argv.is_empty());
    }

    fn opencode_env() -> Vec<(String, String)> {
        vec![(
            OPENCODE_CONFIG_ENV.to_string(),
            serde_json::json!({
                "$schema": "https://opencode.ai/config.json",
                "provider": { "vibex": { "models": {} } },
                "model": "vibex/old-model",
            })
            .to_string(),
        )]
    }

    fn config_model(env: &[(String, String)]) -> String {
        let value = env
            .iter()
            .find(|(key, _)| key == OPENCODE_CONFIG_ENV)
            .map(|(_, value)| value.as_str())
            .expect("the config content is still present");
        serde_json::from_str::<serde_json::Value>(value)
            .expect("the config content stays valid JSON")["model"]
            .as_str()
            .expect("the model is a string")
            .to_string()
    }

    #[test]
    fn opencode_reads_the_model_from_config_instead_of_the_rejected_flag() {
        let mut argv = vec![
            "opencode".to_string(),
            "--model".to_string(),
            "group/model_route_1/deepseek".to_string(),
            "--prompt".to_string(),
            "hello".to_string(),
        ];
        let mut env = opencode_env();
        move_opencode_model_to_config(
            &mut argv,
            &mut env,
            &[(
                "group/model_route_1/deepseek".to_string(),
                "vibex/group/model_route_1/deepseek".to_string(),
            )],
            &agent("opencode"),
        );
        assert_eq!(
            argv,
            vec![
                "opencode".to_string(),
                "--prompt".to_string(),
                "hello".to_string(),
            ]
        );
        assert_eq!(config_model(&env), "vibex/group/model_route_1/deepseek");
        assert_eq!(
            env.iter()
                .filter(|(key, _)| key == OPENCODE_CONFIG_ENV)
                .count(),
            1
        );
    }

    #[test]
    fn opencode_accepts_the_equals_spelling() {
        let mut argv = vec!["opencode".to_string(), "--model=vibex/model-b".to_string()];
        let mut env = opencode_env();
        move_opencode_model_to_config(&mut argv, &mut env, &[], &agent("opencode"));
        assert_eq!(argv, vec!["opencode".to_string()]);
        assert_eq!(config_model(&env), "vibex/model-b");
    }

    #[test]
    fn opencode_without_a_projected_document_still_drops_the_flag() {
        let mut argv = vec![
            "opencode".to_string(),
            "--model".to_string(),
            "model-a".to_string(),
        ];
        let mut env = Vec::new();
        move_opencode_model_to_config(&mut argv, &mut env, &[], &agent("opencode"));
        assert_eq!(argv, vec!["opencode".to_string()]);
        assert!(env.is_empty());
    }

    #[test]
    fn opencode_leaves_an_unparseable_document_alone() {
        let mut argv = vec![
            "opencode".to_string(),
            "--model".to_string(),
            "model-a".to_string(),
        ];
        let mut env = vec![(OPENCODE_CONFIG_ENV.to_string(), "not json".to_string())];
        move_opencode_model_to_config(&mut argv, &mut env, &[], &agent("opencode"));
        assert_eq!(argv, vec!["opencode".to_string()]);
        assert_eq!(
            env,
            vec![(OPENCODE_CONFIG_ENV.to_string(), "not json".to_string())]
        );
    }

    #[test]
    fn another_agent_keeps_its_model_flag_and_environment() {
        let mut argv = vec![
            "claude".to_string(),
            "--model".to_string(),
            "claude-opus-4-6".to_string(),
        ];
        let mut env = Vec::new();
        move_opencode_model_to_config(&mut argv, &mut env, &[], &agent("claude"));
        assert_eq!(
            argv,
            vec![
                "claude".to_string(),
                "--model".to_string(),
                "claude-opus-4-6".to_string(),
            ]
        );
        assert!(env.is_empty());
    }
}

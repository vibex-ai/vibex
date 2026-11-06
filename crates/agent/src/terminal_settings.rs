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
}

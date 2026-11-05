//! Which Agent CLIs can run as a terminal-mode session.
//!
//! Terminal mode hands the conversation to the Agent's own interactive TUI, so
//! Vibex only needs the executable name. That name is deliberately *not* the
//! ACP adapter binary (`claude-agent-acp`, `codex-acp`): the adapter speaks ACP
//! on stdio and has no interface of its own.
//!
//! The supported list is an allow-list, not a capability table. A terminal
//! session is only listed once its TUI, its state recognition, and its restart
//! path were exercised together for this release; an Agent that merely *has* a
//! native CLI stays unverified so the new-session panel cannot advertise a mode
//! nobody tested.

use crate::agent_config::AgentId;

/// The Agent's native interactive executable, when one is known.
pub fn native_cli_executable(agent_id: &AgentId) -> Option<&'static str> {
    match agent_id.as_str() {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        _ => None,
    }
}

/// Whether this release verified terminal mode for the Agent.
///
/// Keeping the answer here, beside the executable table, is what makes the
/// allow-list reviewable: promoting an Agent is one line in two functions.
pub fn terminal_session_support(agent_id: &AgentId) -> crate::terminal::TerminalSessionSupport {
    use crate::terminal::TerminalSessionSupport;
    match agent_id.as_str() {
        "claude" | "codex" => TerminalSessionSupport::Supported,
        _ if native_cli_executable(agent_id).is_some() => TerminalSessionSupport::Unverified,
        _ => TerminalSessionSupport::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str) -> AgentId {
        AgentId::parse(id).expect("test agent ids are valid")
    }

    #[test]
    fn the_native_executable_is_not_the_acp_adapter() {
        // The adapter binary has no interface of its own, so using it would
        // launch a stdio server into a terminal nobody can drive.
        assert_eq!(native_cli_executable(&agent("claude")), Some("claude"));
        assert_eq!(native_cli_executable(&agent("codex")), Some("codex"));
        assert_ne!(
            native_cli_executable(&agent("claude")),
            Some("claude-agent-acp")
        );
        assert_ne!(native_cli_executable(&agent("codex")), Some("codex-acp"));
    }

    #[test]
    fn only_verified_agents_advertise_terminal_mode() {
        assert!(terminal_session_support(&agent("claude")).is_supported());
        assert!(terminal_session_support(&agent("codex")).is_supported());
        assert_eq!(
            terminal_session_support(&agent("opencode")),
            crate::terminal::TerminalSessionSupport::Unavailable
        );
    }
}

/// The arguments that make a native CLI start on a chosen model.
///
/// The new-session page picks a model for the *session*, and a terminal session
/// has no other channel to carry that choice: Vibex is not in the conversation,
/// so the only way the choice reaches the Agent is the command line. Each Agent
/// spells it differently, which is why the mapping lives beside the executable
/// table rather than in the caller.
///
/// Returns nothing when the Agent has no flag for it, or when the selection is
/// the Agent's own default — passing `--model` with a guessed id would start the
/// CLI on a model the reader did not choose.
pub fn native_cli_model_arguments(agent_id: &AgentId, model_id: Option<&str>) -> Vec<String> {
    let Some(model_id) = model_id.filter(|model| !model.trim().is_empty()) else {
        return Vec::new();
    };
    match agent_id.as_str() {
        "claude" | "codex" => vec!["--model".to_string(), model_id.to_string()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod model_argument_tests {
    use super::*;

    fn agent(id: &str) -> AgentId {
        AgentId::parse(id).expect("literal agent id")
    }

    /// The chosen model reaches the CLI, because the CLI is the only thing that
    /// can act on it in terminal mode.
    #[test]
    fn the_chosen_model_becomes_a_flag() {
        assert_eq!(
            native_cli_model_arguments(&agent("claude"), Some("claude-opus-4-6")),
            vec!["--model".to_string(), "claude-opus-4-6".to_string()]
        );
        assert_eq!(
            native_cli_model_arguments(&agent("codex"), Some("gpt-5")),
            vec!["--model".to_string(), "gpt-5".to_string()]
        );
    }

    /// The Agent's own default stays its own: no flag, no guess.
    #[test]
    fn an_agent_default_passes_no_flag() {
        assert!(native_cli_model_arguments(&agent("claude"), None).is_empty());
        assert!(native_cli_model_arguments(&agent("claude"), Some("   ")).is_empty());
    }

    /// An Agent without a known flag gets none, rather than an argument its CLI
    /// would reject.
    #[test]
    fn an_unknown_agent_gets_no_flag() {
        assert!(native_cli_model_arguments(&agent("opencode"), Some("m")).is_empty());
    }
}

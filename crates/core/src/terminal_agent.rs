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

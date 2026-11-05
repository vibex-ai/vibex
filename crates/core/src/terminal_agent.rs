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

/// The arguments that make a native CLI start with the session's other options.
///
/// Terminal mode hands the interface to the Agent, but the page still chooses
/// how that Agent should work. The model is not the only choice: an effort
/// level and a conversation mode are just as visible to the reader, and a CLI
/// that starts without them silently ignores the page. Each is only emitted when
/// the Agent has a flag for it and the value is one the flag accepts, because an
/// argument the CLI rejects is worse than an option it does not apply.
pub fn native_cli_option_arguments(
    agent_id: &AgentId,
    reasoning_effort: Option<&str>,
    mode_id: Option<&str>,
) -> Vec<String> {
    let agent = agent_id.as_str();
    let mut arguments = Vec::new();
    if let Some(effort) = reasoning_effort.filter(|value| !value.trim().is_empty()) {
        match agent {
            // Claude's own ladder, and the only values its flag documents.
            "claude" if matches!(effort, "low" | "medium" | "high" | "xhigh" | "max") => {
                arguments.push("--effort".to_string());
                arguments.push(effort.to_string());
            }
            // Codex takes the effort as configuration rather than a flag of
            // its own.
            "codex" if matches!(effort, "none" | "minimal" | "low" | "medium" | "high" | "xhigh") => {
                arguments.push("-c".to_string());
                arguments.push(format!("model_reasoning_effort=\"{effort}\""));
            }
            _ => {}
        }
    }
    if let Some(mode) = mode_id.filter(|value| !value.trim().is_empty()) {
        match agent {
            // Vibex's conversation modes map onto Claude's permission modes:
            // the two answer the same question, how much the Agent may do
            // without asking.
            "claude" => {
                if let Some(permission) = claude_permission_mode(mode) {
                    arguments.push("--permission-mode".to_string());
                    arguments.push(permission.to_string());
                }
            }
            "codex" => {
                if let Some(approval) = codex_approval_policy(mode) {
                    arguments.push("-c".to_string());
                    arguments.push(format!("approval_policy=\"{approval}\""));
                }
            }
            _ => {}
        }
    }
    arguments
}

/// Vibex's conversation mode as a Claude permission mode.
///
/// Only the modes whose meaning is unambiguous are mapped. A mode Vibex invented
/// has no counterpart, and guessing one would change what the Agent is allowed to
/// do without the reader asking for it.
fn claude_permission_mode(mode: &str) -> Option<&'static str> {
    match mode {
        "ask" | "default" | "manual" => Some("manual"),
        "auto" | "auto-edit" | "accept-edits" => Some("acceptEdits"),
        "plan" | "read-only" | "readonly" => Some("plan"),
        "bypass" | "yolo" | "full-access" => Some("bypassPermissions"),
        "never-ask" | "dont-ask" => Some("dontAsk"),
        _ => None,
    }
}

/// Vibex's conversation mode as a Codex approval policy.
fn codex_approval_policy(mode: &str) -> Option<&'static str> {
    match mode {
        "ask" | "default" | "manual" => Some("on-request"),
        "auto" | "auto-edit" | "accept-edits" => Some("on-failure"),
        "plan" | "read-only" | "readonly" => Some("untrusted"),
        "bypass" | "yolo" | "full-access" | "never-ask" | "dont-ask" => Some("never"),
        _ => None,
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

#[cfg(test)]
mod option_argument_tests {
    use super::*;

    fn agent(id: &str) -> AgentId {
        AgentId::parse(id).expect("literal agent id")
    }

    /// The effort the reader chose reaches the CLI, in that CLI's own spelling.
    #[test]
    fn the_chosen_effort_becomes_a_flag() {
        assert_eq!(
            native_cli_option_arguments(&agent("claude"), Some("xhigh"), None),
            vec!["--effort".to_string(), "xhigh".to_string()]
        );
        assert_eq!(
            native_cli_option_arguments(&agent("codex"), Some("high"), None),
            vec![
                "-c".to_string(),
                "model_reasoning_effort=\"high\"".to_string()
            ]
        );
    }

    /// The conversation mode reaches the CLI as that CLI's permission setting.
    #[test]
    fn the_chosen_mode_becomes_a_permission_setting() {
        assert_eq!(
            native_cli_option_arguments(&agent("claude"), None, Some("plan")),
            vec!["--permission-mode".to_string(), "plan".to_string()]
        );
        assert_eq!(
            native_cli_option_arguments(&agent("claude"), None, Some("auto")),
            vec!["--permission-mode".to_string(), "acceptEdits".to_string()]
        );
        assert_eq!(
            native_cli_option_arguments(&agent("codex"), None, Some("bypass")),
            vec!["-c".to_string(), "approval_policy=\"never\"".to_string()]
        );
    }

    /// A value the flag would reject is not passed at all.
    ///
    /// An argument the CLI refuses stops the Agent from starting, which is worse
    /// than an option that does not apply: the reader at least gets a session.
    #[test]
    fn a_value_the_flag_would_reject_is_skipped() {
        assert!(native_cli_option_arguments(&agent("claude"), Some("ultra"), None).is_empty());
        assert!(native_cli_option_arguments(&agent("claude"), None, Some("vibex-only")).is_empty());
        assert!(native_cli_option_arguments(&agent("opencode"), Some("high"), Some("plan")).is_empty());
    }

    /// Nothing chosen means nothing passed.
    #[test]
    fn an_unset_option_passes_no_flag() {
        assert!(native_cli_option_arguments(&agent("claude"), None, None).is_empty());
        assert!(native_cli_option_arguments(&agent("claude"), Some("  "), Some("")).is_empty());
    }
}

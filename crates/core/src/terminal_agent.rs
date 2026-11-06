//! Which Agent CLIs can run as a terminal-mode session, and how to drive them.
//!
//! Terminal mode hands the conversation to the Agent's own interactive TUI, so
//! Vibex only needs the executable name — but a *complete* terminal session
//! needs more than that. The first prompt, the model, the effort, the
//! permission mode, and the restart command are all inputs the page already
//! collected, and the command line is the only channel that reaches an Agent
//! Vibex is not talking to. Each CLI spells those inputs differently, and some
//! of them (`opencode`, `cline`, `gemini`) are not positional at all.
//!
//! That is why every Agent's launch shape lives here, in one table, rather than
//! in the five call sites that used to each match on the Agent id. A CLI that is
//! added to [`TerminalCli`] gets its prompt, options, restart, and observation
//! behavior from the same row, which is what makes "supported" mean the whole
//! feature and not merely "the binary exists".
//!
//! The supported list is an allow-list, not a capability table. A terminal
//! session is only listed once its TUI, its state recognition, and its restart
//! path were exercised together for this release; an Agent that merely *has* a
//! native CLI stays unverified so the new-session panel cannot advertise a mode
//! nobody tested.

use crate::agent_config::AgentId;

/// One Agent's native interactive CLI.
///
/// The variants are the allow-list. Adding one is a deliberate act: the
/// `native_cli_*` functions below are total over this enum, so a new variant
/// cannot be added without deciding how its prompt, options, and restart are
/// spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalCli {
    Claude,
    Codex,
    OpenCode,
    Gemini,
    Pi,
    Grok,
    Cline,
    Copilot,
    Cursor,
    Kimi,
    Codebuddy,
    Devin,
    Hermes,
}

impl TerminalCli {
    /// Every Agent this release verified for terminal mode.
    pub const ALL: &'static [TerminalCli] = &[
        Self::Claude,
        Self::Codex,
        Self::OpenCode,
        Self::Gemini,
        Self::Pi,
        Self::Grok,
        Self::Cline,
        Self::Copilot,
        Self::Cursor,
        Self::Kimi,
        Self::Codebuddy,
        Self::Devin,
        Self::Hermes,
    ];

    pub fn for_agent(agent_id: &AgentId) -> Option<Self> {
        match agent_id.as_str() {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "opencode" => Some(Self::OpenCode),
            "gemini" => Some(Self::Gemini),
            "pi" => Some(Self::Pi),
            "grok" => Some(Self::Grok),
            "cline" => Some(Self::Cline),
            "copilot" => Some(Self::Copilot),
            "cursor" => Some(Self::Cursor),
            "kimi" => Some(Self::Kimi),
            "codebuddy-code" => Some(Self::Codebuddy),
            "devin" => Some(Self::Devin),
            "hermes" => Some(Self::Hermes),
            _ => None,
        }
    }

    pub fn agent_id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Gemini => "gemini",
            Self::Pi => "pi",
            Self::Grok => "grok",
            Self::Cline => "cline",
            Self::Copilot => "copilot",
            Self::Cursor => "cursor",
            Self::Kimi => "kimi",
            Self::Codebuddy => "codebuddy-code",
            Self::Devin => "devin",
            Self::Hermes => "hermes",
        }
    }

    /// The executable to start, as it is named on `PATH`.
    ///
    /// Deliberately *not* the ACP adapter binary (`claude-agent-acp`,
    /// `codex-acp`): the adapter speaks ACP on stdio and has no interface of its
    /// own. The managed-install resolver may replace this with a path inside a
    /// Vibex-managed installation; the name stays the same so a recorded argv
    /// remains portable across installations.
    pub fn executable(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Gemini => "gemini",
            Self::Pi => "pi",
            Self::Grok => "grok",
            Self::Cline => "cline",
            Self::Copilot => "copilot",
            Self::Cursor => "cursor-agent",
            Self::Kimi => "kimi",
            Self::Codebuddy => "codebuddy",
            Self::Devin => "devin",
            Self::Hermes => "hermes",
        }
    }

    /// Arguments that belong to every launch of this CLI.
    ///
    /// `cline` and `hermes` start a one-shot/non-interactive mode by default in
    /// some configurations; their TUI flag is what makes the mode the page
    /// promises actually appear. The flag is part of the recorded argv, so a
    /// restart keeps the same interface.
    pub fn launch_arguments(self) -> &'static [&'static str] {
        match self {
            Self::Cline => &["--tui"],
            Self::Hermes => &["--tui"],
            _ => &[],
        }
    }

    /// How the reader's first turn reaches the interactive session.
    pub fn prompt_arguments(self, prompt: &str) -> Vec<String> {
        match self {
            Self::Claude
            | Self::Codex
            | Self::Gemini
            | Self::Pi
            | Self::Grok
            | Self::Cursor
            | Self::Codebuddy => vec![prompt.to_string()],
            Self::Cline => vec![prompt.to_string()],
            // OpenCode's positional is the project directory, so a positional
            // prompt would be read as "open this project" and silently dropped.
            Self::OpenCode => vec!["--prompt".to_string(), prompt.to_string()],
            // Copilot's positional is a subcommand; `-i` is the documented
            // "start interactive and run this prompt" entry point.
            Self::Copilot => vec!["-i".to_string(), prompt.to_string()],
            // Devin separates paths from the prompt with `--`.
            Self::Devin => vec!["--".to_string(), prompt.to_string()],
            // Kimi's interactive session has no initial-prompt flag (`-p` is
            // non-interactive), and Hermes' TUI takes none either. The reader
            // types the first turn in the TUI instead of losing it to a flag
            // the CLI would treat as a command.
            Self::Kimi | Self::Hermes => Vec::new(),
        }
    }

    /// Whether this CLI can accept an initial prompt at all.
    pub fn accepts_initial_prompt(self) -> bool {
        !matches!(self, Self::Kimi | Self::Hermes)
    }

    /// The arguments that make a native CLI start on a chosen model.
    pub fn model_arguments(self, model_id: Option<&str>) -> Vec<String> {
        let Some(model_id) = model_id.filter(|model| !model.trim().is_empty()) else {
            return Vec::new();
        };
        vec!["--model".to_string(), model_id.to_string()]
    }

    /// The arguments that make a native CLI start with the session's other
    /// options.
    pub fn option_arguments(
        self,
        reasoning_effort: Option<&str>,
        mode_id: Option<&str>,
    ) -> Vec<String> {
        let mut arguments = Vec::new();
        if let Some(effort) = reasoning_effort.filter(|value| !value.trim().is_empty()) {
            arguments.extend(self.effort_arguments(effort));
        }
        if let Some(mode) = mode_id.filter(|value| !value.trim().is_empty()) {
            arguments.extend(self.mode_arguments(mode));
        }
        arguments
    }

    fn effort_arguments(self, effort: &str) -> Vec<String> {
        match self {
            // Claude's own ladder, and the only values its flag documents.
            Self::Claude if matches!(effort, "low" | "medium" | "high" | "xhigh" | "max") => {
                vec!["--effort".to_string(), effort.to_string()]
            }
            // Codex takes the effort as configuration rather than a flag of
            // its own.
            Self::Codex
                if matches!(
                    effort,
                    "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
                ) =>
            {
                vec![
                    "-c".to_string(),
                    format!("model_reasoning_effort=\"{effort}\""),
                ]
            }
            Self::Pi
                if matches!(
                    effort,
                    "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                ) =>
            {
                vec!["--thinking".to_string(), effort.to_string()]
            }
            // Pi spells "no thinking" as `off`; Vibex's neutral value is
            // `none`, so the two are translated rather than dropped.
            Self::Pi if effort == "none" => {
                vec!["--thinking".to_string(), "off".to_string()]
            }
            // Grok validates the ladder and rejects anything else at startup.
            Self::Grok if matches!(effort, "low" | "medium" | "high" | "xhigh") => {
                // `--effort` is a documented alias of `--reasoning-effort`.
                vec!["--effort".to_string(), effort.to_string()]
            }
            Self::Cline if matches!(effort, "none" | "low" | "medium" | "high" | "xhigh") => {
                vec!["--thinking".to_string(), effort.to_string()]
            }
            Self::Copilot
                if matches!(
                    effort,
                    "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                ) =>
            {
                vec!["--reasoning-effort".to_string(), effort.to_string()]
            }
            Self::Codebuddy
                if matches!(
                    effort,
                    "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                ) =>
            {
                vec!["--effort".to_string(), effort.to_string()]
            }
            // Cursor has no standalone effort flag, and its only spelling is a
            // bracket override on the model id, which this function does not
            // receive. An option the CLI would reject is worse than one it
            // leaves at its own default.
            _ => Vec::new(),
        }
    }

    fn mode_arguments(self, mode: &str) -> Vec<String> {
        let Some(mode) = ConversationMode::parse(mode) else {
            return Vec::new();
        };
        match self {
            // Vibex's conversation modes map onto Claude's permission modes:
            // the two answer the same question, how much the Agent may do
            // without asking.
            Self::Claude => permission_mode("--permission-mode", claude_permission_mode(mode)),
            // Codex spells the same decision as two configuration settings.
            Self::Codex => {
                let mut arguments = Vec::new();
                if let Some(approval) = codex_approval_policy(mode) {
                    arguments.push("-c".to_string());
                    arguments.push(format!("approval_policy=\"{approval}\""));
                }
                if let Some(sandbox) = codex_sandbox_policy(mode) {
                    arguments.push("-c".to_string());
                    arguments.push(format!("sandbox_mode=\"{sandbox}\""));
                }
                arguments
            }
            // Grok reused most of Claude's permission vocabulary, but its
            // `manual` spelling is `default`, and it has no `max` effort.
            Self::Grok => permission_mode("--permission-mode", grok_permission_mode(mode)),
            Self::Gemini => permission_mode("--approval-mode", gemini_approval_mode(mode)),
            Self::Codebuddy => {
                permission_mode("--permission-mode", codebuddy_permission_mode(mode))
            }
            Self::OpenCode => match mode {
                ConversationMode::Plan => vec!["--agent".to_string(), "plan".to_string()],
                ConversationMode::Auto | ConversationMode::Bypass | ConversationMode::Never => {
                    vec!["--auto".to_string()]
                }
                ConversationMode::Ask => Vec::new(),
            },
            // Cline's `--auto-approve` is the all-tools switch; `--yolo`
            // approves only a limited set, so the wider modes use the former.
            Self::Cline => match mode {
                ConversationMode::Plan => vec!["--plan".to_string()],
                ConversationMode::Ask => {
                    vec!["--auto-approve".to_string(), "false".to_string()]
                }
                ConversationMode::Auto | ConversationMode::Bypass | ConversationMode::Never => {
                    vec!["--auto-approve".to_string(), "true".to_string()]
                }
            },
            Self::Copilot => match mode {
                ConversationMode::Bypass | ConversationMode::Never => {
                    vec!["--allow-all-tools".to_string()]
                }
                _ => Vec::new(),
            },
            Self::Cursor => match mode {
                ConversationMode::Plan => vec!["--mode".to_string(), "plan".to_string()],
                ConversationMode::Auto => vec!["--auto-review".to_string()],
                ConversationMode::Bypass | ConversationMode::Never => {
                    vec!["--force".to_string()]
                }
                ConversationMode::Ask => Vec::new(),
            },
            Self::Kimi => match mode {
                ConversationMode::Plan => vec!["--plan".to_string()],
                ConversationMode::Auto => vec!["--yolo".to_string()],
                ConversationMode::Bypass | ConversationMode::Never => {
                    vec!["--auto".to_string()]
                }
                ConversationMode::Ask => Vec::new(),
            },
            Self::Devin => match mode {
                ConversationMode::Ask => permission_mode("--permission-mode", Some("auto")),
                ConversationMode::Auto => permission_mode("--permission-mode", Some("smart")),
                ConversationMode::Bypass | ConversationMode::Never => {
                    permission_mode("--permission-mode", Some("dangerous"))
                }
                // Devin's plan mode is a slash command inside the TUI; its
                // permission modes have no read-only counterpart.
                ConversationMode::Plan => Vec::new(),
            },
            Self::Hermes => match mode {
                ConversationMode::Bypass | ConversationMode::Never => {
                    vec!["--yolo".to_string()]
                }
                _ => Vec::new(),
            },
            Self::Pi => Vec::new(),
        }
    }

    /// The arguments a restart appends to the recorded argv.
    ///
    /// A known native session id always wins: it continues *this*
    /// conversation. Without one, every CLI but Cline can continue its latest
    /// conversation in the checkout, which is this session unless the reader
    /// ran another one there since. Cline only addresses sessions by id, so an
    /// unknown id leaves it with nothing to resume rather than a guessed one.
    pub fn resume_arguments(self, native_session_id: Option<&str>) -> Vec<String> {
        if let Some(id) = native_session_id.filter(|id| !id.trim().is_empty()) {
            return match self {
                Self::Claude => vec!["--resume".to_string(), id.to_string()],
                // Codex spells resume as a subcommand, not a flag.
                Self::Codex => vec!["resume".to_string(), id.to_string()],
                Self::OpenCode => vec!["--session".to_string(), id.to_string()],
                Self::Gemini => vec!["--resume".to_string(), id.to_string()],
                Self::Pi => vec!["--session".to_string(), id.to_string()],
                Self::Grok => vec!["--resume".to_string(), id.to_string()],
                Self::Cline => vec!["--id".to_string(), id.to_string()],
                Self::Copilot => vec!["--resume".to_string(), id.to_string()],
                Self::Cursor => vec!["--resume".to_string(), id.to_string()],
                Self::Kimi => vec!["--session".to_string(), id.to_string()],
                Self::Codebuddy => vec!["--resume".to_string(), id.to_string()],
                Self::Devin => vec!["--resume".to_string(), id.to_string()],
                Self::Hermes => vec!["--resume".to_string(), id.to_string()],
            };
        }
        match self {
            // `--continue` needs no id, which is what makes a restart keep its
            // history even when the CLI exited without printing one.
            Self::Claude => vec!["--continue".to_string()],
            Self::Codex => vec!["resume".to_string(), "--last".to_string()],
            Self::OpenCode => vec!["--continue".to_string()],
            // Gemini resumes by index or `latest`, never by a bare flag.
            Self::Gemini => vec!["--resume".to_string(), "latest".to_string()],
            Self::Pi => vec!["--continue".to_string()],
            Self::Grok => vec!["--continue".to_string()],
            Self::Copilot => vec!["--continue".to_string()],
            Self::Cursor => vec!["--continue".to_string()],
            Self::Kimi => vec!["--continue".to_string()],
            Self::Codebuddy => vec!["--continue".to_string()],
            Self::Devin => vec!["--continue".to_string()],
            Self::Hermes => vec!["--continue".to_string()],
            // Cline's only resume knob is `--id`, and guessing one would open
            // somebody else's conversation.
            Self::Cline => Vec::new(),
        }
    }

    /// Prefixes a CLI prints before its own session id in an exit notice.
    ///
    /// Matched case-insensitively, one line at a time. The list is deliberately
    /// command-shaped: a bare number or a generic "session" word on a screen is
    /// not proof that it identifies the conversation a restart should reopen.
    pub fn resume_notice_prefixes(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["claude --resume ", "claude -r "],
            Self::Codex => &["codex resume ", "codex resume --last "],
            Self::OpenCode => &["opencode --session ", "opencode -s "],
            Self::Gemini => &["gemini --resume ", "gemini -r "],
            Self::Pi => &["pi --session ", "pi --resume "],
            Self::Grok => &["grok --resume ", "grok -r "],
            Self::Cline => &["cline --id ", "cline --tui --id "],
            Self::Copilot => &["copilot --resume ", "copilot --resume=", "copilot -r "],
            Self::Cursor => &[
                "cursor-agent --resume ",
                "cursor-agent resume ",
                "agent --resume ",
            ],
            Self::Kimi => &["kimi --session ", "kimi -s ", "kimi --resume "],
            Self::Codebuddy => &["codebuddy --resume ", "codebuddy -r ", "cbc --resume "],
            Self::Devin => &["devin --resume ", "devin -r "],
            Self::Hermes => &["hermes --resume ", "hermes -r "],
        }
    }
}

/// The Agent's native interactive executable, when one is known.
pub fn native_cli_executable(agent_id: &AgentId) -> Option<&'static str> {
    TerminalCli::for_agent(agent_id).map(TerminalCli::executable)
}

/// Arguments that belong to every launch of the Agent's CLI.
pub fn native_cli_launch_arguments(agent_id: &AgentId) -> Vec<String> {
    TerminalCli::for_agent(agent_id)
        .map(|cli| {
            cli.launch_arguments()
                .iter()
                .map(|argument| (*argument).to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The arguments that deliver the reader's first turn to the CLI.
///
/// Empty when the CLI has no way to accept one in an interactive session; the
/// caller keeps the launch rather than failing it, because losing the first
/// prompt is better than losing the session.
pub fn native_cli_prompt_arguments(agent_id: &AgentId, prompt: &str) -> Vec<String> {
    TerminalCli::for_agent(agent_id)
        .map(|cli| cli.prompt_arguments(prompt))
        .unwrap_or_else(|| vec![prompt.to_string()])
}

/// Whether this Agent's CLI can accept an initial prompt at all.
pub fn native_cli_accepts_initial_prompt(agent_id: &AgentId) -> bool {
    TerminalCli::for_agent(agent_id).is_some_and(TerminalCli::accepts_initial_prompt)
}

/// Whether this release verified terminal mode for the Agent.
///
/// Keeping the answer derived from [`TerminalCli`] is what makes the allow-list
/// reviewable: promoting an Agent is one variant plus its row of decisions.
pub fn terminal_session_support(agent_id: &AgentId) -> crate::terminal::TerminalSessionSupport {
    use crate::terminal::TerminalSessionSupport;
    if TerminalCli::for_agent(agent_id).is_some() {
        TerminalSessionSupport::Supported
    } else {
        TerminalSessionSupport::Unavailable
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
    TerminalCli::for_agent(agent_id)
        .map(|cli| cli.model_arguments(model_id))
        .unwrap_or_default()
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
    TerminalCli::for_agent(agent_id)
        .map(|cli| cli.option_arguments(reasoning_effort, mode_id))
        .unwrap_or_default()
}

/// The arguments a restart appends to the argv recorded at creation.
///
/// A recorded native session id continues that exact conversation; without one,
/// most CLIs can continue the latest conversation in the checkout. The caller
/// passes the id from the exit notice, which is the only place a terminal Agent
/// exposes it.
pub fn native_cli_resume_arguments(
    agent_id: &AgentId,
    native_session_id: Option<&str>,
) -> Vec<String> {
    TerminalCli::for_agent(agent_id)
        .map(|cli| cli.resume_arguments(native_session_id))
        .unwrap_or_default()
}

/// The command prefixes an Agent prints before its own session id.
pub fn native_cli_resume_notice_prefixes(agent_id: &AgentId) -> &'static [&'static str] {
    TerminalCli::for_agent(agent_id)
        .map(TerminalCli::resume_notice_prefixes)
        .unwrap_or(&[])
}

/// Vibex's conversation mode, reduced to the decisions the CLIs share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConversationMode {
    Ask,
    Auto,
    Plan,
    Bypass,
    Never,
}

impl ConversationMode {
    /// Parses both Vibex's own mode ids and the native spellings an ACP Agent
    /// reports, because either can reach the new-session page.
    fn parse(mode: &str) -> Option<Self> {
        match mode.trim().to_ascii_lowercase().as_str() {
            "ask" | "default" | "manual" | "interactive" | "agent" => Some(Self::Ask),
            "auto" | "auto-edit" | "auto_edit" | "acceptedits" | "accept-edits"
            | "accept_edits" | "smart" | "on-failure" => Some(Self::Auto),
            "plan" | "read-only" | "readonly" | "read_only" | "architect" | "plan-mode" => {
                Some(Self::Plan)
            }
            "bypass" | "yolo" | "full-access" | "full_access" | "bypasspermissions"
            | "dangerous" => Some(Self::Bypass),
            "never" | "never-ask" | "never_ask" | "dont-ask" | "dont_ask" | "dontask" => {
                Some(Self::Never)
            }
            _ => None,
        }
    }
}

fn permission_mode(flag: &str, value: Option<&'static str>) -> Vec<String> {
    value
        .map(|value| vec![flag.to_string(), value.to_string()])
        .unwrap_or_default()
}

/// Vibex's conversation mode as a Claude permission mode.
///
/// Only the modes whose meaning is unambiguous are mapped. A mode Vibex invented
/// has no counterpart, and guessing one would change what the Agent is allowed to
/// do without the reader asking for it.
fn claude_permission_mode(mode: ConversationMode) -> Option<&'static str> {
    match mode {
        ConversationMode::Ask => Some("manual"),
        ConversationMode::Auto => Some("acceptEdits"),
        ConversationMode::Plan => Some("plan"),
        ConversationMode::Bypass => Some("bypassPermissions"),
        ConversationMode::Never => Some("dontAsk"),
    }
}

/// Gemini CLI's approval-mode vocabulary.
fn grok_permission_mode(mode: ConversationMode) -> Option<&'static str> {
    match mode {
        ConversationMode::Ask => Some("default"),
        ConversationMode::Auto => Some("acceptEdits"),
        ConversationMode::Plan => Some("plan"),
        ConversationMode::Bypass => Some("bypassPermissions"),
        ConversationMode::Never => Some("dontAsk"),
    }
}

fn gemini_approval_mode(mode: ConversationMode) -> Option<&'static str> {
    match mode {
        ConversationMode::Ask => Some("default"),
        ConversationMode::Auto => Some("auto_edit"),
        ConversationMode::Plan => Some("plan"),
        ConversationMode::Bypass | ConversationMode::Never => Some("yolo"),
    }
}

/// CodeBuddy keeps Claude's permission modes and adds an `auto` one.
fn codebuddy_permission_mode(mode: ConversationMode) -> Option<&'static str> {
    match mode {
        ConversationMode::Ask => Some("default"),
        ConversationMode::Auto => Some("acceptEdits"),
        ConversationMode::Plan => Some("plan"),
        ConversationMode::Bypass => Some("bypassPermissions"),
        ConversationMode::Never => Some("dontAsk"),
    }
}

/// Vibex's conversation mode as a Codex approval policy.
///
/// The values are the ones Codex 0.166 accepts. `untrusted` used to mean "ask
/// before anything the sandbox does not already cover", and it is now rejected
/// outright at startup (`approval_policy = "untrusted" is no longer supported`),
/// which would leave the reader with a session that dies before the interface
/// appears. Read-only is therefore spelled as its two surviving halves: ask on
/// request, and confine execution with the read-only sandbox.
fn codex_approval_policy(mode: ConversationMode) -> Option<&'static str> {
    match mode {
        // Codex 2.x dropped `on-failure`; only `on-request` and `never` are
        // accepted now. Auto therefore asks at the model's discretion rather
        // than failing to start on a removed value.
        ConversationMode::Ask | ConversationMode::Auto | ConversationMode::Plan => {
            Some("on-request")
        }
        ConversationMode::Bypass | ConversationMode::Never => Some("never"),
    }
}

/// The sandbox policy that completes a read-only conversation mode.
///
/// Codex splits the old `untrusted` approval policy into "when may I ask" and
/// "what may I touch". Only the read-only mode needs the second half; the
/// others leave the sandbox at Codex's own default so the reader's configuration
/// still decides.
fn codex_sandbox_policy(mode: ConversationMode) -> Option<&'static str> {
    match mode {
        ConversationMode::Plan => Some("read-only"),
        _ => None,
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
    fn every_verified_agent_advertises_terminal_mode() {
        for cli in TerminalCli::ALL {
            assert!(
                terminal_session_support(&agent(cli.agent_id())).is_supported(),
                "{} must be supported",
                cli.agent_id()
            );
            assert_eq!(
                native_cli_executable(&agent(cli.agent_id())),
                Some(cli.executable())
            );
        }
        assert_eq!(
            terminal_session_support(&agent("deepseek-harness")),
            crate::terminal::TerminalSessionSupport::Unavailable
        );
    }

    /// Every supported Agent resolves through both directions of the table.
    #[test]
    fn the_cli_table_and_the_agent_id_are_bijective() {
        for cli in TerminalCli::ALL {
            assert_eq!(TerminalCli::for_agent(&agent(cli.agent_id())), Some(*cli));
        }
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
        assert_eq!(
            native_cli_model_arguments(&agent("opencode"), Some("anthropic/claude-opus-4-6")),
            vec![
                "--model".to_string(),
                "anthropic/claude-opus-4-6".to_string()
            ]
        );
        assert_eq!(
            native_cli_model_arguments(&agent("cursor"), Some("sonnet-4-thinking")),
            vec!["--model".to_string(), "sonnet-4-thinking".to_string()]
        );
    }

    /// The Agent's own default stays its own: no flag, no guess.
    #[test]
    fn an_agent_default_passes_no_flag() {
        assert!(native_cli_model_arguments(&agent("claude"), None).is_empty());
        assert!(native_cli_model_arguments(&agent("claude"), Some("   ")).is_empty());
    }

    /// An Agent without a terminal mode gets none, rather than an argument its
    /// CLI would reject.
    #[test]
    fn an_unknown_agent_gets_no_flag() {
        assert!(native_cli_model_arguments(&agent("deepseek-harness"), Some("m")).is_empty());
    }

    /// The first prompt uses the interface each CLI actually reads.
    #[test]
    fn the_first_prompt_uses_the_interface_the_cli_reads() {
        assert_eq!(
            native_cli_prompt_arguments(&agent("claude"), "fix the test"),
            vec!["fix the test".to_string()]
        );
        assert_eq!(
            native_cli_prompt_arguments(&agent("opencode"), "fix the test"),
            vec!["--prompt".to_string(), "fix the test".to_string()],
            "opencode's positional is the project path, not the prompt"
        );
        assert_eq!(
            native_cli_prompt_arguments(&agent("copilot"), "fix the test"),
            vec!["-i".to_string(), "fix the test".to_string()],
            "copilot only takes an initial interactive prompt through -i"
        );
        assert_eq!(
            native_cli_prompt_arguments(&agent("devin"), "fix the test"),
            vec!["--".to_string(), "fix the test".to_string()]
        );
        assert!(
            native_cli_prompt_arguments(&agent("kimi"), "fix the test").is_empty(),
            "kimi's interactive session has no initial-prompt flag"
        );
        assert!(!native_cli_accepts_initial_prompt(&agent("hermes")));
        assert!(native_cli_accepts_initial_prompt(&agent("cline")));
    }

    /// The TUIs that need a mode flag to stay interactive get one on every
    /// launch, and the flag is part of the recorded argv so a restart agrees.
    #[test]
    fn the_interactive_launch_arguments_are_recorded() {
        assert_eq!(
            native_cli_launch_arguments(&agent("cline")),
            vec!["--tui".to_string()]
        );
        assert_eq!(
            native_cli_launch_arguments(&agent("hermes")),
            vec!["--tui".to_string()]
        );
        assert!(native_cli_launch_arguments(&agent("claude")).is_empty());
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
        assert_eq!(
            native_cli_option_arguments(&agent("pi"), Some("high"), None),
            vec!["--thinking".to_string(), "high".to_string()]
        );
        assert_eq!(
            native_cli_option_arguments(&agent("cline"), Some("high"), None),
            vec!["--thinking".to_string(), "high".to_string()]
        );
        assert_eq!(
            native_cli_option_arguments(&agent("copilot"), Some("xhigh"), None),
            vec!["--reasoning-effort".to_string(), "xhigh".to_string()]
        );
        assert_eq!(
            native_cli_option_arguments(&agent("codebuddy-code"), Some("max"), None),
            vec!["--effort".to_string(), "max".to_string()]
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
        assert_eq!(
            native_cli_option_arguments(&agent("gemini"), None, Some("auto")),
            vec!["--approval-mode".to_string(), "auto_edit".to_string()]
        );
        assert_eq!(
            native_cli_option_arguments(&agent("grok"), None, Some("bypass")),
            vec![
                "--permission-mode".to_string(),
                "bypassPermissions".to_string()
            ]
        );
        assert_eq!(
            native_cli_option_arguments(&agent("kimi"), None, Some("plan")),
            vec!["--plan".to_string()]
        );
        assert_eq!(
            native_cli_option_arguments(&agent("cursor"), None, Some("bypass")),
            vec!["--force".to_string()]
        );
        assert_eq!(
            native_cli_option_arguments(&agent("devin"), None, Some("bypass")),
            vec!["--permission-mode".to_string(), "dangerous".to_string()]
        );
        assert_eq!(
            native_cli_option_arguments(&agent("cline"), None, Some("ask")),
            vec!["--auto-approve".to_string(), "false".to_string()]
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
        assert!(
            native_cli_option_arguments(&agent("gemini"), Some("high"), Some("plan"))
                .iter()
                .all(|argument| argument != "high")
        );
        assert!(native_cli_option_arguments(&agent("hermes"), Some("high"), None).is_empty());
    }

    /// The values each CLI validates are the ones it gets.
    #[test]
    fn the_effort_and_mode_vocabularies_stay_inside_each_cli() {
        // Grok rejects `max` outright at startup.
        assert!(native_cli_option_arguments(&agent("grok"), Some("max"), None).is_empty());
        // Pi calls no thinking `off`, not `none`.
        assert_eq!(
            native_cli_option_arguments(&agent("pi"), Some("none"), None),
            vec!["--thinking".to_string(), "off".to_string()]
        );
        // Codex 2.x no longer accepts `on-failure`; auto is on-request.
        assert_eq!(
            native_cli_option_arguments(&agent("codex"), None, Some("auto")),
            vec![
                "-c".to_string(),
                "approval_policy=\"on-request\"".to_string()
            ]
        );
        // Grok's ask mode is spelled `default`, not Claude's `manual`.
        assert_eq!(
            native_cli_option_arguments(&agent("grok"), None, Some("default")),
            vec!["--permission-mode".to_string(), "default".to_string()]
        );
    }

    /// Nothing chosen means nothing passed.
    #[test]
    fn an_unset_option_passes_no_flag() {
        assert!(native_cli_option_arguments(&agent("claude"), None, None).is_empty());
        assert!(native_cli_option_arguments(&agent("claude"), Some("  "), Some("")).is_empty());
    }

    /// A restart continues the exact conversation when the exit notice named it.
    #[test]
    fn a_known_session_id_resumes_in_each_cli_spelling() {
        for (agent_id, expected) in [
            ("claude", vec!["--resume", "id-1"]),
            ("codex", vec!["resume", "id-1"]),
            ("opencode", vec!["--session", "id-1"]),
            ("gemini", vec!["--resume", "id-1"]),
            ("pi", vec!["--session", "id-1"]),
            ("grok", vec!["--resume", "id-1"]),
            ("cline", vec!["--id", "id-1"]),
            ("copilot", vec!["--resume", "id-1"]),
            ("cursor", vec!["--resume", "id-1"]),
            ("kimi", vec!["--session", "id-1"]),
            ("codebuddy-code", vec!["--resume", "id-1"]),
            ("devin", vec!["--resume", "id-1"]),
            ("hermes", vec!["--resume", "id-1"]),
        ] {
            assert_eq!(
                native_cli_resume_arguments(&agent(agent_id), Some("id-1")),
                expected.into_iter().map(str::to_string).collect::<Vec<_>>(),
                "{agent_id} must resume by id"
            );
        }
    }

    /// Without an id, each CLI continues its latest conversation where it can.
    #[test]
    fn an_unknown_session_id_falls_back_to_continue() {
        for (agent_id, expected) in [
            ("claude", vec!["--continue"]),
            ("codex", vec!["resume", "--last"]),
            ("opencode", vec!["--continue"]),
            ("gemini", vec!["--resume", "latest"]),
            ("pi", vec!["--continue"]),
            ("grok", vec!["--continue"]),
            ("copilot", vec!["--continue"]),
            ("cursor", vec!["--continue"]),
            ("kimi", vec!["--continue"]),
            ("codebuddy-code", vec!["--continue"]),
            ("devin", vec!["--continue"]),
            ("hermes", vec!["--continue"]),
        ] {
            assert_eq!(
                native_cli_resume_arguments(&agent(agent_id), None),
                expected.into_iter().map(str::to_string).collect::<Vec<_>>(),
                "{agent_id} must continue its latest conversation"
            );
        }
        // Cline only addresses a session by id. An invented id would reopen
        // somebody else's conversation, so it gets no fallback at all.
        assert!(native_cli_resume_arguments(&agent("cline"), None).is_empty());
        // Whitespace is not an id, so the fallback still applies.
        assert_eq!(
            native_cli_resume_arguments(&agent("claude"), Some("  ")),
            vec!["--continue".to_string()]
        );
    }

    /// Every CLI's exit notice is recognized, in the spellings it prints.
    #[test]
    fn every_cli_has_a_resume_notice_prefix() {
        for cli in TerminalCli::ALL {
            assert!(
                !native_cli_resume_notice_prefixes(&agent(cli.agent_id())).is_empty(),
                "{} must expose a resume-notice prefix",
                cli.agent_id()
            );
        }
        assert!(native_cli_resume_notice_prefixes(&agent("deepseek-harness")).is_empty());
    }
}

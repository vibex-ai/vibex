#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpAgentCatalogEntry {
    pub id: &'static str,
    pub preset_id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub version: &'static str,
    /// The oldest Adapter version whose provider projection Vibex still
    /// supports, when that is older than `version`.
    ///
    /// `version` is the pin: what Vibex verifies, installs, and rolls back to.
    /// The floor is the compatibility statement. They diverge when a newer
    /// Adapter changes a wire detail and Vibex keeps a read-back shim for the
    /// older spelling instead of dropping the release: the pin moves forward
    /// while the floor stays on the oldest release the shim still covers.
    /// Collapsing the two would turn a runtime Vibex can still project into a
    /// conservative one (`agent_projection_version_mismatch`), which hides the
    /// provider editor and fails `session/new` for every user who has not yet
    /// upgraded. `None` means the pin is also the floor.
    pub compatible_version: Option<&'static str>,
    pub install_url: &'static str,
    pub command: &'static [&'static str],
    pub env: &'static [(&'static str, &'static str)],
    pub supports_mcp_servers: Option<bool>,
}

impl AcpAgentCatalogEntry {
    const fn new(
        id: &'static str,
        label: &'static str,
        description: &'static str,
        version: &'static str,
        install_url: &'static str,
        command: &'static [&'static str],
    ) -> Self {
        Self {
            id,
            preset_id: id,
            label,
            description,
            version,
            compatible_version: None,
            install_url,
            command,
            env: &[],
            supports_mcp_servers: None,
        }
    }

    /// Declare a compatibility floor older than the catalog pin. See
    /// [`AcpAgentCatalogEntry::compatible_version`].
    const fn with_compatible_version(mut self, version: &'static str) -> Self {
        self.compatible_version = Some(version);
        self
    }

    const fn with_preset_id(mut self, preset_id: &'static str) -> Self {
        self.preset_id = preset_id;
        self
    }

    const fn with_env(mut self, env: &'static [(&'static str, &'static str)]) -> Self {
        self.env = env;
        self
    }

    const fn without_mcp_server_support(mut self) -> Self {
        self.supports_mcp_servers = Some(false);
        self
    }
}

const ACP_AGENT_CATALOG: &[AcpAgentCatalogEntry] = &[
    AcpAgentCatalogEntry::new(
        "antigravity",
        "Google Antigravity",
        "Google's AI coding agent connected through its first-party ACP server",
        // Google publishes no changelog for the ACP server, so this pin rests on
        // the ACP Registry's daily protocol probes: 1.0.0, 1.1.1 and 1.2.1 report
        // the same protocolVersion, auth methods, capabilities and method
        // results, and launch the same `agy_acp_server.par --uid=`. Only the
        // release archive changed its name at 1.2.0, which does not reach Vibex
        // because the archive URL comes from the Registry rather than a template.
        // 1.4.5 is still launched as `agy_acp_server.par` from a Registry
        // archive, so the launch shape and the floor are unchanged.
        "1.4.5",
        "https://antigravity.google/docs/ide/extensions",
        &["agy_acp_server"],
    )
    .with_compatible_version("1.0.0"),
    AcpAgentCatalogEntry::new(
        "amp-acp",
        "Amp",
        "ACP wrapper for Amp - the frontier coding agent",
        "0.7.0",
        "https://github.com/tao12345666333/amp-acp",
        &["amp-acp"],
    ),
    AcpAgentCatalogEntry::new(
        "auggie",
        "Augment CLI",
        "Augment Code's powerful software agent, backed by industry-leading context engine",
        "0.30.0",
        "https://www.augmentcode.com/",
        &["npx", "-y", "@augmentcode/auggie@0.30.0", "--acp"],
    )
    .with_env(&[("AUGMENT_DISABLE_AUTO_UPDATE", "1")]),
    AcpAgentCatalogEntry::new(
        "cline",
        "Cline",
        "Autonomous coding agent CLI - capable of creating/editing files, running commands, using the browser, and more",
        // 3.0.73 changed the config-option surface (`auto_approve`, a third
        // auth method) but not the `CLINE_PROVIDER`/`CLINE_MODEL`/
        // `CLINE_API_KEY` contract the projection writes, so the old pin stays
        // the floor. The pin moves because the CLI's model catalogue is only
        // available inside a running Agent: an id outside the selected
        // provider's catalogue is silently replaced by the provider default
        // from 3.0.49 onward, and the runtime now reads the reported
        // `currentModelId` back instead of trusting `CLINE_MODEL`. 3.0.74 only
        // fixes retries, compaction and `ask_question` answers.
        "3.0.74",
        "https://cline.bot/cli",
        &["npx", "-y", "cline@3.0.74", "--acp"],
    )
    .with_compatible_version("3.0.29"),
    AcpAgentCatalogEntry::new(
        "copilot",
        "GitHub Copilot",
        "GitHub Copilot CLI agent connected through ACP",
        // 1.0.93 made `~/.copilot/settings.json` the only user-settings file;
        // keys in `config.json` are ignored. The ACP handshake and the
        // `mode`/`allow_all` options did not move. Through 1.0.107 the ACP
        // changes are fixes: `session/cancel` stops only the active turn and
        // leaves background shells running, `--agent` and
        // `includeCoAuthoredBy: false` are honored, and an autopilot session can
        // be switched back out.
        "1.0.107",
        "https://docs.github.com/en/copilot/concepts/agents/about-copilot-cli",
        &["copilot", "--acp"],
    )
    .with_compatible_version("1.0.78"),
    AcpAgentCatalogEntry::new(
        "codebuddy-code",
        "Codebuddy",
        "Tencent Cloud's official intelligent coding tool",
        // 2.165.0 moved the ACP SDK to 1.7.0 (stable elicitation, ESM dynamic
        // import), 2.166.0 added background-task methods, and 2.163.0 renamed
        // `CODEBUDDY_DISABLE_SESSION_HISTORY_CLEANUP` to
        // `CODEBUDDY_SESSION_HISTORY_CLEANUP` (the old name is no longer read).
        // `CODEBUDDY_BASE_URL`/`_API_KEY`/`_MODEL` are unchanged, so the
        // projection floor stays at 2.109.0.
        //
        // 2.173.0 removed `delegate` from the ACP permission modes (delegation
        // is now a session config option) and 2.181.0 renamed plugin MCP
        // servers and plugin option variables. Vibex reads the mode list the
        // Agent advertises, delivers session MCP servers on the wire, and the
        // CLI still accepts every `--permission-mode` value the terminal launch
        // writes, so none of those reaches a Vibex surface.
        "2.181.0",
        "https://www.codebuddy.cn/cli/",
        &[
            "npx",
            "-y",
            "@tencent-ai/codebuddy-code@2.181.0",
            "--acp",
        ],
    )
    .with_compatible_version("2.109.0"),
    AcpAgentCatalogEntry::new(
        "codewhale",
        "CodeWhale",
        "Terminal coding agent for DeepSeek V4 and open models",
        "0.8.55",
        "https://codewhale.net/",
        &["codewhale", "serve", "--acp"],
    ),
    AcpAgentCatalogEntry::new(
        "crow-cli",
        "crow-cli",
        "Minimal ACP Native Coding Agent",
        "0.1.23",
        "https://crow-ai.dev/",
        &["crow-cli", "acp"],
    ),
    AcpAgentCatalogEntry::new(
        "cursor",
        "Cursor",
        "Cursor's coding agent",
        // The Registry's `2026.11.05` build resolves to a real artifact under
        // `2026.11.05-86047df`; Cursor publishes no release note for that
        // specific build. Its `cursor/*` extension methods are the same set
        // 2026.10.22 shipped, including the blocking `cursor/ask_question` and
        // `cursor/create_plan` the runtime answers. The catalog records the
        // Registry's base version, and the installer compares release builds
        // by their core triple so the `-<commit>` suffix can neither sort a
        // build below its own base nor hide an update.
        "2026.11.05",
        "https://docs.cursor.com/en/cli/overview",
        &["cursor-agent", "acp"],
    ),
    AcpAgentCatalogEntry::new(
        "deepagents",
        "DeepAgents",
        "Batteries-included AI coding and general purpose agent powered by LangChain.",
        "0.1.15",
        "https://docs.langchain.com/oss/javascript/deepagents/overview",
        &["npx", "-y", "deepagents-acp@0.1.15"],
    ),
    AcpAgentCatalogEntry::new(
        "deepseek-harness",
        "DeepSeek Harness",
        "DeepSeek Harness coding agent connected through the deepseek-harness-acp bridge.",
        // 0.4.40 bundles Harness runtime `0.2.1-rc.2`, and 0.4.39 turned the
        // `DSH_PATH` gate into a minimum floor (`engines.dsh`). Its new RFD
        // subagent sessions are opt-in, and `ask_user_question` now reaches a
        // generic form client without the synthetic "Other" choice or a
        // `minItems` a skippable question could never satisfy, which is the
        // shape Vibex's elicitation form renders.
        //
        // 0.4.38 had kept Harness runtime `0.2.0-rc.2`. That release made the
        // packaged runtime the only default and gated every explicit
        // `--dsh-path`/`DSH_PATH`/`DSH_ACP_HOST` against
        // `0.1.7-rc.2 || 0.2.0-rc.1 || 0.2.0-rc.2` instead of falling back to
        // it, and it advertises only the auth methods the client declared.
        // Vibex therefore leaves `DSH_PATH` unset for a managed launch and
        // re-reads the method list per initialize. 0.4.39 turned that exact
        // list into a `>=0.1.7-rc.2` floor, so the managed launch keeps
        // leaving it unset.
        //
        // 0.4.37 had bundled Harness runtime `0.2.0-rc.2` (0.4.35 bundled
        // `0.1.7-rc.2`). The runtime move changed three behaviors Vibex reads:
        // token deltas now arrive as live `agent/assistant-stream` frames
        // (0.4.36 taught the bridge to project them; 0.4.35 stayed silent and
        // fell back to one whole-message chunk per step), `ask_user_question`
        // reaches the client as standard form elicitation, and a Model that is
        // not in the projected route catalog is substituted at session restore
        // instead of being sent verbatim. 0.4.37 adds ACP `session/fork`,
        // including the inclusive `jetbrains.air.fork` cut. The route is still
        // registered through the `$DSH_HOME/cordis.patch.yml` home patch the
        // bridge composes on every launch, with `settings.yaml` beside it for
        // 0.4.32/0.4.33 (runtime `0.1.5`); the bundled runtime still recognises
        // every `api` spelling the projection writes (`openai-completions`,
        // `openai-responses`, `anthropic-messages`) and 0.2.0 additionally
        // accepts `mistral-conversations`. The release's moves to the Messages
        // API and its shrunken default catalogue apply to the official
        // `deepseek-official` route, which the projected route does not use.
        "0.4.40",
        "https://github.com/openma-ai/deepseek-harness-acp",
        &[
            "npx",
            "-y",
            "@openma/deepseek-harness-acp@0.4.40",
        ],
    )
    // 0.4.33 qualifies every ACP model option id as `route::model`; Vibex keeps
    // the bare id on the wire and reads the qualified spelling back as an alias
    // so 0.4.32 keeps working. The projection floor therefore stays on 0.4.32
    // even though the pin moved past it; that qualification is byte-identical
    // through 0.4.40, so the floor still describes a release the alias covers.
    .with_compatible_version("0.4.32"),
    AcpAgentCatalogEntry::new(
        "devin",
        "Devin CLI",
        "Cognition's Devin for Terminal via Agent Client Protocol",
        ACP_AGENT_MANUAL_VERSION,
        "https://cli.devin.ai/docs",
        &["devin", "acp"],
    ),
    AcpAgentCatalogEntry::new(
        "dimcode",
        "DimCode",
        "A coding agent that puts leading models at your command.",
        "0.2.7",
        "https://dimcode.dev/docs/acp.html",
        &["npx", "-y", "dimcode@0.2.7", "acp"],
    ),
    AcpAgentCatalogEntry::new(
        "dirac",
        "Dirac",
        "Reduces API costs by more than 50%, produces better and faster work. Uses Hash anchored parallel edits, AST manipulation and a whole lot of neat optimizations. Fully Open Source.",
        "0.4.1",
        "https://dirac.run",
        &["npx", "-y", "dirac-cli@0.4.1", "--acp"],
    ),
    AcpAgentCatalogEntry::new(
        "factory-droid",
        "Factory Droid",
        "Factory Droid - AI coding agent powered by Factory AI",
        "0.153.1",
        "https://factory.ai/product/cli",
        &[
            "npx",
            "-y",
            "droid@0.153.1",
            "exec",
            "--output-format",
            "acp-daemon",
        ],
    )
    .with_env(&[
        ("DROID_DISABLE_AUTO_UPDATE", "true"),
        ("FACTORY_DROID_AUTO_UPDATE_ENABLED", "false"),
    ])
    .without_mcp_server_support(),
    AcpAgentCatalogEntry::new(
        "gemini",
        "Gemini CLI",
        "Google's official CLI for Gemini",
        // 0.63 and 0.64 are additive on the ACP surface: `initialize` and
        // `session/new` keep the same auth methods, modes and model list, while
        // `session/prompt` gains a `usage` block plus `usage_update`
        // notifications. A client that ignores them is not refused, so the
        // floor stays on the release the projection was first verified at.
        // 0.65 through 0.67 only fix the CLI; the ACP permission request gains
        // MCP server and tool metadata.
        "0.67.0",
        "https://geminicli.com",
        &["npx", "-y", "@google/gemini-cli@0.67.0", "--acp"],
    )
    .with_compatible_version("0.47.0"),
    AcpAgentCatalogEntry::new(
        "glm-acp-agent",
        "GLM Agent",
        "ACP agent powered by Zhipu AI's GLM Coding Plan models (glm-5.1, glm-5-turbo, glm-4.7, glm-4.5-air). Supports streaming, tool calls, mid-session model switching, image input via Z.AI Coding Plan Vision MCP, and session load/fork/resume with on-disk persistence.",
        "1.1.4",
        "https://github.com/stefandevo/glm-acp-agent",
        &["npx", "-y", "glm-acp-agent@1.1.4"],
    ),
    AcpAgentCatalogEntry::new(
        "goose",
        "goose",
        "A local, extensible, open source AI agent that automates engineering tasks",
        "1.33.1",
        "https://block.github.io/goose/",
        &["goose", "acp"],
    ),
    AcpAgentCatalogEntry::new(
        "grok",
        "Grok Build",
        "xAI's Grok Build agentic coding CLI with plan mode and parallel subagents. Requires a SuperGrok or X Premium+ subscription.",
        // The ACP surface is unchanged from 1.0.44 through 1.0.78: same protocol
        // version, auth methods, session capabilities, model state and
        // `reasoningEfforts`, and the `_x.ai/ask_user_question` and
        // `_x.ai/exit_plan_mode` requests the runtime answers. 1.0.78 only adds
        // `promptCapabilities.image`.
        "1.0.78",
        "https://docs.x.ai/build/overview",
        // xAI's guidance for ACP and headless use is to skip background update
        // checks, because a self-update can swap the binary a live session is
        // executing. The flag is root-level and documented with exactly this
        // launch shape. A managed launch also pins `[cli] auto_update = false`
        // in the overlay Vibex writes; this covers the external CLI, whose
        // config file Vibex does not own.
        &["grok", "--no-auto-update", "agent", "stdio"],
    )
    .with_compatible_version("1.0.8"),
    AcpAgentCatalogEntry::new(
        "hermes",
        "Hermes",
        "Nous Research self-improving AI agent",
        // PyPI stops at 0.19.0; upstream ships 0.22.x only as GitHub source
        // tags with no wheel or sdist, so the uvx channel cannot reach it. The
        // pin is explicit rather than a "latest" lookup that would keep
        // returning 0.19.0, and the managed install uses the same constant.
        // Moving to 0.22 needs a VCS/installer distribution (and a re-check of
        // its canonical session-MCP reattachment rule).
        "0.19.0",
        "https://hermes-agent.nousresearch.com/docs/user-guide/features/acp",
        &["hermes", "acp"],
    ),
    AcpAgentCatalogEntry::new(
        "junie",
        "Junie",
        "AI Coding Agent by JetBrains",
        "1468.30.0",
        "https://junie.jetbrains.com/docs/junie-cli-acp.html",
        &["junie", "--acp", "true"],
    ),
    AcpAgentCatalogEntry::new(
        "kilo",
        "Kilo",
        "The open source coding agent",
        "7.2.40",
        "https://kilo.ai/docs/code-with-ai/platforms/cli",
        &["kilo", "acp"],
    ),
    AcpAgentCatalogEntry::new(
        "kiro",
        "Kiro CLI",
        "Amazon's AI coding agent with native ACP support",
        ACP_AGENT_MANUAL_VERSION,
        "https://kiro.dev/docs/cli/acp/",
        &["kiro-cli", "acp"],
    ),
    AcpAgentCatalogEntry::new(
        "kimi",
        "Kimi Code CLI",
        "Moonshot AI's open-source terminal coding agent",
        // Moonshot archived the Python `kimi-cli` line: 1.51.0 marked it
        // end-of-life and 1.52.0 turned its entry point into a deprecation gate
        // that prints a migration notice and exits 0 without ever speaking ACP.
        // The successor is the npm package `@moonshot-ai/kimi-code`, which keeps
        // the `kimi acp` launch shape and the `config.toml` provider/model
        // tables but renames the state root to `KIMI_CODE_HOME` and respells two
        // provider types.
        //
        // The floor is the successor's own first verified release, `2.2.0`,
        // never the archived CLI. The projection writes the successor's
        // vocabulary, so 1.49.0 could not read what Vibex writes: a runtime
        // still on the old CLI collapses to the conservative surface instead,
        // which is the honest answer, and the managed install moves it forward.
        // 2.2.0 is additive over 2.1.1: the ACP handshake, config option ids
        // and the `config.toml` provider/model tables are unchanged, and the
        // release's only new launch surface is `KIMI_CODE_TRUST_WORKSPACE`,
        // which Vibex sets itself because it already owns the workspace.
        //
        // 2.5.2 still reads `KIMI_CODE_TRUST_WORKSPACE` (2.5.0 adds the
        // equivalent `[workspace] require_trust` key) and keeps the handshake
        // and config option ids. Plan and swarm modes now start only when the
        // user turns them on, and `[secondary_model] force` is ignored; the
        // projection writes neither. The floor is 2.2.0, the release the trust
        // variable first verified at, which still excludes the archived CLI.
        "2.5.2",
        "https://github.com/MoonshotAI/kimi-code",
        &["kimi", "acp"],
    )
    .with_preset_id("kimi-cli")
    .with_compatible_version("2.2.0"),
    AcpAgentCatalogEntry::new(
        "minion-code",
        "Minion Code",
        "An enhanced AI code assistant built on the Minion framework with rich development tools",
        "0.1.44",
        "https://github.com/femto/minion-code",
        &[
            "uvx",
            "--from",
            "minion-code==0.1.44",
            "minion-code",
            "acp",
        ],
    ),
    AcpAgentCatalogEntry::new(
        "mistral-vibe",
        "Mistral Vibe",
        "Mistral's open-source coding assistant",
        "2.9.3",
        "https://github.com/mistralai/mistral-vibe",
        &["vibe-acp"],
    ),
    AcpAgentCatalogEntry::new(
        "nova",
        "Nova",
        "Nova by Compass AI - a fully-fledged software engineer at your command",
        "1.1.18",
        "https://www.compassap.ai/portfolio/nova.html",
        &["npx", "-y", "@compass-ai/nova@1.1.18", "acp"],
    ),
    AcpAgentCatalogEntry::new(
        "poolside",
        "Poolside",
        "Poolside's coding agent",
        "1.0.0",
        "https://docs.poolside.ai/cli/pool",
        &["pool", "acp"],
    ),
    AcpAgentCatalogEntry::new(
        "pi",
        "Pi",
        "Pi coding agent connected through ACP",
        "0.0.34",
        "https://github.com/svkozak/pi-acp",
        &["npx", "-y", "pi-acp@0.0.34"],
    )
    .with_compatible_version("0.0.33"),
    AcpAgentCatalogEntry::new(
        "qoder",
        "Qoder CLI",
        "AI coding assistant with agentic capabilities",
        "1.0.24",
        "https://qoder.com",
        &["npx", "-y", "@qoder-ai/qodercli@1.0.24", "--acp"],
    ),
    AcpAgentCatalogEntry::new(
        "qwen-code",
        "Qwen Code",
        "Alibaba's Qwen coding assistant",
        "0.18.4",
        "https://qwenlm.github.io/qwen-code-docs/en/users/overview",
        &[
            "npx",
            "-y",
            "@qwen-code/qwen-code@0.18.4",
            "--acp",
            "--experimental-skills",
        ],
    ),
    AcpAgentCatalogEntry::new(
        "stakpak",
        "Stakpak",
        "Open-source DevOps agent in Rust with enterprise-grade security",
        "0.3.80",
        "https://stakpak.dev/",
        &["stakpak", "acp"],
    ),
    AcpAgentCatalogEntry::new(
        "vtcode",
        "VT Code",
        "An open-source coding agent with LLM-native code understanding and robust shell safety. Supports multiple LLM providers with automatic failover and efficient context management.",
        "0.96.14",
        "https://github.com/vinhnx/VTCode/blob/main/docs/guides/zed-acp.md",
        &["vtcode", "acp"],
    )
    .with_env(&[("VT_ACP_ENABLED", "1"), ("VT_ACP_ZED_ENABLED", "1")]),
];

pub fn acp_agent_catalog_entries() -> &'static [AcpAgentCatalogEntry] {
    ACP_AGENT_CATALOG
}

/// Catalog version marker for an Agent Vibex does not pin.
///
/// The Adapter ships its own installer, so Vibex has no verified version to
/// install and nothing to roll back to.
pub const ACP_AGENT_MANUAL_VERSION: &str = "manual";

/// `(agent_id, method_id)` pairs whose ACP `authenticate` call actually starts a
/// sign-in the host can hand to the user.
///
/// ACP's `Agent` method kind only means "the Agent handles authentication
/// itself". It does not promise that the call does anything: some Agents answer
/// it with an empty result and no side effect, expecting the credential to be
/// configured out of band. ACP advertises no capability that separates the two,
/// so the only reliable evidence is what the pinned binary does when asked.
///
/// Every pair here was verified by launching the Agent's pinned build, sending
/// `initialize` followed by `authenticate` for that method, and observing a
/// browser launch rather than an immediate empty `{}` result or an error:
///
/// | Agent | method | observed |
/// | --- | --- | --- |
/// | antigravity | `oauth-personal`, `oauth-business` | opens the Google OAuth consent page |
/// | cline | `cline`, `cline-pass` | opens the `authkit.cline.bot` device page |
/// | cline | `openai-codex` | opens the OpenAI OAuth consent page |
/// | codebuddy-code | `external`, `internal`, `iOA`, `selfhosted` | opens its product login page |
/// | cursor | `cursor_login` | opens `cursor.com/loginDeepControl` |
/// | devin | `devin-browser` | opens `app.devin.ai/auth/cli/continue` |
/// | gemini | `oauth-personal` | opens the Google OAuth consent page |
/// | grok | `grok.com` | opens `accounts.x.ai/oauth2/device` |
///
/// Pairs deliberately *absent* are the no-op cases, and the granularity matters:
/// inside the same Agent, Grok's `xai.api_key`, Gemini's `gemini-api-key`, Codex's
/// `api-key` and Cursor's API-key surface answer with an immediate error when the
/// variable is unset, while Gemini's `vertex-ai`/`gateway` return an empty `{}`;
/// none of them opens anything. `zcode`'s `zcode-credentials` reached
/// `-32601 Method not found` through 0.72.0; 0.82.x implements it as a
/// readiness check that answers `{}` when the desktop credential is present
/// and an error when it is not, so it still opens no browser. `glm-acp-agent`
/// documents an empty result.
///
/// This is deliberately a table and not a heuristic on the method name or kind:
/// an Agent that later changes behaviour must fail visibly here rather than
/// silently start presenting a no-op as a sign-in.
pub const ACP_AGENT_HOST_BROWSER_LOGIN_METHODS: &[(&str, &str)] = &[
    ("antigravity", "oauth-personal"),
    ("antigravity", "oauth-business"),
    ("cline", "cline"),
    ("cline", "cline-pass"),
    ("cline", "openai-codex"),
    ("codebuddy-code", "external"),
    ("codebuddy-code", "internal"),
    ("codebuddy-code", "iOA"),
    ("codebuddy-code", "selfhosted"),
    ("cursor", "cursor_login"),
    ("devin", "devin-browser"),
    ("gemini", "oauth-personal"),
    ("grok", "grok.com"),
];

/// Whether this exact `(agent, method)` sign-in opens a browser the host can
/// hand to the user. See [`ACP_AGENT_HOST_BROWSER_LOGIN_METHODS`].
pub fn acp_agent_method_opens_host_browser(agent_id: &str, method_id: &str) -> bool {
    ACP_AGENT_HOST_BROWSER_LOGIN_METHODS
        .iter()
        .any(|(agent, method)| *agent == agent_id && *method == method_id)
}

/// The Adapter version Vibex verified for an Agent — its catalog pin.
///
/// `None` when the Agent is not catalog-managed or its catalog entry declares
/// no verified version. Callers that can act on the answer must also check
/// that the Agent's distribution installs an exact version at all.
pub fn acp_agent_verified_version(agent_id: &str) -> Option<&'static str> {
    acp_agent_catalog_entries()
        .iter()
        .find(|entry| entry.id == agent_id)
        .map(|entry| entry.version)
        .filter(|version| *version != ACP_AGENT_MANUAL_VERSION)
}

/// The DeepSeek Harness bridge release whose bundled runtime substitutes a
/// Model missing from the projected route instead of sending the id verbatim.
///
/// 0.4.35 (runtime `0.1.7-rc.2`) resolves the requested Model and fails the
/// request when the route cannot serve it; 0.4.36 (runtime `0.2.0-rc.2`) picks
/// the removed-id successor or the route's first Model and reports the
/// substitute through its config options. A caller that reads a different
/// Model back than it requested must therefore treat that as expected on
/// 0.4.36+ and as an unexplained mismatch before it.
pub const DEEPSEEK_HARNESS_MODEL_SUBSTITUTION_SINCE: &str = "0.4.36";

/// The DeepSeek Harness bridge release that bundles `mistral-conversations`
/// among the wire protocols its pi-ai gate accepts.
///
/// 0.4.35's runtime refuses an `api` spelling outside its compat gate, so a
/// route projected with this protocol must fall back to a supported spelling
/// (or refuse the projection) on older installs.
pub const DEEPSEEK_HARNESS_MISTRAL_WIRE_SINCE: &str = "0.4.36";

/// The DeepSeek Harness bridge release that advertises ACP `session/fork`,
/// including the inclusive `jetbrains.air.fork` cut. 0.4.35 advertises only
/// `session/list` and `session/resume`.
pub const DEEPSEEK_HARNESS_SESSION_FORK_SINCE: &str = "0.4.37";

/// The installed release of an Agent, when its Adapter is the Agent itself.
///
/// DeepSeek Harness ships its ACP bridge as the Agent, so a managed install
/// records the release in `agent_version` while `adapter_version` stays empty.
/// An explicit Adapter version wins when both are present.
pub fn installed_adapter_version<'a>(
    adapter_version: Option<&'a str>,
    agent_version: Option<&'a str>,
) -> Option<&'a str> {
    let usable = |value: Option<&'a str>| value.map(str::trim).filter(|value| !value.is_empty());
    usable(adapter_version).or_else(|| usable(agent_version))
}

/// Whether an installed Agent release is at least `minimum`.
///
/// `None` (an unknown or manually installed Agent) is never at least anything:
/// a release-gated capability must be proven by a version Vibex can read. A
/// malformed version is likewise refused rather than assumed current.
pub fn adapter_version_at_least(version: Option<&str>, minimum: &str) -> bool {
    let Some(version) = version.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let (Ok(version), Ok(minimum)) = (
        semver::Version::parse(version),
        semver::Version::parse(minimum),
    ) else {
        return false;
    };
    version >= minimum
}

/// Whether one declared Model interface is available on an exact Agent release.
///
/// Interfaces that a newer bridge introduced must not be offered to — or
/// projected onto — an older installation: the bundled runtime validates the
/// complete route, so an unknown `api` spelling makes it refuse the route
/// rather than ignore the field.
pub fn model_interface_available_for_adapter(
    agent_id: &str,
    wire_protocol_id: &str,
    adapter_version: Option<&str>,
    agent_version: Option<&str>,
) -> bool {
    match (agent_id, wire_protocol_id) {
        ("deepseek-harness", crate::WIRE_PROTOCOL_MISTRAL_CONVERSATIONS) => {
            adapter_version_at_least(
                installed_adapter_version(adapter_version, agent_version),
                DEEPSEEK_HARNESS_MISTRAL_WIRE_SINCE,
            )
        }
        _ => true,
    }
}

/// Whether an Agent release substitutes a Model that the projected route no
/// longer lists.
pub fn adapter_substitutes_unavailable_models(
    agent_id: &str,
    adapter_version: Option<&str>,
    agent_version: Option<&str>,
) -> bool {
    agent_id == "deepseek-harness"
        && adapter_version_at_least(
            installed_adapter_version(adapter_version, agent_version),
            DEEPSEEK_HARNESS_MODEL_SUBSTITUTION_SINCE,
        )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn catalog_ids_and_presets_are_unique_and_commands_are_complete() {
        let entries = acp_agent_catalog_entries();
        assert_eq!(entries.len(), 33);

        let ids = entries
            .iter()
            .map(|entry| entry.id)
            .collect::<BTreeSet<_>>();
        let presets = entries
            .iter()
            .map(|entry| entry.preset_id)
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), entries.len());
        assert_eq!(presets.len(), entries.len());
        assert!(entries.iter().all(|entry| !entry.command.is_empty()));
        let antigravity = entries
            .iter()
            .find(|entry| entry.id == "antigravity")
            .unwrap();
        assert_eq!(antigravity.version, "1.4.5");
        assert_eq!(antigravity.command, &["agy_acp_server"]);
        assert_eq!(antigravity.compatible_version, Some("1.0.0"));
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.id == "auggie")
                .unwrap()
                .label,
            "Augment CLI"
        );
        let pi = entries.iter().find(|entry| entry.id == "pi").unwrap();
        assert_eq!(pi.version, "0.0.34");
        assert_eq!(pi.command, &["npx", "-y", "pi-acp@0.0.34"]);
        assert_eq!(pi.compatible_version, Some("0.0.33"));
        let deepseek = entries
            .iter()
            .find(|entry| entry.id == "deepseek-harness")
            .unwrap();
        assert_eq!(deepseek.version, "0.4.40");
        assert_eq!(deepseek.compatible_version, Some("0.4.32"));
        assert_eq!(
            deepseek.command,
            &["npx", "-y", "@openma/deepseek-harness-acp@0.4.40"]
        );
        assert!(!entries.iter().any(|entry| entry.id == "corust-agent"));
    }

    #[test]
    fn verified_versions_skip_agents_vibex_does_not_pin() {
        assert_eq!(
            acp_agent_verified_version("deepseek-harness"),
            Some("0.4.40")
        );
        assert_eq!(acp_agent_verified_version("gemini"), Some("0.67.0"));
        assert_eq!(acp_agent_verified_version("devin"), None);
        assert_eq!(acp_agent_verified_version("kiro"), None);
        assert_eq!(acp_agent_verified_version("not-a-catalog-agent"), None);
    }

    /// Moving a pin forward must not move the compatibility floor with it.
    ///
    /// The floor is a separate statement from the pin: raising it would turn an
    /// already-installed older runtime into a conservative one, hiding the
    /// provider editor and failing `session/new` with
    /// `agent_projection_version_mismatch` for a release the projection still
    /// supports. Every Agent bumped here therefore keeps its previous release as
    /// the floor.
    #[test]
    fn moved_pins_keep_their_compatibility_floor() {
        for (agent_id, pin, floor) in [
            ("antigravity", "1.4.5", "1.0.0"),
            ("cline", "3.0.74", "3.0.29"),
            ("codebuddy-code", "2.181.0", "2.109.0"),
            ("copilot", "1.0.107", "1.0.78"),
            ("deepseek-harness", "0.4.40", "0.4.32"),
            ("gemini", "0.67.0", "0.47.0"),
            ("grok", "1.0.78", "1.0.8"),
            ("kimi", "2.5.2", "2.2.0"),
            ("pi", "0.0.34", "0.0.33"),
        ] {
            let entry = acp_agent_catalog_entries()
                .iter()
                .find(|entry| entry.id == agent_id)
                .unwrap_or_else(|| panic!("{agent_id} is a catalog Agent"));
            assert_eq!(entry.version, pin, "{agent_id} pin");
            assert_eq!(entry.compatible_version, Some(floor), "{agent_id} floor");
            assert_eq!(acp_agent_verified_version(agent_id), Some(pin));
        }
    }

    /// A pinned distribution spec and the entry version move together: the
    /// parser, the install fingerprint, and the version the record reports all
    /// read both. Rewriting only one produces an entry the installer rejects as
    /// `agent_npm_spec_invalid` / `agent_uvx_spec_not_exact`.
    #[test]
    fn pinned_distribution_specs_carry_the_entry_version() {
        for entry in acp_agent_catalog_entries() {
            if !matches!(entry.command.first().copied(), Some("npx" | "uvx")) {
                continue;
            }
            let spec = entry
                .command
                .get(2)
                .unwrap_or_else(|| panic!("{} declares an exact spec", entry.id));
            assert!(
                spec.ends_with(&format!("@{}", entry.version))
                    || spec.ends_with(&format!("=={}", entry.version)),
                "{} pins {spec}, which does not carry version {}",
                entry.id,
                entry.version
            );
        }
    }

    /// DeepSeek Harness behavior changes travel with the bundled Harness
    /// runtime, so each release gate must sit on the bridge version that first
    /// shipped it and must refuse an unknown or malformed release instead of
    /// assuming the newest behavior.
    #[test]
    fn deepseek_harness_release_gates_follow_the_bundled_runtime() {
        // 0.4.35 is the last release on runtime 0.1.7-rc.2: the bridge sends
        // the requested Model verbatim, refuses a `mistral-conversations`
        // route, and advertises no `session/fork`.
        assert!(!adapter_substitutes_unavailable_models(
            "deepseek-harness",
            None,
            Some("0.4.35")
        ));
        assert!(!model_interface_available_for_adapter(
            "deepseek-harness",
            crate::WIRE_PROTOCOL_MISTRAL_CONVERSATIONS,
            None,
            Some("0.4.35")
        ));
        assert!(!adapter_version_at_least(
            Some("0.4.35"),
            DEEPSEEK_HARNESS_SESSION_FORK_SINCE
        ));

        // 0.4.36 moved to runtime 0.2.0-rc.2: substitution and the Mistral
        // protocol arrive, fork does not.
        assert!(adapter_substitutes_unavailable_models(
            "deepseek-harness",
            None,
            Some("0.4.36")
        ));
        assert!(model_interface_available_for_adapter(
            "deepseek-harness",
            crate::WIRE_PROTOCOL_MISTRAL_CONVERSATIONS,
            None,
            Some("0.4.36")
        ));
        assert!(!adapter_version_at_least(
            Some("0.4.36"),
            DEEPSEEK_HARNESS_SESSION_FORK_SINCE
        ));

        // 0.4.37 adds the negotiated fork surface.
        assert!(adapter_version_at_least(
            Some("0.4.37"),
            DEEPSEEK_HARNESS_SESSION_FORK_SINCE
        ));
        // An explicit Adapter version wins over the Agent version, which is
        // how a manually installed bridge under a newer Agent is read.
        assert!(adapter_substitutes_unavailable_models(
            "deepseek-harness",
            Some("0.4.37"),
            Some("0.4.35")
        ));
        assert!(!adapter_substitutes_unavailable_models(
            "deepseek-harness",
            Some("0.4.35"),
            Some("0.4.37")
        ));

        // A release Vibex cannot read proves nothing, and neither does another
        // Agent's version.
        assert!(!adapter_version_at_least(None, "0.4.36"));
        assert!(!adapter_version_at_least(Some("  "), "0.4.36"));
        assert!(!adapter_version_at_least(Some("not-a-version"), "0.4.36"));
        assert!(!adapter_substitutes_unavailable_models(
            "claude",
            None,
            Some("9.9.9")
        ));
        assert!(model_interface_available_for_adapter(
            "claude",
            crate::WIRE_PROTOCOL_MISTRAL_CONVERSATIONS,
            None,
            None
        ));

        assert_eq!(
            installed_adapter_version(None, Some("0.4.37")),
            Some("0.4.37")
        );
        assert_eq!(
            installed_adapter_version(Some("0.4.36"), Some("0.4.37")),
            Some("0.4.36")
        );
        assert_eq!(installed_adapter_version(None, None), None);
    }
}

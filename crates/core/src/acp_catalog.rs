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
        "1.2.1",
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
        "3.0.29",
        "https://cline.bot/cli",
        &["npx", "-y", "cline@3.0.29", "--acp"],
    ),
    AcpAgentCatalogEntry::new(
        "copilot",
        "GitHub Copilot",
        "GitHub Copilot CLI agent connected through ACP",
        "1.0.89",
        "https://docs.github.com/en/copilot/concepts/agents/about-copilot-cli",
        &["copilot", "--acp"],
    )
    .with_compatible_version("1.0.78"),
    AcpAgentCatalogEntry::new(
        "codebuddy-code",
        "Codebuddy",
        "Tencent Cloud's official intelligent coding tool",
        "2.160.0",
        "https://www.codebuddy.cn/cli/",
        &[
            "npx",
            "-y",
            "@tencent-ai/codebuddy-code@2.160.0",
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
        // The ACP Registry advertises `2026.09.26`, but Cursor publishes no
        // CLI release notes for September and its own installer and Homebrew
        // cask both resolve to `2026.09.28-64d2043` — a version string whose
        // real shape carries a commit suffix this catalog does not model. The
        // pin stays on the last release with published notes and a resolvable
        // artifact rather than claiming a version nobody can point at.
        "2026.03.30",
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
        // 0.4.35 keeps reading `$DSH_HOME/settings.yaml` for its standalone
        // default model, but its bundled runtime moved from `0.1.5` to
        // `0.1.7`, which dropped `dsh-settings-file` and no longer derives llm
        // routes from that file. Vibex therefore registers the projected route
        // through the `$DSH_HOME/cordis.patch.yml` home patch the bridge
        // composes on every launch, and keeps writing `settings.yaml` beside it
        // so 0.4.32/0.4.33 (runtime `0.1.5`) project the same route. The
        // bundled runtime still recognises every `api` spelling the projection
        // writes (`openai-completions`, `openai-responses`,
        // `anthropic-messages`). The release's moves to the Messages API and
        // its shrunken default catalogue apply to the official
        // `deepseek-official` route, which the projected route does not use.
        "0.4.35",
        "https://github.com/openma-ai/deepseek-harness-acp",
        &[
            "npx",
            "-y",
            "@openma/deepseek-harness-acp@0.4.35",
        ],
    )
    // 0.4.33 qualifies every ACP model option id as `route::model`; Vibex keeps
    // the bare id on the wire and reads the qualified spelling back as an alias
    // so 0.4.32 keeps working. The projection floor therefore stays on 0.4.32
    // even though the pin moved past it; that qualification is byte-identical in
    // 0.4.35, so the floor still describes a release the alias covers.
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
        "0.62.0",
        "https://geminicli.com",
        &["npx", "-y", "@google/gemini-cli@0.62.0", "--acp"],
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
        // The registry's newest grok build is 1.0.45, which xAI publishes only
        // on its alpha and enterprise channels; `https://x.ai/cli/stable` and
        // the public changelog both stop at 1.0.44. Pin the release with a
        // published channel and release notes instead of the unreviewed build.
        "1.0.44",
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
        // No compatibility floor is declared, and that is deliberate. The
        // projection now writes the successor's vocabulary, so the archived CLI
        // could not read what Vibex writes: leaving 1.49.0 as the floor would
        // advertise a credential and model surface that does not work. A runtime
        // still on the old CLI collapses to the conservative surface instead,
        // which is the honest answer, and the managed install moves it forward.
        "2.1.1",
        "https://github.com/MoonshotAI/kimi-code",
        &["kimi", "acp"],
    )
    .with_preset_id("kimi-cli"),
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
        assert_eq!(antigravity.version, "1.2.1");
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
        assert_eq!(deepseek.version, "0.4.35");
        assert_eq!(deepseek.compatible_version, Some("0.4.32"));
        assert_eq!(
            deepseek.command,
            &["npx", "-y", "@openma/deepseek-harness-acp@0.4.35"]
        );
        assert!(!entries.iter().any(|entry| entry.id == "corust-agent"));
    }

    #[test]
    fn verified_versions_skip_agents_vibex_does_not_pin() {
        assert_eq!(
            acp_agent_verified_version("deepseek-harness"),
            Some("0.4.35")
        );
        assert_eq!(acp_agent_verified_version("gemini"), Some("0.62.0"));
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
            ("antigravity", "1.2.1", "1.0.0"),
            ("codebuddy-code", "2.160.0", "2.109.0"),
            ("copilot", "1.0.89", "1.0.78"),
            ("gemini", "0.62.0", "0.47.0"),
            ("grok", "1.0.44", "1.0.8"),
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
}

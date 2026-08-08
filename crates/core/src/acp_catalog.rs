#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpAgentCatalogEntry {
    pub id: &'static str,
    pub preset_id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub version: &'static str,
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
            install_url,
            command,
            env: &[],
            supports_mcp_servers: None,
        }
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
        "agoragentic-acp",
        "Agoragentic",
        "Agent marketplace with 174+ AI capabilities. Browse, invoke, and pay for agent services settled in USDC on Base L2.",
        "1.3.3",
        "https://agoragentic.com",
        &["npx", "-y", "agoragentic-mcp@1.3.3", "--acp"],
    ),
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
        "autohand",
        "Autohand Code",
        "Autohand Code - AI coding agent powered by Autohand AI",
        "0.2.1",
        "https://www.autohand.ai/cli/",
        &["npx", "-y", "@autohandai/autohand-acp@0.2.1"],
    ),
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
        "1.0.78",
        "https://docs.github.com/en/copilot/concepts/agents/about-copilot-cli",
        &["copilot", "--acp"],
    ),
    AcpAgentCatalogEntry::new(
        "codebuddy-code",
        "Codebuddy Code",
        "Tencent Cloud's official intelligent coding tool",
        "2.109.0",
        "https://www.codebuddy.cn/cli/",
        &[
            "npx",
            "-y",
            "@tencent-ai/codebuddy-code@2.109.0",
            "--acp",
        ],
    ),
    AcpAgentCatalogEntry::new(
        "codewhale",
        "CodeWhale",
        "Terminal coding agent for DeepSeek V4 and open models",
        "0.8.55",
        "https://codewhale.net/",
        &["codewhale", "serve", "--acp"],
    ),
    AcpAgentCatalogEntry::new(
        "cortex-code",
        "Cortex Code",
        "Snowflake's Cortex Code coding agent",
        "1.0.73",
        "https://docs.snowflake.com/en/user-guide/cortex-code/cortex-code-cli",
        &["cortex", "acp", "serve"],
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
        "devin",
        "Devin CLI",
        "Cognition's Devin for Terminal via Agent Client Protocol",
        "manual",
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
        "fast-agent",
        "fast-agent",
        "Code and build agents with comprehensive multi-provider support",
        "0.7.21",
        "https://fast-agent.ai/acp/",
        &[
            "uvx",
            "--from",
            "fast-agent-acp==0.7.21",
            "fast-agent-acp",
            "-x",
        ],
    ),
    AcpAgentCatalogEntry::new(
        "gemini",
        "Gemini CLI",
        "Google's official CLI for Gemini",
        "0.47.0",
        "https://geminicli.com",
        &["npx", "-y", "@google/gemini-cli@0.47.0", "--acp"],
    ),
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
        "Grok",
        "xAI's Grok Build agentic coding CLI with plan mode and parallel subagents. Requires a SuperGrok or X Premium+ subscription.",
        "0.2.11",
        "https://docs.x.ai/build/overview",
        &["grok", "agent", "stdio"],
    ),
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
        "manual",
        "https://kiro.dev/docs/cli/acp/",
        &["kiro-cli", "acp"],
    ),
    AcpAgentCatalogEntry::new(
        "kimi",
        "Kimi Code CLI",
        "Moonshot AI's open-source terminal coding agent",
        "0.11.0",
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
        "0.0.33",
        "https://github.com/svkozak/pi-acp",
        &["npx", "-y", "pi-acp@0.0.33"],
    ),
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
        "sigit",
        "siGit Code",
        "Local-first coding agent. Runs entirely on your machine with optional on-device LLM inference via Onde.",
        "1.0.3",
        "https://github.com/getsigit/sigit",
        &["sigit"],
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn catalog_ids_and_presets_are_unique_and_commands_are_complete() {
        let entries = acp_agent_catalog_entries();
        assert_eq!(entries.len(), 36);

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
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.id == "auggie")
                .unwrap()
                .label,
            "Augment CLI"
        );
        let pi = entries.iter().find(|entry| entry.id == "pi").unwrap();
        assert_eq!(pi.version, "0.0.33");
        assert_eq!(pi.command, &["npx", "-y", "pi-acp@0.0.33"]);
        assert!(!entries.iter().any(|entry| entry.id == "corust-agent"));
    }
}

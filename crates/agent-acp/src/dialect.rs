//! Per-agent ACP dialect knowledge for agents without a managed adapter
//! descriptor.
//!
//! [`AcpAgentCompatibility`](crate::registry::AcpAgentCompatibility) models a
//! *managed* adapter: it pins an exact npm package, an integrity hash and a
//! compatibility identity, which only Claude and Codex have. The catalog ships
//! 30+ further agents that launch from `PATH` or `npx`, so they can never carry
//! that descriptor — yet each still has real wire behavior the runtime must
//! respect (credential scrubbing, launch flags, whether MCP forwarding reaches
//! the model at all, which private requests block the agent).
//!
//! This module is where that knowledge lives. Every field here has a live
//! consumer in `runtime.rs`; a profile is never decoration:
//!
//! | field | consumer |
//! | --- | --- |
//! | `credential_env_keys_to_unset` | `AcpRuntimeClient::agent_account_env_unsets` |
//! | `required_launch_env` | process spawn |
//! | `launch_args` | process spawn |
//! | `mcp_wire_delivery` | `AcpProcess::wire_mcp_servers` |
//! | `event_enricher` | `AcpRuntimeClient::effective_adapter_identity` |
//! | `host_request_dialects` | inbound request dispatch |
//! | `restore_policy` | `AcpRuntimeClient::restore_policy_for_agent` |
//!
//! A descriptor always wins over a profile: managed adapters are exact-version
//! contracts, profiles are best-effort family knowledge.

use crate::registry::{AgentEventEnricherKind, RestorePolicy};

/// How much is actually known about an agent's ACP behavior. Product surfaces
/// must not imply that a `Generic` agent is supported to the same degree as a
/// descriptor-backed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentSupportTier {
    /// Exact-version managed adapter with an integrity-checked install, a
    /// compatibility identity and bridge-contract evidence.
    DescriptorBacked,
    /// No managed install, but the agent's dialect is modeled here.
    DialectProfiled,
    /// Generic ACP happy path only. Capability comes from runtime probing.
    Generic,
}

/// Whether MCP servers forwarded over `session/new.mcpServers` actually reach
/// the agent's model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpWireDelivery {
    /// Normal case: forwarded servers reach the model.
    Delivered,
    /// The agent reads its own MCP configuration file at launch. Forwarding
    /// the same servers over the wire would double-register them.
    NativeConfig,
    /// The agent accepts the `mcpServers` field but never forwards it to the
    /// model. Sending entries is futile and makes tools look available that
    /// the agent can never call.
    AcceptedButDropped,
    /// Any entry makes `session/new` fail. The field itself is tolerated as an
    /// empty array.
    Rejected,
}

impl McpWireDelivery {
    /// Only `Delivered` agents may receive entries; every other tier keeps the
    /// array empty for a different reason.
    pub fn forwards_servers(self) -> bool {
        matches!(self, Self::Delivered)
    }
}

/// Where a dialect launch flag belongs relative to the configured arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchArgPlacement {
    /// Before the configured arguments. Required by CLIs whose root parser
    /// owns the flag and whose subcommand rejects it (`grok --no-auto-update
    /// agent stdio`).
    Leading,
    /// After the configured arguments.
    Trailing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialectLaunchArg {
    pub value: &'static str,
    pub placement: LaunchArgPlacement,
}

const fn leading(value: &'static str) -> DialectLaunchArg {
    DialectLaunchArg {
        value,
        placement: LaunchArgPlacement::Leading,
    }
}

const fn trailing(value: &'static str) -> DialectLaunchArg {
    DialectLaunchArg {
        value,
        placement: LaunchArgPlacement::Trailing,
    }
}

/// A private request an agent issues and **blocks on**. Replying
/// `method-not-found` does not degrade gracefully here: the agent's own tool
/// call fails outright, so each dialect needs a real bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentHostRequestDialect {
    /// `_x.ai/ask_user_question` — Grok's native question tool. Blocks until
    /// the host answers with `{outcome, answers, partial_answers}`.
    GrokAskUserQuestion,
    /// `_x.ai/exit_plan_mode` — Grok's plan approval. Blocks until the host
    /// answers with `{outcome, feedback}`; the agent stays in plan mode until
    /// then.
    GrokExitPlanMode,
}

impl AgentHostRequestDialect {
    pub fn method(self) -> &'static str {
        match self {
            Self::GrokAskUserQuestion => "_x.ai/ask_user_question",
            Self::GrokExitPlanMode => "_x.ai/exit_plan_mode",
        }
    }
}

/// Dialect knowledge for one agent id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentDialectProfile {
    pub agent_id: &'static str,
    pub support_tier: AgentSupportTier,
    /// Cleared only on the agent-account launch path, which is exactly the
    /// "the user signed in through the agent's own CLI" case: an inherited
    /// `XAI_API_KEY` from a dev shell would otherwise be validated by the CLI
    /// and beat the browser-login credential. A provider profile declares its
    /// credentials explicitly and stays authoritative.
    pub credential_env_keys_to_unset: &'static [&'static str],
    pub required_launch_env: &'static [(&'static str, &'static str)],
    pub launch_args: &'static [DialectLaunchArg],
    pub mcp_wire_delivery: McpWireDelivery,
    pub event_enricher: AgentEventEnricherKind,
    pub host_request_dialects: &'static [AgentHostRequestDialect],
    pub restore_policy: RestorePolicy,
    /// Why this agent deviates from the generic path. Recorded for
    /// diagnostics and to keep the table auditable.
    pub rationale: &'static str,
}

impl AgentDialectProfile {
    const fn generic(agent_id: &'static str) -> Self {
        Self {
            agent_id,
            support_tier: AgentSupportTier::Generic,
            credential_env_keys_to_unset: &[],
            required_launch_env: &[],
            launch_args: &[],
            mcp_wire_delivery: McpWireDelivery::Delivered,
            event_enricher: AgentEventEnricherKind::Passthrough,
            host_request_dialects: &[],
            restore_policy: RestorePolicy::ResumeThenLoadThenNew,
            rationale: "generic ACP behavior only; capability comes from runtime probing",
        }
    }

    const fn profiled(mut self, rationale: &'static str) -> Self {
        self.support_tier = AgentSupportTier::DialectProfiled;
        self.rationale = rationale;
        self
    }

    const fn with_credential_scrub(mut self, keys: &'static [&'static str]) -> Self {
        self.credential_env_keys_to_unset = keys;
        self
    }

    const fn with_required_env(mut self, env: &'static [(&'static str, &'static str)]) -> Self {
        self.required_launch_env = env;
        self
    }

    const fn with_launch_args(mut self, args: &'static [DialectLaunchArg]) -> Self {
        self.launch_args = args;
        self
    }

    const fn with_mcp_delivery(mut self, delivery: McpWireDelivery) -> Self {
        self.mcp_wire_delivery = delivery;
        self
    }

    const fn with_enricher(mut self, enricher: AgentEventEnricherKind) -> Self {
        self.event_enricher = enricher;
        self
    }

    const fn with_host_request_dialects(
        mut self,
        dialects: &'static [AgentHostRequestDialect],
    ) -> Self {
        self.host_request_dialects = dialects;
        self
    }

    pub fn handles_host_request(&self, method: &str) -> Option<AgentHostRequestDialect> {
        let mut index = 0;
        while index < self.host_request_dialects.len() {
            let dialect = self.host_request_dialects[index];
            if dialect.method() == method {
                return Some(dialect);
            }
            index += 1;
        }
        None
    }
}

const GROK_LAUNCH_ARGS: &[DialectLaunchArg] = &[leading("--no-auto-update")];
const GEMINI_LAUNCH_ARGS: &[DialectLaunchArg] = &[trailing("--skip-trust")];

const AGENT_DIALECT_PROFILES: &[AgentDialectProfile] = &[
    // Grok reads `~/.grok/config.toml` (`[mcp_servers.<name>]`) at launch, so
    // wire forwarding would double-register the same servers. Its root parser
    // owns `--no-auto-update`; the `agent stdio` subcommand rejects it, hence
    // the leading placement. Vibex pins the catalog version, so letting the
    // CLI self-update would drift the process off that pin.
    AgentDialectProfile::generic("grok")
        .profiled("native MCP config, root-level launch flags and blocking `_x.ai` host requests")
        .with_credential_scrub(&["XAI_API_KEY"])
        .with_launch_args(GROK_LAUNCH_ARGS)
        .with_mcp_delivery(McpWireDelivery::NativeConfig)
        .with_enricher(AgentEventEnricherKind::Grok)
        .with_host_request_dialects(&[
            AgentHostRequestDialect::GrokAskUserQuestion,
            AgentHostRequestDialect::GrokExitPlanMode,
        ]),
    // Copilot CLI activates BYOK purely from `COPILOT_PROVIDER_BASE_URL`
    // (`copilot help providers`): when that variable is present the CLI stops
    // using the GitHub login entirely. An inherited value from a dev shell
    // would therefore silently redirect an agent-account session to a foreign
    // endpoint, so the whole `COPILOT_PROVIDER_*` credential surface is cleared
    // on that path. A provider profile sets these keys explicitly and stays
    // authoritative.
    AgentDialectProfile::generic("copilot")
        .profiled("`COPILOT_PROVIDER_BASE_URL` alone switches the CLI off the GitHub login")
        .with_credential_scrub(&[
            "COPILOT_PROVIDER_BASE_URL",
            "COPILOT_PROVIDER_API_KEY",
            "COPILOT_PROVIDER_BEARER_TOKEN",
            "COPILOT_PROVIDER_TYPE",
            "COPILOT_PROVIDER_WIRE_API",
            "COPILOT_PROVIDER_HEADERS",
            "COPILOT_PROVIDER_MODEL_ID",
            "COPILOT_PROVIDER_WIRE_MODEL",
            "COPILOT_MODEL",
        ]),
    // cursor-agent reads `~/.cursor/mcp.json`, shared with the IDE. A stale
    // `CURSOR_API_KEY` makes the CLI validate that key instead of falling back
    // to the browser login credential.
    AgentDialectProfile::generic("cursor")
        .profiled("native MCP config shared with the IDE; API-key env beats the browser login")
        .with_credential_scrub(&["CURSOR_API_KEY", "CURSOR_API_BASE_URL"])
        .with_mcp_delivery(McpWireDelivery::NativeConfig),
    // pi-acp accepts `mcpServers` and drops it: it never forwards MCP to the
    // inner `pi --mode rpc` process, and pi has no native MCP either.
    // Workspace trust gates config/skill loading only, never execution.
    AgentDialectProfile::generic("pi")
        .profiled("accepts `mcpServers` but never forwards it to the inner pi process")
        .with_required_env(&[("PI_ACP_TRUST_WORKSPACE", "1")])
        .with_mcp_delivery(McpWireDelivery::AcceptedButDropped),
    // Hermes registers `~/.hermes/config.yaml` `mcp_servers` as `mcp-<name>`
    // toolsets at launch.
    AgentDialectProfile::generic("hermes")
        .profiled("registers MCP toolsets from ~/.hermes/config.yaml at launch")
        .with_mcp_delivery(McpWireDelivery::NativeConfig),
    // Kimi Code reads `~/.kimi-code/mcp.json`.
    AgentDialectProfile::generic("kimi")
        .profiled("reads MCP servers from ~/.kimi-code/mcp.json at launch")
        .with_mcp_delivery(McpWireDelivery::NativeConfig),
    // CodeBuddy virtualizes MCP tool calls behind `DeferExecuteTool`, so the
    // live tool call reports the wrapper instead of the real `mcp__…` tool.
    AgentDialectProfile::generic("codebuddy-code")
        .profiled("wraps MCP tool calls in DeferExecuteTool and re-serializes their results")
        .with_enricher(AgentEventEnricherKind::CodeBuddy),
    // gemini-cli prompts for workspace trust on stdin, which no ACP client can
    // answer; without the flag the handshake stalls until the timeout.
    AgentDialectProfile::generic("gemini")
        .profiled("interactive workspace-trust prompt blocks the ACP handshake")
        .with_credential_scrub(&["GEMINI_API_KEY", "GOOGLE_API_KEY"])
        .with_launch_args(GEMINI_LAUNCH_ARGS),
    // Factory Droid rejects forwarded MCP entries; the catalog already opts
    // out of the preset feature, and this keeps the wire empty on every path.
    AgentDialectProfile::generic("factory-droid")
        .profiled("rejects forwarded MCP server entries")
        .with_mcp_delivery(McpWireDelivery::Rejected),
    // OpenCode carries substantial runtime handling (wire-API projection,
    // stream-error classification, prompt correlation) but no managed
    // descriptor; record the tier so coverage reporting is honest. Its restore
    // and MCP behavior are deliberately left at the generic defaults — the
    // runtime negotiates both, and guessing here would override live evidence.
    AgentDialectProfile::generic("opencode")
        .profiled("dedicated runtime handling for wire APIs and stream-error recovery"),
];

/// Dialect profile for `agent_id`, or the generic fallback.
pub fn agent_dialect_profile(agent_id: &str) -> AgentDialectProfile {
    AGENT_DIALECT_PROFILES
        .iter()
        .find(|profile| profile.agent_id == agent_id)
        .copied()
        .unwrap_or(AgentDialectProfile::generic("generic"))
}

/// Every profiled agent, for coverage reporting and tests.
pub fn agent_dialect_profiles() -> &'static [AgentDialectProfile] {
    AGENT_DIALECT_PROFILES
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn profiles_are_unique_and_only_registered_for_catalog_or_builtin_agents() {
        let mut ids = BTreeSet::new();
        for profile in agent_dialect_profiles() {
            assert!(
                ids.insert(profile.agent_id),
                "duplicate dialect profile for {}",
                profile.agent_id
            );
            assert_ne!(
                profile.support_tier,
                AgentSupportTier::Generic,
                "{} is registered but still marked generic",
                profile.agent_id
            );
            assert!(
                !profile.rationale.is_empty(),
                "{} must record why it deviates",
                profile.agent_id
            );
        }

        let known: BTreeSet<&str> = vibex_core::acp_agent_catalog_entries()
            .iter()
            .map(|entry| entry.id)
            .chain(["claude", "codex", "opencode"])
            .collect();
        for profile in agent_dialect_profiles() {
            assert!(
                known.contains(profile.agent_id),
                "{} has a dialect profile but is not a selectable agent",
                profile.agent_id
            );
        }
    }

    #[test]
    fn unknown_agents_fall_back_to_the_generic_profile() {
        let profile = agent_dialect_profile("not-a-real-agent");
        assert_eq!(profile.support_tier, AgentSupportTier::Generic);
        assert!(profile.mcp_wire_delivery.forwards_servers());
        assert!(profile.credential_env_keys_to_unset.is_empty());
        assert!(profile.launch_args.is_empty());
        assert!(profile.host_request_dialects.is_empty());
        assert_eq!(profile.event_enricher, AgentEventEnricherKind::Passthrough);
    }

    #[test]
    fn native_config_and_dropping_agents_never_forward_wire_servers() {
        for agent_id in ["grok", "cursor", "hermes", "kimi", "pi", "factory-droid"] {
            assert!(
                !agent_dialect_profile(agent_id)
                    .mcp_wire_delivery
                    .forwards_servers(),
                "{agent_id} must not receive forwarded MCP servers"
            );
        }
        assert!(
            agent_dialect_profile("gemini")
                .mcp_wire_delivery
                .forwards_servers()
        );
    }

    /// BYOK activates from the presence of `COPILOT_PROVIDER_BASE_URL` alone,
    /// so an inherited dev-shell value would redirect an agent-account session
    /// to a foreign endpoint without any visible signal.
    #[test]
    fn copilot_scrubs_the_whole_byok_env_surface_on_the_agent_account_path() {
        let copilot = agent_dialect_profile("copilot");
        assert_eq!(copilot.support_tier, AgentSupportTier::DialectProfiled);
        for key in [
            "COPILOT_PROVIDER_BASE_URL",
            "COPILOT_PROVIDER_API_KEY",
            "COPILOT_PROVIDER_BEARER_TOKEN",
            "COPILOT_MODEL",
        ] {
            assert!(
                copilot.credential_env_keys_to_unset.contains(&key),
                "{key} must be cleared before an agent-account launch"
            );
        }
        // Copilot has no launch-flag or MCP deviation; the catalog command
        // already carries `--acp` and the private home isolates its config.
        assert!(copilot.launch_args.is_empty());
        assert!(copilot.mcp_wire_delivery.forwards_servers());
    }

    #[test]
    fn grok_declares_its_blocking_host_requests_and_leading_flags() {
        let grok = agent_dialect_profile("grok");
        assert_eq!(
            grok.handles_host_request("_x.ai/ask_user_question"),
            Some(AgentHostRequestDialect::GrokAskUserQuestion)
        );
        assert_eq!(
            grok.handles_host_request("_x.ai/exit_plan_mode"),
            Some(AgentHostRequestDialect::GrokExitPlanMode)
        );
        assert_eq!(grok.handles_host_request("session/update"), None);
        // `agent stdio` rejects the flag, so it must precede the configured
        // subcommand arguments.
        assert_eq!(
            grok.launch_args,
            &[DialectLaunchArg {
                value: "--no-auto-update",
                placement: LaunchArgPlacement::Leading,
            }]
        );
    }
}

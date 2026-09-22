//! Which channel actually delivers an MCP server to one Agent.
//!
//! Vibex has exactly two delivery channels and they are not interchangeable:
//!
//! * the ACP wire, `session/new.mcpServers`, for Agents whose dialect forwards
//!   the field to the model; and
//! * the Agent's own MCP configuration file, for Agents whose CLI reads that
//!   file at launch instead of looking at the wire.
//!
//! [`crate::native_surface`] owns the file half of that knowledge and
//! `crates/agent-acp/src/dialect.rs` owns the wire half. A market install runs
//! below both of those layers, so it cannot read the dialect table directly.
//! This module restates the small set of Agents that deviate from the generic
//! wire path, and a test in `crates/agent-acp/src/dialect.rs` fails whenever the
//! two tables disagree.
//!
//! The classification exists so a market install can be honest about what it
//! achieved. Writing an `enabled` matrix row is not the same as making the
//! Agent able to call the server, and product surfaces must not conflate them.

use crate::native_surface::native_mcp_surface;

/// How a market-installed MCP server reaches one Agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMcpDelivery {
    /// The ACP runtime forwards the server over `session/new.mcpServers`.
    Wire,
    /// The Agent reads its own MCP configuration file at launch, so Vibex has
    /// to write the server there for it to exist at all.
    NativeFile,
    /// The Agent neither forwards the wire field nor reads a file Vibex can
    /// write. Enabling the server would be a no-op, so install refuses it
    /// instead of reporting a success the user cannot observe.
    Unsupported,
}

/// The channel a market install must use for `agent_id`.
///
/// Everything not named here follows the generic ACP path: the runtime probes
/// capabilities and the dialect profile defaults to `Delivered`. The named
/// Agents are the exceptions the dialect table records:
///
/// * `grok`, `cursor`, and `hermes` read their own MCP file and would
///   double-register anything forwarded over the wire;
/// * `pi` accepts `mcpServers` but never forwards it to its inner process, and
///   has no native MCP file either;
/// * `factory-droid` rejects any forwarded entry, and its catalog preset opts
///   out of the MCP feature entirely.
pub fn agent_mcp_delivery(agent_id: &str) -> AgentMcpDelivery {
    match agent_id {
        "grok" | "cursor" | "hermes" => AgentMcpDelivery::NativeFile,
        "pi" | "factory-droid" => AgentMcpDelivery::Unsupported,
        _ => AgentMcpDelivery::Wire,
    }
}

/// Whether Vibex knows a native MCP file for `agent_id`.
///
/// A [`AgentMcpDelivery::NativeFile`] Agent without a surface would have no
/// delivery path at all, so the install path checks this before claiming the
/// Agent is enabled.
pub fn agent_has_native_mcp_file(agent_id: &str) -> bool {
    native_mcp_surface(agent_id).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_config_agents_have_a_file_to_write() {
        for agent_id in ["grok", "cursor", "hermes"] {
            assert_eq!(agent_mcp_delivery(agent_id), AgentMcpDelivery::NativeFile);
            assert!(
                agent_has_native_mcp_file(agent_id),
                "{agent_id} needs a native MCP surface to receive market installs"
            );
        }
    }

    #[test]
    fn wire_agents_keep_the_generic_path() {
        for agent_id in ["claude", "codex", "gemini", "opencode", "kimi"] {
            assert_eq!(agent_mcp_delivery(agent_id), AgentMcpDelivery::Wire);
        }
    }

    #[test]
    fn agents_with_no_delivery_path_are_unsupported() {
        for agent_id in ["pi", "factory-droid"] {
            assert_eq!(agent_mcp_delivery(agent_id), AgentMcpDelivery::Unsupported);
        }
    }
}

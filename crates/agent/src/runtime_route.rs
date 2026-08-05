//! ACP route helpers shared with the adapter crate.

use vibex_core::{AcpAdapterId, AgentId};

/// Transitional placeholder adapter derivation, centralized so the P2
/// Compatibility Registry only has to replace this single function to become
/// the source of truth for adapter ids.
pub fn default_adapter_for_agent(agent_id: &AgentId) -> AcpAdapterId {
    vibex_core::default_acp_adapter_id(agent_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_adapter_is_derived_from_agent_id() {
        let agent = AgentId::parse("opencode").unwrap();
        assert_eq!(default_adapter_for_agent(&agent).as_str(), "opencode-acp");
    }
}

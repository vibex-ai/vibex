use serde::{Deserialize, Serialize};
use std::fmt;

use crate::{
    AgentAuthenticationOperationId, AgentId, ProviderProfileId, TerminalAuthActionDescriptor,
};

/// Product-level ACP authentication method. Protocol-specific payloads stay in
/// the ACP adapter; settings surfaces receive only safe fields they can render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuthMethodKind {
    Agent,
    Environment,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthEnvironmentVariable {
    pub name: String,
    pub label: Option<String>,
    pub secret: bool,
    pub optional: bool,
    /// Whether the selected Provider Profile already owns a usable value for
    /// this exact environment key. The value itself never crosses this API.
    #[serde(default)]
    pub configured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthMethod {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub kind: AgentAuthMethodKind,
    pub environment: Vec<AgentAuthEnvironmentVariable>,
    pub credential_link: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuthStatus {
    Unknown,
    AuthenticationRequired,
    Authenticated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthCatalog {
    pub agent_id: AgentId,
    pub methods: Vec<AgentAuthMethod>,
    pub supports_logout: bool,
    pub status: AgentAuthStatus,
    pub refreshed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAuthenticateRequest {
    pub operation_id: AgentAuthenticationOperationId,
    pub agent_id: AgentId,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub method_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAuthenticationCancelRequest {
    pub operation_id: AgentAuthenticationOperationId,
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAuthenticateResult {
    pub method_id: String,
    pub terminal: Option<TerminalAuthActionDescriptor>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AgentAuthEnvironmentValue {
    pub name: String,
    pub value: Option<String>,
    pub secret: bool,
    pub optional: bool,
    /// Explicit removal intent. A blank value without this flag preserves an
    /// existing credential reference, which keeps masked settings forms safe.
    pub clear: bool,
}

impl fmt::Debug for AgentAuthEnvironmentValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentAuthEnvironmentValue")
            .field("name", &self.name)
            .field(
                "has_value",
                &self.value.as_ref().is_some_and(|v| !v.is_empty()),
            )
            .field("secret", &self.secret)
            .field("optional", &self.optional)
            .field("clear", &self.clear)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct AgentAuthEnvironmentUpdateRequest {
    pub agent_id: AgentId,
    pub provider_profile_id: ProviderProfileId,
    pub method_id: String,
    pub values: Vec<AgentAuthEnvironmentValue>,
}

impl fmt::Debug for AgentAuthEnvironmentUpdateRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentAuthEnvironmentUpdateRequest")
            .field("agent_id", &self.agent_id)
            .field("provider_profile_id", &self.provider_profile_id)
            .field("method_id", &self.method_id)
            .field("values", &self.values)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLogoutRequest {
    pub agent_id: AgentId,
    pub provider_profile_id: Option<ProviderProfileId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_auth_debug_never_contains_credential_values() {
        let value = AgentAuthEnvironmentValue {
            name: "EXAMPLE_API_KEY".to_string(),
            value: Some("secret-value".to_string()),
            secret: true,
            optional: false,
            clear: false,
        };
        let request = AgentAuthEnvironmentUpdateRequest {
            agent_id: AgentId::parse("example-agent").unwrap(),
            provider_profile_id: ProviderProfileId::new(),
            method_id: "api-key".to_string(),
            values: vec![value],
        };

        let debug = format!("{request:?}");
        assert!(debug.contains("EXAMPLE_API_KEY"));
        assert!(debug.contains("has_value: true"));
        assert!(!debug.contains("secret-value"));
    }
}

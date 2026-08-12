use serde::{Deserialize, Serialize};
use std::fmt;

use crate::{
    AgentAuthContextId, AgentAuthenticationOperationId, AgentId, ProviderProfileId,
    SessionConfigValue, SessionRuntimeFeature, TerminalAuthActionDescriptor, VibexSessionId,
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

/// Describes where a successful authentication action leaves reusable
/// credentials. A method is an action, never a durable runtime identity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuthMethodEffect {
    WritesAgentStateHome,
    RequiresProviderProfile,
    AgentManagedExternal,
    #[default]
    Unknown,
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
    #[serde(default)]
    pub effect: AgentAuthMethodEffect,
    pub environment: Vec<AgentAuthEnvironmentVariable>,
    pub credential_link: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuthContextStatus {
    Unverified,
    Verifying,
    AuthenticationRequired,
    Authenticated,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContext {
    pub id: AgentAuthContextId,
    pub agent_id: AgentId,
    pub status: AgentAuthContextStatus,
    pub account_hint: Option<String>,
    pub authenticated_via_method: Option<String>,
    pub revision: i64,
    pub last_verified_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuthenticationOperationState {
    Queued,
    DiscoveringMethods,
    Authenticating,
    AwaitingUser,
    Verifying,
    Succeeded,
    Failed,
    Cancelling,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuthExecutionLocation {
    HostBrowser,
    ClientBrowserWithDeviceCode,
    HostTerminal,
    RemoteAttachableTerminal,
    #[default]
    CompletedOnHost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthenticationOperation {
    pub operation_id: AgentAuthenticationOperationId,
    pub auth_context_id: AgentAuthContextId,
    pub expected_context_revision: i64,
    pub method_id: String,
    pub state: AgentAuthenticationOperationState,
    pub error_code: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentModelDiscoverySource {
    DirectCatalog,
    SessionConfig,
    CompatibilityDescriptor,
    LiveSession,
    AgentDefault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuthModelCatalogStatus {
    Unknown,
    Discovering,
    Available,
    AgentDefaultOnly,
    AuthenticationRequired,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthModelDescriptor {
    pub model_id: String,
    pub label: String,
    #[serde(default)]
    pub reasoning_efforts: Vec<SessionConfigValue>,
    #[serde(default)]
    pub modes: Vec<SessionConfigValue>,
    #[serde(default)]
    pub features: Vec<SessionRuntimeFeature>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthModelCatalogSnapshot {
    pub auth_context_id: AgentAuthContextId,
    pub auth_context_revision: i64,
    pub runtime_fingerprint: String,
    pub discovery_source: AgentModelDiscoverySource,
    pub status: AgentAuthModelCatalogStatus,
    pub models: Vec<AgentAuthModelDescriptor>,
    pub last_success_at_ms: Option<i64>,
    pub last_attempt_at_ms: i64,
    pub last_error_code: Option<String>,
}

/// CAS-fenced request to authenticate the one default account owned by an
/// Agent. `method_id` selects an action only; it never becomes runtime identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextAuthenticateRequest {
    pub operation_id: AgentAuthenticationOperationId,
    pub auth_context_id: AgentAuthContextId,
    pub expected_context_revision: i64,
    pub method_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextVerifyRequest {
    pub auth_context_id: AgentAuthContextId,
    pub expected_context_revision: i64,
    /// Present when verification completes an interactive authentication
    /// operation that previously returned an auth terminal.
    pub operation_id: Option<AgentAuthenticationOperationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextCancelAuthenticationRequest {
    pub operation_id: AgentAuthenticationOperationId,
    pub auth_context_id: AgentAuthContextId,
    pub expected_context_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextRefreshModelsRequest {
    pub auth_context_id: AgentAuthContextId,
    pub expected_context_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextLogoutRequest {
    pub auth_context_id: AgentAuthContextId,
    pub expected_context_revision: i64,
    /// The caller must echo the current impact count after presenting it to
    /// the user. A concurrent session change therefore forces a new preview.
    pub confirmed_affected_session_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextLogoutPreview {
    pub context: AgentAuthContext,
    pub affected_session_ids: Vec<VibexSessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextMutationResult {
    pub context: AgentAuthContext,
    pub model_catalog: Option<AgentAuthModelCatalogSnapshot>,
    #[serde(default)]
    pub affected_session_ids: Vec<VibexSessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthContextAuthenticateResult {
    pub context: AgentAuthContext,
    pub operation: AgentAuthenticationOperation,
    #[serde(default)]
    pub execution_location: AgentAuthExecutionLocation,
    pub terminal: Option<TerminalAuthActionDescriptor>,
    pub model_catalog: Option<AgentAuthModelCatalogSnapshot>,
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

/// Releases the adapter-side ownership for an interactive authentication
/// operation after its terminal has reached a final exit state. This is
/// separate from cancellation because a successful terminal must remain
/// available for verification and final-output inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAuthenticationCompleteRequest {
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

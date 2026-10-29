//! Versioned Agent/model-provider projection contracts.
//!
//! The legacy `ProviderProfile` API remains available during the compatibility
//! window. New code keeps reusable provider data, Agent process identity, and
//! their versioned binding in the three distinct entities defined here.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use crate::agent_provider_runtime::catalog_projection_descriptors;
use crate::{
    AcpAdapterId, AcpProcessStrategy, AcpProviderEnvReference, AgentConfiguredModelBindingId,
    AgentId, AgentModelProviderBindingId, AgentProviderProjectionDescriptorId,
    AgentRuntimeProfileId, AgentRuntimeRouteKey, ModelProviderProfileId, ProviderBindingMetadata,
    ProviderNetworkDefaults, ProviderPermissionDefaults, ProviderProfileId,
    ProviderSandboxDefaults, ProviderSecretBackend, ProviderSecretKind, ProviderSecretSetupState,
    RequestId, TransportKind, VibexError, VibexResult,
};

pub const PROVIDER_PROJECTION_SCHEMA_VERSION: u32 = 1;
pub const WIRE_PROTOCOL_OPENAI_RESPONSES: &str = "openai_responses";
pub const WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS: &str = "openai_chat_completions";
pub const WIRE_PROTOCOL_ANTHROPIC_MESSAGES: &str = "anthropic_messages";
/// Mistral's own Conversations surface. It is not OpenAI-compatible — request
/// body, tool-call shape and streaming frames all differ — so an Agent that
/// speaks it needs a spelling of its own rather than a compatibility switch.
pub const WIRE_PROTOCOL_MISTRAL_CONVERSATIONS: &str = "mistral_conversations";
pub const WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI: &str = "google_generative_ai";
/// Vertex AI's Gemini surface. It carries the same
/// `generateContent`/`streamGenerateContent` body as
/// [`WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI`] but a distinct
/// `/v1beta1/publishers/google/models/<model>` route and GCP credentials, so an
/// Agent that can speak both needs to distinguish them at the endpoint and
/// environment-variable level.
pub const WIRE_PROTOCOL_GOOGLE_VERTEX: &str = "google_vertex";
pub const WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE: &str = "aws_bedrock_converse";

pub const CLAUDE_PROJECTION_DESCRIPTOR_ID: &str = "projection_claude_environment_v1";
pub const CODEX_PROJECTION_DESCRIPTOR_ID: &str = "projection_codex_stable_home_v1";
pub const OPENCODE_PROJECTION_DESCRIPTOR_ID: &str = "projection_opencode_inline_provider_v1";
pub const ZCODE_PROJECTION_DESCRIPTOR_ID: &str = "projection_zcode_private_config_v1";

const CLAUDE_AGENT_ID: &str = "claude";
const CLAUDE_ADAPTER_ID: &str = "claude-agent-acp";
const CLAUDE_ADAPTER_VERSION: &str = "0.93.0";
pub const CLAUDE_COMPATIBLE_ADAPTER_VERSION_REQUIREMENT: &str = ">=0.71.0";
const CODEX_AGENT_ID: &str = "codex";
const CODEX_ADAPTER_ID: &str = "codex-acp";
const CODEX_ADAPTER_VERSION: &str = "2.2.2";
/// The pin moved to 2.2.2 and its runtime to 0.160.1, but the typed
/// projection is still the same contract 1.8.0 introduced: an installed older
/// adapter keeps the provider editor and only misses the exact-identity alias
/// table, which the registry gates separately.
pub const CODEX_COMPATIBLE_ADAPTER_VERSION_REQUIREMENT: &str = ">=1.8.0";
#[cfg(test)]
const CODEX_RUNTIME_PACKAGE: &str = "@openai/codex";
const CODEX_RUNTIME_VERSION: &str = "0.160.1";
const OPENCODE_AGENT_ID: &str = "opencode";
const OPENCODE_ADAPTER_ID: &str = "opencode-acp";
const ZCODE_AGENT_ID: &str = "zcode";
const ZCODE_ADAPTER_ID: &str = "zcode-acp-server";
pub const ZCODE_ADAPTER_VERSION: &str = "0.72.0";
/// Automatic provider projection remains available after an Agent upgrade
/// once the runtime is at least the first verified version for its descriptor.
///
/// The pin and the floor are separate statements: the pin moves to the release
/// Vibex verifies, installs, and rolls back to, while the floor stays on the
/// oldest runtime the inline-provider projection still supports.
pub const OPENCODE_COMPATIBLE_VERSION_REQUIREMENT: &str = ">=1.17.9";
/// `opencode acp` is byte-identical from `1.18.33` through `1.19.3`: the same
/// entry point, session modes, config option ids (`model`/`effort`/`mode`) and
/// inline-provider schema. The releases between them only add provider headers
/// and a config-driven `reasoning_options` field, neither of which the
/// projection writes today, so the floor and the pin can move independently.
pub const OPENCODE_LAST_VERIFIED_VERSION: &str = "1.19.3";

const MAX_DISPLAY_NAME_LEN: usize = 160;
const MAX_MODEL_COUNT: usize = 512;
const MAX_ENV_COUNT: usize = 256;
const MAX_ARGS_COUNT: usize = 256;
const MAX_HEADER_COUNT: usize = 32;
const MAX_HEADER_NAME_LEN: usize = 256;
const MAX_HEADER_LITERAL_LEN: usize = 8192;

/// Whether a byte may appear in an HTTP header field name (RFC 9110 token).
fn is_header_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderProfileStatus {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderEndpointKind {
    Api,
    Auth,
    ModelCatalog,
    Proxy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderEndpoint {
    pub id: String,
    pub kind: ModelProviderEndpointKind,
    pub url: String,
    /// `None` is a provider-wide fallback. A protocol-specific endpoint wins
    /// when the selected Agent model binding uses the same wire protocol.
    #[serde(default)]
    pub wire_protocol_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "endpoint", rename_all = "snake_case")]
pub enum ModelProviderProxyPolicy {
    InheritSystem,
    Disabled,
    Endpoint(String),
}

/// A per-session value a Provider request header can carry instead of a
/// literal.
///
/// The value is filled by the runtime that owns the Agent process, at the
/// moment the request leaves it, so one Provider Profile still describes every
/// session it serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderHeaderVariable {
    /// The Vibex session whose turns the Agent process is serving.
    SessionId,
}

impl ModelProviderHeaderVariable {
    pub const ALL: [Self; 1] = [Self::SessionId];

    /// The stable identifier the Provider editor, the persisted override, and
    /// every projection share.
    pub const fn id(self) -> &'static str {
        match self {
            Self::SessionId => "session_id",
        }
    }

    /// The environment key a projection references the value through. The ACP
    /// runtime fills it from the session that owns the Agent process.
    pub const fn env_key(self) -> &'static str {
        match self {
            Self::SessionId => "VIBEX_SESSION_ID",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|variable| variable.id() == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ModelProviderHeaderValue {
    NonSecretLiteral(String),
    SecretReference(RequestId),
    /// A value the runtime fills from the session it is serving.
    DynamicVariable(ModelProviderHeaderVariable),
}

impl ModelProviderHeaderValue {
    pub const fn dynamic_variable(&self) -> Option<ModelProviderHeaderVariable> {
        match self {
            Self::DynamicVariable(variable) => Some(*variable),
            Self::NonSecretLiteral(_) | Self::SecretReference(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderHeaderReference {
    pub name: String,
    pub value: ModelProviderHeaderValue,
    pub redacted_hint: String,
}

impl ModelProviderHeaderReference {
    /// The environment keys a launch must fill for this Profile's dynamic
    /// headers, in stable order and without duplicates.
    pub fn dynamic_variable_env_keys(headers: &[Self]) -> Vec<String> {
        let mut keys = headers
            .iter()
            .filter_map(|header| header.value.dynamic_variable())
            .map(|variable| variable.env_key().to_string())
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        keys
    }
}

/// Provider-side model metadata. Wire protocol and SDK adapter deliberately
/// live on `AgentConfiguredModelBinding`, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderCatalogEntry {
    pub id: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    #[serde(default)]
    pub metadata: Vec<ProviderBindingMetadata>,
    /// Declared capabilities for this Model. Undeclared fields stay `None` and
    /// are omitted from Agent config overlays.
    #[serde(default)]
    pub capabilities: crate::ProviderModelCapabilities,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCredentialKind {
    ApiKey,
    OAuth,
    Aws,
    Gcp,
    Azure,
    Snowflake,
    Local,
    ManagedSubscription,
}

impl AgentCredentialKind {
    pub const ALL: [Self; 8] = [
        Self::ApiKey,
        Self::OAuth,
        Self::Aws,
        Self::Gcp,
        Self::Azure,
        Self::Snowflake,
        Self::Local,
        Self::ManagedSubscription,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCredentialStatus {
    Missing,
    Referenced,
    Ready,
    Expired,
    AgentManaged,
    Unsupported,
}

/// Opaque Secret locator. This record may be persisted; the Secret value may
/// not. Remote projections must expose only its setup state and redacted hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionSecretReference {
    pub id: RequestId,
    pub backend: ProviderSecretBackend,
    pub setup_state: ProviderSecretSetupState,
    pub lookup_key: String,
    pub redacted_hint: String,
    pub revision: i64,
    #[serde(default)]
    pub legacy_secret_reference_id: Option<RequestId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentCredential {
    ApiKey {
        secret: ProjectionSecretReference,
        target_hint: Option<String>,
    },
    OAuth {
        account_reference: Option<String>,
        host_mediated: bool,
    },
    Aws {
        profile: Option<String>,
        region: Option<String>,
        secret: Option<ProjectionSecretReference>,
    },
    Gcp {
        project: Option<String>,
        location: Option<String>,
        credential: Option<ProjectionSecretReference>,
    },
    Azure {
        resource: Option<String>,
        deployment: Option<String>,
        api_version: Option<String>,
        credential: Option<ProjectionSecretReference>,
    },
    Snowflake {
        connection: Option<String>,
        auth_method: Option<String>,
        credential: Option<ProjectionSecretReference>,
    },
    Local {
        runtime: Option<String>,
    },
    ManagedSubscription {
        account_reference: Option<String>,
    },
}

impl AgentCredential {
    pub const fn kind(&self) -> AgentCredentialKind {
        match self {
            Self::ApiKey { .. } => AgentCredentialKind::ApiKey,
            Self::OAuth { .. } => AgentCredentialKind::OAuth,
            Self::Aws { .. } => AgentCredentialKind::Aws,
            Self::Gcp { .. } => AgentCredentialKind::Gcp,
            Self::Azure { .. } => AgentCredentialKind::Azure,
            Self::Snowflake { .. } => AgentCredentialKind::Snowflake,
            Self::Local { .. } => AgentCredentialKind::Local,
            Self::ManagedSubscription { .. } => AgentCredentialKind::ManagedSubscription,
        }
    }

    pub fn secret_reference(&self) -> Option<&ProjectionSecretReference> {
        match self {
            Self::ApiKey { secret, .. } => Some(secret),
            Self::Aws { secret, .. } => secret.as_ref(),
            Self::Gcp { credential, .. }
            | Self::Azure { credential, .. }
            | Self::Snowflake { credential, .. } => credential.as_ref(),
            Self::OAuth { .. } | Self::Local { .. } | Self::ManagedSubscription { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderCredentialReference {
    pub id: RequestId,
    pub display_name: String,
    /// A masked fingerprint of the stored Secret.
    ///
    /// Surfaces identify a Key by this value instead of its credential id, so
    /// a row is recognizable without resolving the Secret. Empty until the
    /// Secret has been written or read once.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub masked_hint: String,
    pub status: AgentCredentialStatus,
    pub credential: AgentCredential,
    pub revision: i64,
    /// Optional model ids this Key may serve. Empty means every model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// Optional per-Key proxy URL; empty inherits the Provider/gateway proxy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
}

impl Default for ModelProviderCredentialReference {
    fn default() -> Self {
        Self {
            id: RequestId::new(),
            display_name: String::new(),
            masked_hint: String::new(),
            status: AgentCredentialStatus::Missing,
            credential: AgentCredential::Local { runtime: None },
            revision: 1,
            models: Vec::new(),
            proxy_url: None,
        }
    }
}

impl ModelProviderCredentialReference {
    /// The label a surface should use when it names this Key.
    ///
    /// A user-given name wins. The generated `Key <credential id>` placeholder
    /// is not a name — it identifies the record, not the Key — so it falls
    /// through to the masked fingerprint.
    pub fn display_label(&self) -> String {
        let name = self.display_name.trim();
        if !name.is_empty() && !self.has_generated_name() {
            return name.to_string();
        }
        if !self.masked_hint.is_empty() {
            return self.masked_hint.clone();
        }
        "********".to_string()
    }

    /// Whether the stored name is the retired generated placeholder.
    pub fn has_generated_name(&self) -> bool {
        self.display_name.trim() == format!("Key {}", self.id.as_str())
    }
}

/// Masks a stored Secret for display.
///
/// Keeps one to four leading and trailing characters and replaces the middle
/// with a fixed eight asterisks, so the mask identifies a Key without
/// disclosing its full value or its exact length. A Secret too short to split
/// is masked entirely.
pub fn masked_secret_hint(secret: &str) -> String {
    if secret.is_empty() {
        return String::new();
    }
    let characters: Vec<char> = secret.chars().collect();
    if characters.len() <= 2 {
        return "********".to_string();
    }
    let visible = (characters.len() / 4).clamp(1, 4);
    let front: String = characters[..visible].iter().collect();
    let back: String = characters[characters.len() - visible..].iter().collect();
    format!("{front}********{back}")
}

/// Provider metadata that is not part of the launch contract.
///
/// Kept in one record so future advisory fields do not widen the Provider
/// profile column by column. Every field is optional or defaulted; an older
/// record reads as the empty value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderMetadata {
    /// The matching models.dev provider id, when the editor knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_id: Option<String>,
    /// Keep the Provider's models out of Agent pickers; route groups still
    /// reach the Provider.
    #[serde(default)]
    pub only_through_route_groups: bool,
    /// A bundled icon key, when the Provider has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The picker category: vendor, relay, or local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// How the outbound auth header is shaped: auto, x-api-key, bearer, or both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header_mode: Option<String>,
    /// A JSON object merged into the request body; never carries a Secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_body_json: Option<String>,
    /// `default` or `pass_through` for the Agent's User-Agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent_mode: Option<String>,
    /// Only meaningful for a custom or local Provider; requires confirmation.
    #[serde(default)]
    pub tls_accept_invalid: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_idle_timeout_ms: Option<u64>,
    #[serde(default)]
    pub auto_refresh_models: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_aliases: Vec<ProviderModelAlias>,
}

/// One local model id mapped to the upstream id the Provider serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelAlias {
    pub alias: String,
    pub upstream: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderProfile {
    pub id: ModelProviderProfileId,
    #[serde(default)]
    pub legacy_provider_profile_id: Option<ProviderProfileId>,
    pub display_name: String,
    pub vendor_hint: Option<String>,
    pub endpoints: Vec<ModelProviderEndpoint>,
    pub proxy_policy: ModelProviderProxyPolicy,
    pub credentials: Vec<ModelProviderCredentialReference>,
    pub configured_models: Vec<ModelProviderCatalogEntry>,
    pub default_model_id: Option<String>,
    pub headers: Vec<ModelProviderHeaderReference>,
    /// Advisory metadata; never read by a launch projection.
    #[serde(default)]
    pub metadata: ModelProviderMetadata,
    pub status: ModelProviderProfileStatus,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
}

impl Default for ModelProviderProfile {
    /// An empty, disabled Provider.
    ///
    /// This exists so a caller — a fixture, or a page that has not loaded yet —
    /// can name only the fields it cares about. It is never a valid record to
    /// persist: `validate` still rejects an empty display name.
    fn default() -> Self {
        Self {
            id: ModelProviderProfileId::new(),
            legacy_provider_profile_id: None,
            display_name: String::new(),
            vendor_hint: None,
            endpoints: Vec::new(),
            proxy_policy: ModelProviderProxyPolicy::InheritSystem,
            credentials: Vec::new(),
            configured_models: Vec::new(),
            default_model_id: None,
            headers: Vec::new(),
            metadata: ModelProviderMetadata::default(),
            status: ModelProviderProfileStatus::Disabled,
            revision: 1,
            created_at_ms: 0,
            updated_at_ms: 0,
            deleted_at_ms: None,
        }
    }
}

impl ModelProviderProfile {
    pub fn validate(&self) -> VibexResult<()> {
        validate_display_name(&self.display_name, "model_provider_display_name_invalid")?;
        if self.configured_models.len() > MAX_MODEL_COUNT {
            return Err(VibexError::validation(
                "model_provider_model_limit_exceeded",
                "model provider profile contains too many configured models",
            ));
        }
        unique_non_empty(
            self.endpoints.iter().map(|endpoint| endpoint.id.as_str()),
            "model_provider_endpoint_invalid",
            "model provider endpoint ids must be non-empty and unique",
        )?;
        for endpoint in &self.endpoints {
            if endpoint
                .wire_protocol_id
                .as_deref()
                .is_some_and(|protocol| !is_model_provider_wire_protocol(protocol))
            {
                return Err(VibexError::validation(
                    "model_provider_endpoint_protocol_invalid",
                    "model provider endpoint references an unsupported wire protocol",
                )
                .with_diagnostic("endpointId", endpoint.id.as_str())
                .with_diagnostic(
                    "wireProtocolId",
                    endpoint.wire_protocol_id.as_deref().unwrap_or_default(),
                ));
            }
        }
        unique_non_empty(
            self.configured_models.iter().map(|model| model.id.as_str()),
            "model_provider_model_invalid",
            "model provider model ids must be non-empty and unique",
        )?;
        unique_non_empty(
            self.credentials
                .iter()
                .map(|credential| credential.id.as_str()),
            "model_provider_credential_duplicate",
            "model provider credential ids must be unique",
        )?;
        if self.headers.len() > MAX_HEADER_COUNT {
            return Err(VibexError::validation(
                "model_provider_header_limit_exceeded",
                "model provider profile contains too many request headers",
            ));
        }
        let mut header_names = BTreeSet::new();
        for header in &self.headers {
            let name = header.name.trim();
            if name.is_empty()
                || name.len() > MAX_HEADER_NAME_LEN
                || !name.bytes().all(is_header_name_byte)
            {
                return Err(VibexError::validation(
                    "model_provider_header_invalid",
                    "model provider request header names must be non-empty HTTP field names",
                )
                .with_diagnostic("headerName", header.name.as_str()));
            }
            if !header_names.insert(name.to_ascii_lowercase()) {
                return Err(VibexError::validation(
                    "model_provider_header_duplicate",
                    "model provider request header names must be unique",
                )
                .with_diagnostic("headerName", name));
            }
            if let ModelProviderHeaderValue::NonSecretLiteral(value) = &header.value
                && value.len() > MAX_HEADER_LITERAL_LEN
            {
                return Err(VibexError::validation(
                    "model_provider_header_value_invalid",
                    "model provider request header value exceeds the supported length",
                )
                .with_diagnostic("headerName", name));
            }
        }
        if let Some(default_model_id) = self.default_model_id.as_deref()
            && !self
                .configured_models
                .iter()
                .any(|model| model.id == default_model_id && model.enabled)
        {
            return Err(VibexError::validation(
                "model_provider_default_model_invalid",
                "default model must reference an enabled configured model",
            ));
        }
        Ok(())
    }

    pub fn primary_api_endpoint(&self) -> Option<&ModelProviderEndpoint> {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.kind == ModelProviderEndpointKind::Api)
    }

    pub fn primary_api_endpoint_for_protocol(
        &self,
        wire_protocol_id: &str,
    ) -> Option<&ModelProviderEndpoint> {
        self.endpoints
            .iter()
            .find(|endpoint| {
                endpoint.kind == ModelProviderEndpointKind::Api
                    && endpoint.wire_protocol_id.as_deref() == Some(wire_protocol_id)
            })
            .or_else(|| {
                self.endpoints.iter().find(|endpoint| {
                    endpoint.kind == ModelProviderEndpointKind::Api
                        && endpoint.wire_protocol_id.is_none()
                })
            })
    }
}

pub fn is_model_provider_wire_protocol(value: &str) -> bool {
    matches!(
        value.trim(),
        WIRE_PROTOCOL_OPENAI_RESPONSES
            | WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS
            | WIRE_PROTOCOL_ANTHROPIC_MESSAGES
            | WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI
            | WIRE_PROTOCOL_GOOGLE_VERTEX
            | WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentVersionSource {
    Managed,
    Detected,
    Manual,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeVersionIdentity {
    pub route: AgentRuntimeRouteKey,
    pub adapter_version: Option<String>,
    pub agent_version: Option<String>,
    #[serde(default)]
    pub runtime_dependencies: BTreeMap<String, String>,
    pub source: AgentVersionSource,
}

impl AgentRuntimeVersionIdentity {
    pub fn is_automatic_projection_eligible(&self) -> bool {
        !matches!(
            self.source,
            AgentVersionSource::Manual | AgentVersionSource::Unknown
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeHomeStrategy {
    VibexPrivate,
    AgentManaged,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHostCapabilities {
    pub filesystem: bool,
    pub terminal: bool,
    pub terminal_auth: bool,
    pub mcp: bool,
    pub session_config: bool,
}

impl Default for AgentHostCapabilities {
    fn default() -> Self {
        Self {
            filesystem: true,
            terminal: false,
            terminal_auth: false,
            mcp: false,
            session_config: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeResourcePolicy {
    pub sandbox: ProviderSandboxDefaults,
    pub network: ProviderNetworkDefaults,
    pub permissions: ProviderPermissionDefaults,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeProfile {
    pub id: AgentRuntimeProfileId,
    #[serde(default)]
    pub legacy_provider_profile_id: Option<ProviderProfileId>,
    pub version_identity: AgentRuntimeVersionIdentity,
    pub command: String,
    pub args: Vec<String>,
    pub safe_env_references: Vec<AcpProviderEnvReference>,
    pub cwd_template: Option<String>,
    pub process_strategy: AcpProcessStrategy,
    pub runtime_home_strategy: AgentRuntimeHomeStrategy,
    pub host_capabilities: AgentHostCapabilities,
    pub resource_policy: AgentRuntimeResourcePolicy,
    /// The ACP catalogue this launch offers.
    ///
    /// These four are what `AcpProviderConfig` needs beyond the command line. They
    /// used to live only inside the legacy row's serialised config, which is why the
    /// launch path could not be moved off that row: a runtime Profile without them
    /// cannot describe a session.
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub modes: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
}

impl AgentRuntimeProfile {
    pub fn validate(&self) -> VibexResult<()> {
        if self.command.trim().is_empty() || self.command.len() > 4096 {
            return Err(VibexError::validation(
                "agent_runtime_command_invalid",
                "Agent runtime command must be non-empty and bounded",
            ));
        }
        if self.args.len() > MAX_ARGS_COUNT || self.safe_env_references.len() > MAX_ENV_COUNT {
            return Err(VibexError::validation(
                "agent_runtime_configuration_limit_exceeded",
                "Agent runtime args or environment references exceed the supported limit",
            ));
        }
        unique_non_empty(
            self.safe_env_references
                .iter()
                .map(|entry| entry.key.as_str()),
            "agent_runtime_env_duplicate",
            "Agent runtime environment keys must be non-empty and unique",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfiguredModelBinding {
    pub id: AgentConfiguredModelBindingId,
    pub provider_model_id: String,
    pub agent_model_id: String,
    pub wire_protocol_id: String,
    pub sdk_adapter_id: Option<String>,
    pub deployment: Option<String>,
    pub enabled: bool,
    #[serde(default)]
    pub process_scoped: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProviderProjectionOverrides {
    pub endpoint_id: Option<String>,
    pub credential_id: Option<RequestId>,
    pub default_model_binding_id: Option<AgentConfiguredModelBindingId>,
    #[serde(default)]
    pub non_secret_env: BTreeMap<String, String>,
    #[serde(default)]
    pub advanced_custom_env: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentModelProviderBindingStatus {
    Draft,
    Ready,
    StaleRestartRequired,
    Unverified,
    Unsupported,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionEvidenceState {
    Verified,
    Documented,
    AgentManaged,
    Local,
    ServiceMarketplace,
    Unsupported,
    Unverified,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionVerificationState {
    pub state: ProjectionEvidenceState,
    pub descriptor_version: String,
    pub source_evidence_reference: Option<String>,
    pub runtime_evidence_reference: Option<String>,
    pub verified_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderBinding {
    pub id: AgentModelProviderBindingId,
    #[serde(default)]
    pub legacy_provider_profile_id: Option<ProviderProfileId>,
    pub agent_id: AgentId,
    pub runtime_profile_id: AgentRuntimeProfileId,
    /// The Provider this Agent sends to, when it has one.
    ///
    /// `None` is a real case, not a gap: an Agent that authenticates as itself —
    /// `claude login`, a Codex subscription — has a launch command and no Provider
    /// endpoint. The binding used to require a Provider, so such an Agent got a
    /// placeholder row with no address and no Key purely to satisfy the column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider_profile_id: Option<ModelProviderProfileId>,
    pub projection_descriptor_id: AgentProviderProjectionDescriptorId,
    pub projection_overrides: AgentProviderProjectionOverrides,
    pub configured_models: Vec<AgentConfiguredModelBinding>,
    /// How this Agent names the account behind this Provider.
    ///
    /// Agent-scoped, not Provider-scoped: two Agents may share one Provider and
    /// call it different things, because the alias is what the reader sees in
    /// one Agent's picker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_alias: Option<String>,
    /// The reasoning effort this Agent asks this Provider for.
    ///
    /// Agent-scoped for the same reason: one Provider may serve an Agent that
    /// wants high effort and another that wants none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Launch options that have no structured home yet.
    ///
    /// These carry the protocol pin, the ACP configuration and the native-import
    /// markers. They are Agent-scoped because they describe how *this* Agent
    /// calls the Provider, which is exactly why the reverse projection has
    /// always refused to overwrite them from the shared record.
    #[serde(default = "default_provider_options")]
    pub provider_options: crate::ProviderOptions,
    pub projection_fingerprint: Option<String>,
    pub status: AgentModelProviderBindingStatus,
    pub verification: ProjectionVerificationState,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
}

/// An empty option set, for records written before the field existed.
fn default_provider_options() -> crate::ProviderOptions {
    crate::ProviderOptions::empty()
}

impl Default for AgentModelProviderBinding {
    /// An empty binding, for callers that only care about a few fields.
    fn default() -> Self {
        Self {
            id: AgentModelProviderBindingId::new(),
            legacy_provider_profile_id: None,
            agent_id: AgentId::parse("unknown").expect("a literal Agent id is valid"),
            runtime_profile_id: AgentRuntimeProfileId::new(),
            model_provider_profile_id: None,
            projection_descriptor_id: AgentProviderProjectionDescriptorId::parse(
                "projection_unknown_runtime_v1",
            )
            .expect("a literal descriptor id is valid"),
            projection_overrides: AgentProviderProjectionOverrides::default(),
            configured_models: Vec::new(),
            account_alias: None,
            reasoning_effort: None,
            provider_options: crate::ProviderOptions::empty(),
            projection_fingerprint: None,
            status: AgentModelProviderBindingStatus::Draft,
            verification: ProjectionVerificationState {
                state: ProjectionEvidenceState::Unverified,
                descriptor_version: "0".to_string(),
                source_evidence_reference: None,
                runtime_evidence_reference: None,
                verified_at_ms: None,
            },
            revision: 1,
            created_at_ms: 0,
            updated_at_ms: 0,
            deleted_at_ms: None,
        }
    }
}

impl AgentModelProviderBinding {
    pub fn validate(&self) -> VibexResult<()> {
        if self.configured_models.len() > MAX_MODEL_COUNT {
            return Err(VibexError::validation(
                "agent_model_binding_limit_exceeded",
                "Agent provider binding contains too many model bindings",
            ));
        }
        if self.projection_overrides.non_secret_env.len() > MAX_ENV_COUNT {
            return Err(VibexError::validation(
                "agent_model_binding_env_limit_exceeded",
                "Agent provider binding contains too many environment overrides",
            ));
        }
        unique_non_empty(
            self.configured_models
                .iter()
                .map(|model| model.agent_model_id.as_str()),
            "agent_model_binding_duplicate",
            "Agent model ids must be non-empty and unique within a binding",
        )
    }

    pub fn validate_against_descriptor(
        &self,
        descriptor: &AgentProviderProjectionDescriptor,
    ) -> VibexResult<()> {
        self.validate()?;
        if self.agent_id != descriptor.route.agent_id
            || self.projection_descriptor_id != descriptor.id
        {
            return Err(VibexError::validation(
                "agent_projection_descriptor_mismatch",
                "Agent provider binding does not match the selected projection descriptor",
            ));
        }
        for model in self.configured_models.iter().filter(|model| model.enabled) {
            let supported = descriptor.model_interfaces.iter().any(|interface| {
                interface.wire_protocol_id == model.wire_protocol_id
                    && interface.sdk_adapter_id == model.sdk_adapter_id
            });
            if !supported {
                return Err(VibexError::validation(
                    "agent_model_interface_unsupported",
                    "model interface is not supported by the selected Agent projection descriptor",
                )
                .with_diagnostic("agentId", self.agent_id.as_str())
                .with_diagnostic("wireProtocolId", model.wire_protocol_id.as_str()));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentVersionCompatibility {
    Exact {
        adapter_version: Option<String>,
        agent_version: Option<String>,
        #[serde(default)]
        runtime_dependencies: BTreeMap<String, String>,
    },
    SemverRange {
        adapter_range: Option<String>,
        agent_range: Option<String>,
        #[serde(default)]
        runtime_dependency_ranges: BTreeMap<String, String>,
    },
    ManualVersionUnverified,
}

impl AgentVersionCompatibility {
    fn matches_exact(&self, identity: &AgentRuntimeVersionIdentity) -> bool {
        let Self::Exact {
            adapter_version,
            agent_version,
            runtime_dependencies,
        } = self
        else {
            return false;
        };
        optional_exact(
            adapter_version.as_deref(),
            identity.adapter_version.as_deref(),
        ) && optional_exact(agent_version.as_deref(), identity.agent_version.as_deref())
            && runtime_dependencies.iter().all(|(package, version)| {
                identity.runtime_dependencies.get(package) == Some(version)
            })
    }

    fn matches_range(&self, identity: &AgentRuntimeVersionIdentity) -> bool {
        let Self::SemverRange {
            adapter_range,
            agent_range,
            runtime_dependency_ranges,
        } = self
        else {
            return false;
        };
        optional_range_matches(
            adapter_range.as_deref(),
            identity.adapter_version.as_deref(),
        ) && optional_range_matches(agent_range.as_deref(), identity.agent_version.as_deref())
            && runtime_dependency_ranges.iter().all(|(package, range)| {
                identity
                    .runtime_dependencies
                    .get(package)
                    .is_some_and(|version| semver_matches(range, version))
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigOverlayStrategy {
    ClaudeEnvironment,
    CodexStableHome,
    DeepseekHarnessSettingsYaml,
    OpenCodeInlineProvider,
    GenericEnvironmentDescriptor,
    StructuredJsonOverlay,
    StructuredTomlOverlay,
    StructuredYamlOverlay,
    /// Cline CLI's `<CLINE_DATA_DIR>/settings/providers.json`. Retained so
    /// stored capability snapshots keep deserializing: Cline's ACP bridge reads
    /// `CLINE_PROVIDER`, `CLINE_MODEL`, and `CLINE_API_KEY` from the
    /// environment and never consumes this store, so no catalog descriptor
    /// projects it and Cline declares
    /// [`AgentProviderControl::FixedEndpoint`] instead.
    ClineProvidersJson,
    CrowCliYaml,
    DiracToml,
    FactoryDroidJson,
    GooseJson,
    GrokToml,
    HermesYaml,
    KiloInlineJson,
    KimiToml,
    MistralVibeToml,
    PiModelsJson,
    QwenCodeJson,
    StakpakToml,
    VtcodeToml,
    ZcodeJson,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentProviderControl {
    Environment {
        base_url_key: Option<String>,
    },
    ManagedConfigOverlay {
        strategy: ConfigOverlayStrategy,
    },
    /// The Agent owns the endpoint: it accepts no caller-supplied base URL, so
    /// the profile is pinned to `base_url` instead of projecting one. The
    /// endpoint control stays visible but is not editable.
    FixedEndpoint {
        base_url: String,
    },
    AdvertisedSessionOption {
        option_ids: Vec<String>,
    },
    AgentManaged,
    LocalModel,
    ServiceMarketplace,
    Unsupported,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentCredentialControl {
    Environment {
        secret_env_key: String,
        accepted_secret_kinds: Vec<ProviderSecretKind>,
    },
    ManagedConfigOverlay {
        strategy: ConfigOverlayStrategy,
    },
    AdvertisedAuthMethod {
        method_ids: Vec<String>,
    },
    OAuthAgentManaged,
    AgentManaged,
    Local,
    ServiceMarketplace,
    Unsupported,
    Unverified,
}

impl AgentCredentialControl {
    pub const fn automatically_projects_secret(&self) -> bool {
        matches!(
            self,
            Self::Environment { .. } | Self::ManagedConfigOverlay { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentModelControl {
    AcpSetModel,
    AcpConfigOption { aliases: Vec<String> },
    ProcessEnvironment { key: String },
    ManagedConfigOverlay { strategy: ConfigOverlayStrategy },
    AgentManaged,
    LocalModel,
    ServiceMarketplace,
    Unsupported,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelInterfaceDescriptor {
    pub wire_protocol_id: String,
    pub sdk_adapter_id: Option<String>,
    pub transport: String,
    #[serde(default)]
    pub integration_kind: AgentModelInterfaceIntegrationKind,
    pub user_selectable: bool,
    #[serde(default)]
    pub process_scoped: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentModelInterfaceIntegrationKind {
    #[default]
    Direct,
    Bridged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSwitchBehavior {
    LiveSessionConfig,
    RestartAndResume,
    RestartFreshAndBridge,
    AgentManaged,
    Unsupported,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionEvidenceReference {
    pub state: ProjectionEvidenceState,
    pub source_reference: Option<String>,
    pub runtime_reference: Option<String>,
    #[serde(default)]
    pub diagnostic_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProviderProjectionDescriptor {
    pub id: AgentProviderProjectionDescriptorId,
    pub descriptor_version: String,
    pub route: AgentRuntimeRouteKey,
    pub compatibility: AgentVersionCompatibility,
    pub provider_control: AgentProviderControl,
    pub credential_control: AgentCredentialControl,
    pub model_control: AgentModelControl,
    pub credential_kinds: Vec<AgentCredentialKind>,
    pub model_interfaces: Vec<AgentModelInterfaceDescriptor>,
    pub runtime_home_strategy: AgentRuntimeHomeStrategy,
    pub switch_behavior: ProviderSwitchBehavior,
    pub evidence: ProjectionEvidenceReference,
}

impl AgentProviderProjectionDescriptor {
    pub fn validate(&self) -> VibexResult<()> {
        if self.descriptor_version.trim().is_empty() {
            return Err(VibexError::validation(
                "agent_projection_descriptor_version_invalid",
                "projection descriptor version must not be empty",
            ));
        }
        if self.evidence.state == ProjectionEvidenceState::Verified
            && (self
                .evidence
                .source_reference
                .as_deref()
                .is_none_or(str::is_empty)
                || self
                    .evidence
                    .runtime_reference
                    .as_deref()
                    .is_none_or(str::is_empty))
        {
            return Err(VibexError::validation(
                "agent_projection_evidence_missing",
                "verified projection descriptors require source and runtime evidence references",
            ));
        }
        if self.evidence.state == ProjectionEvidenceState::Verified
            && self.evidence.diagnostic_code.is_some()
        {
            return Err(VibexError::validation(
                "agent_projection_verified_diagnostic_invalid",
                "verified projection descriptors cannot retain a conservative diagnostic",
            ));
        }
        if self
            .evidence
            .diagnostic_code
            .as_deref()
            .is_some_and(|code| {
                code.is_empty()
                    || code.len() > 96
                    || !code
                        .chars()
                        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
            })
        {
            return Err(VibexError::validation(
                "agent_projection_evidence_diagnostic_invalid",
                "projection evidence diagnostics must be bounded stable codes",
            ));
        }
        if self.credential_control.automatically_projects_secret()
            && self.credential_kinds.is_empty()
        {
            return Err(VibexError::validation(
                "agent_projection_credential_capability_missing",
                "secret-capable projection descriptors must declare credential capabilities",
            ));
        }
        let has_unverified_control =
            matches!(self.provider_control, AgentProviderControl::Unverified)
                || matches!(self.credential_control, AgentCredentialControl::Unverified)
                || matches!(self.model_control, AgentModelControl::Unverified);
        if (has_unverified_control || self.evidence.state == ProjectionEvidenceState::Unverified)
            && (!matches!(self.provider_control, AgentProviderControl::Unverified)
                || !matches!(self.credential_control, AgentCredentialControl::Unverified)
                || !matches!(self.model_control, AgentModelControl::Unverified)
                || self.evidence.state != ProjectionEvidenceState::Unverified
                || self.evidence.diagnostic_code.is_none()
                || self.switch_behavior != ProviderSwitchBehavior::Unverified
                || !self.credential_kinds.is_empty()
                || !self.model_interfaces.is_empty())
        {
            return Err(VibexError::validation(
                "agent_projection_unverified_contract_incomplete",
                "unverified descriptors must fail closed with one explicit conservative diagnostic",
            ));
        }
        if matches!(
            self.provider_control,
            AgentProviderControl::AgentManaged
                | AgentProviderControl::LocalModel
                | AgentProviderControl::ServiceMarketplace
                | AgentProviderControl::Unsupported
                | AgentProviderControl::Unverified
        ) && self.credential_control.automatically_projects_secret()
        {
            return Err(VibexError::validation(
                "agent_projection_secret_control_invalid",
                "managed, local, marketplace, unsupported and unverified descriptors cannot project Secrets automatically",
            ));
        }
        unique_non_empty(
            self.model_interfaces
                .iter()
                .map(|interface| interface.wire_protocol_id.as_str()),
            "agent_projection_model_interface_duplicate",
            "projection model interface ids must be non-empty and unique",
        )?;
        match &self.compatibility {
            AgentVersionCompatibility::Exact {
                adapter_version,
                agent_version,
                runtime_dependencies,
            } if adapter_version.is_none()
                && agent_version.is_none()
                && runtime_dependencies.is_empty() =>
            {
                return Err(VibexError::validation(
                    "agent_projection_exact_identity_empty",
                    "exact projection compatibility must pin at least one version component",
                ));
            }
            AgentVersionCompatibility::SemverRange {
                adapter_range,
                agent_range,
                runtime_dependency_ranges,
            } => {
                if adapter_range.is_none()
                    && agent_range.is_none()
                    && runtime_dependency_ranges.is_empty()
                {
                    return Err(VibexError::validation(
                        "agent_projection_range_identity_empty",
                        "range projection compatibility must constrain at least one version component",
                    ));
                }
                for range in adapter_range
                    .iter()
                    .chain(agent_range.iter())
                    .chain(runtime_dependency_ranges.values())
                {
                    VersionReq::parse(range).map_err(|_| {
                        VibexError::validation(
                            "agent_projection_version_range_invalid",
                            "projection descriptor contains an invalid semantic version range",
                        )
                    })?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionDescriptorMatch {
    Exact,
    SemverRange,
    Conservative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProviderProjectionResolution {
    pub descriptor: AgentProviderProjectionDescriptor,
    pub match_kind: ProjectionDescriptorMatch,
    pub diagnostic_code: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentProviderProjectionRegistry {
    descriptors: BTreeMap<AgentProviderProjectionDescriptorId, AgentProviderProjectionDescriptor>,
}

impl AgentProviderProjectionRegistry {
    pub fn builtin() -> VibexResult<Self> {
        let mut registry = Self::default();
        for descriptor in [
            claude_projection_descriptor()?,
            codex_projection_descriptor()?,
            opencode_projection_descriptor()?,
            zcode_projection_descriptor()?,
        ] {
            registry.register(descriptor)?;
        }
        for descriptor in catalog_projection_descriptors()? {
            registry.register(descriptor)?;
        }
        Ok(registry)
    }

    pub fn register(&mut self, descriptor: AgentProviderProjectionDescriptor) -> VibexResult<()> {
        descriptor.validate()?;
        if self.descriptors.contains_key(&descriptor.id) {
            return Err(VibexError::conflict(
                "agent_projection_descriptor_duplicate",
                "projection descriptor id is already registered",
            ));
        }
        for existing in self
            .descriptors
            .values()
            .filter(|existing| existing.route == descriptor.route)
        {
            match (&existing.compatibility, &descriptor.compatibility) {
                (
                    AgentVersionCompatibility::Exact { .. },
                    AgentVersionCompatibility::Exact { .. },
                ) if existing.compatibility == descriptor.compatibility => {
                    return Err(VibexError::conflict(
                        "agent_projection_exact_identity_duplicate",
                        "an exact Agent projection identity is already registered",
                    ));
                }
                (
                    AgentVersionCompatibility::SemverRange { .. },
                    AgentVersionCompatibility::SemverRange { .. },
                ) => {
                    // One range per route keeps overlap impossible. Add another
                    // only after the registry gains a proven overlap checker.
                    return Err(VibexError::conflict(
                        "agent_projection_version_range_overlap",
                        "only one non-overlapping version range may be registered per Agent route",
                    ));
                }
                _ => {}
            }
        }
        self.descriptors.insert(descriptor.id.clone(), descriptor);
        Ok(())
    }

    pub fn descriptor(
        &self,
        id: &AgentProviderProjectionDescriptorId,
    ) -> Option<&AgentProviderProjectionDescriptor> {
        self.descriptors.get(id)
    }

    pub fn descriptors(&self) -> impl Iterator<Item = &AgentProviderProjectionDescriptor> {
        self.descriptors.values()
    }

    pub fn descriptors_for_agent(
        &self,
        agent_id: &AgentId,
    ) -> impl Iterator<Item = &AgentProviderProjectionDescriptor> {
        self.descriptors
            .values()
            .filter(move |descriptor| &descriptor.route.agent_id == agent_id)
    }

    pub fn resolve(
        &self,
        identity: &AgentRuntimeVersionIdentity,
    ) -> VibexResult<AgentProviderProjectionResolution> {
        if !identity.is_automatic_projection_eligible() {
            return Ok(conservative_resolution(
                identity,
                "agent_projection_version_untrusted",
            ));
        }
        // A Model interface a newer Adapter introduced is dropped for an older
        // installation before anything else reads the descriptor: its bundled
        // runtime validates the whole route and refuses one that names a
        // protocol it does not know, so the choice must not reach the provider
        // editor and a binding already holding it must fail its projection
        // check instead of writing an unusable route.
        let for_identity = |descriptor: &AgentProviderProjectionDescriptor| {
            let mut descriptor = descriptor.clone();
            descriptor.model_interfaces.retain(|interface| {
                crate::model_interface_available_for_adapter(
                    identity.route.agent_id.as_str(),
                    interface.wire_protocol_id.as_str(),
                    identity.adapter_version.as_deref(),
                    identity.agent_version.as_deref(),
                )
            });
            descriptor
        };
        if let Some(descriptor) = self.descriptors.values().find(|descriptor| {
            descriptor.route == identity.route && descriptor.compatibility.matches_exact(identity)
        }) {
            return Ok(AgentProviderProjectionResolution {
                descriptor: for_identity(descriptor),
                match_kind: ProjectionDescriptorMatch::Exact,
                diagnostic_code: descriptor.evidence.diagnostic_code.clone(),
            });
        }
        if let Some(descriptor) = self.descriptors.values().find(|descriptor| {
            descriptor.route == identity.route && descriptor.compatibility.matches_range(identity)
        }) {
            return Ok(AgentProviderProjectionResolution {
                descriptor: for_identity(descriptor),
                match_kind: ProjectionDescriptorMatch::SemverRange,
                diagnostic_code: descriptor.evidence.diagnostic_code.clone(),
            });
        }
        let has_route = self
            .descriptors
            .values()
            .any(|descriptor| descriptor.route == identity.route);
        Ok(conservative_resolution(
            identity,
            if has_route {
                "agent_projection_version_mismatch"
            } else {
                "agent_projection_descriptor_missing"
            },
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentProjectionFormControl {
    Endpoint,
    ApiKey,
    OAuth,
    Aws,
    Gcp,
    Azure,
    Snowflake,
    WireProtocol,
    Model,
    AgentManagedStatus,
    LocalRuntime,
    ServiceMarketplace,
    AdvancedCustomEnvironment,
    ProjectionPreview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionAuthState {
    Missing,
    Ready,
    AgentManaged,
    NotApplicable,
    Unsupported,
    Unknown,
}

/// Display-safe capability snapshot shared by Desktop, Web, and Mobile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProviderProjectionCapability {
    pub schema_version: u32,
    pub agent_id: AgentId,
    pub adapter_id: AcpAdapterId,
    pub descriptor_id: Option<AgentProviderProjectionDescriptorId>,
    pub descriptor_version: String,
    pub detected_agent_version: Option<String>,
    pub detected_adapter_version: Option<String>,
    pub match_kind: ProjectionDescriptorMatch,
    pub evidence_state: ProjectionEvidenceState,
    pub auth_state: ProjectionAuthState,
    pub provider_control: AgentProviderControl,
    pub credential_control: AgentCredentialControl,
    pub model_control: AgentModelControl,
    pub credential_kinds: Vec<AgentCredentialKind>,
    pub model_interfaces: Vec<AgentModelInterfaceDescriptor>,
    pub switch_behavior: ProviderSwitchBehavior,
    pub form_controls: Vec<AgentProjectionFormControl>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

impl AgentProviderProjectionCapability {
    /// The endpoint the Agent pins itself to when it accepts no caller-supplied
    /// base URL. Clients keep the endpoint control visible but read-only.
    pub fn fixed_endpoint(&self) -> Option<&str> {
        match &self.provider_control {
            AgentProviderControl::FixedEndpoint { base_url } => Some(base_url.as_str()),
            _ => None,
        }
    }

    /// Whether the projection carries user-configured request headers.
    ///
    /// Only the Agents whose projected config owns a header map read them —
    /// the DeepSeek Harness route and OpenCode's inline provider. Every other
    /// Agent's projection has no place to write one, so the Provider editor
    /// hides the rows instead of storing headers the request never carries.
    pub fn supports_request_headers(&self) -> bool {
        let owns_headers = |control: &AgentProviderControl| match control {
            AgentProviderControl::ManagedConfigOverlay { strategy } => matches!(
                strategy,
                ConfigOverlayStrategy::DeepseekHarnessSettingsYaml
                    | ConfigOverlayStrategy::OpenCodeInlineProvider
            ),
            _ => false,
        };
        owns_headers(&self.provider_control)
    }

    pub fn from_resolution(
        identity: &AgentRuntimeVersionIdentity,
        resolution: &AgentProviderProjectionResolution,
        auth_state: ProjectionAuthState,
    ) -> Self {
        let descriptor = &resolution.descriptor;
        let mut controls = BTreeSet::from([AgentProjectionFormControl::ProjectionPreview]);
        match descriptor.provider_control {
            AgentProviderControl::Environment { .. }
            | AgentProviderControl::ManagedConfigOverlay { .. }
            | AgentProviderControl::FixedEndpoint { .. } => {
                controls.insert(AgentProjectionFormControl::Endpoint);
            }
            AgentProviderControl::AgentManaged => {
                controls.insert(AgentProjectionFormControl::AgentManagedStatus);
            }
            AgentProviderControl::LocalModel => {
                controls.insert(AgentProjectionFormControl::LocalRuntime);
            }
            AgentProviderControl::ServiceMarketplace => {
                controls.insert(AgentProjectionFormControl::ServiceMarketplace);
            }
            AgentProviderControl::AdvertisedSessionOption { .. }
            | AgentProviderControl::Unsupported
            | AgentProviderControl::Unverified => {}
        }
        for kind in &descriptor.credential_kinds {
            controls.insert(match kind {
                AgentCredentialKind::ApiKey => AgentProjectionFormControl::ApiKey,
                AgentCredentialKind::OAuth => AgentProjectionFormControl::OAuth,
                AgentCredentialKind::Aws => AgentProjectionFormControl::Aws,
                AgentCredentialKind::Gcp => AgentProjectionFormControl::Gcp,
                AgentCredentialKind::Azure => AgentProjectionFormControl::Azure,
                AgentCredentialKind::Snowflake => AgentProjectionFormControl::Snowflake,
                AgentCredentialKind::Local => AgentProjectionFormControl::LocalRuntime,
                AgentCredentialKind::ManagedSubscription => {
                    AgentProjectionFormControl::AgentManagedStatus
                }
            });
        }
        if !descriptor.model_interfaces.is_empty() {
            controls.insert(AgentProjectionFormControl::Model);
        }
        if descriptor
            .model_interfaces
            .iter()
            .any(|interface| interface.user_selectable)
        {
            controls.insert(AgentProjectionFormControl::WireProtocol);
        }
        if resolution.match_kind == ProjectionDescriptorMatch::Conservative {
            controls.clear();
            controls.insert(AgentProjectionFormControl::AdvancedCustomEnvironment);
            controls.insert(AgentProjectionFormControl::ProjectionPreview);
        }
        Self {
            schema_version: PROVIDER_PROJECTION_SCHEMA_VERSION,
            agent_id: identity.route.agent_id.clone(),
            adapter_id: identity.route.adapter_id.clone(),
            descriptor_id: (resolution.match_kind != ProjectionDescriptorMatch::Conservative)
                .then(|| descriptor.id.clone()),
            descriptor_version: descriptor.descriptor_version.clone(),
            detected_agent_version: identity.agent_version.clone(),
            detected_adapter_version: identity.adapter_version.clone(),
            match_kind: resolution.match_kind,
            evidence_state: descriptor.evidence.state,
            auth_state,
            provider_control: descriptor.provider_control.clone(),
            credential_control: descriptor.credential_control.clone(),
            model_control: descriptor.model_control.clone(),
            credential_kinds: descriptor.credential_kinds.clone(),
            model_interfaces: descriptor.model_interfaces.clone(),
            switch_behavior: descriptor.switch_behavior,
            form_controls: controls.into_iter().collect(),
            diagnostics: resolution
                .diagnostic_code
                .iter()
                .map(|code| ProviderBindingMetadata {
                    key: "projectionStatus".to_string(),
                    value: code.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionTargetKind {
    Environment,
    ManagedOverlay,
    AcpModel,
    AcpConfigOption,
    AdvertisedAuthMethod,
    AgentManaged,
    LocalRuntime,
    ServiceMarketplace,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionTargetPreview {
    pub field: String,
    pub target_kind: ProjectionTargetKind,
    pub target: String,
    pub value_preview: String,
    pub secret: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionOverlayPreview {
    pub relative_path: String,
    pub format: String,
    pub contains_secret_reference: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProviderProjectionPreview {
    pub schema_version: u32,
    pub binding_id: AgentModelProviderBindingId,
    pub descriptor_id: AgentProviderProjectionDescriptorId,
    pub descriptor_version: String,
    pub evidence_state: ProjectionEvidenceState,
    pub command_summary: String,
    pub targets: Vec<ProjectionTargetPreview>,
    pub overlay_files: Vec<ProjectionOverlayPreview>,
    pub effective_model: Option<String>,
    pub switch_behavior: ProviderSwitchBehavior,
    pub projection_fingerprint: String,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProviderProjectionCapabilityRequest {
    pub runtime_profile_id: AgentRuntimeProfileId,
    #[serde(default)]
    pub binding_id: Option<AgentModelProviderBindingId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProviderProjectionPreviewRequest {
    pub binding_id: AgentModelProviderBindingId,
    /// Stable, non-path workspace identity. Callers must not send a native
    /// filesystem path across Remote boundaries.
    pub workspace_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderBindingListRequest {
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    #[serde(default)]
    pub model_provider_profile_id: Option<ModelProviderProfileId>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ManagedProjectionOverlay {
    pub relative_path: String,
    pub format: String,
    pub content: String,
    pub contains_secret_reference: bool,
}

impl fmt::Debug for ManagedProjectionOverlay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedProjectionOverlay")
            .field("relative_path", &self.relative_path)
            .field("format", &self.format)
            .field("content", &"[redacted]")
            .field("contains_secret_reference", &self.contains_secret_reference)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSecretEnvReference {
    pub key: String,
    pub credential_id: RequestId,
    pub secret_reference: ProjectionSecretReference,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AgentProviderProjectionPlan {
    pub binding_id: AgentModelProviderBindingId,
    pub descriptor_id: AgentProviderProjectionDescriptorId,
    pub non_secret_env: BTreeMap<String, String>,
    pub secret_env: Vec<ProjectionSecretEnvReference>,
    pub overlay_files: Vec<ManagedProjectionOverlay>,
    /// Optional agent-specific home override populated during materialization.
    /// The value is an environment key, never a filesystem path.
    pub runtime_home_env_key: Option<String>,
    /// Relative launch-argument templates. `{projectionRoot}` is replaced
    /// with the private binding/workspace root immediately before spawn.
    pub process_args: Vec<String>,
    pub session_config: Vec<ProviderBindingMetadata>,
    pub effective_model: Option<String>,
    pub switch_behavior: ProviderSwitchBehavior,
    pub fingerprint: String,
    pub preview: AgentProviderProjectionPreview,
    pub diagnostics: Vec<ProviderBindingMetadata>,
    /// Environment keys whose value is the current Vibex session identity,
    /// referenced by the projected route's dynamic request headers. The launch
    /// fills them from the session that owns the Agent process and only when
    /// that process is session-scoped.
    pub session_env_keys: Vec<String>,
    /// The local placeholder the Agent sends to the Vibex gateway.
    ///
    /// The hidden gateway Profile authenticates to the upstream with the Key it
    /// selects, and ignores whatever the Agent sends. It therefore stores no
    /// credential, which reads as "missing" to the resolver and refused the
    /// launch with `agent_projection_secret_missing`. When set, the resolver
    /// uses this value for every Secret target instead of looking one up.
    pub local_placeholder_secret: Option<String>,
}

impl fmt::Debug for AgentProviderProjectionPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentProviderProjectionPlan")
            .field("binding_id", &self.binding_id)
            .field("descriptor_id", &self.descriptor_id)
            .field(
                "non_secret_env_keys",
                &self.non_secret_env.keys().collect::<Vec<_>>(),
            )
            .field(
                "secret_env_keys",
                &self
                    .secret_env
                    .iter()
                    .map(|entry| &entry.key)
                    .collect::<Vec<_>>(),
            )
            .field("overlay_files", &self.overlay_files)
            .field("runtime_home_env_key", &self.runtime_home_env_key)
            .field(
                "process_args",
                &format_args!("{} args", self.process_args.len()),
            )
            .field("session_config", &self.session_config)
            .field("effective_model", &self.effective_model)
            .field("switch_behavior", &self.switch_behavior)
            .field("fingerprint", &self.fingerprint)
            .field("diagnostics", &self.diagnostics)
            .field("session_env_keys", &self.session_env_keys)
            // The value is a local placeholder, never a real Secret, but it is
            // still printed as a flag: a debug dump should not look like a
            // resolved credential.
            .field(
                "local_placeholder_secret",
                &self.local_placeholder_secret.is_some(),
            )
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderProfileCreateRequest {
    pub display_name: String,
    pub vendor_hint: Option<String>,
    pub endpoints: Vec<ModelProviderEndpoint>,
    pub proxy_policy: ModelProviderProxyPolicy,
    pub credentials: Vec<ModelProviderCredentialReference>,
    pub configured_models: Vec<ModelProviderCatalogEntry>,
    pub default_model_id: Option<String>,
    pub headers: Vec<ModelProviderHeaderReference>,
    pub status: ModelProviderProfileStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderProfileUpdateRequest {
    pub profile: ModelProviderProfile,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeProfileCreateRequest {
    pub version_identity: AgentRuntimeVersionIdentity,
    pub command: String,
    pub args: Vec<String>,
    pub safe_env_references: Vec<AcpProviderEnvReference>,
    pub cwd_template: Option<String>,
    pub process_strategy: AcpProcessStrategy,
    pub runtime_home_strategy: AgentRuntimeHomeStrategy,
    pub host_capabilities: AgentHostCapabilities,
    pub resource_policy: AgentRuntimeResourcePolicy,
    /// The ACP catalogue this launch offers, omitted by callers that offer none.
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub modes: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub disabled_tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeProfileUpdateRequest {
    pub profile: AgentRuntimeProfile,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderBindingCreateRequest {
    pub agent_id: AgentId,
    pub runtime_profile_id: AgentRuntimeProfileId,
    pub model_provider_profile_id: ModelProviderProfileId,
    pub projection_descriptor_id: AgentProviderProjectionDescriptorId,
    pub projection_overrides: AgentProviderProjectionOverrides,
    pub configured_models: Vec<AgentConfiguredModelBinding>,
    /// Agent-scoped launch fields, omitted by callers that have none to set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default = "default_provider_options")]
    pub provider_options: crate::ProviderOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderBindingUpdateRequest {
    pub binding: AgentModelProviderBinding,
    pub expected_revision: i64,
}

/// Adds one API key to a shared Provider Profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddModelProviderCredentialRequest {
    pub model_provider_profile_id: ModelProviderProfileId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The key itself. Never logged, never persisted.
    pub value: String,
}

/// Removes one key from a shared Provider Profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveModelProviderCredentialRequest {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
}

/// Reads one shared Provider credential's stored Secret.
///
/// The response is only produced for an explicit reveal; ordinary reads carry
/// the masked hint instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderCredentialSecretRequest {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderCredentialSecretResponse {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
    pub display_name: String,
    pub masked_hint: String,
    pub value: Option<String>,
}

impl fmt::Debug for ModelProviderCredentialSecretResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelProviderCredentialSecretResponse")
            .field("model_provider_profile_id", &self.model_provider_profile_id)
            .field("credential_id", &self.credential_id)
            .field("display_name", &self.display_name)
            .field("masked_hint", &self.masked_hint)
            .field("value", &self.value.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProviderCredentialSecretMutationRequest {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
    pub touched: bool,
    pub clear: bool,
    pub value: Option<String>,
}

impl fmt::Debug for ProviderCredentialSecretMutationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderCredentialSecretMutationRequest")
            .field("model_provider_profile_id", &self.model_provider_profile_id)
            .field("credential_id", &self.credential_id)
            .field("touched", &self.touched)
            .field("clear", &self.clear)
            .field("value", &self.value.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

fn claude_projection_descriptor() -> VibexResult<AgentProviderProjectionDescriptor> {
    Ok(AgentProviderProjectionDescriptor {
        id: AgentProviderProjectionDescriptorId::parse(CLAUDE_PROJECTION_DESCRIPTOR_ID)?,
        descriptor_version: "1".to_string(),
        route: route(CLAUDE_AGENT_ID, CLAUDE_ADAPTER_ID)?,
        compatibility: AgentVersionCompatibility::SemverRange {
            adapter_range: Some(CLAUDE_COMPATIBLE_ADAPTER_VERSION_REQUIREMENT.to_string()),
            agent_range: None,
            runtime_dependency_ranges: BTreeMap::new(),
        },
        provider_control: AgentProviderControl::Environment {
            base_url_key: Some("ANTHROPIC_BASE_URL".to_string()),
        },
        credential_control: AgentCredentialControl::Environment {
            secret_env_key: "ANTHROPIC_API_KEY".to_string(),
            accepted_secret_kinds: vec![ProviderSecretKind::ApiKey, ProviderSecretKind::AuthToken],
        },
        model_control: AgentModelControl::AcpConfigOption {
            aliases: vec!["model".to_string()],
        },
        credential_kinds: vec![
            AgentCredentialKind::ApiKey,
            AgentCredentialKind::ManagedSubscription,
        ],
        // The bundled Claude Code CLI also speaks Bedrock and Vertex, but both
        // authenticate through AWS SigV4 / Google ADC rather than an API key,
        // and `ModelProviderProfile` secrets cannot carry either. They are
        // therefore deliberately absent instead of selectable-but-unusable.
        model_interfaces: vec![AgentModelInterfaceDescriptor {
            wire_protocol_id: WIRE_PROTOCOL_ANTHROPIC_MESSAGES.to_string(),
            sdk_adapter_id: None,
            transport: "https".to_string(),
            integration_kind: AgentModelInterfaceIntegrationKind::Direct,
            user_selectable: false,
            process_scoped: false,
        }],
        runtime_home_strategy: AgentRuntimeHomeStrategy::VibexPrivate,
        switch_behavior: ProviderSwitchBehavior::RestartAndResume,
        evidence: verified_evidence(
            "provider-config/claude-environment-v1",
            &format!("acp-smoke/claude-agent-acp-{CLAUDE_ADAPTER_VERSION}"),
        ),
    })
}

fn codex_projection_descriptor() -> VibexResult<AgentProviderProjectionDescriptor> {
    Ok(AgentProviderProjectionDescriptor {
        id: AgentProviderProjectionDescriptorId::parse(CODEX_PROJECTION_DESCRIPTOR_ID)?,
        descriptor_version: "1".to_string(),
        route: route(CODEX_AGENT_ID, CODEX_ADAPTER_ID)?,
        compatibility: AgentVersionCompatibility::SemverRange {
            // Managed installation records expose the Adapter package version.
            // Use it as the compatible runtime identity rather than requiring
            // transient nested Codex package metadata at process launch.
            adapter_range: Some(CODEX_COMPATIBLE_ADAPTER_VERSION_REQUIREMENT.to_string()),
            agent_range: None,
            runtime_dependency_ranges: BTreeMap::new(),
        },
        provider_control: AgentProviderControl::ManagedConfigOverlay {
            strategy: ConfigOverlayStrategy::CodexStableHome,
        },
        credential_control: AgentCredentialControl::Environment {
            secret_env_key: "CODEX_API_KEY".to_string(),
            accepted_secret_kinds: vec![ProviderSecretKind::ApiKey],
        },
        model_control: AgentModelControl::AcpConfigOption {
            aliases: vec!["model".to_string()],
        },
        credential_kinds: vec![
            AgentCredentialKind::ApiKey,
            AgentCredentialKind::ManagedSubscription,
        ],
        model_interfaces: vec![AgentModelInterfaceDescriptor {
            wire_protocol_id: WIRE_PROTOCOL_OPENAI_RESPONSES.to_string(),
            sdk_adapter_id: None,
            transport: "https".to_string(),
            integration_kind: AgentModelInterfaceIntegrationKind::Direct,
            user_selectable: false,
            process_scoped: false,
        }],
        runtime_home_strategy: AgentRuntimeHomeStrategy::VibexPrivate,
        switch_behavior: ProviderSwitchBehavior::RestartAndResume,
        evidence: verified_evidence(
            "provider-config/codex-stable-home-v1",
            &format!("acp-smoke/codex-acp-{CODEX_ADAPTER_VERSION}-codex-{CODEX_RUNTIME_VERSION}"),
        ),
    })
}

fn opencode_projection_descriptor() -> VibexResult<AgentProviderProjectionDescriptor> {
    Ok(AgentProviderProjectionDescriptor {
        id: AgentProviderProjectionDescriptorId::parse(OPENCODE_PROJECTION_DESCRIPTOR_ID)?,
        descriptor_version: "1".to_string(),
        route: route(OPENCODE_AGENT_ID, OPENCODE_ADAPTER_ID)?,
        compatibility: AgentVersionCompatibility::SemverRange {
            adapter_range: None,
            agent_range: Some(OPENCODE_COMPATIBLE_VERSION_REQUIREMENT.to_string()),
            runtime_dependency_ranges: BTreeMap::new(),
        },
        provider_control: AgentProviderControl::ManagedConfigOverlay {
            strategy: ConfigOverlayStrategy::OpenCodeInlineProvider,
        },
        credential_control: AgentCredentialControl::Environment {
            secret_env_key: "VIBEX_OPENCODE_PROVIDER_API_KEY".to_string(),
            accepted_secret_kinds: vec![ProviderSecretKind::ApiKey, ProviderSecretKind::AuthToken],
        },
        model_control: AgentModelControl::ManagedConfigOverlay {
            strategy: ConfigOverlayStrategy::OpenCodeInlineProvider,
        },
        credential_kinds: vec![AgentCredentialKind::ApiKey],
        model_interfaces: vec![
            interface(WIRE_PROTOCOL_OPENAI_RESPONSES, "@ai-sdk/openai"),
            interface(
                WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
                "@ai-sdk/openai-compatible",
            ),
            interface(WIRE_PROTOCOL_ANTHROPIC_MESSAGES, "@ai-sdk/anthropic"),
            interface(WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI, "@ai-sdk/google"),
            interface(WIRE_PROTOCOL_GOOGLE_VERTEX, "@ai-sdk/google-vertex"),
            interface(WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE, "@ai-sdk/amazon-bedrock"),
        ],
        runtime_home_strategy: AgentRuntimeHomeStrategy::VibexPrivate,
        switch_behavior: ProviderSwitchBehavior::RestartAndResume,
        evidence: verified_evidence(
            "provider-config/opencode-inline-provider-v1",
            &format!("acp-smoke/opencode-{OPENCODE_LAST_VERIFIED_VERSION}"),
        ),
    })
}

fn zcode_projection_descriptor() -> VibexResult<AgentProviderProjectionDescriptor> {
    Ok(AgentProviderProjectionDescriptor {
        id: AgentProviderProjectionDescriptorId::parse(ZCODE_PROJECTION_DESCRIPTOR_ID)?,
        descriptor_version: "1".to_string(),
        route: route(ZCODE_AGENT_ID, ZCODE_ADAPTER_ID)?,
        compatibility: AgentVersionCompatibility::Exact {
            adapter_version: Some(ZCODE_ADAPTER_VERSION.to_string()),
            agent_version: None,
            runtime_dependencies: BTreeMap::new(),
        },
        provider_control: AgentProviderControl::ManagedConfigOverlay {
            strategy: ConfigOverlayStrategy::ZcodeJson,
        },
        credential_control: AgentCredentialControl::Environment {
            secret_env_key: "ANTHROPIC_API_KEY".to_string(),
            accepted_secret_kinds: vec![ProviderSecretKind::ApiKey, ProviderSecretKind::AuthToken],
        },
        model_control: AgentModelControl::ManagedConfigOverlay {
            strategy: ConfigOverlayStrategy::ZcodeJson,
        },
        credential_kinds: vec![AgentCredentialKind::ApiKey],
        model_interfaces: vec![
            interface(WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS, "openai-compatible"),
            interface(WIRE_PROTOCOL_ANTHROPIC_MESSAGES, "anthropic"),
        ],
        runtime_home_strategy: AgentRuntimeHomeStrategy::VibexPrivate,
        switch_behavior: ProviderSwitchBehavior::RestartAndResume,
        evidence: ProjectionEvidenceReference {
            state: ProjectionEvidenceState::Documented,
            source_reference: Some(
                "zcode-acp-server@0.72.0/provider-registry-and-runtime-model".to_string(),
            ),
            runtime_reference: None,
            diagnostic_code: Some("agent_projection_runtime_verification_required".to_string()),
        },
    })
}

fn interface(wire_protocol_id: &str, sdk_adapter_id: &str) -> AgentModelInterfaceDescriptor {
    AgentModelInterfaceDescriptor {
        wire_protocol_id: wire_protocol_id.to_string(),
        sdk_adapter_id: Some(sdk_adapter_id.to_string()),
        transport: "https".to_string(),
        integration_kind: AgentModelInterfaceIntegrationKind::Direct,
        user_selectable: true,
        process_scoped: true,
    }
}

fn verified_evidence(source: &str, runtime: &str) -> ProjectionEvidenceReference {
    ProjectionEvidenceReference {
        state: ProjectionEvidenceState::Verified,
        source_reference: Some(source.to_string()),
        runtime_reference: Some(runtime.to_string()),
        diagnostic_code: None,
    }
}

fn route(agent_id: &str, adapter_id: &str) -> VibexResult<AgentRuntimeRouteKey> {
    Ok(AgentRuntimeRouteKey {
        agent_id: AgentId::parse(agent_id)?,
        transport_kind: TransportKind::Acp,
        adapter_id: AcpAdapterId::parse(adapter_id)?,
    })
}

fn conservative_resolution(
    identity: &AgentRuntimeVersionIdentity,
    diagnostic_code: &str,
) -> AgentProviderProjectionResolution {
    let safe_id = identity
        .route
        .agent_id
        .as_str()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect::<String>();
    let id = AgentProviderProjectionDescriptorId::parse(format!(
        "projection_unverified_{}",
        if safe_id.is_empty() {
            "agent"
        } else {
            &safe_id
        }
    ))
    .expect("sanitized conservative descriptor ids are valid");
    AgentProviderProjectionResolution {
        descriptor: AgentProviderProjectionDescriptor {
            id,
            descriptor_version: "conservative-v1".to_string(),
            route: identity.route.clone(),
            compatibility: AgentVersionCompatibility::ManualVersionUnverified,
            provider_control: AgentProviderControl::Unverified,
            credential_control: AgentCredentialControl::Unverified,
            model_control: AgentModelControl::Unverified,
            credential_kinds: Vec::new(),
            model_interfaces: Vec::new(),
            runtime_home_strategy: AgentRuntimeHomeStrategy::None,
            switch_behavior: ProviderSwitchBehavior::Unverified,
            evidence: ProjectionEvidenceReference {
                state: ProjectionEvidenceState::Unverified,
                source_reference: None,
                runtime_reference: None,
                diagnostic_code: Some(diagnostic_code.to_string()),
            },
        },
        match_kind: ProjectionDescriptorMatch::Conservative,
        diagnostic_code: Some(diagnostic_code.to_string()),
    }
}

fn optional_exact(expected: Option<&str>, actual: Option<&str>) -> bool {
    expected.is_none_or(|expected| actual == Some(expected))
}

fn optional_range_matches(range: Option<&str>, actual: Option<&str>) -> bool {
    range.is_none_or(|range| actual.is_some_and(|actual| semver_matches(range, actual)))
}

fn semver_matches(range: &str, version: &str) -> bool {
    VersionReq::parse(range)
        .ok()
        .zip(Version::parse(version).ok())
        .is_some_and(|(range, version)| range.matches(&version))
}

fn validate_display_name(value: &str, code: &'static str) -> VibexResult<()> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_DISPLAY_NAME_LEN || value.chars().any(char::is_control)
    {
        return Err(VibexError::validation(
            code,
            "display name must be non-empty, bounded and free of control characters",
        ));
    }
    Ok(())
}

fn unique_non_empty<'a>(
    values: impl Iterator<Item = &'a str>,
    code: &'static str,
    message: &'static str,
) -> VibexResult<()> {
    let mut seen = BTreeSet::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() || !seen.insert(value) {
            return Err(VibexError::validation(code, message));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin_identity(agent: &str) -> AgentRuntimeVersionIdentity {
        match agent {
            "claude" => AgentRuntimeVersionIdentity {
                route: route(CLAUDE_AGENT_ID, CLAUDE_ADAPTER_ID).unwrap(),
                adapter_version: Some(CLAUDE_ADAPTER_VERSION.to_string()),
                agent_version: None,
                runtime_dependencies: BTreeMap::new(),
                source: AgentVersionSource::Managed,
            },
            "codex" => AgentRuntimeVersionIdentity {
                route: route(CODEX_AGENT_ID, CODEX_ADAPTER_ID).unwrap(),
                adapter_version: Some(CODEX_ADAPTER_VERSION.to_string()),
                agent_version: Some(CODEX_RUNTIME_VERSION.to_string()),
                runtime_dependencies: BTreeMap::from([(
                    CODEX_RUNTIME_PACKAGE.to_string(),
                    CODEX_RUNTIME_VERSION.to_string(),
                )]),
                source: AgentVersionSource::Managed,
            },
            "opencode" => AgentRuntimeVersionIdentity {
                route: route(OPENCODE_AGENT_ID, OPENCODE_ADAPTER_ID).unwrap(),
                adapter_version: None,
                agent_version: Some(OPENCODE_LAST_VERIFIED_VERSION.to_string()),
                runtime_dependencies: BTreeMap::new(),
                source: AgentVersionSource::Detected,
            },
            _ => unreachable!(),
        }
    }

    #[test]
    fn credential_union_covers_all_required_kinds() {
        let credentials = vec![
            AgentCredential::ApiKey {
                secret: ProjectionSecretReference {
                    id: RequestId::new(),
                    backend: ProviderSecretBackend::Placeholder,
                    setup_state: ProviderSecretSetupState::Missing,
                    lookup_key: "secret-ref".to_string(),
                    redacted_hint: "missing".to_string(),
                    revision: 1,
                    legacy_secret_reference_id: None,
                },
                target_hint: None,
            },
            AgentCredential::OAuth {
                account_reference: None,
                host_mediated: true,
            },
            AgentCredential::Aws {
                profile: None,
                region: None,
                secret: None,
            },
            AgentCredential::Gcp {
                project: None,
                location: None,
                credential: None,
            },
            AgentCredential::Azure {
                resource: None,
                deployment: None,
                api_version: None,
                credential: None,
            },
            AgentCredential::Snowflake {
                connection: None,
                auth_method: None,
                credential: None,
            },
            AgentCredential::Local { runtime: None },
            AgentCredential::ManagedSubscription {
                account_reference: None,
            },
        ];
        assert_eq!(
            credentials
                .iter()
                .map(AgentCredential::kind)
                .collect::<Vec<_>>(),
            AgentCredentialKind::ALL
        );
        for credential in credentials {
            let encoded = serde_json::to_string(&credential).unwrap();
            let decoded: AgentCredential = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, credential);
        }
    }

    #[test]
    fn provider_models_do_not_own_a_global_wire_protocol() {
        let model = ModelProviderCatalogEntry {
            id: "shared-model".to_string(),
            display_name: None,
            enabled: true,
            metadata: Vec::new(),
            capabilities: crate::ProviderModelCapabilities::default(),
        };
        let encoded = serde_json::to_string(&model).unwrap();
        assert!(!encoded.contains("wire"));
        assert!(!encoded.contains("sdk"));
    }

    #[test]
    fn undeclared_model_capabilities_round_trip_as_absent() {
        let entry = ModelProviderCatalogEntry {
            id: "shared-model".to_string(),
            display_name: None,
            enabled: true,
            metadata: Vec::new(),
            capabilities: crate::ProviderModelCapabilities::default(),
        };

        // An undeclared capability must not serialize, so it can never be read
        // back as an explicit "unsupported".
        let encoded = serde_json::to_string(&entry).unwrap();
        assert!(!encoded.contains("reasoning"), "{encoded}");
        assert!(!encoded.contains("imageInput"), "{encoded}");
        assert!(!encoded.contains("contextTokens"), "{encoded}");

        // Rows written before this field existed still load.
        let legacy: ModelProviderCatalogEntry = serde_json::from_str(
            r#"{"id":"shared-model","displayName":null,"enabled":true,"metadata":[]}"#,
        )
        .unwrap();
        assert_eq!(legacy, entry);
        assert!(legacy.capabilities.is_empty());
    }

    #[test]
    fn declared_model_capabilities_round_trip() {
        let entry = ModelProviderCatalogEntry {
            id: "shared-model".to_string(),
            display_name: None,
            enabled: true,
            metadata: Vec::new(),
            capabilities: crate::ProviderModelCapabilities {
                reasoning: Some(true),
                image_input: Some(true),
                context_tokens: Some(200_000),
                ..Default::default()
            },
        };

        let encoded = serde_json::to_string(&entry).unwrap();
        let decoded: ModelProviderCatalogEntry = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, entry);
        assert!(!decoded.capabilities.is_empty());
        // A declared `false` stays distinguishable from "not declared".
        assert_eq!(decoded.capabilities.pdf_input, None);
    }

    #[test]
    fn model_provider_endpoints_select_protocol_specific_urls_and_validate_ids() {
        let mut profile = ModelProviderProfile {
            id: ModelProviderProfileId::new(),
            legacy_provider_profile_id: None,
            display_name: "Shared gateway".to_string(),
            vendor_hint: None,
            endpoints: vec![
                ModelProviderEndpoint {
                    id: "fallback".to_string(),
                    kind: ModelProviderEndpointKind::Api,
                    url: "https://fallback.example.invalid/v1".to_string(),
                    wire_protocol_id: None,
                },
                ModelProviderEndpoint {
                    id: "google".to_string(),
                    kind: ModelProviderEndpointKind::Api,
                    url: "https://google.example.invalid/v1beta".to_string(),
                    wire_protocol_id: Some(WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI.to_string()),
                },
            ],
            proxy_policy: ModelProviderProxyPolicy::InheritSystem,
            credentials: Vec::new(),
            configured_models: Vec::new(),
            default_model_id: None,
            headers: Vec::new(),
            metadata: Default::default(),
            status: ModelProviderProfileStatus::Enabled,
            revision: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
            deleted_at_ms: None,
        };

        profile.validate().unwrap();
        assert_eq!(
            profile
                .primary_api_endpoint_for_protocol(WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI)
                .map(|endpoint| endpoint.id.as_str()),
            Some("google")
        );
        assert_eq!(
            profile
                .primary_api_endpoint_for_protocol(WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE)
                .map(|endpoint| endpoint.id.as_str()),
            Some("fallback")
        );

        profile.endpoints[1].wire_protocol_id = Some("unsupported_protocol".to_string());
        assert_eq!(
            profile.validate().unwrap_err().code,
            "model_provider_endpoint_protocol_invalid"
        );
    }

    #[test]
    fn provider_header_references_validate_and_round_trip_dynamic_variables() {
        let mut profile = ModelProviderProfile {
            id: ModelProviderProfileId::new(),
            legacy_provider_profile_id: None,
            display_name: "Header gateway".to_string(),
            vendor_hint: None,
            endpoints: Vec::new(),
            proxy_policy: ModelProviderProxyPolicy::InheritSystem,
            credentials: Vec::new(),
            configured_models: Vec::new(),
            default_model_id: None,
            headers: vec![
                ModelProviderHeaderReference {
                    name: "x-opencode-session".to_string(),
                    value: ModelProviderHeaderValue::DynamicVariable(
                        ModelProviderHeaderVariable::SessionId,
                    ),
                    redacted_hint: "session variable".to_string(),
                },
                ModelProviderHeaderReference {
                    name: "User-Agent".to_string(),
                    value: ModelProviderHeaderValue::NonSecretLiteral("vibex/1.0".to_string()),
                    redacted_hint: "non-secret header".to_string(),
                },
            ],
            metadata: Default::default(),
            status: ModelProviderProfileStatus::Enabled,
            revision: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
            deleted_at_ms: None,
        };
        profile.validate().unwrap();
        assert_eq!(
            ModelProviderHeaderReference::dynamic_variable_env_keys(&profile.headers),
            vec!["VIBEX_SESSION_ID".to_string()]
        );
        let encoded = serde_json::to_string(&profile.headers).unwrap();
        assert!(encoded.contains("\"type\":\"dynamic_variable\""));
        assert!(encoded.contains("\"value\":\"session_id\""));
        let decoded: Vec<ModelProviderHeaderReference> = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, profile.headers);

        profile.headers.push(profile.headers[0].clone());
        assert_eq!(
            profile.validate().unwrap_err().code,
            "model_provider_header_duplicate"
        );
        profile.headers.pop();
        profile.headers[0].name = "x opencode session".to_string();
        assert_eq!(
            profile.validate().unwrap_err().code,
            "model_provider_header_invalid"
        );
    }

    #[test]
    fn builtin_opencode_descriptor_exposes_every_direct_protocol() {
        let descriptor = AgentProviderProjectionRegistry::builtin()
            .unwrap()
            .resolve(&builtin_identity("opencode"))
            .unwrap()
            .descriptor;
        let interfaces = descriptor
            .model_interfaces
            .iter()
            .map(|interface| {
                (
                    interface.wire_protocol_id.as_str(),
                    interface.sdk_adapter_id.as_deref(),
                    interface.integration_kind,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            interfaces,
            vec![
                (
                    WIRE_PROTOCOL_OPENAI_RESPONSES,
                    Some("@ai-sdk/openai"),
                    AgentModelInterfaceIntegrationKind::Direct,
                ),
                (
                    WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
                    Some("@ai-sdk/openai-compatible"),
                    AgentModelInterfaceIntegrationKind::Direct,
                ),
                (
                    WIRE_PROTOCOL_ANTHROPIC_MESSAGES,
                    Some("@ai-sdk/anthropic"),
                    AgentModelInterfaceIntegrationKind::Direct,
                ),
                (
                    WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI,
                    Some("@ai-sdk/google"),
                    AgentModelInterfaceIntegrationKind::Direct,
                ),
                (
                    WIRE_PROTOCOL_GOOGLE_VERTEX,
                    Some("@ai-sdk/google-vertex"),
                    AgentModelInterfaceIntegrationKind::Direct,
                ),
                (
                    WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE,
                    Some("@ai-sdk/amazon-bedrock"),
                    AgentModelInterfaceIntegrationKind::Direct,
                ),
            ]
        );
    }

    #[test]
    fn builtin_registry_resolves_at_or_above_verified_identities_and_fails_closed() {
        let registry = AgentProviderProjectionRegistry::builtin().unwrap();
        for agent in ["claude", "codex"] {
            let resolution = registry.resolve(&builtin_identity(agent)).unwrap();
            assert_eq!(
                resolution.match_kind,
                ProjectionDescriptorMatch::SemverRange
            );
            assert_eq!(
                resolution.descriptor.evidence.state,
                ProjectionEvidenceState::Verified
            );
        }

        let opencode = registry.resolve(&builtin_identity("opencode")).unwrap();
        assert_eq!(opencode.match_kind, ProjectionDescriptorMatch::SemverRange);
        assert!(
            opencode
                .descriptor
                .credential_kinds
                .contains(&AgentCredentialKind::ApiKey)
        );
        assert_eq!(
            opencode.descriptor.evidence.state,
            ProjectionEvidenceState::Verified
        );

        let mut upgraded_codex = builtin_identity("codex");
        upgraded_codex.adapter_version = Some("1.8.13".to_string());
        upgraded_codex.agent_version = None;
        upgraded_codex.runtime_dependencies.clear();
        assert_eq!(
            registry.resolve(&upgraded_codex).unwrap().match_kind,
            ProjectionDescriptorMatch::SemverRange
        );

        let mut mismatch = builtin_identity("codex");
        mismatch.adapter_version = Some("1.7.9".to_string());
        let resolution = registry.resolve(&mismatch).unwrap();
        assert_eq!(
            resolution.match_kind,
            ProjectionDescriptorMatch::Conservative
        );
        assert!(
            !resolution
                .descriptor
                .credential_control
                .automatically_projects_secret()
        );
        assert_eq!(
            resolution.diagnostic_code.as_deref(),
            Some("agent_projection_version_mismatch")
        );

        let mut manual = builtin_identity("claude");
        manual.source = AgentVersionSource::Manual;
        let resolution = registry.resolve(&manual).unwrap();
        assert_eq!(
            resolution.match_kind,
            ProjectionDescriptorMatch::Conservative
        );
        assert_eq!(
            resolution.diagnostic_code.as_deref(),
            Some("agent_projection_version_untrusted")
        );
    }

    #[test]
    fn opencode_range_exposes_api_key_at_or_above_the_verified_version() {
        let registry = AgentProviderProjectionRegistry::builtin().unwrap();
        let supported = builtin_identity("opencode");
        let resolution = registry.resolve(&supported).unwrap();
        let capability = AgentProviderProjectionCapability::from_resolution(
            &supported,
            &resolution,
            ProjectionAuthState::Missing,
        );
        assert_eq!(
            resolution.match_kind,
            ProjectionDescriptorMatch::SemverRange
        );
        assert!(
            capability
                .form_controls
                .contains(&AgentProjectionFormControl::ApiKey)
        );

        let mut future = supported;
        future.agent_version = Some("2.0.0".to_string());
        let resolution = registry.resolve(&future).unwrap();
        assert_eq!(
            resolution.match_kind,
            ProjectionDescriptorMatch::SemverRange
        );
        assert!(
            resolution
                .descriptor
                .credential_control
                .automatically_projects_secret()
        );

        let mut older = future;
        older.agent_version = Some("1.17.8".to_string());
        let resolution = registry.resolve(&older).unwrap();
        assert_eq!(
            resolution.match_kind,
            ProjectionDescriptorMatch::Conservative
        );
        assert!(
            !resolution
                .descriptor
                .credential_control
                .automatically_projects_secret()
        );
    }

    #[test]
    fn deepseek_harness_projects_the_compatibility_floor_below_the_catalog_pin() {
        let registry = AgentProviderProjectionRegistry::builtin().unwrap();
        let route = route(
            "deepseek-harness",
            crate::agent_provider_runtime::default_acp_adapter_id(
                &AgentId::parse("deepseek-harness").unwrap(),
            )
            .as_str(),
        )
        .unwrap();
        let identity = |version: &str| AgentRuntimeVersionIdentity {
            route: route.clone(),
            adapter_version: None,
            agent_version: Some(version.to_string()),
            runtime_dependencies: BTreeMap::new(),
            source: AgentVersionSource::Detected,
        };

        // The catalog pin moved to 0.4.33, but 0.4.32 still speaks the projected
        // bare model id, so its runtime keeps the typed surface instead of
        // collapsing to `agent_projection_version_mismatch`.
        let floored = identity("0.4.32");
        let resolution = registry.resolve(&floored).unwrap();
        assert_eq!(
            resolution.match_kind,
            ProjectionDescriptorMatch::SemverRange
        );
        let capability = AgentProviderProjectionCapability::from_resolution(
            &floored,
            &resolution,
            ProjectionAuthState::Missing,
        );
        for control in [
            AgentProjectionFormControl::ApiKey,
            AgentProjectionFormControl::Model,
        ] {
            assert!(capability.form_controls.contains(&control), "{control:?}");
        }

        let pinned = identity("0.4.33");
        assert_eq!(
            registry.resolve(&pinned).unwrap().match_kind,
            ProjectionDescriptorMatch::SemverRange
        );

        let below = identity("0.4.31");
        let resolution = registry.resolve(&below).unwrap();
        assert_eq!(
            resolution.match_kind,
            ProjectionDescriptorMatch::Conservative
        );
        assert_eq!(
            resolution.diagnostic_code.as_deref(),
            Some("agent_projection_version_mismatch")
        );

        // The wire protocol the 0.2.0 bundled runtime introduced is offered
        // only once the bridge that bundles it is installed; an older runtime
        // refuses a route that names it.
        let supports_mistral = |version: &str| {
            let identity = identity(version);
            let resolution = registry.resolve(&identity).unwrap();
            AgentProviderProjectionCapability::from_resolution(
                &identity,
                &resolution,
                ProjectionAuthState::Missing,
            )
            .model_interfaces
            .iter()
            .any(|interface| {
                interface.wire_protocol_id == crate::WIRE_PROTOCOL_MISTRAL_CONVERSATIONS
            })
        };
        assert!(!supports_mistral("0.4.35"));
        assert!(supports_mistral("0.4.36"));
        assert!(supports_mistral("0.4.37"));
    }

    /// A binding already holding the Mistral protocol must not project onto an
    /// install whose bundled runtime refuses that route: the interface is
    /// dropped from the resolved descriptor, so the plan-time validation fails
    /// before a file is written.
    #[test]
    fn mistral_binding_is_refused_on_a_runtime_without_the_protocol() {
        let registry = AgentProviderProjectionRegistry::builtin().unwrap();
        let route = route(
            "deepseek-harness",
            crate::agent_provider_runtime::default_acp_adapter_id(
                &AgentId::parse("deepseek-harness").unwrap(),
            )
            .as_str(),
        )
        .unwrap();
        let resolve = |version: &str| {
            registry
                .resolve(&AgentRuntimeVersionIdentity {
                    route: route.clone(),
                    adapter_version: None,
                    agent_version: Some(version.to_string()),
                    runtime_dependencies: BTreeMap::new(),
                    source: AgentVersionSource::Managed,
                })
                .unwrap()
        };
        let binding = |descriptor_id| AgentModelProviderBinding {
            id: AgentModelProviderBindingId::new(),
            legacy_provider_profile_id: None,
            agent_id: AgentId::parse("deepseek-harness").unwrap(),
            runtime_profile_id: AgentRuntimeProfileId::new(),
            model_provider_profile_id: Some(ModelProviderProfileId::new()),
            projection_descriptor_id: descriptor_id,
            projection_overrides: AgentProviderProjectionOverrides::default(),
            configured_models: vec![AgentConfiguredModelBinding {
                id: AgentConfiguredModelBindingId::new(),
                provider_model_id: "mistral-large".to_string(),
                agent_model_id: "mistral-large".to_string(),
                wire_protocol_id: WIRE_PROTOCOL_MISTRAL_CONVERSATIONS.to_string(),
                sdk_adapter_id: None,
                deployment: None,
                enabled: true,
                process_scoped: false,
            }],
            account_alias: None,
            reasoning_effort: None,
            provider_options: crate::ProviderOptions::empty(),
            projection_fingerprint: None,
            status: AgentModelProviderBindingStatus::Draft,
            verification: ProjectionVerificationState {
                state: ProjectionEvidenceState::Verified,
                descriptor_version: "1".to_string(),
                source_evidence_reference: None,
                runtime_evidence_reference: None,
                verified_at_ms: None,
            },
            revision: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
            deleted_at_ms: None,
        };

        let current = resolve("0.4.37");
        let current_binding = binding(current.descriptor.id.clone());
        assert!(
            current_binding
                .validate_against_descriptor(&current.descriptor)
                .is_ok()
        );

        let legacy = resolve("0.4.35");
        let legacy_binding = binding(legacy.descriptor.id.clone());
        let error = legacy_binding
            .validate_against_descriptor(&legacy.descriptor)
            .unwrap_err();
        assert_eq!(error.code, "agent_model_interface_unsupported");
    }

    #[test]
    fn codex_0146_descriptor_and_binding_reject_chat() {
        let registry = AgentProviderProjectionRegistry::builtin().unwrap();
        let descriptor = registry
            .resolve(&builtin_identity("codex"))
            .unwrap()
            .descriptor;
        assert_eq!(descriptor.model_interfaces.len(), 1);
        assert_eq!(
            descriptor.model_interfaces[0].wire_protocol_id,
            WIRE_PROTOCOL_OPENAI_RESPONSES
        );

        let binding = AgentModelProviderBinding {
            id: AgentModelProviderBindingId::new(),
            legacy_provider_profile_id: None,
            agent_id: AgentId::parse("codex").unwrap(),
            runtime_profile_id: AgentRuntimeProfileId::new(),
            model_provider_profile_id: Some(ModelProviderProfileId::new()),
            projection_descriptor_id: descriptor.id.clone(),
            projection_overrides: AgentProviderProjectionOverrides::default(),
            configured_models: vec![AgentConfiguredModelBinding {
                id: AgentConfiguredModelBindingId::new(),
                provider_model_id: "gpt-test".to_string(),
                agent_model_id: "gpt-test".to_string(),
                wire_protocol_id: WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
                sdk_adapter_id: None,
                deployment: None,
                enabled: true,
                process_scoped: false,
            }],
            account_alias: None,
            reasoning_effort: None,
            provider_options: crate::ProviderOptions::empty(),
            projection_fingerprint: None,
            status: AgentModelProviderBindingStatus::Draft,
            verification: ProjectionVerificationState {
                state: ProjectionEvidenceState::Verified,
                descriptor_version: "1".to_string(),
                source_evidence_reference: None,
                runtime_evidence_reference: None,
                verified_at_ms: None,
            },
            revision: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
            deleted_at_ms: None,
        };
        let error = binding
            .validate_against_descriptor(&descriptor)
            .unwrap_err();
        assert_eq!(error.code, "agent_model_interface_unsupported");
    }

    #[test]
    fn exact_descriptor_precedes_semver_range() {
        let mut registry = AgentProviderProjectionRegistry::default();
        let mut exact = opencode_projection_descriptor().unwrap();
        exact.id = AgentProviderProjectionDescriptorId::parse("projection_opencode_exact").unwrap();
        exact.compatibility = AgentVersionCompatibility::Exact {
            adapter_version: None,
            agent_version: Some(OPENCODE_LAST_VERIFIED_VERSION.to_string()),
            runtime_dependencies: BTreeMap::new(),
        };
        registry
            .register(opencode_projection_descriptor().unwrap())
            .unwrap();
        registry.register(exact).unwrap();
        let resolution = registry.resolve(&builtin_identity("opencode")).unwrap();
        assert_eq!(resolution.match_kind, ProjectionDescriptorMatch::Exact);
        assert_eq!(
            resolution.descriptor.evidence.state,
            ProjectionEvidenceState::Verified
        );
    }

    #[test]
    fn capabilities_distinguish_selectable_and_unverified_controls() {
        let registry = AgentProviderProjectionRegistry::builtin().unwrap();
        let codex_identity = builtin_identity("codex");
        let capability = AgentProviderProjectionCapability::from_resolution(
            &codex_identity,
            &registry.resolve(&codex_identity).unwrap(),
            ProjectionAuthState::Ready,
        );
        assert!(
            capability
                .form_controls
                .contains(&AgentProjectionFormControl::ApiKey)
        );
        assert!(
            !capability
                .form_controls
                .contains(&AgentProjectionFormControl::WireProtocol)
        );

        let mut unknown = builtin_identity("opencode");
        unknown.source = AgentVersionSource::Unknown;
        unknown.agent_version = None;
        let capability = AgentProviderProjectionCapability::from_resolution(
            &unknown,
            &registry.resolve(&unknown).unwrap(),
            ProjectionAuthState::Unknown,
        );
        assert_eq!(
            capability.form_controls,
            vec![
                AgentProjectionFormControl::AdvancedCustomEnvironment,
                AgentProjectionFormControl::ProjectionPreview,
            ]
        );
    }
}

/// Asks a saved shared Provider for the model list its endpoint serves.
///
/// The shared record owns the address and the credential reference; the
/// service resolves the Secret only for this call and never returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderFetchModelsRequest {
    pub model_provider_profile_id: ModelProviderProfileId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderFetchModelsResponse {
    pub model_provider_profile_id: ModelProviderProfileId,
    /// The catalogue as the endpoint listed it, with existing declarations
    /// carried over so capabilities survive a refresh.
    pub models: Vec<ModelProviderCatalogEntry>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

/// Probes a shared Provider's saved address and credential.
///
/// The probe is a model-list request: it proves the address, the credential,
/// and the protocol without sending a prompt or spending completion tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderTestRequest {
    pub model_provider_profile_id: ModelProviderProfileId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderKeyTestRequest {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderEndpointProbeRequest {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderEndpointProbe {
    pub endpoint_id: String,
    pub url: String,
    pub kind: ModelProviderEndpointKind,
    pub status: crate::provider::AgentModelProviderTestStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<i64>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderEndpointProbeResponse {
    pub probes: Vec<ModelProviderEndpointProbe>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderProxyTestRequest {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderTestResult {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub status: crate::provider::AgentModelProviderTestStatus,
    pub code: String,
    pub message: String,
    pub diagnostics: Vec<ProviderBindingMetadata>,
    pub checked_at_ms: i64,
}

#[cfg(test)]
mod advanced_metadata_tests {
    use super::*;

    #[test]
    fn advanced_overrides_round_trip_without_money_fields() {
        let metadata = ModelProviderMetadata {
            auth_header_mode: Some("both".to_string()),
            extra_body_json: Some(r#"{"reasoning":{"effort":"high"}}"#.to_string()),
            user_agent_mode: Some("pass_through".to_string()),
            tls_accept_invalid: true,
            connect_timeout_ms: Some(5_000),
            request_timeout_ms: Some(60_000),
            stream_idle_timeout_ms: Some(30_000),
            auto_refresh_models: true,
            model_aliases: vec![ProviderModelAlias {
                alias: "fast".to_string(),
                upstream: "gpt-4.1-mini".to_string(),
            }],
            ..Default::default()
        };
        let encoded = serde_json::to_string(&metadata).unwrap();
        assert!(!encoded.contains("cost"));
        assert!(!encoded.contains("price"));
        assert!(!encoded.contains("quota"));
        let decoded: ModelProviderMetadata = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, metadata);
    }
}

#[cfg(test)]
mod key_policy_tests {
    use super::*;

    #[test]
    fn per_key_model_scope_and_proxy_round_trip() {
        let credential = ModelProviderCredentialReference {
            display_name: "Primary".to_string(),
            models: vec!["gpt-4.1".to_string(), "gpt-4.1-mini".to_string()],
            proxy_url: Some("http://127.0.0.1:7890".to_string()),
            ..Default::default()
        };
        let encoded = serde_json::to_string(&credential).unwrap();
        let decoded: ModelProviderCredentialReference = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, credential);
    }

    #[test]
    fn a_secret_mask_keeps_the_prefix_and_suffix_but_not_the_length() {
        assert_eq!(
            masked_secret_hint("sk-1234567890abcdef"),
            "sk-1********cdef"
        );
        assert_eq!(masked_secret_hint("abc"), "a********c");
        assert_eq!(masked_secret_hint("ab"), "********");
        assert!(masked_secret_hint("").is_empty());
        assert!(!masked_secret_hint("sk-1234567890abcdef").contains("1234567890"));
    }

    #[test]
    fn a_generated_key_name_falls_back_to_the_mask() {
        let credential = ModelProviderCredentialReference {
            id: RequestId::new(),
            display_name: String::new(),
            masked_hint: "sk-1********cdef".to_string(),
            ..Default::default()
        };
        assert_eq!(credential.display_label(), "sk-1********cdef");

        let generated = ModelProviderCredentialReference {
            id: credential.id.clone(),
            display_name: format!("Key {}", credential.id.as_str()),
            masked_hint: "sk-1********cdef".to_string(),
            ..Default::default()
        };
        assert_eq!(generated.display_label(), "sk-1********cdef");

        let named = ModelProviderCredentialReference {
            display_name: "Work account".to_string(),
            masked_hint: "sk-1********cdef".to_string(),
            ..Default::default()
        };
        assert_eq!(named.display_label(), "Work account");
    }

    #[test]
    fn an_older_key_record_reads_with_an_empty_mask() {
        let encoded = r#"{
            "id": "request_00000000000000000000000000000001",
            "displayName": "Key request_00000000000000000000000000000001",
            "status": "ready",
            "credential": {"kind": "local"},
            "revision": 1
        }"#;
        let decoded: ModelProviderCredentialReference = serde_json::from_str(encoded).unwrap();
        assert!(decoded.masked_hint.is_empty());
        assert_eq!(decoded.display_label(), "********");
    }
}

//! Contracts for IM chat channels.
//!
//! An IM channel connects an external messaging account (Telegram, Lark) to one
//! local Vibex runtime. The runtime owns the channel, its external identities,
//! and the session bindings; the contracts here are the platform-neutral shape
//! that the runtime, the backend facade, and the desktop settings page share.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::agent_config::AgentId;
use crate::ids::{ImBindingId, ImChannelId, ImPrincipalId, VibexSessionId};
use crate::runtime::{RuntimeAuthSource, RuntimeModelSelection, SessionRuntimeSelection};
use crate::workspace::WorkspaceMode;

/// The IM platforms Vibex can host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImChannelKind {
    Telegram,
    Lark,
}

impl ImChannelKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Telegram => "telegram",
            Self::Lark => "lark",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "telegram" => Some(Self::Telegram),
            "lark" => Some(Self::Lark),
            _ => None,
        }
    }
}

/// Non-secret channel configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImChannelConfig {
    Telegram(ImTelegramConfig),
    Lark(ImLarkConfig),
}

impl ImChannelConfig {
    pub const fn kind(&self) -> ImChannelKind {
        match self {
            Self::Telegram(_) => ImChannelKind::Telegram,
            Self::Lark(_) => ImChannelKind::Lark,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImTelegramConfig {
    /// The chat the bot serves. Numeric ids and `@username` are both accepted.
    pub chat_id: String,
    #[serde(default)]
    pub topic_mode: bool,
    #[serde(default = "default_true")]
    pub require_mention_in_groups: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImLarkConfig {
    pub app_id: String,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub transport: ImLarkTransport,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImLarkTransport {
    #[default]
    LongConnection,
    Webhook,
}

/// A serializable projection of [`SessionRuntimeSelection`].
///
/// `SessionRuntimeSelection` deliberately does not implement `Serialize`; this
/// is the durable shape a channel stores for `/new`, converted at use time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImSessionDefaults {
    pub workspace_root: String,
    pub workspace_mode: WorkspaceMode,
    pub agent_id: AgentId,
    pub auth_source: RuntimeAuthSource,
    pub model: RuntimeModelSelection,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub mode_id: Option<String>,
    #[serde(default)]
    pub config_values: BTreeMap<String, String>,
}

impl ImSessionDefaults {
    pub fn runtime_selection(&self) -> SessionRuntimeSelection {
        SessionRuntimeSelection {
            agent_id: self.agent_id.clone(),
            auth_source: self.auth_source.clone(),
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            mode_id: self.mode_id.clone(),
            config_values: self.config_values.clone(),
        }
    }
}

/// The full persisted envelope behind `im_channels.config_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImChannelConfigEnvelope {
    pub schema_version: u32,
    pub config: ImChannelConfig,
    #[serde(default)]
    pub default_session: Option<ImSessionDefaults>,
}

impl ImChannelConfigEnvelope {
    pub fn new(config: ImChannelConfig) -> Self {
        Self {
            schema_version: 1,
            config,
            default_session: None,
        }
    }
}

/// A create request from the settings page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImChannelDraft {
    pub display_name: String,
    pub config: ImChannelConfig,
    #[serde(default)]
    pub default_session: Option<ImSessionDefaults>,
    #[serde(default)]
    pub event_filter: Vec<String>,
    #[serde(default)]
    pub locale: Option<String>,
}

/// The event ids an IM channel may forward. `None`/empty means the default set.
pub const DEFAULT_IM_EVENT_FILTER: &[&str] = &[
    "turn_complete",
    "turn_failed",
    "permission_request",
    "permission_resolution",
    "elicitation_request",
    "elicitation_resolution",
    "error",
    "needs_input",
];

/// The id the UI uses for an event in its filter checklist.
pub const IM_EVENT_FILTER_IDS: &[&str] = &[
    "turn_complete",
    "turn_failed",
    "permission_request",
    "permission_resolution",
    "elicitation_request",
    "elicitation_resolution",
    "error",
    "needs_input",
    "user_prompt_sent",
    "tool_progress",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImPrincipalRole {
    Observer,
    Approver,
    Operator,
    Admin,
}

impl ImPrincipalRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observer => "observer",
            Self::Approver => "approver",
            Self::Operator => "operator",
            Self::Admin => "admin",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "observer" => Some(Self::Observer),
            "approver" => Some(Self::Approver),
            "operator" => Some(Self::Operator),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }

    pub const fn can_approve(self) -> bool {
        matches!(self, Self::Approver | Self::Operator | Self::Admin)
    }

    pub const fn can_prompt(self) -> bool {
        matches!(self, Self::Operator | Self::Admin)
    }

    pub const fn can_admin(self) -> bool {
        matches!(self, Self::Admin)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImPrincipalStatus {
    Pending,
    Active,
    Revoked,
}

impl ImPrincipalStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Revoked => "revoked",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "active" => Some(Self::Active),
            "revoked" => Some(Self::Revoked),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImPrincipalSummary {
    pub principal_id: ImPrincipalId,
    pub channel_id: ImChannelId,
    pub external_user_id: String,
    pub display_name: String,
    pub role: ImPrincipalRole,
    pub status: ImPrincipalStatus,
    pub paired_at_ms: Option<i64>,
    pub last_seen_at_ms: Option<i64>,
    pub revoked_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImBindingStatus {
    Active,
    Paused,
    Stale,
    Revoked,
}

impl ImBindingStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Stale => "stale",
            Self::Revoked => "revoked",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "paused" => Some(Self::Paused),
            "stale" => Some(Self::Stale),
            "revoked" => Some(Self::Revoked),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImBindingSummary {
    pub binding_id: ImBindingId,
    pub channel_id: ImChannelId,
    pub external_chat_id: String,
    pub thread_key: String,
    pub thread_kind: Option<String>,
    pub session_id: VibexSessionId,
    pub status: ImBindingStatus,
    pub title: Option<String>,
    pub last_delivered_sequence: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImChannelState {
    Disabled,
    Starting,
    Connected,
    Degraded,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImChannelErrorView {
    pub code: String,
    pub message: String,
    pub retry_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImChannelStatus {
    pub state: ImChannelState,
    pub since_ms: i64,
    pub last_inbound_at_ms: Option<i64>,
    pub last_outbound_at_ms: Option<i64>,
    pub pending_outbox: usize,
    pub consecutive_failures: u32,
    pub last_error: Option<ImChannelErrorView>,
    pub generation: u64,
}

impl Default for ImChannelStatus {
    fn default() -> Self {
        Self {
            state: ImChannelState::Disabled,
            since_ms: crate::time::unix_timestamp_ms(),
            last_inbound_at_ms: None,
            last_outbound_at_ms: None,
            pending_outbox: 0,
            consecutive_failures: 0,
            last_error: None,
            generation: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImChannelSummary {
    pub channel_id: ImChannelId,
    pub display_name: String,
    pub kind: ImChannelKind,
    pub enabled: bool,
    /// Whether every required secret is present.
    pub configured: bool,
    #[serde(default)]
    pub default_session: Option<ImSessionDefaults>,
    pub event_filter: Vec<String>,
    pub locale: Option<String>,
    pub status: ImChannelStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImChannelTestReport {
    pub ok: bool,
    pub bot_name: Option<String>,
    pub chat_title: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImPairingRequest {
    pub channel_id: ImChannelId,
    pub role: ImPrincipalRole,
    pub ttl_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImPairingCodeInfo {
    pub channel_id: ImChannelId,
    pub code: String,
    pub role: ImPrincipalRole,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImSecretKind {
    BotToken,
    AppSecret,
}

impl ImSecretKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BotToken => "bot_token",
            Self::AppSecret => "app_secret",
        }
    }
}

fn default_true() -> bool {
    true
}

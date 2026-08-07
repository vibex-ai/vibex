use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::acp_catalog::{AcpAgentCatalogEntry, acp_agent_catalog_entries};
use crate::error::{VibexError, VibexResult};
use crate::provider::{ProviderBindingMetadata, ProviderKind};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AgentId(String);

impl AgentId {
    pub fn parse(value: impl Into<String>) -> VibexResult<Self> {
        let value = value.into();
        if is_valid_agent_id(&value) {
            Ok(Self(value))
        } else {
            Err(
                VibexError::validation("invalid_agent_id", "agent id must be lowercase kebab-case")
                    .with_diagnostic("agentId", value),
            )
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentId {
    type Err = VibexError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for AgentId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeKind {
    Acp,
}

impl AgentRuntimeKind {
    pub const fn provider_kind(self) -> ProviderKind {
        ProviderKind::Acp
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSourceKind {
    Builtin,
    Catalog,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentInstallStatus {
    Unknown,
    Installed,
    Missing,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentManagedDistributionKind {
    Binary,
    Npm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentManagedInstallStatus {
    External,
    NotInstalled,
    Installing,
    Installed,
    UpdateAvailable,
    Upgrading,
    Failed,
    Uninstalling,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentManagedInstallState {
    pub managed: bool,
    pub status: AgentManagedInstallStatus,
    pub distribution_kind: Option<AgentManagedDistributionKind>,
    pub installed_version: Option<String>,
    pub available_version: Option<String>,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
    pub updated_at_ms: Option<i64>,
}

impl AgentManagedInstallState {
    pub fn external() -> Self {
        Self {
            managed: false,
            status: AgentManagedInstallStatus::External,
            distribution_kind: None,
            installed_version: None,
            available_version: None,
            last_error_code: None,
            last_error_message: None,
            updated_at_ms: None,
        }
    }

    pub fn not_installed() -> Self {
        Self {
            managed: true,
            status: AgentManagedInstallStatus::NotInstalled,
            distribution_kind: None,
            installed_version: None,
            available_version: None,
            last_error_code: None,
            last_error_message: None,
            updated_at_ms: None,
        }
    }

    pub fn has_usable_installation(&self) -> bool {
        matches!(
            self.status,
            AgentManagedInstallStatus::Installed | AgentManagedInstallStatus::UpdateAvailable
        ) && self.installed_version.is_some()
    }
}

impl Default for AgentManagedInstallState {
    fn default() -> Self {
        Self::external()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentConfigStatus {
    Unknown,
    Configured,
    NeedsConfiguration,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeStatus {
    Unknown,
    Ready,
    Unavailable,
    Disabled,
    ProbeFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCommandConfig {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefinition {
    pub id: AgentId,
    pub label: String,
    pub description: Option<String>,
    pub runtime_kind: AgentRuntimeKind,
    pub source_kind: AgentSourceKind,
    pub default_enabled: bool,
    pub order_index: i64,
    pub command: Option<AgentCommandConfig>,
    pub env: BTreeMap<String, String>,
    pub params: serde_json::Value,
    pub modes: Vec<String>,
    pub capability_hints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    pub agent_id: AgentId,
    pub runtime_kind: AgentRuntimeKind,
    pub source_kind: AgentSourceKind,
    pub label_override: Option<String>,
    pub description_override: Option<String>,
    pub enabled: bool,
    pub order_index: i64,
    pub command: Option<AgentCommandConfig>,
    pub env: BTreeMap<String, String>,
    pub params: serde_json::Value,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDiscoveryRecord {
    pub discovery_record_id: String,
    pub agent_id: AgentId,
    pub cwd_scope: String,
    pub install_status: AgentInstallStatus,
    pub config_status: AgentConfigStatus,
    pub runtime_status: AgentRuntimeStatus,
    pub binary_path: Option<String>,
    pub version: Option<String>,
    pub native_config_paths: Vec<String>,
    pub models: Vec<String>,
    pub modes: Vec<String>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
    pub discovered_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSnapshotEntry {
    pub id: AgentId,
    pub label: String,
    pub description: Option<String>,
    pub runtime_kind: AgentRuntimeKind,
    pub source_kind: AgentSourceKind,
    pub added: bool,
    pub enabled: bool,
    pub order_index: i64,
    pub installed: bool,
    pub configured: bool,
    pub install_status: AgentInstallStatus,
    #[serde(default)]
    pub managed_install: AgentManagedInstallState,
    pub config_status: AgentConfigStatus,
    pub runtime_status: AgentRuntimeStatus,
    pub command: Option<AgentCommandConfig>,
    pub env: BTreeMap<String, String>,
    pub params: serde_json::Value,
    pub models: Vec<String>,
    pub modes: Vec<String>,
    pub capability_hints: Vec<String>,
    pub native_config_paths: Vec<String>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
    pub discovered_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
    pub deleted_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentListRequest {
    pub include_disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentListResponse {
    pub agents: Vec<AgentSnapshotEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUpdateConfigRequest {
    pub agent_id: AgentId,
    pub added: Option<bool>,
    pub enabled: Option<bool>,
    pub label_override: Option<String>,
    pub description_override: Option<String>,
    pub order_index: Option<i64>,
    pub command: Option<AgentCommandConfig>,
    pub env: Option<BTreeMap<String, String>>,
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRefreshSnapshotRequest {
    pub agent_id: AgentId,
    pub cwd_scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRefreshSnapshotResponse {
    pub agent: AgentSnapshotEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCatalogListResponse {
    pub agents: Vec<AgentDefinition>,
}

impl AgentSnapshotEntry {
    pub fn from_definition(
        definition: &AgentDefinition,
        config: Option<&AgentConfig>,
        discovery: Option<&AgentDiscoveryRecord>,
    ) -> Self {
        let added = config
            .map(|config| config.deleted_at_ms.is_none())
            .unwrap_or(definition.default_enabled);
        let enabled = config
            .map(|config| config.enabled)
            .unwrap_or(definition.default_enabled);
        let label = config
            .and_then(|config| config.label_override.clone())
            .unwrap_or_else(|| definition.label.clone());
        let description = config
            .and_then(|config| config.description_override.clone())
            .or_else(|| definition.description.clone());
        let runtime_kind = config
            .map(|config| config.runtime_kind)
            .unwrap_or(definition.runtime_kind);
        let command = config
            .and_then(|config| config.command.clone())
            .or_else(|| definition.command.clone());
        let env = config
            .map(|config| config.env.clone())
            .unwrap_or_else(|| definition.env.clone());
        let params = config
            .map(|config| config.params.clone())
            .unwrap_or_else(|| definition.params.clone());
        let order_index = config
            .map(|config| config.order_index)
            .unwrap_or(definition.order_index);
        let install_status = if !added {
            AgentInstallStatus::Disabled
        } else {
            discovery
                .map(|record| record.install_status)
                .unwrap_or(AgentInstallStatus::Unknown)
        };
        let config_status = if !added {
            AgentConfigStatus::Disabled
        } else {
            discovery
                .map(|record| record.config_status)
                .unwrap_or(AgentConfigStatus::Unknown)
        };
        let runtime_status = if !added {
            AgentRuntimeStatus::Disabled
        } else {
            discovery
                .map(|record| record.runtime_status)
                .unwrap_or(AgentRuntimeStatus::Unknown)
        };

        Self {
            id: definition.id.clone(),
            label,
            description,
            runtime_kind,
            source_kind: config
                .map(|config| config.source_kind)
                .unwrap_or(definition.source_kind),
            added,
            enabled,
            order_index,
            installed: matches!(install_status, AgentInstallStatus::Installed),
            configured: matches!(config_status, AgentConfigStatus::Configured),
            install_status,
            managed_install: if acp_registry_agent_id(&definition.id).is_some() {
                AgentManagedInstallState::not_installed()
            } else {
                AgentManagedInstallState::external()
            },
            config_status,
            runtime_status,
            command,
            env,
            params,
            models: discovery
                .map(|record| record.models.clone())
                .unwrap_or_default(),
            modes: discovery
                .map(|record| record.modes.clone())
                .filter(|modes| !modes.is_empty())
                .unwrap_or_else(|| definition.modes.clone()),
            capability_hints: definition.capability_hints.clone(),
            native_config_paths: discovery
                .map(|record| record.native_config_paths.clone())
                .unwrap_or_default(),
            diagnostics: discovery
                .map(|record| record.diagnostics.clone())
                .unwrap_or_default(),
            discovered_at_ms: discovery.map(|record| record.discovered_at_ms),
            updated_at_ms: config.map(|config| config.updated_at_ms),
            deleted_at_ms: config.and_then(|config| config.deleted_at_ms),
        }
    }

    pub fn apply_managed_install_state(&mut self, state: AgentManagedInstallState) {
        if self.added && state.has_usable_installation() {
            self.installed = true;
            self.install_status = AgentInstallStatus::Installed;
        }
        self.managed_install = state;
    }
}

/// Maps Vibex's stable Agent ids onto the upstream ACP Registry identities.
/// Agents absent from the Registry remain user-managed and retain PATH probing.
pub fn acp_registry_agent_id(agent_id: &AgentId) -> Option<&'static str> {
    match agent_id.as_str() {
        "claude" => return Some("claude-acp"),
        "codex" => return Some("codex-acp"),
        "copilot" => return Some("github-copilot-cli"),
        "grok" => return Some("grok-build"),
        "opencode" => return Some("opencode"),
        // These entries are absent from the Registry or do not currently
        // publish a binary or npm distribution that Vibex can install.
        "codewhale" | "fast-agent" | "hermes" | "kiro" | "minion-code" => {
            return None;
        }
        _ => {}
    }
    acp_agent_catalog_entries()
        .iter()
        .find(|entry| entry.id == agent_id.as_str())
        .map(|entry| entry.id)
}

impl From<ProviderKind> for AgentId {
    fn from(value: ProviderKind) -> Self {
        let id = match value {
            ProviderKind::Codex => "codex",
            ProviderKind::Claude => "claude",
            ProviderKind::Acp => "opencode",
        };
        Self(id.to_string())
    }
}

pub fn agent_id_for_provider_kind(provider_kind: ProviderKind) -> AgentId {
    AgentId::from(provider_kind)
}

pub fn builtin_agent_definitions() -> Vec<AgentDefinition> {
    let mut definitions = vec![
        AgentDefinition {
            id: AgentId::parse("claude").expect("builtin agent ids are valid"),
            label: "Claude Code".to_string(),
            description: Some("Claude Code connected through ACP".to_string()),
            runtime_kind: AgentRuntimeKind::Acp,
            source_kind: AgentSourceKind::Builtin,
            default_enabled: true,
            order_index: 10,
            command: Some(AgentCommandConfig {
                command: "claude-agent-acp".to_string(),
                args: Vec::new(),
            }),
            env: BTreeMap::new(),
            params: serde_json::json!({ "connection": "acp", "preset": "claude-agent-acp" }),
            modes: vec!["default".to_string()],
            capability_hints: vec![
                "agent_messages".to_string(),
                "tool_calls".to_string(),
                "permission_requests".to_string(),
                "slash_commands".to_string(),
                "skills".to_string(),
                "mcp".to_string(),
            ],
        },
        AgentDefinition {
            id: AgentId::parse("codex").expect("builtin agent ids are valid"),
            label: "Codex".to_string(),
            description: Some("Codex connected through ACP".to_string()),
            runtime_kind: AgentRuntimeKind::Acp,
            source_kind: AgentSourceKind::Builtin,
            default_enabled: true,
            order_index: 20,
            command: Some(AgentCommandConfig {
                command: "codex-acp".to_string(),
                args: Vec::new(),
            }),
            env: BTreeMap::new(),
            params: serde_json::json!({ "connection": "acp", "preset": "codex-acp" }),
            modes: vec!["default".to_string()],
            capability_hints: vec![
                "agent_messages".to_string(),
                "tool_calls".to_string(),
                "permission_requests".to_string(),
                "skills".to_string(),
                "mcp".to_string(),
            ],
        },
        AgentDefinition {
            id: AgentId::parse("opencode").expect("builtin agent ids are valid"),
            label: "OpenCode".to_string(),
            description: Some("OpenCode agent connected through ACP".to_string()),
            runtime_kind: AgentRuntimeKind::Acp,
            source_kind: AgentSourceKind::Builtin,
            default_enabled: false,
            order_index: 30,
            command: Some(AgentCommandConfig {
                command: "opencode".to_string(),
                args: vec!["acp".to_string()],
            }),
            env: BTreeMap::new(),
            params: serde_json::json!({ "connection": "acp", "preset": "opencode" }),
            modes: vec!["default".to_string()],
            capability_hints: vec![
                "agent_messages".to_string(),
                "tool_calls".to_string(),
                "permission_requests".to_string(),
                "acp_connection".to_string(),
            ],
        },
    ];
    definitions.extend(
        acp_agent_catalog_entries()
            .iter()
            .enumerate()
            .map(|(index, entry)| acp_catalog_agent_definition(entry, 40 + index as i64 * 10)),
    );
    definitions
}

fn acp_catalog_agent_definition(entry: &AcpAgentCatalogEntry, order_index: i64) -> AgentDefinition {
    let (command, args) = entry
        .command
        .split_first()
        .expect("bundled ACP Agent commands are non-empty");
    let mut params = serde_json::json!({
        "connection": "acp",
        "preset": entry.preset_id,
        "version": entry.version,
        "installUrl": entry.install_url,
    });
    if let Some(supports_mcp_servers) = entry.supports_mcp_servers {
        params["supportsMcpServers"] = serde_json::Value::Bool(supports_mcp_servers);
    }

    AgentDefinition {
        id: AgentId::parse(entry.id).expect("builtin agent ids are valid"),
        label: entry.label.to_string(),
        description: Some(entry.description.to_string()),
        runtime_kind: AgentRuntimeKind::Acp,
        source_kind: AgentSourceKind::Catalog,
        default_enabled: false,
        order_index,
        command: Some(AgentCommandConfig {
            command: (*command).to_string(),
            args: args.iter().map(ToString::to_string).collect(),
        }),
        env: entry
            .env
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
        params,
        modes: vec!["default".to_string()],
        capability_hints: vec![
            "agent_messages".to_string(),
            "tool_calls".to_string(),
            "permission_requests".to_string(),
            "acp_connection".to_string(),
        ],
    }
}

fn is_valid_agent_id(value: &str) -> bool {
    let len = value.len();
    if !(2..=80).contains(&len) {
        return false;
    }
    let bytes = value.as_bytes();
    if bytes.first() == Some(&b'-') || bytes.last() == Some(&b'-') {
        return false;
    }
    let mut last_dash = false;
    for byte in bytes {
        let valid = byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-';
        if !valid {
            return false;
        }
        if *byte == b'-' {
            if last_dash {
                return false;
            }
            last_dash = true;
        } else {
            last_dash = false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_id_accepts_lowercase_kebab_ids() {
        assert_eq!(AgentId::parse("claude").unwrap().as_str(), "claude");
        assert_eq!(AgentId::parse("open-code").unwrap().as_str(), "open-code");
    }

    #[test]
    fn agent_id_rejects_invalid_shapes() {
        for value in ["Claude", "open_code", "-bad", "bad-", "bad--id", ""] {
            assert!(AgentId::parse(value).is_err(), "{value}");
        }
    }

    #[test]
    fn builtin_online_agents_use_fixed_acp_adapter_commands() {
        let definitions = builtin_agent_definitions();
        assert!(
            definitions
                .iter()
                .all(|definition| definition.runtime_kind == AgentRuntimeKind::Acp)
        );
        for (agent_id, command, preset) in [
            ("claude", "claude-agent-acp", "claude-agent-acp"),
            ("codex", "codex-acp", "codex-acp"),
        ] {
            let definition = definitions
                .iter()
                .find(|definition| definition.id.as_str() == agent_id)
                .unwrap();
            assert_eq!(
                definition.command.as_ref().unwrap().command,
                command,
                "{agent_id} must use its Compatibility Registry adapter binary"
            );
            assert_eq!(
                definition
                    .params
                    .get("preset")
                    .and_then(serde_json::Value::as_str),
                Some(preset)
            );
        }
    }

    #[test]
    fn builtin_agent_catalog_contains_all_generic_acp_agents() {
        let definitions = builtin_agent_definitions();
        let actual = definitions
            .iter()
            .map(|definition| definition.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let expected = [
            "agoragentic-acp",
            "amp-acp",
            "auggie",
            "autohand",
            "claude",
            "cline",
            "codebuddy-code",
            "codex",
            "codewhale",
            "copilot",
            "cortex-code",
            "crow-cli",
            "cursor",
            "deepagents",
            "devin",
            "dimcode",
            "dirac",
            "factory-droid",
            "fast-agent",
            "gemini",
            "glm-acp-agent",
            "goose",
            "grok",
            "hermes",
            "junie",
            "kilo",
            "kimi",
            "kiro",
            "minion-code",
            "mistral-vibe",
            "nova",
            "opencode",
            "poolside",
            "qoder",
            "qwen-code",
            "sigit",
            "stakpak",
            "vtcode",
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(actual, expected);
    }

    #[test]
    fn generic_acp_agents_preserve_pinned_commands_and_environment() {
        let definitions = builtin_agent_definitions();
        for (agent_id, command, args) in [
            ("glm-acp-agent", "npx", &["-y", "glm-acp-agent@1.1.4"][..]),
            (
                "fast-agent",
                "uvx",
                &["--from", "fast-agent-acp==0.7.21", "fast-agent-acp", "-x"][..],
            ),
            ("cursor", "cursor-agent", &["acp"][..]),
        ] {
            let definition = definitions
                .iter()
                .find(|definition| definition.id.as_str() == agent_id)
                .unwrap();
            let configured = definition.command.as_ref().unwrap();
            assert_eq!(configured.command, command);
            assert_eq!(configured.args, args);
        }

        let factory = definitions
            .iter()
            .find(|definition| definition.id.as_str() == "factory-droid")
            .unwrap();
        assert_eq!(
            factory
                .env
                .get("DROID_DISABLE_AUTO_UPDATE")
                .map(String::as_str),
            Some("true")
        );
        assert_eq!(
            factory
                .env
                .get("FACTORY_DROID_AUTO_UPDATE_ENABLED")
                .map(String::as_str),
            Some("false")
        );
        assert_eq!(factory.params["supportsMcpServers"], false);
    }

    #[test]
    fn managed_registry_mapping_is_explicit_and_fail_closed() {
        assert_eq!(
            acp_registry_agent_id(&AgentId::parse("claude").unwrap()),
            Some("claude-acp")
        );
        assert_eq!(
            acp_registry_agent_id(&AgentId::parse("copilot").unwrap()),
            Some("github-copilot-cli")
        );
        assert_eq!(
            acp_registry_agent_id(&AgentId::parse("gemini").unwrap()),
            Some("gemini")
        );
        for managed in [
            "cortex-code",
            "crow-cli",
            "cursor",
            "devin",
            "junie",
            "stakpak",
            "vtcode",
        ] {
            assert_eq!(
                acp_registry_agent_id(&AgentId::parse(managed).unwrap()),
                Some(managed),
                "{managed} must use its Registry binary distribution"
            );
        }
        for external in ["codewhale", "fast-agent", "hermes", "kiro", "minion-code"] {
            assert_eq!(
                acp_registry_agent_id(&AgentId::parse(external).unwrap()),
                None,
                "{external} must remain external until the Registry publishes an installable distribution"
            );
        }
    }

    #[test]
    fn snapshots_expose_managed_install_ownership_before_installation() {
        let definitions = builtin_agent_definitions();
        let gemini = definitions
            .iter()
            .find(|definition| definition.id.as_str() == "gemini")
            .unwrap();
        let cursor = definitions
            .iter()
            .find(|definition| definition.id.as_str() == "cursor")
            .unwrap();
        let gemini = AgentSnapshotEntry::from_definition(gemini, None, None);
        let cursor = AgentSnapshotEntry::from_definition(cursor, None, None);
        assert!(gemini.managed_install.managed);
        assert_eq!(
            gemini.managed_install.status,
            AgentManagedInstallStatus::NotInstalled
        );
        assert!(cursor.managed_install.managed);
        assert_eq!(
            cursor.managed_install.status,
            AgentManagedInstallStatus::NotInstalled
        );
    }
}

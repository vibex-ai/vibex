use serde::{Deserialize, Serialize};

use crate::agent_config::{AgentId, agent_id_for_provider_kind};
use crate::ids::{
    HookId, McpServerId, ProjectId, PromptId, ProviderProfileId, RequestId, SkillId,
    VibexSessionId, WorkspaceId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Codex,
    Claude,
    Acp,
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Acp => "acp",
        };
        f.write_str(value)
    }
}

impl ProviderKind {
    pub const fn local_default_profile_id(self) -> &'static str {
        match self {
            Self::Codex => "provider_local_default_codex",
            Self::Claude => "provider_local_default_claude",
            Self::Acp => "provider_local_default_acp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderVersionInfo {
    pub provider_version: Option<String>,
    pub adapter_version: String,
    pub capability_source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProfileStatus {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSecretKind {
    ApiKey,
    AuthToken,
    OAuthAccount,
    PrivateKey,
    Header,
    Environment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSecretBackend {
    Placeholder,
    OsKeychain,
    Environment,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSecretSetupState {
    Missing,
    Referenced,
    Available,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDefaultScopeKind {
    Global,
    Project,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerTransportKind {
    Stdio,
    Http,
    Sse,
}

impl std::fmt::Display for McpServerTransportKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
            Self::Sse => "sse",
        };
        f.write_str(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerScopeKind {
    Global,
    User,
    Project,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerStatus {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpSecretTarget {
    Environment,
    Header,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerSecretReference {
    pub id: RequestId,
    pub mcp_server_id: McpServerId,
    pub secret_kind: ProviderSecretKind,
    pub backend: ProviderSecretBackend,
    pub setup_state: ProviderSecretSetupState,
    pub lookup_key: String,
    pub display_label: String,
    pub redacted_hint: String,
    pub target: McpSecretTarget,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerSecretReferenceCreateRequest {
    pub secret_kind: ProviderSecretKind,
    pub backend: ProviderSecretBackend,
    pub setup_state: ProviderSecretSetupState,
    pub lookup_key: String,
    pub display_label: String,
    pub redacted_hint: String,
    pub target: McpSecretTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerProviderMatrix {
    pub provider_kind: ProviderKind,
    pub enabled: bool,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceAgentMatrixSourceKind {
    Manual,
    NativeImport,
    LegacyBackfill,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerAgentMatrix {
    pub agent_id: AgentId,
    pub enabled: bool,
    pub source_kind: ResourceAgentMatrixSourceKind,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceDiscoveryStatus {
    Importable,
    AlreadyImported,
    Unsupported,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    pub id: McpServerId,
    pub display_name: String,
    pub transport_kind: McpServerTransportKind,
    pub status: McpServerStatus,
    pub scope_kind: McpServerScopeKind,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub secret_references: Vec<McpServerSecretReference>,
    pub provider_matrix: Vec<McpServerProviderMatrix>,
    pub agent_matrix: Vec<McpServerAgentMatrix>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
}

impl McpServer {
    pub fn summary(&self) -> McpServerSummary {
        McpServerSummary {
            id: self.id.clone(),
            display_name: self.display_name.clone(),
            transport_kind: self.transport_kind,
            status: self.status,
            scope_kind: self.scope_kind,
            enabled_provider_kinds: self
                .provider_matrix
                .iter()
                .filter(|entry| entry.enabled)
                .map(|entry| entry.provider_kind)
                .collect(),
            enabled_agent_ids: self
                .agent_matrix
                .iter()
                .filter(|entry| entry.enabled)
                .map(|entry| entry.agent_id.clone())
                .collect(),
            updated_at_ms: self.updated_at_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerSummary {
    pub id: McpServerId,
    pub display_name: String,
    pub transport_kind: McpServerTransportKind,
    pub status: McpServerStatus,
    pub scope_kind: McpServerScopeKind,
    pub enabled_provider_kinds: Vec<ProviderKind>,
    pub enabled_agent_ids: Vec<AgentId>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerCreateRequest {
    pub display_name: String,
    pub transport_kind: McpServerTransportKind,
    pub status: McpServerStatus,
    pub scope_kind: McpServerScopeKind,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub secret_references: Vec<McpServerSecretReferenceCreateRequest>,
    pub provider_matrix: Vec<McpServerProviderMatrix>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerUpdateRequest {
    pub mcp_server_id: McpServerId,
    pub display_name: Option<String>,
    pub transport_kind: Option<McpServerTransportKind>,
    pub status: Option<McpServerStatus>,
    pub scope_kind: Option<McpServerScopeKind>,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub url: Option<String>,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerDeleteRequest {
    pub mcp_server_id: McpServerId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerSetProviderMatrixRequest {
    pub mcp_server_id: McpServerId,
    pub provider_matrix: Vec<McpServerProviderMatrix>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerSetAgentMatrixRequest {
    pub mcp_server_id: McpServerId,
    pub agent_matrix: Vec<McpServerAgentMatrix>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerAgentMatrixListRequest {
    pub mcp_server_id: McpServerId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerForAgentListRequest {
    pub agent_id: AgentId,
    pub provider_kind: ProviderKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerDiscoverRequest {
    pub source_agent_id: Option<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerDiscovery {
    pub discovery_id: String,
    pub source_agent_id: AgentId,
    pub source_path: String,
    pub import_key: String,
    pub status: ResourceDiscoveryStatus,
    pub candidate: Option<McpServerCreateRequest>,
    pub existing_mcp_server_id: Option<McpServerId>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerDiscoveryResponse {
    pub discoveries: Vec<McpServerDiscovery>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerImportSelection {
    pub discovery_id: String,
    pub source_agent_id: AgentId,
    pub candidate: McpServerCreateRequest,
    pub enable_agent_ids: Vec<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerImportRequest {
    pub selections: Vec<McpServerImportSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerImportResult {
    pub imported: Vec<McpServer>,
    pub created_count: usize,
    pub updated_count: usize,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerValidateRequest {
    pub mcp_server_id: Option<McpServerId>,
    pub candidate: Option<McpServerCreateRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerValidationStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerValidationResult {
    pub status: McpServerValidationStatus,
    pub code: String,
    pub message: String,
    pub diagnostics: Vec<ProviderBindingMetadata>,
    pub checked_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSourceKind {
    Manual,
    GitRepo,
    LocalFolder,
    Marketplace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillScopeKind {
    Global,
    User,
    Project,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillStatus {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillProviderMatrix {
    pub provider_kind: ProviderKind,
    pub enabled: bool,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillAgentMatrix {
    pub agent_id: AgentId,
    pub enabled: bool,
    pub source_kind: ResourceAgentMatrixSourceKind,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub id: SkillId,
    pub display_name: String,
    pub source_kind: SkillSourceKind,
    pub status: SkillStatus,
    pub scope_kind: SkillScopeKind,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
    pub source_uri: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub content_preview: Option<String>,
    pub provider_matrix: Vec<SkillProviderMatrix>,
    pub agent_matrix: Vec<SkillAgentMatrix>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
}

impl Skill {
    pub fn summary(&self) -> SkillSummary {
        SkillSummary {
            id: self.id.clone(),
            display_name: self.display_name.clone(),
            source_kind: self.source_kind,
            status: self.status,
            scope_kind: self.scope_kind,
            enabled_provider_kinds: self
                .provider_matrix
                .iter()
                .filter(|entry| entry.enabled)
                .map(|entry| entry.provider_kind)
                .collect(),
            enabled_agent_ids: self
                .agent_matrix
                .iter()
                .filter(|entry| entry.enabled)
                .map(|entry| entry.agent_id.clone())
                .collect(),
            updated_at_ms: self.updated_at_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSummary {
    pub id: SkillId,
    pub display_name: String,
    pub source_kind: SkillSourceKind,
    pub status: SkillStatus,
    pub scope_kind: SkillScopeKind,
    pub enabled_provider_kinds: Vec<ProviderKind>,
    pub enabled_agent_ids: Vec<AgentId>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCreateRequest {
    pub display_name: String,
    pub source_kind: SkillSourceKind,
    pub status: SkillStatus,
    pub scope_kind: SkillScopeKind,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
    pub source_uri: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub content_preview: Option<String>,
    pub provider_matrix: Vec<SkillProviderMatrix>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillUpdateRequest {
    pub skill_id: SkillId,
    pub display_name: Option<String>,
    pub source_kind: Option<SkillSourceKind>,
    pub status: Option<SkillStatus>,
    pub scope_kind: Option<SkillScopeKind>,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
    pub source_uri: Option<String>,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    pub content_preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDeleteRequest {
    pub skill_id: SkillId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSetProviderMatrixRequest {
    pub skill_id: SkillId,
    pub provider_matrix: Vec<SkillProviderMatrix>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSetAgentMatrixRequest {
    pub skill_id: SkillId,
    pub agent_matrix: Vec<SkillAgentMatrix>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillAgentMatrixListRequest {
    pub skill_id: SkillId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillForAgentListRequest {
    pub agent_id: AgentId,
    pub provider_kind: ProviderKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDiscoverRequest {
    pub source_agent_id: Option<AgentId>,
    pub workspace_id: Option<WorkspaceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDiscovery {
    pub discovery_id: String,
    pub source_agent_id: AgentId,
    pub source_path: String,
    pub import_key: String,
    pub status: ResourceDiscoveryStatus,
    pub display_name: String,
    pub command_name: String,
    pub description: Option<String>,
    pub content_preview: Option<String>,
    pub existing_skill_id: Option<SkillId>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDiscoveryResponse {
    pub discoveries: Vec<SkillDiscovery>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillImportSelection {
    pub discovery_id: String,
    pub source_agent_id: AgentId,
    pub source_path: String,
    pub display_name: String,
    pub command_name: String,
    pub description: Option<String>,
    pub content_preview: Option<String>,
    pub enable_agent_ids: Vec<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillImportRequest {
    pub selections: Vec<SkillImportSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillImportResult {
    pub imported: Vec<Skill>,
    pub created_count: usize,
    pub updated_count: usize,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillValidateRequest {
    pub skill_id: Option<SkillId>,
    pub candidate: Option<SkillCreateRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillValidationStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillValidationResult {
    pub status: SkillValidationStatus,
    pub code: String,
    pub message: String,
    pub diagnostics: Vec<ProviderBindingMetadata>,
    pub checked_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptKind {
    ReusablePrompt,
    SlashCommand,
    SystemSnippet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptScopeKind {
    Global,
    User,
    Project,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptStatus {
    Enabled,
    Disabled,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    pub id: PromptId,
    pub display_name: String,
    pub kind: PromptKind,
    pub status: PromptStatus,
    pub scope_kind: PromptScopeKind,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
    pub body: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
}

impl Prompt {
    pub fn summary(&self) -> PromptSummary {
        PromptSummary {
            id: self.id.clone(),
            display_name: self.display_name.clone(),
            kind: self.kind,
            status: self.status,
            scope_kind: self.scope_kind,
            updated_at_ms: self.updated_at_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSummary {
    pub id: PromptId,
    pub display_name: String,
    pub kind: PromptKind,
    pub status: PromptStatus,
    pub scope_kind: PromptScopeKind,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCreateRequest {
    pub display_name: String,
    pub kind: PromptKind,
    pub status: PromptStatus,
    pub scope_kind: PromptScopeKind,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
    pub body: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptUpdateRequest {
    pub prompt_id: PromptId,
    pub display_name: Option<String>,
    pub kind: Option<PromptKind>,
    pub status: Option<PromptStatus>,
    pub scope_kind: Option<PromptScopeKind>,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
    pub body: Option<String>,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptDeleteRequest {
    pub prompt_id: PromptId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptValidateRequest {
    pub prompt_id: Option<PromptId>,
    pub candidate: Option<PromptCreateRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptValidationStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptValidationResult {
    pub status: PromptValidationStatus,
    pub code: String,
    pub message: String,
    pub diagnostics: Vec<ProviderBindingMetadata>,
    pub checked_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEventKind {
    TerminalActivity,
    SessionStart,
    SessionStop,
    PermissionRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookStatus {
    Draft,
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookInstallState {
    NotInstalled,
    PreviewOnly,
    Installed,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hook {
    pub id: HookId,
    pub display_name: String,
    pub provider_kind: ProviderKind,
    pub event_kind: HookEventKind,
    pub status: HookStatus,
    pub install_state: HookInstallState,
    pub command_preview: Option<String>,
    pub managed_marker: String,
    pub description: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookCreateRequest {
    pub display_name: String,
    pub provider_kind: ProviderKind,
    pub event_kind: HookEventKind,
    pub status: HookStatus,
    pub command_preview: Option<String>,
    pub managed_marker: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookUpdateRequest {
    pub hook_id: HookId,
    pub display_name: Option<String>,
    pub provider_kind: Option<ProviderKind>,
    pub event_kind: Option<HookEventKind>,
    pub status: Option<HookStatus>,
    pub install_state: Option<HookInstallState>,
    pub command_preview: Option<String>,
    pub managed_marker: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookDeleteRequest {
    pub hook_id: HookId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookInstallPreviewRequest {
    pub hook_id: HookId,
    pub target_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookInstallPreview {
    pub preview_id: RequestId,
    pub hook_id: HookId,
    pub target_path: String,
    pub marker: String,
    pub redacted_preview: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSandboxDefaults {
    pub permission_mode: String,
    pub ask_on_risk: bool,
    pub bypass_all_permissions: bool,
}

impl ProviderSandboxDefaults {
    pub fn workspace_write_ask_on_risk() -> Self {
        Self {
            permission_mode: "workspace_write".to_string(),
            ask_on_risk: true,
            bypass_all_permissions: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNetworkDefaults {
    pub allow_network: bool,
    pub proxy_url: Option<String>,
}

impl ProviderNetworkDefaults {
    pub fn local_default() -> Self {
        Self {
            allow_network: false,
            proxy_url: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPermissionDefaults {
    pub file_read: String,
    pub file_write: String,
    pub command: String,
    pub network: String,
}

impl ProviderPermissionDefaults {
    pub fn ask_on_risk() -> Self {
        Self {
            file_read: "allow_workspace".to_string(),
            file_write: "ask_on_risk".to_string(),
            command: "ask_on_risk".to_string(),
            network: "ask".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderOptions {
    pub schema_version: u32,
    pub entries: Vec<ProviderBindingMetadata>,
}

impl ProviderOptions {
    pub fn empty() -> Self {
        Self {
            schema_version: 1,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpProviderEnvSource {
    ProcessEnvironment,
    SecretReference,
    Literal,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpProcessStrategy {
    #[default]
    PerSession,
    PerProfilePool,
    Auto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpProviderEnvReference {
    pub key: String,
    pub source: AcpProviderEnvSource,
    pub value: Option<String>,
    pub secret_lookup_key: Option<String>,
    pub redacted_hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpProviderConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<AcpProviderEnvReference>,
    pub cwd_template: Option<String>,
    #[serde(default)]
    pub process_strategy: AcpProcessStrategy,
    #[serde(default)]
    pub terminal_tools: bool,
    #[serde(default)]
    pub terminal_auth: bool,
    pub models: Vec<String>,
    pub modes: Vec<String>,
    pub features: Vec<String>,
    pub disabled_tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpProviderCatalogPreset {
    pub preset_id: String,
    pub display_name: String,
    pub description: String,
    pub default_config: AcpProviderConfig,
    pub tags: Vec<String>,
    pub editable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpProviderCatalogListResponse {
    pub presets: Vec<AcpProviderCatalogPreset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpProviderProfileCreateRequest {
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    pub display_name: String,
    pub account_alias: Option<String>,
    pub preset_id: Option<String>,
    pub config: Option<AcpProviderConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpProviderProfileUpdateRequest {
    pub provider_profile_id: ProviderProfileId,
    pub config: AcpProviderConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSecretReference {
    pub id: RequestId,
    pub provider_profile_id: ProviderProfileId,
    pub secret_kind: ProviderSecretKind,
    pub backend: ProviderSecretBackend,
    pub setup_state: ProviderSecretSetupState,
    pub lookup_key: String,
    pub display_label: String,
    pub redacted_hint: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl ProviderSecretReference {
    pub fn placeholder(
        provider_profile_id: ProviderProfileId,
        secret_kind: ProviderSecretKind,
        lookup_key: impl Into<String>,
        display_label: impl Into<String>,
    ) -> Self {
        let now = crate::unix_timestamp_ms();
        Self {
            id: RequestId::new(),
            provider_profile_id,
            secret_kind,
            backend: ProviderSecretBackend::Placeholder,
            setup_state: ProviderSecretSetupState::Missing,
            lookup_key: lookup_key.into(),
            display_label: display_label.into(),
            redacted_hint: "not configured".to_string(),
            created_at_ms: now,
            updated_at_ms: now,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderModelWireApi {
    OpenaiResponses,
    OpenaiChatCompletions,
    AnthropicMessages,
    GoogleGenerativeAi,
    AwsBedrockConverse,
}

impl ProviderModelWireApi {
    pub const ALL: [Self; 5] = [
        Self::OpenaiResponses,
        Self::OpenaiChatCompletions,
        Self::AnthropicMessages,
        Self::GoogleGenerativeAi,
        Self::AwsBedrockConverse,
    ];

    pub const fn wire_protocol_id(self) -> &'static str {
        match self {
            Self::OpenaiResponses => crate::WIRE_PROTOCOL_OPENAI_RESPONSES,
            Self::OpenaiChatCompletions => crate::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
            Self::AnthropicMessages => crate::WIRE_PROTOCOL_ANTHROPIC_MESSAGES,
            Self::GoogleGenerativeAi => crate::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI,
            Self::AwsBedrockConverse => crate::WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE,
        }
    }

    pub fn from_wire_protocol_id(value: &str) -> Option<Self> {
        match value.trim() {
            crate::WIRE_PROTOCOL_OPENAI_RESPONSES => Some(Self::OpenaiResponses),
            crate::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS => Some(Self::OpenaiChatCompletions),
            crate::WIRE_PROTOCOL_ANTHROPIC_MESSAGES => Some(Self::AnthropicMessages),
            crate::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI => Some(Self::GoogleGenerativeAi),
            crate::WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE => Some(Self::AwsBedrockConverse),
            _ => None,
        }
    }

    pub fn protocol_base_url_option_key(self) -> String {
        format!("protocolBaseUrl.{}", self.wire_protocol_id())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfiguredModel {
    pub id: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    #[serde(default)]
    pub wire_api: Option<ProviderModelWireApi>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfile {
    pub id: ProviderProfileId,
    pub agent_id: AgentId,
    pub kind: ProviderKind,
    pub display_name: String,
    pub status: ProviderProfileStatus,
    pub account_alias: Option<String>,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub small_model: Option<String>,
    pub large_model: Option<String>,
    pub configured_models: Vec<ProviderConfiguredModel>,
    pub reasoning_effort: Option<String>,
    pub sandbox_defaults: ProviderSandboxDefaults,
    pub network_defaults: ProviderNetworkDefaults,
    pub permission_defaults: ProviderPermissionDefaults,
    pub provider_options: ProviderOptions,
    pub secrets: Vec<ProviderSecretReference>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
}

impl ProviderProfile {
    pub fn local_default(kind: ProviderKind) -> Self {
        let now = crate::unix_timestamp_ms();
        Self {
            id: ProviderProfileId::parse(kind.local_default_profile_id().to_string())
                .expect("local default profile ids must use provider prefix"),
            agent_id: agent_id_for_provider_kind(kind),
            kind,
            display_name: format!("{} local default", kind),
            status: ProviderProfileStatus::Disabled,
            account_alias: None,
            base_url: None,
            default_model: None,
            small_model: None,
            large_model: None,
            configured_models: Vec::new(),
            reasoning_effort: None,
            sandbox_defaults: ProviderSandboxDefaults::workspace_write_ask_on_risk(),
            network_defaults: ProviderNetworkDefaults::local_default(),
            permission_defaults: ProviderPermissionDefaults::ask_on_risk(),
            provider_options: ProviderOptions::empty(),
            secrets: Vec::new(),
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        }
    }

    pub fn summary(&self) -> ProviderProfileSummary {
        ProviderProfileSummary {
            id: self.id.clone(),
            agent_id: self.agent_id.clone(),
            kind: self.kind,
            display_name: self.display_name.clone(),
            status: self.status,
            account_alias: self.account_alias.clone(),
            default_model: self.default_model.clone(),
            configured_models: self.configured_models.clone(),
            secret_setup_state: summarize_secret_setup(&self.secrets),
            updated_at_ms: self.updated_at_ms,
        }
    }
}

fn summarize_secret_setup(secrets: &[ProviderSecretReference]) -> ProviderSecretSetupState {
    if secrets.is_empty() {
        return ProviderSecretSetupState::Missing;
    }
    if secrets
        .iter()
        .any(|secret| secret.setup_state == ProviderSecretSetupState::Available)
    {
        ProviderSecretSetupState::Available
    } else if secrets
        .iter()
        .any(|secret| secret.setup_state == ProviderSecretSetupState::Referenced)
    {
        ProviderSecretSetupState::Referenced
    } else {
        ProviderSecretSetupState::Missing
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfileSummary {
    pub id: ProviderProfileId,
    pub agent_id: AgentId,
    pub kind: ProviderKind,
    pub display_name: String,
    pub status: ProviderProfileStatus,
    pub account_alias: Option<String>,
    pub default_model: Option<String>,
    pub configured_models: Vec<ProviderConfiguredModel>,
    pub secret_setup_state: ProviderSecretSetupState,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfileCreateRequest {
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    pub kind: ProviderKind,
    pub display_name: String,
    pub account_alias: Option<String>,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub small_model: Option<String>,
    pub large_model: Option<String>,
    #[serde(default)]
    pub configured_models: Vec<ProviderConfiguredModel>,
    pub reasoning_effort: Option<String>,
    pub sandbox_defaults: Option<ProviderSandboxDefaults>,
    pub network_defaults: Option<ProviderNetworkDefaults>,
    pub permission_defaults: Option<ProviderPermissionDefaults>,
    pub provider_options: Option<ProviderOptions>,
    pub secret_references: Vec<ProviderSecretReferenceCreateRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSecretReferenceCreateRequest {
    pub secret_kind: ProviderSecretKind,
    pub backend: ProviderSecretBackend,
    pub setup_state: ProviderSecretSetupState,
    pub lookup_key: String,
    pub display_label: String,
    pub redacted_hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfileUpdateRequest {
    pub provider_profile_id: ProviderProfileId,
    pub display_name: Option<String>,
    pub status: Option<ProviderProfileStatus>,
    pub account_alias: Option<String>,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub small_model: Option<String>,
    pub large_model: Option<String>,
    pub configured_models: Option<Vec<ProviderConfiguredModel>>,
    pub reasoning_effort: Option<String>,
    pub sandbox_defaults: Option<ProviderSandboxDefaults>,
    pub network_defaults: Option<ProviderNetworkDefaults>,
    pub permission_defaults: Option<ProviderPermissionDefaults>,
    pub provider_options: Option<ProviderOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfileDuplicateRequest {
    pub provider_profile_id: ProviderProfileId,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfileDeleteRequest {
    pub provider_profile_id: ProviderProfileId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfileDefaultScope {
    pub kind: ProviderDefaultScopeKind,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
}

impl ProviderProfileDefaultScope {
    pub fn storage_key(&self) -> String {
        match self.kind {
            ProviderDefaultScopeKind::Global => "global".to_string(),
            ProviderDefaultScopeKind::Project => self
                .project_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "project_missing".to_string()),
            ProviderDefaultScopeKind::Workspace => self
                .workspace_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "workspace_missing".to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfileSetDefaultRequest {
    pub scope: ProviderProfileDefaultScope,
    pub provider_kind: ProviderKind,
    pub provider_profile_id: ProviderProfileId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfileDefaultSelection {
    pub scope: ProviderProfileDefaultScope,
    pub provider_kind: ProviderKind,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub updated_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderProfileListRequest {
    pub agent_id: AgentId,
    pub include_disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderProfileListResponse {
    pub profiles: Vec<AgentModelProviderProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderProfile {
    pub profile: ProviderProfile,
    pub is_default: bool,
    pub failover_order_index: Option<i64>,
    pub in_failover_queue: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderProfileCreateRequest {
    pub agent_id: AgentId,
    pub display_name: String,
    pub account_alias: Option<String>,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub small_model: Option<String>,
    pub large_model: Option<String>,
    #[serde(default)]
    pub configured_models: Vec<ProviderConfiguredModel>,
    pub reasoning_effort: Option<String>,
    pub sandbox_defaults: Option<ProviderSandboxDefaults>,
    pub network_defaults: Option<ProviderNetworkDefaults>,
    pub permission_defaults: Option<ProviderPermissionDefaults>,
    pub provider_options: Option<ProviderOptions>,
    pub secret_references: Vec<ProviderSecretReferenceCreateRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderProfileUpdateRequest {
    pub agent_id: AgentId,
    pub provider_profile_id: ProviderProfileId,
    pub display_name: Option<String>,
    pub status: Option<ProviderProfileStatus>,
    pub account_alias: Option<String>,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub small_model: Option<String>,
    pub large_model: Option<String>,
    pub configured_models: Option<Vec<ProviderConfiguredModel>>,
    pub reasoning_effort: Option<String>,
    pub sandbox_defaults: Option<ProviderSandboxDefaults>,
    pub network_defaults: Option<ProviderNetworkDefaults>,
    pub permission_defaults: Option<ProviderPermissionDefaults>,
    pub provider_options: Option<ProviderOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderProfileDeleteRequest {
    pub agent_id: AgentId,
    pub provider_profile_id: ProviderProfileId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderProfileFetchModelsRequest {
    pub agent_id: AgentId,
    pub provider_profile_id: ProviderProfileId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderProfileFetchModelsResponse {
    pub agent_id: AgentId,
    pub provider_profile_id: ProviderProfileId,
    pub models: Vec<ProviderConfiguredModel>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderProfileTestRequest {
    pub agent_id: AgentId,
    pub provider_profile_id: ProviderProfileId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderProfileSecretValueRequest {
    pub agent_id: AgentId,
    pub provider_profile_id: ProviderProfileId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderProfileSecretValueResponse {
    pub agent_id: AgentId,
    pub provider_profile_id: ProviderProfileId,
    pub secret_kind: ProviderSecretKind,
    pub backend: ProviderSecretBackend,
    pub setup_state: ProviderSecretSetupState,
    pub lookup_key: Option<String>,
    pub display_label: String,
    pub redacted_hint: String,
    pub value: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderProfileSecretValueUpdateRequest {
    pub agent_id: AgentId,
    pub provider_profile_id: ProviderProfileId,
    pub value: Option<String>,
    pub clear: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentModelProviderTestStatus {
    Pass,
    Warn,
    Fail,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderProfileTestResult {
    pub agent_id: AgentId,
    pub provider_profile_id: ProviderProfileId,
    pub status: AgentModelProviderTestStatus,
    pub code: String,
    pub message: String,
    pub diagnostics: Vec<ProviderBindingMetadata>,
    pub checked_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderDefaultRequest {
    pub scope: ProviderProfileDefaultScope,
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderSetDefaultRequest {
    pub scope: ProviderProfileDefaultScope,
    pub agent_id: AgentId,
    pub provider_profile_id: ProviderProfileId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderDefaultSelection {
    pub scope: ProviderProfileDefaultScope,
    pub agent_id: AgentId,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub updated_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderFailoverEntry {
    pub agent_id: AgentId,
    pub provider_profile_id: ProviderProfileId,
    pub display_name: String,
    pub status: ProviderProfileStatus,
    pub order_index: i64,
    pub enabled: bool,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderFailoverListRequest {
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderFailoverListResponse {
    pub entries: Vec<AgentModelProviderFailoverEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderFailoverSetEntry {
    pub provider_profile_id: ProviderProfileId,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelProviderFailoverSetRequest {
    pub agent_id: AgentId,
    pub entries: Vec<AgentModelProviderFailoverSetEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderInjectionStrategy {
    SdkParameters,
    CliArgs,
    ProcessEnvironment,
    TemporaryConfigOverlay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInjectionField {
    pub key: String,
    pub value: String,
    pub secret: bool,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInjectionOverlayFile {
    pub path: String,
    pub description: String,
    pub redacted_preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInjectionPreviewRequest {
    pub provider_profile_id: ProviderProfileId,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
    pub session_id: Option<VibexSessionId>,
    pub persist: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInjectionPreview {
    pub preview_id: RequestId,
    pub profile: ProviderProfileSummary,
    pub strategy_order: Vec<ProviderInjectionStrategy>,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub sdk_options: Vec<ProviderInjectionField>,
    pub cli_args: Vec<ProviderInjectionField>,
    pub env: Vec<ProviderInjectionField>,
    pub overlay_files: Vec<ProviderInjectionOverlayFile>,
    pub mcp_servers: Vec<String>,
    pub skills: Vec<String>,
    pub sandbox_defaults: ProviderSandboxDefaults,
    pub network_defaults: ProviderNetworkDefaults,
    pub permission_defaults: ProviderPermissionDefaults,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderHealthProbeKind {
    BinaryExists,
    Version,
    AuthStatus,
    ModelList,
    StreamingFirstByte,
    SimplePrompt,
}

impl ProviderHealthProbeKind {
    pub const fn all() -> [Self; 6] {
        [
            Self::BinaryExists,
            Self::Version,
            Self::AuthStatus,
            Self::ModelList,
            Self::StreamingFirstByte,
            Self::SimplePrompt,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderHealthStatus {
    Unknown,
    Pass,
    Warn,
    Fail,
    Skipped,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderHealthProbeRequest {
    pub provider_profile_id: ProviderProfileId,
    pub probe_kind: ProviderHealthProbeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderHealthProbeResult {
    pub health_record_id: RequestId,
    pub provider_profile_id: ProviderProfileId,
    pub provider_kind: ProviderKind,
    pub probe_kind: ProviderHealthProbeKind,
    pub status: ProviderHealthStatus,
    pub summary: String,
    pub latency_ms: Option<u32>,
    pub checked_at_ms: i64,
    pub expires_at_ms: Option<i64>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderHealthSummary {
    pub profile: ProviderProfileSummary,
    pub overall_status: ProviderHealthStatus,
    pub last_checked_at_ms: Option<i64>,
    pub expires_at_ms: Option<i64>,
    pub probe_results: Vec<ProviderHealthProbeResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRunHealthProbesRequest {
    pub provider_profile_ids: Option<Vec<ProviderProfileId>>,
    pub probe_kinds: Option<Vec<ProviderHealthProbeKind>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRunHealthProbesResult {
    pub results: Vec<ProviderHealthProbeResult>,
    pub summaries: Vec<ProviderHealthSummary>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCapabilityProbeStatus {
    Unknown,
    Pass,
    Warn,
    Fail,
    Stale,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilityProbeRequest {
    pub provider_profile_id: ProviderProfileId,
    pub force_refresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilityProbeResult {
    pub capability_record_id: RequestId,
    pub provider_profile_id: ProviderProfileId,
    pub provider_kind: ProviderKind,
    pub status: ProviderCapabilityProbeStatus,
    pub summary: String,
    pub capabilities: ProviderCapabilities,
    pub source: String,
    pub checked_at_ms: i64,
    pub expires_at_ms: Option<i64>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilitySummary {
    pub profile: ProviderProfileSummary,
    pub status: ProviderCapabilityProbeStatus,
    pub effective_capabilities: ProviderCapabilities,
    pub capability_source: String,
    pub fresh: bool,
    pub last_checked_at_ms: Option<i64>,
    pub expires_at_ms: Option<i64>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRunCapabilityProbesRequest {
    pub provider_profile_ids: Option<Vec<ProviderProfileId>>,
    pub force_refresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRunCapabilityProbesResult {
    pub results: Vec<ProviderCapabilityProbeResult>,
    pub summaries: Vec<ProviderCapabilitySummary>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsageUnit {
    Tokens,
    Requests,
    Usd,
    Credits,
    Percent,
    ContextWindow,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageWindow {
    pub label: String,
    pub started_at_ms: Option<i64>,
    pub ends_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageBalance {
    pub unit: ProviderUsageUnit,
    pub label: String,
    pub used: Option<f64>,
    pub limit_value: Option<f64>,
    pub remaining: Option<f64>,
    pub window: Option<ProviderUsageWindow>,
    pub recorded_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageRecord {
    pub usage_record_id: RequestId,
    pub provider_profile_id: ProviderProfileId,
    pub provider_kind: ProviderKind,
    pub source: String,
    pub unit: ProviderUsageUnit,
    pub label: String,
    pub used: Option<f64>,
    pub limit_value: Option<f64>,
    pub remaining: Option<f64>,
    pub window: Option<ProviderUsageWindow>,
    pub recorded_at_ms: i64,
    pub metadata: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageSummary {
    pub profile: ProviderProfileSummary,
    pub balances: Vec<ProviderUsageBalance>,
    pub latest_recorded_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageListRequest {
    pub provider_profile_ids: Option<Vec<ProviderProfileId>>,
    pub include_empty: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFailoverRecommendationStatus {
    NoAction,
    Recommended,
    Blocked,
    InsufficientData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFailoverRecommendationReason {
    FailingHealth,
    MissingAuth,
    UsageExhausted,
    StaleHealth,
    CandidateAvailable,
    NoCandidate,
    DisabledProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderFailoverRecommendation {
    pub recommendation_id: RequestId,
    pub source_profile: ProviderProfileSummary,
    pub candidate_profile: Option<ProviderProfileSummary>,
    pub status: ProviderFailoverRecommendationStatus,
    pub reasons: Vec<ProviderFailoverRecommendationReason>,
    pub confidence: f64,
    pub message: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderFailoverRecommendationRequest {
    pub provider_profile_ids: Option<Vec<ProviderProfileId>>,
    pub max_candidates_per_profile: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderNativeImportSource {
    Codex,
    Claude,
    CcSwitch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderNativeConfigFileKind {
    CodexAuthJson,
    CodexConfigToml,
    CodexModelsCacheJson,
    CodexModelCatalogJson,
    ClaudeSettingsJson,
    ClaudeLegacyJson,
    ClaudeMcpJson,
    CcSwitchDatabase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderNativeConfigFileStatus {
    Missing,
    Parsed,
    ParseError,
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeImportDiagnostic {
    pub code: String,
    pub message: String,
    pub source: ProviderNativeImportSource,
    pub file_kind: Option<ProviderNativeConfigFileKind>,
    pub redacted_details: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeConfigFile {
    pub source: ProviderNativeImportSource,
    pub kind: ProviderNativeConfigFileKind,
    pub path: String,
    pub status: ProviderNativeConfigFileStatus,
    pub diagnostics: Vec<ProviderNativeImportDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderNativeImportItemStatus {
    Importable,
    NeedsSecretSetup,
    Partial,
    BlockedByParseError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeImportRedactedField {
    pub key: String,
    pub source: ProviderNativeImportSource,
    pub file_kind: ProviderNativeConfigFileKind,
    pub hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeImportItem {
    pub import_item_id: RequestId,
    pub source: ProviderNativeImportSource,
    pub provider_kind: ProviderKind,
    pub agent_id: Option<AgentId>,
    pub display_name: String,
    pub account_alias: Option<String>,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub small_model: Option<String>,
    pub large_model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub provider_options: ProviderOptions,
    pub secret_references: Vec<ProviderSecretReferenceCreateRequest>,
    pub status: ProviderNativeImportItemStatus,
    pub redacted_fields: Vec<ProviderNativeImportRedactedField>,
    pub diagnostics: Vec<ProviderNativeImportDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeImportPreviewRequest {
    pub sources: Vec<ProviderNativeImportSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeImportPreview {
    pub preview_id: RequestId,
    pub sources: Vec<ProviderNativeImportSource>,
    pub files: Vec<ProviderNativeConfigFile>,
    pub items: Vec<ProviderNativeImportItem>,
    pub diagnostics: Vec<ProviderNativeImportDiagnostic>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeImportCreateRequest {
    pub preview_request: ProviderNativeImportPreviewRequest,
    pub import_item_id: RequestId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeImportCreateResult {
    pub profile: ProviderProfile,
    pub source: ProviderNativeImportSource,
    pub diagnostics: Vec<ProviderNativeImportDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderNativeExportSource {
    Codex,
    Claude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderNativeExportMode {
    ProviderProfile,
    Mcp,
    Skills,
    Prompts,
    Combined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderNativeExportOperationKind {
    CreateFile,
    UpdateFile,
    Blocked,
    NoOp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderNativeExportFileStatus {
    Ready,
    Blocked,
    Applied,
    Restored,
    Failed,
    NoOp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderNativeExportApplyStatus {
    Applied,
    PartiallyApplied,
    FailedRestored,
    FailedUnrestored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderNativeExportRollbackStatus {
    Restored,
    PartiallyRestored,
    Failed,
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeExportPreviewRequest {
    pub provider_profile_id: ProviderProfileId,
    pub source: ProviderNativeExportSource,
    pub mode: ProviderNativeExportMode,
    pub persist: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeExportFilePlan {
    pub operation_id: RequestId,
    pub source: ProviderNativeExportSource,
    pub file_kind: ProviderNativeConfigFileKind,
    pub operation_kind: ProviderNativeExportOperationKind,
    pub target_path: String,
    pub backup_path: Option<String>,
    pub temp_path: Option<String>,
    pub marker: Option<String>,
    pub redacted_before: String,
    pub redacted_after: String,
    pub redacted_diff: String,
    pub rollback_plan: String,
    pub diagnostics: Vec<ProviderBindingMetadata>,
    pub status: ProviderNativeExportFileStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeExportPreview {
    pub export_id: RequestId,
    pub provider_profile_id: ProviderProfileId,
    pub source: ProviderNativeExportSource,
    pub mode: ProviderNativeExportMode,
    pub files: Vec<ProviderNativeExportFilePlan>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeExportApplyRequest {
    pub export_id: RequestId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeExportApplyResult {
    pub export_id: RequestId,
    pub status: ProviderNativeExportApplyStatus,
    pub files: Vec<ProviderNativeExportFilePlan>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
    pub applied_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeExportRollbackRequest {
    pub export_id: RequestId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeExportRollbackResult {
    pub export_id: RequestId,
    pub status: ProviderNativeExportRollbackStatus,
    pub files: Vec<ProviderNativeExportFilePlan>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
    pub rolled_back_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeExportListRequest {
    pub provider_profile_id: Option<ProviderProfileId>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeExportRecordSummary {
    pub export_id: RequestId,
    pub provider_profile_id: ProviderProfileId,
    pub source: ProviderNativeExportSource,
    pub mode: ProviderNativeExportMode,
    pub status: String,
    pub file_count: u32,
    pub blocked_count: u32,
    pub applied_at_ms: Option<i64>,
    pub rolled_back_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub kind: ProviderKind,
    pub version: ProviderVersionInfo,
    pub streaming: bool,
    pub session_persistence: bool,
    pub session_listing: bool,
    pub dynamic_modes: bool,
    pub model_list: bool,
    pub mcp_servers: bool,
    pub slash_commands: bool,
    pub skills: bool,
    pub reasoning_stream: bool,
    pub plan: bool,
    pub tool_invocations: bool,
    pub permission_requests: bool,
    #[serde(default)]
    pub elicitation: bool,
    pub image_input: bool,
    pub file_attachments: bool,
    pub fork_rollback: bool,
    pub interrupt: bool,
    pub terminal_tools: bool,
    pub terminal_auth: bool,
    pub terminal_activity_hooks: bool,
}

impl ProviderCapabilities {
    pub fn conservative(kind: ProviderKind, capability_source: impl Into<String>) -> Self {
        Self {
            kind,
            version: ProviderVersionInfo {
                provider_version: None,
                adapter_version: env!("CARGO_PKG_VERSION").to_string(),
                capability_source: capability_source.into(),
            },
            streaming: false,
            session_persistence: false,
            session_listing: false,
            dynamic_modes: false,
            model_list: false,
            mcp_servers: false,
            slash_commands: false,
            skills: false,
            reasoning_stream: false,
            plan: false,
            tool_invocations: false,
            permission_requests: false,
            elicitation: false,
            image_input: false,
            file_attachments: false,
            fork_rollback: false,
            interrupt: false,
            terminal_tools: false,
            terminal_auth: false,
            terminal_activity_hooks: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNativeBinding {
    pub native_session_id: Option<String>,
    pub native_thread_id: Option<String>,
    pub native_resume_token: Option<String>,
    pub session_config_state: Option<ProviderSessionConfigState>,
    pub redacted_metadata: Vec<ProviderBindingMetadata>,
}

impl ProviderNativeBinding {
    pub fn empty() -> Self {
        Self {
            native_session_id: None,
            native_thread_id: None,
            native_resume_token: None,
            session_config_state: None,
            redacted_metadata: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSessionConfigOptionKind {
    Boolean,
    Select,
    String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSessionConfigValue {
    pub value: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSessionConfigOption {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub kind: ProviderSessionConfigOptionKind,
    pub current_value: Option<ProviderSessionConfigValue>,
    pub default_value: Option<ProviderSessionConfigValue>,
    pub values: Vec<ProviderSessionConfigValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSessionConfigState {
    pub provider_kind: ProviderKind,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub native_session_id: Option<String>,
    pub current_model: Option<ProviderSessionConfigValue>,
    pub models: Vec<ProviderSessionConfigValue>,
    pub current_mode: Option<ProviderSessionConfigValue>,
    pub modes: Vec<ProviderSessionConfigValue>,
    pub options: Vec<ProviderSessionConfigOption>,
    pub source: String,
    pub updated_at_ms: i64,
    pub metadata: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderBindingMetadata {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderBinding {
    pub session_id: VibexSessionId,
    pub provider_kind: ProviderKind,
    pub provider_profile_id: ProviderProfileId,
    pub native: ProviderNativeBinding,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterDiagnosticLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterDiagnostic {
    pub session_id: Option<VibexSessionId>,
    pub provider_kind: ProviderKind,
    pub level: AdapterDiagnosticLevel,
    pub code: String,
    pub message: String,
    pub redacted_details: Vec<ProviderBindingMetadata>,
    pub timestamp_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::{ProviderConfiguredModel, ProviderModelWireApi};

    #[test]
    fn model_wire_protocol_ids_round_trip_the_canonical_five_protocols() {
        assert_eq!(ProviderModelWireApi::ALL.len(), 5);
        for wire_api in ProviderModelWireApi::ALL {
            let protocol_id = wire_api.wire_protocol_id();
            assert_eq!(
                ProviderModelWireApi::from_wire_protocol_id(protocol_id),
                Some(wire_api)
            );
            assert_eq!(
                wire_api.protocol_base_url_option_key(),
                format!("protocolBaseUrl.{protocol_id}")
            );
        }
        assert_eq!(
            ProviderModelWireApi::from_wire_protocol_id("openai_completions"),
            None
        );
    }

    #[test]
    fn configured_model_without_wire_api_remains_backward_compatible() {
        let model: ProviderConfiguredModel =
            serde_json::from_str(r#"{"id":"legacy-model","displayName":null,"enabled":true}"#)
                .unwrap();

        assert_eq!(model.id, "legacy-model");
        assert_eq!(model.wire_api, None);

        let anthropic: ProviderConfiguredModel = serde_json::from_str(
            r#"{"id":"claude-test","displayName":"Claude Test","enabled":true,"wireApi":"anthropic_messages"}"#,
        )
        .unwrap();
        assert_eq!(
            anthropic.wire_api,
            Some(ProviderModelWireApi::AnthropicMessages)
        );
    }
}

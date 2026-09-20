//! Marketplace catalog vocabulary.
//!
//! These are pure data types: no I/O, no network, no clock. The fetch lives in
//! `vibex-config-switch` because the authoritative state owner is the one that
//! talks to the network; the desktop renders these values and never fetches a
//! catalog itself. Keeping the mapping types I/O-free is what lets the catalog
//! adapters be unit-tested without a socket.

use serde::{Deserialize, Serialize};

use crate::agent_config::AgentId;
use crate::provider::{
    McpServer, McpServerCreateRequest, McpServerEnvEntry, McpServerTransportKind,
    ProviderBindingMetadata, Skill,
};

/// Where a market source's entries come from.
///
/// The kind decides which adapter parses the response, so a source whose kind
/// does not match its payload fails as one source instead of poisoning the
/// merged list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketSourceKind {
    /// An MCP registry speaking the official registry protocol.
    McpRegistry,
    /// A static MCP catalog JSON document.
    McpCatalog,
    /// A static Skill catalog JSON document.
    SkillCatalog,
    /// A GitHub repository scanned for `SKILL.md` documents.
    SkillRepository,
}

impl MarketSourceKind {
    pub const fn is_mcp(self) -> bool {
        matches!(self, Self::McpRegistry | Self::McpCatalog)
    }

    pub const fn is_skill(self) -> bool {
        matches!(self, Self::SkillCatalog | Self::SkillRepository)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::McpRegistry => "mcp_registry",
            Self::McpCatalog => "mcp_catalog",
            Self::SkillCatalog => "skill_catalog",
            Self::SkillRepository => "skill_repository",
        }
    }

    /// Parses the wire name. Named `parse` rather than `from_str` so it
    /// cannot be mistaken for the `std::str::FromStr` trait method.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "mcp_registry" => Some(Self::McpRegistry),
            "mcp_catalog" => Some(Self::McpCatalog),
            "skill_catalog" => Some(Self::SkillCatalog),
            "skill_repository" => Some(Self::SkillRepository),
            _ => None,
        }
    }
}

/// A user-configurable or builtin catalog source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketSource {
    pub id: String,
    pub name: String,
    pub url: String,
    pub kind: MarketSourceKind,
    /// Builtin sources ship with the app: they cannot be edited or removed, and
    /// they stay as the offline floor when every user source is unreachable.
    #[serde(default)]
    pub builtin: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketSourceListResponse {
    pub sources: Vec<MarketSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketSourceSetRequest {
    pub sources: Vec<MarketSource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpMarketCategory {
    Devtools,
    Web,
    Docs,
    Data,
    Productivity,
}

impl McpMarketCategory {
    pub const ALL: [Self; 5] = [
        Self::Devtools,
        Self::Web,
        Self::Docs,
        Self::Data,
        Self::Productivity,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Devtools => "devtools",
            Self::Web => "web",
            Self::Docs => "docs",
            Self::Data => "data",
            Self::Productivity => "productivity",
        }
    }

    /// Parses the wire name. Named `parse` rather than `from_str` so it
    /// cannot be mistaken for the `std::str::FromStr` trait method.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|category| category.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillMarketCategory {
    Workflow,
    Writing,
    Coding,
    Data,
    Docs,
}

impl SkillMarketCategory {
    pub const ALL: [Self; 5] = [
        Self::Workflow,
        Self::Writing,
        Self::Coding,
        Self::Data,
        Self::Docs,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workflow => "workflow",
            Self::Writing => "writing",
            Self::Coding => "coding",
            Self::Data => "data",
            Self::Docs => "docs",
        }
    }

    /// Parses the wire name. Named `parse` rather than `from_str` so it
    /// cannot be mistaken for the `std::str::FromStr` trait method.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|category| category.as_str() == value)
    }
}

/// One environment variable a market entry needs before it can run.
///
/// `secret` asks the install form to mask the value; `defaultValue` is what the
/// catalog publisher supplied and only ever pre-fills the field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketEnvRequirement {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub default_value: Option<String>,
    #[serde(default)]
    pub placeholder: Option<String>,
}

/// One installable MCP server as the market lists it.
///
/// This is a *template*, not a saved server: it carries the launcher fields the
/// catalog published and never an id from the local database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpMarketEntry {
    pub id: String,
    pub source_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub categories: Vec<McpMarketCategory>,
    pub transport: McpServerTransportKind,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub env: Vec<MarketEnvRequirement>,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
}

/// One installable Skill as the market lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketEntry {
    pub id: String,
    pub source_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub categories: Vec<SkillMarketCategory>,
    /// Absolute HTTPS URL of the raw markdown document.
    pub document_url: String,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
}

/// A fetched Skill document split from its frontmatter.
///
/// The body excludes the original frontmatter block: the create path renders
/// its own, so carrying the original through would produce a doubled block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketDocument {
    pub entry_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub body: String,
    pub bytes: u64,
    /// True when the document exceeds the Skill write limit. The UI refuses the
    /// install instead of letting the host reject it after the fact.
    pub too_large: bool,
}

/// Why one source contributed nothing.
///
/// A search never fails as a whole because one source is down; the failure is
/// reported per source so the UI can name the host that refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketSourceFailure {
    pub source_id: String,
    pub source_name: String,
    pub code: String,
    pub message: String,
    /// The host that actually failed, which for a repository scan is the API or
    /// CDN host rather than the URL the user typed.
    #[serde(default)]
    pub host: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpMarketSearchRequest {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub category: Option<McpMarketCategory>,
    /// Empty means "every enabled source".
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpMarketSearchResponse {
    pub entries: Vec<McpMarketEntry>,
    #[serde(default)]
    pub failed_sources: Vec<MarketSourceFailure>,
    /// True when at least one source still has pages left to serve.
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketSearchRequest {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub category: Option<SkillMarketCategory>,
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketSearchResponse {
    pub entries: Vec<SkillMarketEntry>,
    #[serde(default)]
    pub failed_sources: Vec<MarketSourceFailure>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpMarketEntryRequest {
    pub source_id: String,
    pub entry_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketDocumentRequest {
    pub source_id: String,
    pub entry_id: String,
}

/// Install one market entry as a real MCP server.
///
/// `candidate` is the editable install form's result. When present it wins over
/// the catalog template, which is what makes the JSON escape hatch work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpMarketInstallRequest {
    pub source_id: String,
    pub entry_id: String,
    #[serde(default)]
    pub agent_ids: Vec<AgentId>,
    #[serde(default)]
    pub env_values: Vec<McpServerEnvEntry>,
    #[serde(default)]
    pub candidate: Option<McpServerCreateRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpMarketInstallResult {
    pub server: McpServer,
    /// False when an existing server for the same entry was updated instead.
    pub created: bool,
    pub enabled_agent_ids: Vec<AgentId>,
    /// Selected agents that cannot host this transport. They are reported
    /// rather than silently dropped, and their stale entries are removed.
    pub skipped_agent_ids: Vec<AgentId>,
    #[serde(default)]
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

/// Install one market entry as a real Skill.
///
/// The previewed `document` is re-sent so the bytes that were disclosed are the
/// bytes that get written: install cannot be swapped out from under a preview
/// the user already approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketInstallRequest {
    pub source_id: String,
    pub entry_id: String,
    #[serde(default)]
    pub agent_ids: Vec<AgentId>,
    pub document: SkillMarketDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketInstallResult {
    pub skill: Skill,
    pub created: bool,
    pub enabled_agent_ids: Vec<AgentId>,
    #[serde(default)]
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

/// Skill documents are markdown instructions an Agent will follow, so the
/// market refuses anything larger than the host will accept.
pub const MAX_SKILL_MARKET_DOCUMENT_BYTES: u64 = 128 * 1024;

/// Upper bound on a catalog response body. A catalog is a list of small
/// records; anything larger is a misconfigured or hostile source.
pub const MAX_MARKET_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// At most this many sources are queried for one search.
pub const MAX_MARKET_SOURCES_PER_SEARCH: usize = 16;

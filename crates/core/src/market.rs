//! Marketplace catalog vocabulary.
//!
//! These are pure data types: no I/O, no network, no clock. The fetch lives in
//! `vibex-config-switch` because the authoritative state owner is the one that
//! talks to the network; the desktop renders these values and never fetches a
//! catalog itself. Keeping the mapping types I/O-free is what lets the catalog
//! adapters be unit-tested without a socket.
//!
//! Each market has exactly one upstream. MCP reads the official registry; the
//! Skill market reads a public skill index. Nothing here describes where a
//! catalog comes from, because that is not a user decision.

use serde::{Deserialize, Serialize};

use crate::agent_config::AgentId;
use crate::provider::{
    McpServer, McpServerCreateRequest, McpServerEnvEntry, McpServerTransportKind,
    ProviderBindingMetadata, Skill,
};

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
/// registry published and never an id from the local database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpMarketEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
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

/// One installable Skill as the public index lists it.
///
/// The index publishes a name, the repository it lives in, and an install
/// count; it does not publish the document or a description. The document is
/// resolved when the user asks to see one, so a search stays a single request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketEntry {
    /// Index id, `owner/repo/skill`.
    pub id: String,
    /// Directory name of the skill inside its repository.
    pub skill_id: String,
    pub name: String,
    /// Repository the skill lives in, `owner/repo`.
    pub source: String,
    #[serde(default)]
    pub installs: u64,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpMarketSearchRequest {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpMarketSearchResponse {
    pub entries: Vec<McpMarketEntry>,
    /// True when the registry reported another page.
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketSearchRequest {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketSearchResponse {
    pub entries: Vec<SkillMarketEntry>,
    /// How many entries this response carries.
    ///
    /// The index reports a count capped at the page size and ignores the
    /// pagination offset, so there is no grand total to report and no second
    /// page to fetch. This equals `entries.len()`.
    pub total: u64,
    /// Always false for this market: the index cannot serve a further page.
    pub has_more: bool,
}

/// Resolve one Skill document.
///
/// The repository and directory are carried explicitly rather than parsed back
/// out of the entry id, because a directory name may itself contain a slash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketDocumentRequest {
    pub entry_id: String,
    pub source: String,
    pub skill_id: String,
}

/// Install one market entry as a real MCP server.
///
/// `candidate` is the entry the user was shown, filled in with whatever the
/// install form collected. It is validated exactly like a hand-written server,
/// so the market adds no write path of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpMarketInstallRequest {
    pub entry_id: String,
    #[serde(default)]
    pub agent_ids: Vec<AgentId>,
    #[serde(default)]
    pub env_values: Vec<McpServerEnvEntry>,
    pub candidate: McpServerCreateRequest,
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

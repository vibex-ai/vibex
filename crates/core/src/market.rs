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
    /// What the value is, for a form that has to say where it goes.
    ///
    /// The registry publishes a header with a remote entry and an environment
    /// variable with a package; both become inputs, and the saved server needs
    /// the difference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<MarketEnvTarget>,
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

/// Where a market input's value is written on the saved server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MarketEnvTarget {
    /// An environment variable of a server Vibex starts.
    Env,
    /// A header sent to a remote server.
    Header,
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
    pub version: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    /// Source repository the publisher named, when it named one.
    #[serde(default)]
    pub repository: Option<String>,
    /// Which published form the launcher was built from: `npm`, `pypi`, or
    /// `remote`. The registry also publishes `oci` and `mcpb` packages, but the
    /// product cannot start those, so an entry is only ever built from a form
    /// the Agent can actually run.
    #[serde(default)]
    pub package_kind: Option<String>,
    /// Registry lifecycle status: `active`, `deprecated`, or `deleted`.
    #[serde(default)]
    pub status: Option<String>,
    /// RFC3339 timestamp of the registry's last update to this version.
    #[serde(default)]
    pub updated_at: Option<String>,
    /// Picture the registry published for this entry, when it published one.
    ///
    /// It is the publisher's own address, not a copy: the market loads it
    /// through the runtime so a paired client shows the same picture without
    /// reaching the publisher itself.
    #[serde(default)]
    pub icon_url: Option<String>,
}

/// One installable Skill as the registry lists it.
///
/// The registry publishes a name, a publisher, and the counters it ranks by; it
/// does not publish the document. The document is resolved when the user asks
/// to see one, so a search stays a single request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketEntry {
    /// Registry identity, `ownerHandle/slug`, or the bare slug when the
    /// registry did not name a publisher.
    ///
    /// This is what a stored Skill records as its origin, so it has to be
    /// stable across a rename of the display name.
    pub id: String,
    /// Routable slug. The registry keys a Skill by this, and it is what the
    /// document and download endpoints take.
    pub slug: String,
    /// Publisher handle.
    ///
    /// A slug is not unique across publishers: the registry answers with the
    /// candidate handles rather than a document when several owners hold the
    /// same one, so the handle travels with every request that resolves a slug.
    #[serde(default)]
    pub owner_handle: Option<String>,
    pub name: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub installs: u64,
    #[serde(default)]
    pub stars: u64,
    /// Registry update time, epoch milliseconds.
    #[serde(default)]
    pub updated_at: Option<i64>,
}

/// Ranking a Skill search asks the registry for.
///
/// The registry ranks its own catalog, so an empty query returns its order
/// rather than the caller's; asking for a ranking is therefore how a browse
/// view states what it wants to see first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillMarketSort {
    /// The registry's own default ranking.
    Recommended,
    Downloads,
    Stars,
    Installs,
    Updated,
    Newest,
    Trending,
}

impl SkillMarketSort {
    /// The registry's own name for this ranking. Sending anything else is a
    /// bad request, so the vocabulary is closed here rather than at the call.
    pub fn as_registry_value(self) -> &'static str {
        match self {
            Self::Recommended => "recommended",
            Self::Downloads => "downloads",
            Self::Stars => "stars",
            Self::Installs => "installs",
            Self::Updated => "updated",
            Self::Newest => "newest",
            Self::Trending => "trending",
        }
    }
}

/// One text file a Skill bundle carries beside its manifest.
///
/// The content travels with the document so the bytes disclosed by the preview
/// are the bytes the install writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketBundleFile {
    /// Path inside the Skill folder, `/`-separated and relative.
    pub path: String,
    pub content: String,
    pub bytes: u64,
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
    /// Text files the bundle carries beside its manifest, in write order.
    ///
    /// A published Skill is a folder, not one file: references, scripts and
    /// templates are part of the instructions. They travel here so an install
    /// writes the same bundle the preview disclosed.
    #[serde(default)]
    pub files: Vec<SkillMarketBundleFile>,
    /// Bytes the whole bundle occupies, manifest included.
    #[serde(default)]
    pub bundle_bytes: u64,
    /// Files the bundle carried that Vibex will not write, each with the reason.
    ///
    /// Reported rather than dropped silently: a Skill whose scripts did not
    /// arrive is not the Skill the publisher described.
    #[serde(default)]
    pub skipped_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpMarketSearchRequest {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    /// Entries to skip before the window starts.
    ///
    /// The registry pages by an opaque cursor, but a pager counts pages, so the
    /// runtime walks the cursor until the offset is covered and cuts the window
    /// from what it read.
    #[serde(default)]
    pub offset: Option<u32>,
    /// Cursor the previous answer ended on, echoed back to reach the next one.
    ///
    /// When it is present the runtime starts there instead of walking from the
    /// first page, which is what keeps deep paging cheap.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Narrow the answer to one transport family, before the window is cut.
    #[serde(default)]
    pub transport: Option<McpMarketTransportFilter>,
}

/// Which transport family a market search is narrowed to.
///
/// `http` and `sse` share one answer: from a user's side both are a remote
/// service, and two names for one decision would be a worse filter than one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpMarketTransportFilter {
    /// A server Vibex runs as a local process.
    Local,
    /// A server Vibex connects to over the network.
    Remote,
}

impl McpMarketTransportFilter {
    /// Whether an entry belongs to the filtered family.
    pub fn matches(self, transport: McpServerTransportKind) -> bool {
        match self {
            Self::Local => transport == McpServerTransportKind::Stdio,
            Self::Remote => transport != McpServerTransportKind::Stdio,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpMarketSearchResponse {
    pub entries: Vec<McpMarketEntry>,
    /// True when the registry has more entries after this window.
    pub has_more: bool,
    /// Cursor the next window starts at, absent when nothing follows.
    ///
    /// An opaque registry value the caller echoes back untouched; reading
    /// further is a request to the registry rather than a page the client cut
    /// for itself.
    #[serde(default)]
    pub next_cursor: Option<String>,
    /// Entries the registry matched for this question, across the window read.
    ///
    /// A search counts what the registry returned rather than what fits on one
    /// page, which is what lets the pager show a real number of pages instead
    /// of a total that grows under the reader.
    #[serde(default)]
    pub total_matches: usize,
    /// Whether `total_matches` is the registry's own count.
    ///
    /// A ranked search can report it; a browse pages by name and the runtime
    /// stops after a bounded walk, so the count there is what was read.
    #[serde(default)]
    pub total_is_exact: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketSearchRequest {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor the registry handed back with the previous page.
    ///
    /// The registry pages by an opaque cursor rather than by an offset, so a
    /// caller that wants the next page sends back exactly what it was given.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Ranking to ask for. Absent means the registry's own default order.
    #[serde(default)]
    pub sort: Option<SkillMarketSort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketSearchResponse {
    pub entries: Vec<SkillMarketEntry>,
    /// Cursor for the page after this one, absent at the end of the catalog.
    ///
    /// An opaque registry value the caller echoes back untouched; nothing here
    /// interprets it.
    #[serde(default)]
    pub next_cursor: Option<String>,
    /// True when the registry reported another page beyond this one.
    pub has_more: bool,
}

/// Resolve one Skill document.
///
/// The registry identity is carried explicitly rather than parsed back out of
/// the entry id, because a slug may itself contain a slash and a publisher
/// handle is what disambiguates a slug several owners hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillMarketDocumentRequest {
    pub entry_id: String,
    pub slug: String,
    #[serde(default)]
    pub owner_handle: Option<String>,
    /// Version to resolve. Absent means the registry's latest release.
    #[serde(default)]
    pub version: Option<String>,
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

/// Text files one Skill bundle may contribute beside its manifest.
///
/// A published Skill is a folder of references, scripts and templates. The
/// ceiling is set well above what the registry's own bundles carry so a real
/// Skill is never truncated, while a hostile archive still cannot turn one
/// install into an unbounded write.
pub const MAX_SKILL_MARKET_BUNDLE_FILES: usize = 128;

/// Largest single file a bundle may contribute.
pub const MAX_SKILL_MARKET_BUNDLE_FILE_BYTES: u64 = 512 * 1024;

/// Largest total a bundle's files may occupy once written.
pub const MAX_SKILL_MARKET_BUNDLE_BYTES: u64 = 4 * 1024 * 1024;

/// Upper bound on a catalog response body. A catalog is a list of small
/// records; anything larger is a misconfigured or hostile source.
pub const MAX_MARKET_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// Upper bound on a Skill bundle download.
///
/// The registry serves bundles as zip archives, so this bounds the compressed
/// bytes; the uncompressed side is bounded separately by the per-file and
/// per-bundle limits above, which is what makes a decompression bomb a refused
/// file rather than an exhausted process.
pub const MAX_SKILL_MARKET_BUNDLE_DOWNLOAD_BYTES: u64 = 16 * 1024 * 1024;

/// Largest picture the market will download for an entry.
///
/// Icons are avatars and product marks at display size, so a picture larger
/// than this is not one the market asked for; refusing it also keeps a hostile
/// publisher from turning a card into a download.
pub const MAX_MARKET_ICON_BYTES: u64 = 1024 * 1024;

/// One entry's picture as the runtime loaded it.
///
/// The bytes travel with the media type rather than being written to disk: the
/// market shows a picture, it does not hand out files, so a `data:` URL is all
/// the card needs and a paired client needs no second request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpMarketIcon {
    #[serde(default)]
    pub mime: Option<String>,
    #[serde(default)]
    pub bytes: Vec<u8>,
}

impl McpMarketIcon {
    /// Whether the runtime actually loaded a picture.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

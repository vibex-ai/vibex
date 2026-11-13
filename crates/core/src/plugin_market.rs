//! Desktop plugin market vocabulary.
//!
//! These are pure data types: no I/O, no network, no clock. The fetch lives in
//! `vibex-plugin` because the authoritative state owner is the one that talks
//! to the network; the desktop renders these values and never fetches a
//! catalog itself.
//!
//! Two sources feed one list (see the plugin market design):
//!
//! - a **signed static index** (`index.json` + `.sig`) built from a catalog
//!   repository, carrying the identity, permissions and package hashes of every
//!   reviewed plugin;
//! - **GitHub topic discovery** (`vibex-plugin`), which indexes repositories
//!   their authors tagged, without a review or a signature.
//!
//! A plugin is either a `Native` first-party panel compiled into the
//! application (the right rail's Files, Git, Child Agents, ...) or a `Script`
//! package written against the scripting host. Both are managed by the same
//! Plugins page, so both are described by the same projection.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Every category the market may publish.
///
/// The set is closed: a catalog entry naming anything else lands in `Other`
/// and is never used to widen a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginCategory {
    Panels,
    Themes,
    Agents,
    Integrations,
    DeveloperTools,
    Productivity,
    Languages,
    Viewers,
    Other,
}

impl Default for PluginCategory {
    fn default() -> Self {
        Self::Other
    }
}

impl PluginCategory {
    /// Every category, in the order the filter chips show them.
    pub const ALL: [Self; 9] = [
        Self::Panels,
        Self::Themes,
        Self::Agents,
        Self::Integrations,
        Self::DeveloperTools,
        Self::Productivity,
        Self::Languages,
        Self::Viewers,
        Self::Other,
    ];

    /// The stable key a catalog publishes and a stored filter keeps.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Panels => "panels",
            Self::Themes => "themes",
            Self::Agents => "agents",
            Self::Integrations => "integrations",
            Self::DeveloperTools => "developer-tools",
            Self::Productivity => "productivity",
            Self::Languages => "languages",
            Self::Viewers => "viewers",
            Self::Other => "other",
        }
    }

    /// The English label the page falls back to.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Panels => "Panels",
            Self::Themes => "Themes",
            Self::Agents => "Agents & Tools",
            Self::Integrations => "Integrations",
            Self::DeveloperTools => "Developer Tools",
            Self::Productivity => "Productivity",
            Self::Languages => "Languages",
            Self::Viewers => "Viewers",
            Self::Other => "Other",
        }
    }

    /// The glyph a card uses when the entry names no picture.
    pub const fn icon_path(self) -> &'static str {
        match self {
            Self::Panels => "icons/vibex/panel.svg",
            Self::Themes => "icons/vibex/palette.svg",
            Self::Agents => "icons/vibex/bot.svg",
            Self::Integrations => "icons/vibex/plug.svg",
            Self::DeveloperTools => "icons/vibex/terminal.svg",
            Self::Productivity => "icons/vibex/zap.svg",
            Self::Languages => "icons/vibex/languages.svg",
            Self::Viewers => "icons/vibex/eye.svg",
            Self::Other => "icons/vibex/package.svg",
        }
    }

    /// Reads a key a catalog published; an unknown key is `Other`.
    pub fn from_key(value: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|category| category.key() == value)
            .unwrap_or(Self::Other)
    }
}

/// How a browse list is ranked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginMarketSort {
    /// The index's own order: featured first, then installs.
    Recommended,
    /// Most installed first.
    Installs,
    /// Most recently updated first.
    Updated,
    /// Name, case-insensitively.
    Name,
}

impl Default for PluginMarketSort {
    fn default() -> Self {
        Self::Recommended
    }
}

/// What a plugin is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    /// A first-party panel compiled into the application.
    Native,
    /// A package written against the scripting host.
    Script,
}

impl Default for PluginKind {
    fn default() -> Self {
        Self::Script
    }
}

/// Where an installed plugin came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginSource {
    /// Shipped with the application; never installed or removed by the user.
    Bundled,
    /// Installed from the market (the signed index or topic discovery).
    Market,
    /// Installed from a local package file.
    Local,
    /// A development directory the user linked.
    Dev,
}

impl Default for PluginSource {
    fn default() -> Self {
        Self::Market
    }
}

/// Which market source a listed entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginListingSource {
    /// The signed static index.
    Index,
    /// GitHub topic discovery.
    Github,
    /// Shipped with the application.
    Bundled,
    /// A catalog the reader added themselves.
    User,
}

impl Default for PluginListingSource {
    fn default() -> Self {
        Self::Index
    }
}

/// How much a plugin is trusted.
///
/// The order is the strength of the statement: `Official` is first-party and
/// bundled, `Unverified` is anything discovered without a signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginTrust {
    Official,
    Verified,
    Community,
    Unverified,
}

impl Default for PluginTrust {
    fn default() -> Self {
        Self::Unverified
    }
}

impl PluginTrust {
    /// Whether the page may show a "verified" mark.
    pub const fn verified_mark(self) -> bool {
        matches!(self, Self::Official | Self::Verified)
    }

    /// Whether the trust level admits an automatic update.
    ///
    /// A script plugin discovered without a signature never updates on its own.
    pub const fn allows_auto_update(self) -> bool {
        matches!(self, Self::Official | Self::Verified | Self::Community)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Official => "Official",
            Self::Verified => "Verified",
            Self::Community => "Community",
            Self::Unverified => "Unverified",
        }
    }
}

/// How dangerous a permission is, for the consent sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginPermissionRisk {
    Low,
    Medium,
    High,
}

impl Default for PluginPermissionRisk {
    fn default() -> Self {
        Self::Low
    }
}

/// One permission a plugin declares it needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginPermission {
    /// The stable permission key (`storage`, `ui.panel`, `net.fetch`, ...).
    pub key: String,
    /// What the permission reaches, when it has a scope (`api.example.com`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default)]
    pub risk: PluginPermissionRisk,
    /// The publisher's own sentence about why it needs the permission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl PluginPermission {
    pub fn new(key: impl Into<String>, risk: PluginPermissionRisk) -> Self {
        Self {
            key: key.into(),
            scope: None,
            risk,
            reason: None,
        }
    }

    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = Some(scope.into());
        self
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// Who publishes a plugin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginPublisher {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
}

impl PluginPublisher {
    /// The publisher the application itself uses for bundled plugins.
    pub fn vibex() -> Self {
        Self {
            id: "vibex".to_string(),
            display_name: "Vibex".to_string(),
            verified: true,
            homepage: None,
        }
    }

    /// The publisher given to a repository discovered through a topic.
    pub fn from_login(login: &str) -> Self {
        Self {
            id: login.to_lowercase(),
            display_name: login.to_string(),
            verified: false,
            homepage: None,
        }
    }
}

/// A plugin's rating as the index publishes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRating {
    /// Half-star units, 2..=10, absent when nobody rated the plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub average_half_stars: Option<u8>,
    #[serde(default)]
    pub count: u64,
    /// Five whole-star buckets, floored.
    #[serde(default)]
    pub distribution: Vec<u64>,
}

/// Which hosts and platforms a version runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCompatibility {
    /// Desktop platforms the version supports (`macos`, `windows`, `linux`).
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_vibex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_shell: Option<String>,
}

impl Default for PluginCompatibility {
    fn default() -> Self {
        Self {
            platforms: Vec::new(),
            min_vibex: None,
            min_shell: None,
        }
    }
}

impl PluginCompatibility {
    /// The host platform key this build runs as.
    pub const fn host_platform() -> &'static str {
        if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else {
            "linux"
        }
    }

    /// Whether the version admits this machine.
    ///
    /// An empty platform list means every desktop platform.
    pub fn supports_host(&self) -> bool {
        self.platforms.is_empty()
            || self
                .platforms
                .iter()
                .any(|platform| platform == Self::host_platform())
    }
}

/// A localized override one catalog may publish.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketLocalized {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One plugin as the market lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketEntry {
    /// Market identity: `<publisher>.<name>`.
    pub id: String,
    pub name: String,
    /// Native entries are synthesized locally; the index publishes scripts.
    #[serde(default)]
    pub kind: PluginKind,
    #[serde(default)]
    pub bundled: bool,
    #[serde(default)]
    pub listing_source: PluginListingSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    pub publisher: PluginPublisher,
    #[serde(default)]
    pub categories: Vec<PluginCategory>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    #[serde(default)]
    pub installs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<PluginRating>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default)]
    pub trust: PluginTrust,
    #[serde(default)]
    pub permissions: Vec<PluginPermission>,
    #[serde(default)]
    pub compatibility: PluginCompatibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub featured: bool,
    /// The catalog the reader added this entry through, when it was one.
    ///
    /// An entry from the official index or topic discovery has no source name:
    /// only a catalog the reader typed needs saying where it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub i18n: BTreeMap<String, PluginMarketLocalized>,
}

impl PluginMarketEntry {
    /// Whether the entry may be installed on this machine.
    pub fn installable_here(&self) -> bool {
        self.compatibility.supports_host()
    }

    /// The name to show for a locale, falling back to the entry's own.
    pub fn localized_name(&self, locale: &str) -> &str {
        self.i18n
            .get(locale)
            .and_then(|localized| localized.name.as_deref())
            .unwrap_or(&self.name)
    }

    /// The summary to show for a locale, falling back to the entry's own.
    pub fn localized_summary(&self, locale: &str) -> Option<&str> {
        self.i18n
            .get(locale)
            .and_then(|localized| localized.summary.as_deref())
            .or(self.summary.as_deref())
    }

    /// The long description to show for a locale, when one is published.
    pub fn localized_description(&self, locale: &str) -> Option<&str> {
        self.i18n
            .get(locale)
            .and_then(|localized| localized.description.as_deref())
    }
}

/// The detached signature of a package version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSignature {
    pub alg: String,
    pub key_id: String,
    pub signature: String,
}

/// How a version was built.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built_at: Option<String>,
}

/// A review decision the index recorded for a version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginReviewRecord {
    pub decision: String,
    #[serde(default)]
    pub risk: PluginPermissionRisk,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<String>,
}

/// One published version of a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketVersion {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha512: Option<String>,
    /// The package download address, or a `github:owner/repo` spec.
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_vibex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_shell: Option<String>,
    #[serde(default)]
    pub permissions: Vec<PluginPermission>,
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub yanked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yanked_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<PluginSignature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<PluginProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<PluginReviewRecord>,
}

/// A rating summary the review index published.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginReviewsSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub average_half_stars: Option<u8>,
    #[serde(default)]
    pub count: u64,
    #[serde(default)]
    pub distribution: Vec<u64>,
    #[serde(default)]
    pub positive: u64,
    #[serde(default)]
    pub neutral: u64,
    #[serde(default)]
    pub negative: u64,
}

/// One plugin's detail page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketDetail {
    pub entry: PluginMarketEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readme: Option<String>,
    #[serde(default)]
    pub readme_bytes: u64,
    #[serde(default)]
    pub screenshots: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default)]
    pub versions: Vec<PluginMarketVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviews: Option<PluginReviewsSummary>,
    #[serde(default)]
    pub requested_permissions: Vec<PluginPermission>,
    /// The fingerprint of the version the detail page is offering.
    ///
    /// A market install must send this back; the service recomputes it from
    /// the catalog and refuses the install when the two differ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent_fingerprint: Option<String>,
}

impl PluginMarketDetail {
    /// The version a fresh install would take.
    pub fn latest_version(&self) -> Option<&PluginMarketVersion> {
        self.versions.iter().find(|version| !version.yanked)
    }

    /// The version with this exact number, yanked or not.
    pub fn version(&self, version: &str) -> Option<&PluginMarketVersion> {
        self.versions
            .iter()
            .find(|candidate| candidate.version == version)
    }
}

/// Ask the market to fetch and merge both sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketIndexRequest {
    /// Force a refresh instead of serving the cache.
    #[serde(default)]
    pub refresh: bool,
    /// Include GitHub topic discovery. On by default.
    #[serde(default = "default_true")]
    pub include_github: bool,
}

impl Default for PluginMarketIndexRequest {
    fn default() -> Self {
        Self {
            refresh: false,
            include_github: true,
        }
    }
}

/// The merged catalog: signed index entries plus topic-discovered ones.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketIndexResponse {
    pub entries: Vec<PluginMarketEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub revision: u64,
    /// True when the response came from cache after a failed refresh.
    #[serde(default)]
    pub stale: bool,
    /// Why the response is partial. Never passed off as "there are none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial_reason: Option<String>,
}

/// One page of a search over the merged catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketSearchRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<PluginCategory>,
    #[serde(default)]
    pub sort: PluginMarketSort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketSearchResponse {
    pub entries: Vec<PluginMarketEntry>,
    pub has_more: bool,
    #[serde(default)]
    pub total_matches: usize,
}

/// Resolve one plugin's detail page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketDetailRequest {
    pub entry_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// The lowest rating a review may carry.
pub const MIN_PLUGIN_REVIEW_RATING: u8 = 1;
/// The highest rating a review may carry.
pub const MAX_PLUGIN_REVIEW_RATING: u8 = 5;

/// One rating the reader left, kept on this machine.
///
/// There is no review service yet, so a rating is local: the detail page shows
/// it beside the catalog's published aggregate rather than folding it in. When
/// a service exists, it can publish what this stores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginLocalReview {
    pub plugin_id: String,
    /// One to five stars.
    pub rating: u8,
    pub updated_at_ms: i64,
}

/// Sets or replaces the reader's rating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginReviewSubmitRequest {
    pub plugin_id: String,
    pub rating: u8,
}

/// Clears the reader's rating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginReviewClearRequest {
    pub plugin_id: String,
}

/// Checks a rating is within the star range.
pub fn validate_plugin_review_rating(rating: u8) -> Result<(), String> {
    if (MIN_PLUGIN_REVIEW_RATING..=MAX_PLUGIN_REVIEW_RATING).contains(&rating) {
        Ok(())
    } else {
        Err(format!(
            "a rating is between {MIN_PLUGIN_REVIEW_RATING} and {MAX_PLUGIN_REVIEW_RATING} stars"
        ))
    }
}

/// A permission the user granted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginPermissionGrant {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl From<&PluginPermission> for PluginPermissionGrant {
    fn from(permission: &PluginPermission) -> Self {
        Self {
            key: permission.key.clone(),
            scope: permission.scope.clone(),
        }
    }
}

/// Install one market entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketInstallRequest {
    pub entry_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub granted_permissions: Vec<PluginPermissionGrant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent_fingerprint: Option<String>,
    #[serde(default = "default_true")]
    pub enable: bool,
    #[serde(default)]
    pub auto_update: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketInstallResult {
    pub plugin: InstalledPlugin,
    pub created: bool,
}

/// Install a package from a local file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginLocalInstallRequest {
    pub path: String,
    #[serde(default = "default_true")]
    pub enable: bool,
}

/// How an update's permissions differ from the granted ones.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginPermissionDiff {
    #[serde(default)]
    pub added: Vec<PluginPermission>,
    #[serde(default)]
    pub removed: Vec<PluginPermission>,
    /// True when the consent fingerprint changed for any reason.
    #[serde(default)]
    pub fingerprint_changed: bool,
}

impl PluginPermissionDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && !self.fingerprint_changed
    }

    /// Whether the new version asks for a permission the user has not granted.
    ///
    /// The fingerprint changes on every version, so it cannot be the trigger:
    /// otherwise every update would demand a fresh consent screen.
    pub fn requires_consent(&self) -> bool {
        !self.added.is_empty()
    }
}

/// One available update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdate {
    pub plugin_id: String,
    pub installed_version: String,
    pub available_version: String,
    #[serde(default)]
    pub permission_diff: PluginPermissionDiff,
    /// Everything the available version asks for, so the consent sheet can
    /// show the full request rather than only the newly added keys.
    #[serde(default)]
    pub requested_permissions: Vec<PluginPermission>,
    /// The fingerprint of the available version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent_fingerprint: Option<String>,
    #[serde(default)]
    pub yanked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdateCheckRequest {
    #[serde(default)]
    pub plugin_ids: Vec<String>,
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdateCheckResponse {
    pub updates: Vec<PluginUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdateApplyRequest {
    #[serde(default)]
    pub consents: Vec<PluginUpdateConsent>,
}

/// The user's approval of one update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdateConsent {
    pub plugin_id: String,
    /// The exact version the user reviewed.
    pub version: String,
    #[serde(default)]
    pub granted_permissions: Vec<PluginPermissionGrant>,
    pub consent_fingerprint: String,
}

/// The lifecycle state one installed plugin is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstalledPluginStatus {
    Enabled,
    Disabled,
    UpdateAvailable,
    Broken,
    /// The market's revocation list blocks this version.
    Revoked,
}

impl Default for InstalledPluginStatus {
    fn default() -> Self {
        Self::Disabled
    }
}

/// One plugin the application manages, native or script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPlugin {
    pub plugin_id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub kind: PluginKind,
    #[serde(default)]
    pub source: PluginSource,
    #[serde(default)]
    pub bundled: bool,
    #[serde(default)]
    pub uninstallable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin_key: Option<String>,
    #[serde(default)]
    pub publisher: PluginPublisher,
    #[serde(default)]
    pub trust: PluginTrust,
    #[serde(default)]
    pub categories: Vec<PluginCategory>,
    pub enabled: bool,
    #[serde(default)]
    pub auto_update: bool,
    /// The version the user pinned, if any. A pinned plugin is not updated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_version: Option<String>,
    /// The older kept version a rollback would use, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_version: Option<String>,
    #[serde(default)]
    pub status: InstalledPluginStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_path: Option<String>,
    #[serde(default)]
    pub permissions: Vec<PluginPermission>,
    #[serde(default)]
    pub granted_permissions: Vec<PluginPermissionGrant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<PluginUpdate>,
}

impl InstalledPlugin {
    /// Whether the page may offer an uninstall action.
    pub fn can_uninstall(&self) -> bool {
        self.uninstallable && self.kind == PluginKind::Script
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnabledRequest {
    pub plugin_id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUninstallRequest {
    pub plugin_id: String,
    #[serde(default)]
    pub remove_data: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginAutoUpdateRequest {
    pub plugin_id: String,
    pub auto_update: bool,
}

/// Pins a plugin to a version, or clears the pin with `version: null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginVersionPinRequest {
    pub plugin_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Rolls a plugin back to an older kept version, or to the previous one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRollbackRequest {
    pub plugin_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// A contribution one plugin registered with the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginContribution {
    pub plugin_id: String,
    pub kind: String,
    pub contribution_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the contribution points at, when it points at something.
    ///
    /// A web panel carries its address here; a command or a script panel has
    /// nothing to put in it. The field is one string rather than a per-kind
    /// object because every contribution kind the host has so far needs at
    /// most one piece of extra data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default)]
    pub enabled: bool,
}

/// Upper bound on a catalog response body.
pub const MAX_PLUGIN_MARKET_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
/// Upper bound on the README the detail page renders.
pub const MAX_PLUGIN_README_BYTES: u64 = 512 * 1024;
/// Upper bound on the screenshots one entry may publish.
pub const MAX_PLUGIN_SCREENSHOTS: usize = 5;
/// Upper bound on a plugin package download.
pub const MAX_PLUGIN_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;
/// Upper bound on a package once unpacked (the decompression-bomb bound).
pub const MAX_PLUGIN_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;
/// Upper bound on the files one package may contribute.
pub const MAX_PLUGIN_BUNDLE_FILES: usize = 16 * 1024;
/// Upper bound on one bundled file.
pub const MAX_PLUGIN_BUNDLE_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Upper bound on one icon or screenshot.
pub const MAX_PLUGIN_ICON_BYTES: u64 = 1024 * 1024;

/// The largest number of catalogs a reader may add.
///
/// The list is drawn in one card and every source is fetched on a refresh, so
/// the bound keeps a paste-happy reader from turning the catalog into a crawl.
pub const MAX_PLUGIN_USER_SOURCES: usize = 8;

/// What a catalog the reader added is.
///
/// The two kinds read different documents: an index source is the same signed
/// `index.json` the official catalog publishes, while a repository source is a
/// Git repository whose root carries plugin manifests. A repository needs no
/// signature because it publishes no packages and no hashes — every entry it
/// yields installs through the same unverified path topic discovery uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginUserSourceKind {
    /// A signed static index, fetched over https.
    #[default]
    Index,
    /// A Git repository whose root carries plugin manifests.
    Repository,
}

impl PluginUserSourceKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Repository => "repository",
        }
    }
}

/// One catalog the reader added themselves.
///
/// An index source is an ordinary signed index — the same document the official
/// catalog publishes — at an address the reader typed. It is not signed by the
/// root key the application embeds, so it can never grant `Official` or
/// `Verified`: everything it lists is `Community` at best, and its packages
/// still have to match the hashes it publishes.
///
/// A repository source is a Git repository, read the way topic discovery reads
/// one: its manifest becomes an entry, and its archive at a pinned commit is
/// what installs. That path carries no published hash, so it is `Unverified`
/// and never auto-updates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUserSource {
    /// A stable id derived from the address, so two sources cannot collide.
    pub source_id: String,
    /// The name the reader gave it, shown on every entry it lists.
    pub name: String,
    #[serde(default)]
    pub kind: PluginUserSourceKind,
    /// The index address, or the repository's `owner/repo`.
    pub index_url: String,
    /// The public key its signature is checked against.
    ///
    /// Absent means the reader accepted an unsigned catalog, which is only
    /// allowed for a source they added themselves. A repository source never
    /// uses one: it publishes no signed document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    pub added_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<i64>,
    /// Why the last refresh failed, kept so the page can say so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Adds a catalog the reader typed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUserSourceAddRequest {
    pub name: String,
    #[serde(default)]
    pub kind: PluginUserSourceKind,
    /// An `https` index address, or a repository the reader named as
    /// `owner/repo` or a `https://github.com/owner/repo` address.
    pub index_url: String,
    /// The index signer's public key, base64. Required unless the reader
    /// explicitly accepts an unsigned catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    /// Whether the reader accepts a catalog with no signature at all.
    #[serde(default)]
    pub allow_unsigned: bool,
}

/// Removes one catalog the reader added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUserSourceRemoveRequest {
    pub source_id: String,
}

/// Turns one catalog the reader added on or off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUserSourceEnabledRequest {
    pub source_id: String,
    pub enabled: bool,
}

/// Checks a source the reader typed before it is stored.
///
/// An index address must be an absolute `https` URL: it is fetched over the
/// same public-https policy as the official one, so a plaintext or local
/// address would either be refused later or reach a machine on the network.
/// A repository is named `owner/repo` or as a `https://github.com/owner/repo`
/// address, which is normalized to the former; it publishes no signed
/// document, so it never needs a key.
pub fn validate_plugin_user_source(
    name: &str,
    kind: PluginUserSourceKind,
    address: &str,
    public_key: Option<&str>,
    allow_unsigned: bool,
) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("the source needs a name".to_string());
    }
    if name.chars().count() > 60 {
        return Err("the source name is too long".to_string());
    }
    match kind {
        PluginUserSourceKind::Index => {
            let url = url::Url::parse(address.trim())
                .map_err(|error| format!("the index address is not a URL: {error}"))?;
            if url.scheme() != "https" {
                return Err("the index address must be https".to_string());
            }
            if url.host_str().is_none() {
                return Err("the index address has no host".to_string());
            }
            match public_key.map(str::trim).filter(|key| !key.is_empty()) {
                Some(_) => Ok(()),
                None if allow_unsigned => Ok(()),
                None => Err(
                    "the source needs the index signer's public key, or an explicit \
                     acceptance of an unsigned catalog"
                        .to_string(),
                ),
            }
        }
        PluginUserSourceKind::Repository => {
            parse_plugin_repository(address).map(|_| ()).ok_or_else(|| {
                "the repository must be `owner/repo` or https://github.com/owner/repo".to_string()
            })
        }
    }
}

/// Normalizes a repository the reader typed to `owner/repo`.
///
/// Both `owner/repo` and `https://github.com/owner/repo` (with or without a
/// trailing `.git` or slash) are accepted, because those are the two forms a
/// person copies. Anything else — another host, a deep path, a branch — is
/// refused rather than guessed at.
pub fn parse_plugin_repository(address: &str) -> Option<String> {
    let address = address.trim().trim_end_matches('/');
    let path = if let Some(rest) = address
        .strip_prefix("https://github.com/")
        .or_else(|| address.strip_prefix("http://github.com/"))
    {
        rest
    } else if address.contains("://") {
        return None;
    } else {
        address
    };
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut segments = path.split('/');
    let owner = segments.next()?;
    let repo = segments.next()?;
    if segments.next().is_some() {
        return None;
    }
    let valid = |part: &str| {
        !part.is_empty()
            && part.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
            })
    };
    (valid(owner) && valid(repo)).then(|| format!("{owner}/{repo}"))
}

/// The stable id for a catalog address.
///
/// Derived from the address alone so re-adding the same URL updates the source
/// instead of duplicating it, and so the id survives a rename.
pub fn plugin_user_source_id(index_url: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(index_url.trim().as_bytes());
    let mut id = String::with_capacity(16);
    for byte in &digest[..8] {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_category_falls_back_to_other() {
        assert_eq!(PluginCategory::from_key("panels"), PluginCategory::Panels);
        assert_eq!(
            PluginCategory::from_key("not-a-category"),
            PluginCategory::Other
        );
        assert_eq!(PluginCategory::from_key(""), PluginCategory::Other);
    }

    #[test]
    fn category_keys_are_unique_and_round_trip() {
        for category in PluginCategory::ALL {
            assert_eq!(PluginCategory::from_key(category.key()), category);
        }
    }

    #[test]
    fn compatibility_defaults_to_every_platform() {
        let compatibility = PluginCompatibility::default();
        assert!(compatibility.supports_host());
        let only_other = PluginCompatibility {
            platforms: vec!["plan9".to_string()],
            ..Default::default()
        };
        assert!(!only_other.supports_host());
    }

    #[test]
    fn permission_diff_requires_consent_only_for_additions() {
        let removed = PluginPermissionDiff {
            removed: vec![PluginPermission::new(
                "clipboard",
                PluginPermissionRisk::Medium,
            )],
            ..Default::default()
        };
        assert!(!removed.requires_consent());
        let added = PluginPermissionDiff {
            added: vec![PluginPermission::new(
                "net.fetch",
                PluginPermissionRisk::Medium,
            )],
            ..Default::default()
        };
        assert!(added.requires_consent());
        let version_only = PluginPermissionDiff {
            fingerprint_changed: true,
            ..Default::default()
        };
        assert!(!version_only.requires_consent());
    }

    #[test]
    fn bundled_plugins_are_not_uninstallable() {
        let native = InstalledPlugin {
            plugin_id: "vibex.system.files".to_string(),
            name: "Files".to_string(),
            version: "0.1.2".to_string(),
            kind: PluginKind::Native,
            source: PluginSource::Bundled,
            bundled: true,
            uninstallable: false,
            builtin_key: Some("vibex.system.files".to_string()),
            publisher: PluginPublisher::vibex(),
            trust: PluginTrust::Official,
            categories: vec![PluginCategory::Panels],
            enabled: true,
            auto_update: false,
            pinned_version: None,
            rollback_version: None,
            status: InstalledPluginStatus::Enabled,
            install_path: None,
            data_path: None,
            permissions: Vec::new(),
            granted_permissions: Vec::new(),
            consent_fingerprint: None,
            installed_at: None,
            updated_at: None,
            last_checked_at: None,
            last_error: None,
            size_bytes: None,
            update: None,
        };
        assert!(!native.can_uninstall());
    }

    #[test]
    fn trust_decides_auto_update() {
        assert!(PluginTrust::Verified.allows_auto_update());
        assert!(!PluginTrust::Unverified.allows_auto_update());
    }

    #[test]
    fn a_source_needs_https_a_name_and_an_explicit_unsigned_acceptance() {
        assert!(
            validate_plugin_user_source(
                "Acme",
                PluginUserSourceKind::Index,
                "https://acme.example/i.json",
                None,
                false
            )
            .is_err()
        );
        assert!(
            validate_plugin_user_source(
                "Acme",
                PluginUserSourceKind::Index,
                "https://acme.example/i.json",
                None,
                true
            )
            .is_ok()
        );
        assert!(
            validate_plugin_user_source(
                "Acme",
                PluginUserSourceKind::Index,
                "https://acme.example/i.json",
                Some("key"),
                false
            )
            .is_ok()
        );
        assert!(
            validate_plugin_user_source(
                "",
                PluginUserSourceKind::Index,
                "https://acme.example/i.json",
                Some("key"),
                false
            )
            .is_err()
        );
        assert!(
            validate_plugin_user_source(
                "Acme",
                PluginUserSourceKind::Index,
                "http://acme.example/i.json",
                Some("key"),
                false
            )
            .is_err()
        );
        assert!(
            validate_plugin_user_source(
                "Acme",
                PluginUserSourceKind::Index,
                "not a url",
                Some("key"),
                false
            )
            .is_err()
        );
        assert!(
            validate_plugin_user_source(
                "Acme",
                PluginUserSourceKind::Index,
                "https://acme.example/i.json",
                Some("  "),
                false
            )
            .is_err()
        );
    }

    #[test]
    fn a_repository_is_named_or_pasted_and_normalized_to_owner_repo() {
        for address in [
            "acme/widgets",
            "https://github.com/acme/widgets",
            "https://github.com/acme/widgets/",
            "https://github.com/acme/widgets.git",
            "  acme/widgets  ",
        ] {
            assert_eq!(
                parse_plugin_repository(address).as_deref(),
                Some("acme/widgets"),
                "{address}"
            );
        }
        // Another host, a deep path, or a missing half is refused rather than
        // guessed at: fetching the wrong repository is worse than refusing.
        for address in [
            "https://gitlab.com/acme/widgets",
            "https://github.com/acme/widgets/tree/main",
            "acme",
            "",
            "acme/widgets/extra",
            "acme//widgets",
        ] {
            assert_eq!(parse_plugin_repository(address), None, "{address}");
        }
    }

    #[test]
    fn a_repository_source_needs_no_key_but_an_index_one_does() {
        let kind = PluginUserSourceKind::Repository;
        assert!(validate_plugin_user_source("Acme", kind, "acme/widgets", None, false).is_ok());
        assert!(validate_plugin_user_source("Acme", kind, "nonsense", None, false).is_err());
        assert!(validate_plugin_user_source("", kind, "acme/widgets", None, false).is_err());

        let index = PluginUserSourceKind::Index;
        assert!(
            validate_plugin_user_source("Acme", index, "https://acme.example/i.json", None, false)
                .is_err()
        );
    }

    #[test]
    fn a_source_id_is_stable_and_address_derived() {
        let first = plugin_user_source_id("https://acme.example/i.json");
        let second = plugin_user_source_id(" https://acme.example/i.json ");
        let other = plugin_user_source_id("https://other.example/i.json");
        assert_eq!(first, second);
        assert_ne!(first, other);
        assert_eq!(first.len(), 16);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }
}

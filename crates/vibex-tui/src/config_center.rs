//! The configuration centre: the desktop's module rail, rebuilt for a grid.
//!
//! The desktop config centre is an information architecture, not a screen: four
//! modules (Agent, MCP, Skills, Prompts), each with its own set of function
//! areas, each area a list of records with a detail pane and a set of actions
//! that depend on the record. The TUI keeps that architecture and re-draws it
//! for a character grid — a tab strip for the modules, a rail for the areas, a
//! table for the records and a detail pane under it — because a client that
//! invents its own structure is a second product to learn.
//!
//! Everything here is pure: the projections read [`ManagementData`] and answer
//! rows, the state machine answers where the cursor is, and neither performs
//! I/O. The reducer turns the row a cursor is on into an effect; the renderer
//! draws the rows this module projects. That split is what keeps the mouse and
//! the keyboard acting on the same object.

use std::collections::BTreeMap;

use vibex_core::{
    AgentManagedInstallStatus, AgentRuntimeStatus, McpServerTransportKind, PromptKind,
    ProviderSecretSetupState, ResourceDiscoveryStatus,
};

use crate::app::ManagementData;
use crate::keymap::Scope;
use crate::locale::Strings;

/// How many registry entries one page holds.
///
/// The authority cuts the MCP page with it and the client cuts the Skill one,
/// so both sides of the same table agree on what a page is.
pub const MARKET_PAGE_SIZE: usize = 20;

/// How many Skill entries one read fetches.
///
/// The Skill index pages by an opaque cursor and reports no total, so the client
/// takes one window and cuts its own pages out of it — three pages of
/// [`MARKET_PAGE_SIZE`], which is enough to browse with and few enough that a
/// reader who never turns a page pays for one read.
pub const SKILL_MARKET_FETCH_LIMIT: usize = MARKET_PAGE_SIZE * 3;

/// How many ticks apart a registry whose index is still widening is asked again.
///
/// The index is walked by the runtime on its own clock and nothing arrives to
/// say it grew, so a page that says "still indexing" has to keep asking or the
/// sentence becomes a claim the client stopped checking. Twelve ticks is about
/// two and a half seconds at the idle tick — slow enough that the poll is not
/// the client's own traffic, quick enough that a reader watching the list sees
/// it fill.
pub const REGISTRY_POLL_TICKS: u8 = 12;

/// The four modules the configuration centre is organised into.
///
/// The order is the desktop's: the Agent is what everything else attaches to,
/// and the three resource kinds follow in the order the session composer
/// reaches for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigModule {
    Agent,
    Mcp,
    Skills,
    Prompts,
}

impl ConfigModule {
    pub const ALL: [Self; 4] = [Self::Agent, Self::Mcp, Self::Skills, Self::Prompts];

    pub const fn key(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Mcp => "mcp",
            Self::Skills => "skills",
            Self::Prompts => "prompts",
        }
    }

    pub const fn label(self, strings: Strings) -> &'static str {
        match self {
            Self::Agent => strings.config_module_agent(),
            Self::Mcp => strings.config_module_mcp(),
            Self::Skills => strings.config_module_skills(),
            Self::Prompts => strings.config_module_prompts(),
        }
    }

    /// The function areas this module offers, in rail order.
    pub const fn sections(self) -> &'static [ConfigSection] {
        match self {
            Self::Agent => &[ConfigSection::Agents, ConfigSection::Providers],
            Self::Mcp => &[
                ConfigSection::Servers,
                ConfigSection::Catalog,
                ConfigSection::Sources,
            ],
            Self::Skills => &[
                ConfigSection::Installed,
                ConfigSection::Catalog,
                ConfigSection::Sources,
            ],
            Self::Prompts => &[ConfigSection::Prompts],
        }
    }

    pub const fn default_section(self) -> ConfigSection {
        match self {
            Self::Agent => ConfigSection::Agents,
            Self::Mcp => ConfigSection::Servers,
            Self::Skills => ConfigSection::Installed,
            Self::Prompts => ConfigSection::Prompts,
        }
    }
}

/// One function area inside a module: the second level of the navigation.
///
/// A section owns its own list, its own filter and its own cursor. `Catalog`
/// and `Sources` are shared by MCP and Skills because they mean the same thing
/// in both — the registry the resource can be installed from, and the Agent
/// configuration it can be imported from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigSection {
    // ---- Agent ----------------------------------------------------------
    Agents,
    Providers,
    // ---- MCP ------------------------------------------------------------
    Servers,
    // ---- Skills ---------------------------------------------------------
    Installed,
    // ---- MCP + Skills ---------------------------------------------------
    Catalog,
    Sources,
    // ---- Prompts --------------------------------------------------------
    Prompts,
}

impl ConfigSection {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Providers => "providers",
            Self::Servers => "servers",
            Self::Installed => "installed",
            Self::Catalog => "catalog",
            Self::Sources => "sources",
            Self::Prompts => "prompts",
        }
    }

    pub const fn label(self, strings: Strings) -> &'static str {
        match self {
            Self::Agents => strings.config_section_agents(),
            Self::Providers => strings.config_section_providers(),
            Self::Servers => strings.config_section_servers(),
            Self::Installed => strings.config_section_installed(),
            Self::Catalog => strings.config_section_catalog(),
            Self::Sources => strings.config_section_sources(),
            Self::Prompts => strings.config_section_prompts(),
        }
    }

    /// The hint under the rail entry: what the area is for.
    pub const fn hint(self, strings: Strings) -> &'static str {
        match self {
            Self::Agents => strings.config_hint_agents(),
            Self::Providers => strings.config_hint_providers(),
            Self::Servers => strings.config_hint_servers(),
            Self::Installed => strings.config_hint_installed(),
            Self::Catalog => strings.config_hint_catalog(),
            Self::Sources => strings.config_hint_sources(),
            Self::Prompts => strings.config_hint_prompts(),
        }
    }
}

/// Which of the centre's three panes owns the keyboard.
///
/// Three, because that is how many the surface is: the module tabs across the
/// top, the area rail down the side, and the records — table and detail
/// together — in the middle. `←`/`→` and `Tab` walk them in this order and
/// wrap, and the mouse sets the zone by clicking in it, so the pane that is lit
/// and the pane a key acts on cannot disagree.
///
/// The detail is part of the records pane rather than a fourth stop: it shows
/// the row the cursor is on, and a focus ring on a pane that has no cursor of
/// its own only makes the reader hunt for what they are controlling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigZone {
    Modules,
    Rail,
    Rows,
}

impl ConfigZone {
    pub const ALL: [Self; 3] = [Self::Modules, Self::Rail, Self::Rows];

    pub const fn index(self) -> usize {
        match self {
            Self::Modules => 0,
            Self::Rail => 1,
            Self::Rows => 2,
        }
    }

    /// What the pane is called, for the one line that names the focus.
    pub const fn label(self, strings: Strings) -> &'static str {
        match self {
            Self::Modules => strings.config_pane_modules(),
            Self::Rail => strings.config_pane_areas(),
            Self::Rows => strings.config_pane_records(),
        }
    }
}

/// The tone a status chip is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigTone {
    Ok,
    Warn,
    Bad,
    Muted,
}

/// One action a row offers, and the label it is drawn under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigAction {
    /// Install an Agent, or install a registry entry.
    Install,
    /// Re-install the version Vibex verified for an Agent.
    Rollback,
    /// Re-read what the Agent's own installer advertises.
    CheckUpdate,
    Uninstall,
    /// Enable or disable the record.
    Toggle,
    Edit,
    Delete,
    /// Re-read this Agent's snapshot from its own runtime.
    RefreshSnapshot,
    /// Open the Agent's authentication methods.
    Auth,
    Logout,
    /// Make a model provider profile the active one.
    Activate,
    /// Make one of a Provider's Keys the one the router tries first.
    ///
    /// Pinning and the member strategy are properties of a route group now, so
    /// there is no gateway-wide verb for either.
    Prefer,
    /// Edit the gateway's local token ceiling.
    TokenLimit,
    /// Read the recent requests the gateway served.
    Requests,
    /// Write a new credential for a profile.
    Secret,
    /// Forget the credential a profile holds.
    ClearSecret,
    Test,
    FetchModels,
    Validate,
    /// Make a second profile with the same settings under a new name.
    Duplicate,
    /// Open the page an Agent that is missing can be installed from.
    InstallDocs,
    /// Probe this machine for Agent CLIs that have not been added yet.
    Discover,
    /// Open the page a market card names for itself.
    OpenPage,
    /// Install a market entry that is already installed.
    Reinstall,
    /// Move a managed Agent onto the version the authority published.
    Upgrade,
    /// Take an Agent into the roster the reader works with.
    AddToRoster,
    /// Whether this Agent receives the embedded browser's tools.
    BrowserTools,
    /// Whether it receives the computer-use tools.
    ComputerTools,
    /// Take an Agent out of it, without uninstalling anything.
    RemoveFromRoster,
    /// Move a profile one place earlier in its Agent's order.
    MoveUp,
    /// Move a profile one place later in its Agent's order.
    MoveDown,
    /// Choose which Agents receive a resource.
    Matrix,
    /// Import a discovered resource.
    Import,
}

impl ConfigAction {
    pub const fn label(self, strings: Strings) -> &'static str {
        match self {
            Self::Install => strings.config_action_install(),
            Self::Rollback => strings.config_action_rollback(),
            Self::CheckUpdate => strings.config_action_check_update(),
            Self::Uninstall => strings.config_action_uninstall(),
            Self::Toggle => strings.config_action_toggle(),
            Self::Edit => strings.config_action_edit(),
            Self::Delete => strings.config_action_delete(),
            Self::RefreshSnapshot => strings.config_action_refresh(),
            Self::Auth => strings.config_action_sign_in(),
            Self::Logout => strings.config_action_sign_out(),
            Self::Activate => strings.config_action_activate(),
            Self::Prefer => strings.config_action_prefer(),
            Self::TokenLimit => strings.config_action_token_limit(),
            Self::Requests => strings.config_action_requests(),
            Self::Secret => strings.config_action_secret(),
            Self::ClearSecret => strings.config_action_clear_secret(),
            Self::Test => strings.config_action_test(),
            Self::FetchModels => strings.config_action_fetch_models(),
            Self::Validate => strings.config_action_validate(),
            Self::Matrix => strings.config_action_matrix(),
            Self::Import => strings.config_action_import(),
            Self::Duplicate => strings.config_action_duplicate(),
            Self::InstallDocs => strings.config_action_install_docs(),
            Self::Discover => strings.config_action_discover(),
            Self::OpenPage => strings.config_action_open_page(),
            Self::Reinstall => strings.config_action_reinstall(),
            Self::Upgrade => strings.config_action_upgrade(),
            Self::AddToRoster => strings.config_action_add_roster(),
            Self::BrowserTools => strings.config_action_browser_tools(),
            Self::ComputerTools => strings.config_action_computer_tools(),
            Self::RemoveFromRoster => strings.config_action_remove_roster(),
            Self::MoveUp => strings.config_action_move_up(),
            Self::MoveDown => strings.config_action_move_down(),
        }
    }

    /// Whether the action asks a destructive question first.
    pub const fn is_destructive(self) -> bool {
        matches!(self, Self::Delete | Self::Uninstall)
    }
}

/// One record as the table and the detail pane draw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRow {
    /// Stable identity of the record. The cursor is an index, but an action
    /// resolves through this so a list that refreshed under the reader cannot
    /// make a press act on a different record than the one that was lit.
    pub key: String,
    pub title: String,
    /// Short facts, one per table column after the title.
    pub cells: Vec<String>,
    pub chip: String,
    pub tone: ConfigTone,
    pub enabled: bool,
    /// Key/value lines for the detail pane.
    pub detail: Vec<(String, String)>,
    /// Free text shown above the key/value lines.
    pub summary: Option<String>,
    pub actions: Vec<ConfigAction>,
}

impl ConfigRow {
    fn new(key: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            cells: Vec::new(),
            chip: String::new(),
            tone: ConfigTone::Muted,
            enabled: true,
            detail: Vec::new(),
            summary: None,
            actions: Vec::new(),
        }
    }

    fn cell(mut self, value: impl Into<String>) -> Self {
        self.cells.push(value.into());
        self
    }

    fn chip(mut self, label: impl Into<String>, tone: ConfigTone) -> Self {
        self.chip = label.into();
        self.tone = tone;
        self
    }

    fn detail(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        let value = value.into();
        if !value.trim().is_empty() {
            self.detail.push((label.into(), value));
        }
        self
    }

    fn summary(mut self, value: impl Into<String>) -> Self {
        let value = value.into();
        if !value.trim().is_empty() {
            self.summary = Some(value);
        }
        self
    }

    fn actions(mut self, actions: &[ConfigAction]) -> Self {
        self.actions = actions.to_vec();
        self
    }

    fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Whether the row still matches a filter, over everything it shows.
    fn matches(&self, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        let needle = needle.to_lowercase();
        self.title.to_lowercase().contains(&needle)
            || self.chip.to_lowercase().contains(&needle)
            || self
                .cells
                .iter()
                .any(|cell| cell.to_lowercase().contains(&needle))
            || self.detail.iter().any(|(label, value)| {
                label.to_lowercase().contains(&needle) || value.to_lowercase().contains(&needle)
            })
            || self
                .summary
                .as_ref()
                .is_some_and(|summary| summary.to_lowercase().contains(&needle))
    }
}

/// The table's column headings for one section.
pub fn columns(
    module: ConfigModule,
    section: ConfigSection,
    strings: Strings,
) -> Vec<&'static str> {
    match section {
        ConfigSection::Agents => vec![
            strings.config_col_install(),
            strings.config_col_version(),
            strings.config_col_runtime(),
            strings.config_col_auth(),
            strings.config_col_models(),
        ],
        ConfigSection::Providers => vec![
            strings.config_col_upstreams(),
            strings.config_col_keys(),
            strings.config_col_state(),
            strings.config_col_models(),
        ],
        ConfigSection::Servers => vec![
            strings.config_col_transport(),
            strings.config_col_scope(),
            strings.config_col_agents(),
            strings.config_col_tags(),
        ],
        ConfigSection::Installed => vec![
            strings.config_col_source(),
            strings.config_col_scope(),
            strings.config_col_agents(),
            strings.config_col_updated(),
        ],
        ConfigSection::Catalog => match module {
            ConfigModule::Mcp => vec![
                strings.config_col_transport(),
                strings.config_col_version(),
                strings.config_col_publisher(),
                strings.config_col_package(),
            ],
            _ => vec![
                strings.config_col_kind(),
                strings.config_col_version(),
                strings.config_col_publisher(),
                strings.config_col_installs(),
            ],
        },
        ConfigSection::Sources => vec![
            strings.config_col_agent(),
            strings.config_col_path(),
            strings.config_col_state(),
        ],
        ConfigSection::Prompts => vec![
            strings.config_col_kind(),
            strings.config_col_scope(),
            strings.config_col_tags(),
            strings.config_col_updated(),
        ],
    }
}

/// Everything the centre remembers between frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigCenter {
    pub module: ConfigModule,
    pub section: ConfigSection,
    pub zone: ConfigZone,
    /// The inline filter. One per section would be four strings nobody asked
    /// for; the filter is cleared when the reader changes area, which is what
    /// keeps a hidden row from looking deleted.
    pub filter: String,
    pub filtering: bool,
    /// Rows the detail pane has scrolled past.
    pub detail_scroll: u16,
    /// One cursor per area, so leaving an area and coming back lands where the
    /// reader left rather than at the top.
    cursor: BTreeMap<&'static str, usize>,
    /// The record key each area's cursor was last resolved against, so a list
    /// that refreshed can keep the cursor on the same record.
    pinned: BTreeMap<&'static str, String>,
    /// What the last write said, drawn under the table.
    ///
    /// It is the same sentence the reader was toasted with, kept in place so a
    /// reader who looked away can still read what happened rather than having
    /// to repeat the action to find out.
    pub notice: Option<(String, ConfigTone)>,
    /// Records with a write in flight, so a press cannot be queued twice, the
    /// row can say it is working, and the answer can name what it was about.
    pub pending: BTreeMap<String, PendingWrite>,
    /// Ticks left before a still-widening registry is asked again. Zero when
    /// nothing is being polled, which is also where the clock starts.
    pub registry_poll_ticks: u8,
    /// Which page of which registry the reader is looking at.
    ///
    /// A registry is not a list: it answers the page it was asked for, so the
    /// question the centre put to it has to be remembered somewhere that
    /// outlives the answer. It is reset with the area, for the same reason the
    /// filter is: a page number carried into another registry would name a page
    /// that registry never had.
    pub registry: RegistryQuery,
}

/// One control on the registry bar, as a press on it means.
///
/// The bar draws the question the page was asked with, so each control is a way
/// of asking it again: the pointer and the keyboard reach the same handler
/// rather than two copies of "what does this chip do".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryControl {
    /// Cycle the transport family an MCP page is narrowed to.
    Transport,
    /// Cycle the ranking a Skill page is asked for.
    Sort,
    /// Show the page before this one.
    PagePrevious,
    /// Show the page after this one.
    PageNext,
}

/// Which page of a registry, narrowed how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryQuery {
    /// The transport family an MCP page is narrowed to. `None` is every family.
    pub transport: Option<McpServerTransportFilter>,
    /// The ranking a Skill page is asked for.
    pub sort: SkillMarketSortKind,
    /// Zero-based page, within whichever addressing the registry uses.
    pub page: usize,
}

impl Default for RegistryQuery {
    fn default() -> Self {
        Self {
            transport: None,
            // The registry's own ranking, which is what a browse view that has
            // expressed no preference is asking for.
            sort: SkillMarketSortKind::Recommended,
            page: 0,
        }
    }
}

impl RegistryQuery {
    /// Move to the next page, or back to the one before it, without running off
    /// either end. Answers whether the page moved.
    pub fn step_page(&mut self, delta: isize, pages: usize) -> bool {
        let last = pages.saturating_sub(1);
        let next = self.page.saturating_add_signed(delta).min(last);
        if next == self.page {
            return false;
        }
        self.page = next;
        true
    }

    /// Narrow the registry to the next transport family, wrapping back through
    /// "every family". A narrowing is a new question, so it starts at the first
    /// page of its own answer.
    pub fn cycle_transport(&mut self) {
        self.transport = match self.transport {
            None => Some(McpServerTransportFilter::Local),
            Some(filter) => filter.next(),
        };
        self.page = 0;
    }

    /// Ask for the next ranking, wrapping. Like the transport, a ranking is a
    /// new question.
    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        self.page = 0;
    }

    /// The transport filter as the request carries it: the wire type is the
    /// registry's, and `None` already means "every family" on both sides.
    pub fn transport_filter(self) -> Option<vibex_core::McpMarketTransportFilter> {
        self.transport.map(|transport| match transport {
            McpServerTransportFilter::Local => vibex_core::McpMarketTransportFilter::Local,
            McpServerTransportFilter::Remote => vibex_core::McpMarketTransportFilter::Remote,
        })
    }
}

/// Which transport family an MCP page is narrowed to, as the centre's own
/// vocabulary rather than the registry's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerTransportFilter {
    /// A server Vibex runs as a local process.
    Local,
    /// A server Vibex connects to over the network.
    Remote,
}

impl McpServerTransportFilter {
    /// The families in the order the control cycles them, with "every family"
    /// first.
    pub const ALL: [Option<Self>; 3] = [None, Some(Self::Local), Some(Self::Remote)];

    pub const fn label(self, strings: Strings) -> &'static str {
        match self {
            Self::Local => strings.config_registry_transport_local(),
            Self::Remote => strings.config_registry_transport_remote(),
        }
    }

    /// The next family in the cycle, wrapping through "every family".
    pub fn next(self) -> Option<Self> {
        let all = Self::ALL;
        let index = all
            .iter()
            .position(|entry| *entry == Some(self))
            .unwrap_or(0);
        all[(index + 1) % all.len()]
    }
}

/// The ranking a Skill page is asked for, as the centre's own vocabulary.
///
/// The discriminants are the cycle order [`Self::next`] walks, so they are part
/// of the type rather than an accident of declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillMarketSortKind {
    Recommended = 0,
    Downloads = 1,
    Stars = 2,
    Installs = 3,
    Updated = 4,
    Newest = 5,
    Trending = 6,
}

impl SkillMarketSortKind {
    pub const ALL: [Self; 7] = [
        Self::Recommended,
        Self::Downloads,
        Self::Stars,
        Self::Installs,
        Self::Updated,
        Self::Newest,
        Self::Trending,
    ];

    pub const fn label(self, strings: Strings) -> &'static str {
        match self {
            Self::Recommended => strings.config_registry_sort_recommended(),
            Self::Downloads => strings.config_registry_sort_downloads(),
            Self::Stars => strings.config_registry_sort_stars(),
            Self::Installs => strings.config_registry_sort_installs(),
            Self::Updated => strings.config_registry_sort_updated(),
            Self::Newest => strings.config_registry_sort_newest(),
            Self::Trending => strings.config_registry_sort_trending(),
        }
    }

    /// The next ranking in the cycle, wrapping.
    pub const fn next(self) -> Self {
        Self::ALL[(self as usize + 1) % Self::ALL.len()]
    }

    /// The registry's own name for this ranking. The vocabulary is closed on
    /// both sides, so this is a translation rather than a formatting.
    pub const fn wire(self) -> vibex_core::SkillMarketSort {
        match self {
            Self::Recommended => vibex_core::SkillMarketSort::Recommended,
            Self::Downloads => vibex_core::SkillMarketSort::Downloads,
            Self::Stars => vibex_core::SkillMarketSort::Stars,
            Self::Installs => vibex_core::SkillMarketSort::Installs,
            Self::Updated => vibex_core::SkillMarketSort::Updated,
            Self::Newest => vibex_core::SkillMarketSort::Newest,
            Self::Trending => vibex_core::SkillMarketSort::Trending,
        }
    }
}

/// The key a form's write is filed under.
///
/// A form creates or edits a record the table has not drawn yet, so there is no
/// row to wear the marker and nothing to name the answer after: while the form
/// is open, the form *is* the record whose write is in flight.
pub const FORM_RECORD: &str = "form";

/// One record's write, from the moment it is asked for until it answers.
///
/// The key alone is not enough to report the result: a toast that read
/// `agent:codex: refused` names the database row rather than the Agent, and a
/// reader cannot tell "Check update" from "Install" once the call is over. The
/// subject and the verb travel with the write so the answer is a sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingWrite {
    /// The record as the reader reads it — the row's own title.
    pub subject: String,
    pub verb: ConfigVerb,
}

impl PendingWrite {
    /// What the reader is told as the write lands.
    ///
    /// The verb says what was done; the outcome says what came back, when the
    /// runtime had something to say about it — a version, a verdict, how many
    /// methods the Agent advertises.
    pub fn message(&self, strings: Strings, outcome: &crate::app::MutationOutcome) -> String {
        use crate::app::MutationOutcome as Outcome;
        let done = self.verb.done(strings);
        let detail = match outcome {
            Outcome::Plain => None,
            Outcome::UpToDate => Some(strings.config_result_up_to_date().to_string()),
            Outcome::UpdateAvailable { version } => Some(format!(
                "{} {version} {}",
                strings.config_result_version(),
                strings.config_result_available()
            )),
            Outcome::Installed { version } => Some(match version {
                Some(version) => format!("{} {version}", strings.config_result_version()),
                None => strings.config_result_installed().to_string(),
            }),
            Outcome::Auth {
                methods,
                account,
                authenticated,
            } => Some(if *authenticated {
                account
                    .clone()
                    .unwrap_or_else(|| strings.config_result_signed_in().to_string())
            } else {
                // How many ways in there are is the useful half of "nobody is
                // signed in": it says the verb found something to offer.
                format!(
                    "{} · {methods} {}",
                    strings.config_result_signed_out(),
                    strings.config_result_methods()
                )
            }),
            Outcome::Models { count } => {
                Some(format!("{count} {}", strings.config_result_models()))
            }
            Outcome::Discovered { found } => Some(
                if *found == 0 {
                    strings.config_discover_none()
                } else {
                    strings.config_discover_found()
                }
                .replace("{count}", &found.to_string()),
            ),
            Outcome::AuthWaiting {
                location, detail, ..
            } => {
                // `RemoteAttachableTerminal` is what the runtime reports for a
                // terminal method: it is a PTY the authority opened, which a
                // client attaches to by id. It belongs on the same line as
                // `HostTerminal` — a terminal the reader finishes in — and a
                // branch that only matched the latter sent every real sign-in to
                // the catch-all "still signing in", which names nothing.
                let template = match location {
                    vibex_core::AgentAuthExecutionLocation::HostTerminal
                    | vibex_core::AgentAuthExecutionLocation::RemoteAttachableTerminal => {
                        strings.config_auth_waiting_host_terminal()
                    }
                    vibex_core::AgentAuthExecutionLocation::HostBrowser => {
                        strings.config_auth_waiting_host_browser()
                    }
                    vibex_core::AgentAuthExecutionLocation::ClientBrowserWithDeviceCode => {
                        strings.config_auth_waiting_device_code()
                    }
                    vibex_core::AgentAuthExecutionLocation::CompletedOnHost => {
                        strings.config_auth_waiting_elsewhere()
                    }
                };
                let rendered = match detail {
                    Some(command) if !command.trim().is_empty() => template.replace(
                        "{detail}",
                        &strings
                            .config_auth_waiting_detail()
                            .replace("{command}", command),
                    ),
                    _ => template.replace("{detail}", ""),
                };
                Some(rendered)
            }
            Outcome::DefaultScope { workspace } => Some(
                if *workspace {
                    strings.config_result_workspace_default()
                } else {
                    strings.config_result_global_default()
                }
                .to_string(),
            ),
            Outcome::Verdict {
                passed,
                warned,
                message,
            } => Some(format!(
                "{} · {message}",
                if !passed {
                    strings.config_result_failed()
                } else if *warned {
                    strings.config_result_warned()
                } else {
                    strings.config_result_ok()
                }
            )),
        };
        match detail {
            Some(detail) if !detail.is_empty() => {
                format!("{}: {done} · {detail}", self.subject)
            }
            _ => format!("{}: {done}", self.subject),
        }
    }

    /// What the reader is told when the write was refused.
    ///
    /// The refusal names the record and the verb for the same reason the
    /// success does: "install_agent: connection lost" says which row the key
    /// was on to a reader who has to guess, and nothing about what was being
    /// attempted.
    pub fn failure(&self, strings: Strings, reason: &str) -> String {
        format!(
            "{}: {} · {}: {reason}",
            self.subject,
            self.verb.noun(strings),
            strings.config_result_failed()
        )
    }
}

/// What a write is doing, as the two lines that report it read.
///
/// The verb is the record's alone; the subject is carried beside it. Together
/// they are the whole report — `Codex: Installing…` while it runs and
/// `Codex: Installed · version 1.3.0` when it lands — which is what a client
/// needs to never leave the reader guessing whether a key did anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigVerb {
    Install,
    Rollback,
    CheckUpdate,
    RefreshSnapshot,
    Enable,
    Disable,
    SignIn,
    SignOut,
    Uninstall,
    Delete,
    Save,
    Create,
    Activate,
    Test,
    FetchModels,
    Validate,
    Import,
    InstallFromRegistry,
    SetAgents,
    Discover,
    Search,
    /// Taking a record into the set the reader works with.
    Add,
    /// Taking it out again, without destroying anything.
    Remove,
}

impl ConfigVerb {
    /// What is happening, while it happens.
    pub const fn running(self, strings: Strings) -> &'static str {
        match self {
            Self::Install => strings.config_running_install(),
            Self::Rollback => strings.config_running_rollback(),
            Self::CheckUpdate => strings.config_running_check_update(),
            Self::RefreshSnapshot => strings.config_running_refresh(),
            Self::Enable | Self::Disable => strings.config_running_toggle(),
            Self::SignIn => strings.config_running_sign_in(),
            Self::SignOut => strings.config_running_sign_out(),
            Self::Uninstall => strings.config_running_uninstall(),
            Self::Delete => strings.config_running_delete(),
            Self::Save => strings.config_running_save(),
            Self::Create => strings.config_running_create(),
            Self::Activate => strings.config_running_activate(),
            Self::Test => strings.config_running_test(),
            Self::FetchModels => strings.config_running_fetch_models(),
            Self::Validate => strings.config_running_validate(),
            Self::Import => strings.config_running_import(),
            Self::InstallFromRegistry => strings.config_running_registry_install(),
            Self::SetAgents => strings.config_running_matrix(),
            Self::Discover => strings.config_running_discover(),
            Self::Search => strings.config_running_search(),
            Self::Add => strings.config_running_roster_add(),
            Self::Remove => strings.config_running_roster_remove(),
        }
    }

    /// What was done, once it is done.
    pub const fn done(self, strings: Strings) -> &'static str {
        match self {
            Self::Install => strings.config_done_install(),
            Self::Rollback => strings.config_done_rollback(),
            Self::CheckUpdate => strings.config_done_check_update(),
            Self::RefreshSnapshot => strings.config_done_refresh(),
            Self::Enable => strings.config_done_enable(),
            Self::Disable => strings.config_done_disable(),
            Self::SignIn => strings.config_done_sign_in(),
            Self::SignOut => strings.config_done_sign_out(),
            Self::Uninstall => strings.config_done_uninstall(),
            Self::Delete => strings.config_done_delete(),
            Self::Save => strings.config_done_save(),
            Self::Create => strings.config_done_create(),
            Self::Activate => strings.config_done_activate(),
            Self::Test => strings.config_done_test(),
            Self::FetchModels => strings.config_done_fetch_models(),
            Self::Validate => strings.config_done_validate(),
            Self::Import => strings.config_done_import(),
            Self::InstallFromRegistry => strings.config_done_registry_install(),
            Self::SetAgents => strings.config_done_matrix(),
            Self::Discover => strings.config_done_discover(),
            Self::Search => strings.config_done_search(),
            Self::Add => strings.config_done_roster_add(),
            Self::Remove => strings.config_done_roster_remove(),
        }
    }

    /// What the verb is called when it did not happen.
    ///
    /// A refusal cannot borrow [`Self::done`]: "Installed · failed" contradicts
    /// itself, and the past tense that reads as a record of what happened on
    /// success reads as a mistake on failure. The noun is the phrase the reader
    /// used when they pressed the key, so the sentence that names the failure
    /// names the thing they asked for.
    pub const fn noun(self, strings: Strings) -> &'static str {
        match self {
            Self::Install => strings.config_noun_install(),
            Self::Rollback => strings.config_noun_rollback(),
            Self::CheckUpdate => strings.config_noun_check_update(),
            Self::RefreshSnapshot => strings.config_noun_refresh(),
            Self::Enable | Self::Disable => strings.config_noun_toggle(),
            Self::SignIn => strings.config_noun_sign_in(),
            Self::SignOut => strings.config_noun_sign_out(),
            Self::Uninstall => strings.config_noun_uninstall(),
            Self::Delete => strings.config_noun_delete(),
            Self::Save => strings.config_noun_save(),
            Self::Create => strings.config_noun_create(),
            Self::Activate => strings.config_noun_activate(),
            Self::Test => strings.config_noun_test(),
            Self::FetchModels => strings.config_noun_fetch_models(),
            Self::Validate => strings.config_noun_validate(),
            Self::Import => strings.config_noun_import(),
            Self::InstallFromRegistry => strings.config_noun_registry_install(),
            Self::SetAgents => strings.config_noun_matrix(),
            Self::Discover => strings.config_noun_discover(),
            Self::Search => strings.config_noun_search(),
            Self::Add => strings.config_noun_roster_add(),
            Self::Remove => strings.config_noun_roster_remove(),
        }
    }
}

impl Default for ConfigCenter {
    fn default() -> Self {
        Self {
            module: ConfigModule::Agent,
            section: ConfigModule::Agent.default_section(),
            zone: ConfigZone::Rows,
            filter: String::new(),
            filtering: false,
            detail_scroll: 0,
            cursor: BTreeMap::new(),
            pinned: BTreeMap::new(),
            notice: None,
            pending: BTreeMap::new(),
            registry: RegistryQuery::default(),
            registry_poll_ticks: 0,
        }
    }
}

impl ConfigCenter {
    /// The scope every key of the centre is bound in.
    pub const SCOPE: Scope = Scope::Management;

    /// Show a module, opening its first area unless the caller names one that
    /// the module actually has.
    pub fn show_module(&mut self, module: ConfigModule, section: Option<ConfigSection>) {
        let section = match section {
            Some(section) if module.sections().contains(&section) => section,
            _ => module.default_section(),
        };
        if self.module != module || self.section != section {
            self.notice = None;
            self.detail_scroll = 0;
            // A page number is a fact about one registry's answer, so it does
            // not travel: the next module opens on its own first page.
            self.registry = RegistryQuery::default();
        }
        self.module = module;
        self.section = section;
    }

    pub fn show_section(&mut self, section: ConfigSection) {
        if !self.module.sections().contains(&section) {
            return;
        }
        if self.section != section {
            self.filter.clear();
            self.filtering = false;
            self.detail_scroll = 0;
            self.notice = None;
            self.registry = RegistryQuery::default();
        }
        self.section = section;
    }

    /// Move the module cursor by `delta`, wrapping. The area the module opens
    /// on is the one the reader last used in it, so walking the tabs does not
    /// reset the rail.
    pub fn step_module(&mut self, delta: isize) {
        let all = ConfigModule::ALL;
        let index = all.iter().position(|m| *m == self.module).unwrap_or(0) as isize;
        let next = (index + delta).rem_euclid(all.len() as isize) as usize;
        let module = all[next];
        let remembered = self
            .pinned
            .get(module.default_section().key())
            .map(|_| module.default_section());
        let section = self.section_for_module(module).or(remembered);
        self.show_module(module, section);
    }

    /// The area of `module` the reader was last in, when they have been there.
    fn section_for_module(&self, module: ConfigModule) -> Option<ConfigSection> {
        module
            .sections()
            .iter()
            .copied()
            .find(|section| self.cursor.contains_key(section.key()))
    }

    pub fn step_section(&mut self, delta: isize) {
        let sections = self.module.sections();
        let index = sections
            .iter()
            .position(|s| *s == self.section)
            .unwrap_or(0) as isize;
        let next = (index + delta).rem_euclid(sections.len() as isize) as usize;
        if let Some(section) = sections.get(next).copied() {
            self.show_section(section);
        }
    }

    pub fn step_zone(&mut self, delta: isize) {
        let all = ConfigZone::ALL;
        let index = all.iter().position(|zone| *zone == self.zone).unwrap_or(0) as isize;
        let next = (index + delta).rem_euclid(all.len() as isize) as usize;
        self.zone = all[next];
    }

    /// Rows for the current area, before the filter is applied.
    pub fn rows(&self, data: &ManagementData, strings: Strings) -> Vec<ConfigRow> {
        match self.section {
            ConfigSection::Agents => agent_rows(data, strings),
            ConfigSection::Providers => provider_rows(data, strings),
            ConfigSection::Servers => server_rows(data, strings),
            ConfigSection::Installed => skill_rows(data, strings),
            ConfigSection::Catalog => match self.module {
                ConfigModule::Mcp => market_rows(data, strings),
                _ => skill_market_rows(data, strings),
            },
            ConfigSection::Sources => match self.module {
                ConfigModule::Mcp => mcp_source_rows(data, strings),
                _ => skill_source_rows(data, strings),
            },
            ConfigSection::Prompts => prompt_rows(data, strings),
        }
    }

    /// The rows the table draws: the area's records, filtered, and — for a
    /// registry that hands its whole window over at once — the page of them the
    /// reader asked for.
    pub fn visible(&self, data: &ManagementData, strings: Strings) -> Vec<ConfigRow> {
        let needle = self.filter.trim();
        let rows = self
            .rows(data, strings)
            .into_iter()
            .filter(|row| row.matches(needle))
            .collect::<Vec<_>>();
        self.page_slice(rows)
    }

    /// Count one tick towards asking the registry again.
    ///
    /// Answers whether the question is due. The clock only runs while the index
    /// is still widening: a finished index, a page that is not a registry, and a
    /// reader who has left the area all stop it, and the next page they open
    /// starts the clock over rather than inheriting a count from the last one.
    pub fn poll_registry(&mut self, still_widening: bool) -> bool {
        if !still_widening {
            self.registry_poll_ticks = 0;
            return false;
        }
        if self.registry_poll_ticks == 0 {
            // The page has just been read: the clock starts now.
            self.registry_poll_ticks = REGISTRY_POLL_TICKS;
            return false;
        }
        if self.registry_poll_ticks > 1 {
            self.registry_poll_ticks -= 1;
            return false;
        }
        self.registry_poll_ticks = REGISTRY_POLL_TICKS;
        true
    }

    /// How many pages the registry on screen has, at least one.
    ///
    /// The two registries answer differently and the count has to follow: MCP
    /// is asked for one page at a time and reports how many entries matched in
    /// all, while the Skill index hands over one window and pages by cursor, so
    /// its pages are cut here out of what was fetched.
    pub fn registry_pages(&self, data: &ManagementData) -> usize {
        if self.section != ConfigSection::Catalog {
            return 1;
        }
        let total = match self.module {
            ConfigModule::Mcp => data
                .market_note
                .as_ref()
                .and_then(|note| note.matched)
                .unwrap_or(0),
            _ => data.skill_market.len(),
        };
        total.div_ceil(MARKET_PAGE_SIZE).max(1)
    }

    /// The page of `rows` the reader is on, for a registry the client pages
    /// itself. A registry the authority pages arrives already cut.
    fn page_slice(&self, rows: Vec<ConfigRow>) -> Vec<ConfigRow> {
        if self.section != ConfigSection::Catalog || self.module == ConfigModule::Mcp {
            return rows;
        }
        let start = self.registry.page * MARKET_PAGE_SIZE;
        rows.into_iter()
            .skip(start)
            .take(MARKET_PAGE_SIZE)
            .collect()
    }

    /// The cursor index inside the visible rows, clamped.
    pub fn cursor(&self, visible: usize) -> usize {
        self.cursor
            .get(self.section.key())
            .copied()
            .unwrap_or(0)
            .min(visible.saturating_sub(1))
    }

    pub fn set_cursor(&mut self, index: usize) {
        self.cursor.insert(self.section.key(), index);
        self.pinned.remove(self.section.key());
        self.detail_scroll = 0;
    }

    /// Keep the cursor on the same record across a refresh, falling back to the
    /// index when the record is gone.
    pub fn reconcile(&mut self, rows: &[ConfigRow]) {
        let key = self.section.key();
        let Some(pinned) = self.pinned.get(key) else {
            return;
        };
        if let Some(index) = rows.iter().position(|row| row.key == *pinned) {
            self.cursor.insert(key, index);
        }
    }

    /// Pin the cursor to the record it is on, so a read that lands later cannot
    /// move the reader's place.
    pub fn pin(&mut self, row: Option<&ConfigRow>) {
        match row {
            Some(row) => {
                self.pinned.insert(self.section.key(), row.key.clone());
            }
            None => {
                self.pinned.remove(self.section.key());
            }
        }
    }

    pub fn step_cursor(&mut self, rows: &[ConfigRow], delta: isize) {
        if rows.is_empty() {
            return;
        }
        let current = self.cursor(rows.len()) as isize;
        let next = (current + delta).rem_euclid(rows.len() as isize) as usize;
        self.set_cursor(next);
        self.pin(rows.get(next));
    }

    /// The row the cursor is on, if the area has one.
    pub fn selected(&self, data: &ManagementData, strings: Strings) -> Option<ConfigRow> {
        let rows = self.visible(data, strings);
        let index = self.cursor(rows.len());
        rows.into_iter().nth(index)
    }

    pub fn is_pending(&self, key: &str) -> bool {
        self.pending.contains_key(key)
    }

    /// Whether any write the reader asked for is still in flight.
    ///
    /// A record being saved and a form being submitted are one fact to the
    /// clock: each draws a spinner where it was started, and that spinner's
    /// frame comes from the animation phase, so the clock has to keep stepping
    /// while one stands there. A frame held still for the length of a round trip
    /// reads as a press the client dropped, which is the one thing a write
    /// marker exists to deny.
    pub fn writes_in_flight(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Note a write in flight. Answers false when the record already has one,
    /// which is what keeps a held key from queueing the same call twice.
    pub fn begin_pending(
        &mut self,
        key: impl Into<String>,
        subject: impl Into<String>,
        verb: ConfigVerb,
    ) -> bool {
        let key = key.into();
        if self.pending.contains_key(&key) {
            return false;
        }
        self.pending.insert(
            key,
            PendingWrite {
                subject: subject.into(),
                verb,
            },
        );
        true
    }

    /// Retire a write and hand back what it was about, so the answer can name
    /// the record and the verb rather than the key they were filed under.
    pub fn finish_pending(&mut self, key: &str) -> Option<PendingWrite> {
        self.pending.remove(key)
    }

    pub fn set_notice(&mut self, message: impl Into<String>, tone: ConfigTone) {
        self.notice = Some((message.into(), tone));
    }

    /// How many rows each area would draw, for the rail's counts.
    pub fn counts(&self, data: &ManagementData, strings: Strings) -> Vec<usize> {
        self.module
            .sections()
            .iter()
            .map(|section| {
                let mut probe = self.clone();
                probe.section = *section;
                probe.rows(data, strings).len()
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// projections
// ---------------------------------------------------------------------------

fn tone_for_enabled(enabled: bool) -> ConfigTone {
    if enabled {
        ConfigTone::Ok
    } else {
        ConfigTone::Muted
    }
}

fn runtime_tone(status: AgentRuntimeStatus) -> ConfigTone {
    match status {
        AgentRuntimeStatus::Ready => ConfigTone::Ok,
        AgentRuntimeStatus::ProbeFailed | AgentRuntimeStatus::Unavailable => ConfigTone::Bad,
        AgentRuntimeStatus::Disabled | AgentRuntimeStatus::Unknown => ConfigTone::Muted,
    }
}

/// A probe status as a word. The core enum carries no `Display` of its own,
/// and a chip that read `ProbeFailed` would be the wire spelling, not English.
fn discovery_tone(status: ResourceDiscoveryStatus) -> ConfigTone {
    match status {
        ResourceDiscoveryStatus::Importable => ConfigTone::Ok,
        ResourceDiscoveryStatus::AlreadyImported => ConfigTone::Muted,
        ResourceDiscoveryStatus::Unsupported | ResourceDiscoveryStatus::Error => ConfigTone::Warn,
    }
}

fn scope_label(scope: vibex_core::McpServerScopeKind, strings: Strings) -> &'static str {
    match scope {
        vibex_core::McpServerScopeKind::Global => strings.config_scope_global(),
        vibex_core::McpServerScopeKind::User => strings.config_scope_user(),
        vibex_core::McpServerScopeKind::Project => strings.config_scope_project(),
        vibex_core::McpServerScopeKind::Workspace => strings.config_scope_workspace(),
    }
}

fn skill_scope_label(scope: vibex_core::SkillScopeKind, strings: Strings) -> &'static str {
    match scope {
        vibex_core::SkillScopeKind::Global => strings.config_scope_global(),
        vibex_core::SkillScopeKind::User => strings.config_scope_user(),
        vibex_core::SkillScopeKind::Project => strings.config_scope_project(),
        vibex_core::SkillScopeKind::Workspace => strings.config_scope_workspace(),
    }
}

fn prompt_scope_label(scope: vibex_core::PromptScopeKind, strings: Strings) -> &'static str {
    match scope {
        vibex_core::PromptScopeKind::Global => strings.config_scope_global(),
        vibex_core::PromptScopeKind::Project => strings.config_scope_project(),
        vibex_core::PromptScopeKind::Workspace => strings.config_scope_workspace(),
        vibex_core::PromptScopeKind::User => strings.config_scope_user(),
    }
}

fn transport_label(kind: McpServerTransportKind) -> &'static str {
    match kind {
        McpServerTransportKind::Stdio => "stdio",
        McpServerTransportKind::Http => "http",
        McpServerTransportKind::Sse => "sse",
    }
}

/// An epoch-millisecond stamp as the reader's own civil date and time.
fn stamp(at_ms: i64) -> String {
    crate::text::format_utc_timestamp(at_ms)
}

fn secret_label(state: ProviderSecretSetupState, strings: Strings) -> &'static str {
    match state {
        ProviderSecretSetupState::Available => strings.config_secret_set(),
        ProviderSecretSetupState::Missing => strings.config_secret_missing(),
        ProviderSecretSetupState::Referenced => strings.config_secret_referenced(),
        ProviderSecretSetupState::Unsupported => strings.config_secret_unsupported(),
    }
}

fn prompt_kind_label(kind: PromptKind, strings: Strings) -> &'static str {
    match kind {
        PromptKind::ReusablePrompt => strings.config_prompt_kind_reusable(),
        PromptKind::SlashCommand => strings.config_prompt_kind_slash(),
        PromptKind::SystemSnippet => strings.config_prompt_kind_snippet(),
    }
}

fn agents_label(matrix: &[vibex_core::McpServerAgentMatrix], strings: Strings) -> String {
    let enabled = matrix.iter().filter(|entry| entry.enabled).count();
    if matrix.is_empty() {
        strings.config_agents_none().to_string()
    } else {
        format!("{enabled}/{}", matrix.len())
    }
}

fn skill_agents_label(matrix: &[vibex_core::SkillAgentMatrix], strings: Strings) -> String {
    let enabled = matrix.iter().filter(|entry| entry.enabled).count();
    if matrix.is_empty() {
        strings.config_agents_none().to_string()
    } else {
        format!("{enabled}/{}", matrix.len())
    }
}

/// Whether the Agent's own installer owns the binary, and at what version.
fn managed_line(agent: &vibex_core::AgentSnapshotEntry, strings: Strings) -> String {
    if !agent.managed_install.managed {
        return strings.config_managed_external().to_string();
    }
    let Some(version) = agent.managed_install.installed_version.clone() else {
        return match agent.managed_install.status {
            AgentManagedInstallStatus::NotInstalled => strings.config_managed_absent().to_string(),
            _ => strings.config_managed_unknown().to_string(),
        };
    };
    match agent.managed_install.available_version.clone() {
        Some(available) if available != version => format!("{version} → {available}"),
        _ => version,
    }
}

pub fn agent_rows(data: &ManagementData, strings: Strings) -> Vec<ConfigRow> {
    // The three groups are contiguous: enabled first, then added-but-off, then
    // the ones the roster does not hold. A stable sort keeps the authority's
    // own order inside each group.
    let mut ordered = data.agents.iter().collect::<Vec<_>>();
    // The group first, then the order the authority holds — which every client
    // reads, so the arrangement is shared rather than local to this one — and
    // the id last so two records that claim the same place still have one.
    ordered.sort_by(|left, right| {
        agent_group(left)
            .cmp(&agent_group(right))
            .then(left.order_index.cmp(&right.order_index))
            .then(left.id.as_str().cmp(right.id.as_str()))
    });
    ordered
        .into_iter()
        // The desktop hides the integrations Vibex is not ready to expose, and
        // the two clients read the same roster: an Agent the desktop will not
        // show is one the reader cannot meet in the TUI either. A Custom Agent
        // is the reader's own and stays visible whatever its id, which is the
        // same rule the desktop applies.
        .filter(|agent| {
            agent.source_kind == vibex_core::AgentSourceKind::Custom
                || vibex_core::is_user_visible_agent(&agent.id)
        })
        .map(|agent| {
            let install = match agent.install_status {
                vibex_core::AgentInstallStatus::Installed => strings.config_state_installed(),
                vibex_core::AgentInstallStatus::Missing => strings.config_state_missing(),
                vibex_core::AgentInstallStatus::Disabled => strings.config_state_disabled(),
                vibex_core::AgentInstallStatus::Unknown => strings.config_state_unknown(),
            };
            let runtime = match agent.runtime_status {
                AgentRuntimeStatus::Ready => strings.config_state_ready(),
                AgentRuntimeStatus::Unavailable => strings.config_state_unavailable(),
                AgentRuntimeStatus::ProbeFailed => strings.config_state_probe_failed(),
                AgentRuntimeStatus::Disabled => strings.config_state_disabled(),
                AgentRuntimeStatus::Unknown => strings.config_state_unknown(),
            };
            // The roster is three groups, and which one an Agent is in is the
            // first thing the reader needs: an Agent that is not added is one
            // they have not agreed to run, and an Agent added but switched off
            // is one they have. The group is a column and the rows are sorted
            // by it, so the three are contiguous rather than interleaved.
            let roster = match (agent.added, agent.enabled) {
                (true, true) => strings.config_roster_enabled(),
                (true, false) => strings.config_roster_added_disabled(),
                (false, _) => strings.config_roster_not_added(),
            };
            // The authentication the runtime advertises for this Agent, when
            // the reader has asked for it: the row says "not signed in" rather
            // than leaving the sign-in verb looking like it did nothing. It is
            // read here rather than beside the cells because the verbs below
            // depend on it.
            let auth = auth_state(data, &agent.id);
            let mut actions = vec![ConfigAction::Toggle, ConfigAction::RefreshSnapshot];
            // Signing in is the way in, so the verb stays on the row whether or
            // not an account exists yet: the panel behind it is what reads the
            // methods and says there are none, which is a better answer than a
            // key that does nothing. Signing out is the other half — it ends a
            // context, so without one there is nothing for it to end, and a
            // verb drawn over an Agent nobody signed into is a verb pressed to
            // be told so.
            actions.push(ConfigAction::Auth);
            if auth.has_context() {
                actions.push(ConfigAction::Logout);
            }
            // Joining and leaving are the roster's own two verbs: taking an
            // Agent out is not an uninstall, and adding one is not an install —
            // the reader is saying what they want to see, not what is on disk.
            actions.insert(
                0,
                if agent.added {
                    ConfigAction::RemoveFromRoster
                } else {
                    ConfigAction::AddToRoster
                },
            );
            // The two places are offered on every row: a row at the end of its
            // group has nowhere to go, and the reducer is where that is known.
            actions.push(ConfigAction::MoveUp);
            actions.push(ConfigAction::MoveDown);
            // The two panels reach an Agent the user never configured, so the
            // switch is per Agent and per panel: an Agent that cannot receive
            // MCP tools at all gets the computer-use CLI instead, and one that
            // can gets the tools.
            actions.push(ConfigAction::BrowserTools);
            actions.push(ConfigAction::ComputerTools);
            if agent.managed_install.managed {
                // An Agent the authority already has at a newer version is
                // upgraded rather than installed again: the two run the same
                // call, and the verb the reader presses should be the one that
                // describes what it does.
                actions.insert(
                    0,
                    if agent.managed_install.status
                        == vibex_core::AgentManagedInstallStatus::UpdateAvailable
                    {
                        ConfigAction::Upgrade
                    } else {
                        ConfigAction::Install
                    },
                );
                actions.insert(1, ConfigAction::CheckUpdate);
                if agent.installed {
                    actions.insert(2, ConfigAction::Rollback);
                    actions.push(ConfigAction::Uninstall);
                }
            } else {
                actions.insert(0, ConfigAction::Install);
            }
            // An Agent that is not installed and whose Install cannot run is a
            // row with nothing to press: the vendor's page is the one thing
            // this client can still do about it.
            if !agent.installed && vibex_core::agent_install_url(agent).is_some() {
                actions.push(ConfigAction::InstallDocs);
            }
            // The authentication the runtime advertises for this Agent, when
            // the reader has asked for it: the row says "not signed in" rather
            // than leaving the sign-in verb looking like it did nothing.
            let mut row = ConfigRow::new(agent.id.as_str().to_string(), agent.label.clone())
                .cell(install)
                .cell(version_label(agent, strings))
                .cell(runtime)
                .cell(auth.label(strings))
                .cell(agent.models.len().to_string())
                .chip(
                    if agent.managed_install.status == AgentManagedInstallStatus::Failed {
                        strings.config_managed_failed()
                    } else if !agent.added {
                        // Not in the roster is a different thing from in it and
                        // switched off, and the chip is where the difference is
                        // read at a glance: the three groups are contiguous and
                        // the chip names which one the row is in.
                        strings.config_roster_not_added()
                    } else if agent.enabled {
                        strings.config_state_enabled()
                    } else {
                        strings.config_state_disabled()
                    },
                    if agent.managed_install.status == AgentManagedInstallStatus::Failed {
                        ConfigTone::Bad
                    } else if !agent.added {
                        ConfigTone::Warn
                    } else if agent.enabled {
                        runtime_tone(agent.runtime_status)
                    } else {
                        ConfigTone::Muted
                    },
                )
                .enabled(agent.enabled)
                .summary(agent.description.clone().unwrap_or_default())
                .detail(strings.config_field_id(), agent.id.as_str())
                .detail(
                    strings.config_field_install(),
                    format!(
                        "{install} · {}",
                        match agent.managed_install.status {
                            AgentManagedInstallStatus::External =>
                                strings.config_managed_external(),
                            AgentManagedInstallStatus::NotInstalled =>
                                strings.config_managed_absent(),
                            AgentManagedInstallStatus::Installed =>
                                strings.config_managed_installed(),
                            AgentManagedInstallStatus::UpdateAvailable =>
                                strings.config_managed_update(),
                            AgentManagedInstallStatus::Installing
                            | AgentManagedInstallStatus::Upgrading
                            | AgentManagedInstallStatus::Uninstalling => strings.config_working(),
                            AgentManagedInstallStatus::Failed => strings.config_managed_failed(),
                        }
                    ),
                )
                .detail(strings.config_field_version(), managed_line(agent, strings))
                // A failure the reader cannot see is a failure they will meet
                // again: the authority's own words are kept on the record, and
                // the row is where they are read.
                // The models the Agent advertises come before the flags: a
                // detail pane scrolls, and what a reader opens this one for is
                // what the Agent can run rather than whether it is configured.
                .detail(
                    strings.config_field_models(),
                    if agent.models.is_empty() {
                        strings.config_agents_none().to_string()
                    } else {
                        agent.models.join(", ")
                    },
                )
                .detail(strings.config_col_roster(), roster)
                .detail(
                    strings.config_field_tools(),
                    format!(
                        "{} {} · {} {}",
                        strings.config_action_browser_tools(),
                        if data
                            .agent_tools
                            .enabled(agent.id.as_str(), vibex_core::AgentBuiltinTool::Browser)
                        {
                            strings.config_state_on()
                        } else {
                            strings.config_state_off()
                        },
                        strings.config_action_computer_tools(),
                        if data
                            .agent_tools
                            .enabled(agent.id.as_str(), vibex_core::AgentBuiltinTool::Computer)
                        {
                            strings.config_state_on()
                        } else {
                            strings.config_state_off()
                        }
                    ),
                )
                .detail(
                    strings.config_field_last_error(),
                    agent
                        .managed_install
                        .last_error_message
                        .clone()
                        .or_else(|| agent.managed_install.last_error_code.clone())
                        .unwrap_or_default(),
                )
                .detail(
                    strings.config_field_auth(),
                    match &auth {
                        AuthState::Unknown => strings.config_state_unknown().to_string(),
                        AuthState::Required { methods } => {
                            format!("{} ({methods})", strings.config_result_signed_out())
                        }
                        AuthState::SignedIn { account } => account
                            .clone()
                            .unwrap_or_else(|| strings.config_result_signed_in().to_string()),
                    },
                )
                .detail(
                    strings.config_field_source(),
                    format!("{:?}", agent.source_kind),
                )
                .detail(
                    strings.config_field_configured(),
                    if agent.configured {
                        strings.config_yes()
                    } else {
                        strings.config_no()
                    },
                )
                .detail(strings.config_field_command(), {
                    let mut command = agent
                        .command
                        .as_ref()
                        .map(|command| command.command.clone())
                        .unwrap_or_default();
                    if let Some(config) = agent.command.as_ref()
                        && !config.args.is_empty()
                    {
                        command.push(' ');
                        command.push_str(&config.args.join(" "));
                    }
                    command
                })
                .detail(
                    strings.config_field_path(),
                    agent.native_config_paths.join("\n"),
                )
                .actions(&actions);
            row.enabled = agent.enabled;
            row
        })
        .collect()
}

/// What the runtime last said about one Agent's account.
///
/// The TUI does not run the interactive sign-in itself, so this is what the
/// runtime reports after a read: no context is "unknown" rather than "signed
/// out", because a client that has not asked must not claim an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AuthState {
    Unknown,
    Required { methods: usize },
    SignedIn { account: Option<String> },
}

impl AuthState {
    fn label(&self, strings: Strings) -> String {
        match self {
            Self::Unknown => strings.config_state_unknown().to_string(),
            Self::Required { .. } => strings.config_result_signed_out().to_string(),
            Self::SignedIn { account } => account
                .clone()
                .unwrap_or_else(|| strings.config_result_signed_in().to_string()),
        }
    }

    /// Whether the authority holds an account for this Agent at all.
    ///
    /// A context is created the first time an Agent is asked for its methods,
    /// so `Unknown` after that read means "no account" rather than "not asked":
    /// it is the answer for an Agent this client has not opened the panel on,
    /// which is also an Agent there is nothing to sign out of.
    const fn has_context(&self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

fn auth_state(data: &ManagementData, agent_id: &vibex_core::AgentId) -> AuthState {
    let contexts = data
        .agent_auth
        .iter()
        .filter(|context| context.agent_id == *agent_id)
        .collect::<Vec<_>>();
    if contexts.is_empty() {
        return AuthState::Unknown;
    }
    let authenticated = contexts
        .iter()
        .find(|context| context.status == vibex_core::AgentAuthContextStatus::Authenticated);
    match authenticated {
        Some(context) => AuthState::SignedIn {
            account: context.account_hint.clone(),
        },
        None => AuthState::Required {
            methods: contexts.len(),
        },
    }
}

/// The version cell: what is installed, and what is waiting.
fn version_label(agent: &vibex_core::AgentSnapshotEntry, strings: Strings) -> String {
    let installed = agent.managed_install.installed_version.clone();
    match (installed, agent.managed_install.available_version.clone()) {
        (Some(installed), Some(available)) if available != installed => {
            format!("{installed} → {available}")
        }
        (Some(installed), _) => installed,
        (None, Some(available)) => format!("→ {available}"),
        (None, None) => strings.config_agents_none().to_string(),
    }
}

/// The gateway's row, above the direct Providers.
///
/// It is a row rather than a page because it answers the same question the
/// other rows do — which Provider serves a model — and because a reader who
/// cannot see it cannot tell why a model they added is reachable from three
/// Agents at once.
fn gateway_row(data: &ManagementData, strings: Strings) -> Option<ConfigRow> {
    let snapshot = data.gateway.as_ref()?;
    let state = match snapshot.runtime.status {
        vibex_core::ModelGatewayStatus::Running => strings.config_state_ready(),
        // A degraded gateway is still serving every group that did not step
        // aside, so it is not "off" and not "unavailable".
        vibex_core::ModelGatewayStatus::Degraded => {
            if snapshot.runtime.degraded_routes > 0 {
                strings.config_state_degraded()
            } else {
                strings.config_state_ready()
            }
        }
        vibex_core::ModelGatewayStatus::Disabled => strings.config_state_off(),
        _ => strings.config_state_unavailable(),
    };
    let cooling = snapshot.cooling_key_count();
    let invalid = snapshot.invalid_key_count();
    let total_keys = snapshot
        .upstreams
        .iter()
        .map(|upstream| upstream.keys.len())
        .sum::<usize>();
    let key_summary = match (cooling, invalid) {
        (0, 0) => format!("{total_keys} keys"),
        (cooling, 0) => format!("{total_keys} keys · {cooling} cooling"),
        (0, invalid) => format!("{total_keys} keys · {invalid} invalid"),
        (cooling, invalid) => {
            format!("{total_keys} keys · {cooling} cooling · {invalid} invalid")
        }
    };
    // The gateway has no Agent list of its own: an Agent reaches it through a
    // route group, so the count that matters here is how many groups it serves.
    let route_count = snapshot
        .models
        .iter()
        .filter_map(|model| model.route_id.as_deref())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let mut row = ConfigRow::new("gateway", snapshot.settings.display_name.clone())
        .cell(route_count.to_string())
        .cell(key_summary)
        .cell(state)
        .cell(snapshot.models.len().to_string())
        .chip(
            state,
            if snapshot.runtime.status.is_routable() {
                ConfigTone::Ok
            } else {
                ConfigTone::Warn
            },
        )
        .detail(strings.config_field_kind(), "gateway")
        .detail(
            "address",
            snapshot
                .runtime
                .base_url()
                .unwrap_or_else(|| "not running".to_string()),
        )
        .detail("routes", route_count.to_string())
        .detail(
            "models",
            snapshot
                .models
                .iter()
                .map(|model| model.display_id.clone())
                .collect::<Vec<_>>()
                .join(", "),
        )
        // Routing strategy and pinning belong to a route group now, not to the
        // gateway: one listener serves many groups and each decides its own.
        .actions(&[
            ConfigAction::Toggle,
            ConfigAction::TokenLimit,
            ConfigAction::Requests,
            ConfigAction::RefreshSnapshot,
        ]);
    if let Some(notice) = snapshot.runtime.notice.as_deref() {
        row = row.detail("state", notice.to_string());
    }
    Some(row)
}

/// The Provider and Key rows under the gateway.
///
/// These are the Providers the enabled route groups name, drawn as `├` with
/// each of their Keys as `│ ├`. They are a read: a Provider joins the gateway
/// by being a group's member, not by being listed here.
fn gateway_child_rows(data: &ManagementData, strings: Strings) -> Vec<ConfigRow> {
    let Some(snapshot) = data.gateway.as_ref() else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for upstream in &snapshot.upstreams {
        let total = upstream.keys.len();
        let cooling = upstream
            .keys
            .iter()
            .filter(|key| matches!(key.state, vibex_core::GatewayKeyState::Cooling { .. }))
            .count();
        let invalid = upstream
            .keys
            .iter()
            .filter(|key| key.state == vibex_core::GatewayKeyState::Invalid)
            .count();
        let state = match (cooling, invalid) {
            (0, 0) => strings.config_state_ready().to_string(),
            (cooling, 0) => format!("{cooling} cooling"),
            (0, invalid) => format!("{invalid} invalid"),
            (cooling, invalid) => format!("{cooling} cooling · {invalid} invalid"),
        };
        rows.push(
            ConfigRow::new(
                format!(
                    "gateway-upstream:{}",
                    upstream.model_provider_profile_id.as_str()
                ),
                format!("  ├ {}", upstream.display_name),
            )
            .cell("1")
            .cell(total.to_string())
            .cell(state.clone())
            .cell(String::new())
            .chip(
                &state,
                if invalid > 0 {
                    ConfigTone::Bad
                } else if cooling > 0 || total == 0 {
                    ConfigTone::Warn
                } else {
                    ConfigTone::Ok
                },
            )
            .detail(
                strings.config_field_id(),
                upstream.model_provider_profile_id.as_str(),
            )
            .detail(
                strings.config_field_models(),
                upstream.model_count.to_string(),
            )
            .detail(strings.config_col_keys(), total.to_string())
            .actions(&[ConfigAction::RefreshSnapshot]),
        );
        for key in &upstream.keys {
            let (state, tone) = match key.state {
                vibex_core::GatewayKeyState::Ready => {
                    (strings.config_state_ready().to_string(), ConfigTone::Ok)
                }
                vibex_core::GatewayKeyState::Cooling { .. } => {
                    ("cooling".to_string(), ConfigTone::Warn)
                }
                vibex_core::GatewayKeyState::Invalid => {
                    (strings.config_state_invalid().to_string(), ConfigTone::Bad)
                }
                vibex_core::GatewayKeyState::Disabled => {
                    (strings.config_state_off().to_string(), ConfigTone::Muted)
                }
            };
            // The row identifies the Key by its masked fingerprint, never by
            // the credential id. A record written before the mask existed falls
            // back to a fixed mask rather than exposing the id it replaced.
            let key_label = if !key.masked_hint.trim().is_empty() {
                key.masked_hint.clone()
            } else if key.display_name.trim().is_empty()
                || key.display_name.trim() == format!("Key {}", key.credential_id.as_str())
            {
                "********".to_string()
            } else {
                key.display_name.clone()
            };
            rows.push(
                ConfigRow::new(
                    format!(
                        "gateway-key:{}:{}",
                        upstream.model_provider_profile_id.as_str(),
                        key.credential_id.as_str()
                    ),
                    format!("  │  ├ {key_label}"),
                )
                .cell(String::new())
                .cell("1")
                .cell(state.clone())
                .cell(String::new())
                .chip(&state, tone)
                .enabled(key.state != vibex_core::GatewayKeyState::Disabled)
                .detail(strings.config_field_kind(), "Key")
                .detail(strings.config_col_state(), state.clone())
                .detail(strings.config_field_models(), key.used_tokens.to_string())
                .actions(&[ConfigAction::Toggle, ConfigAction::Prefer]),
            );
        }
    }
    rows
}

fn provider_rows(data: &ManagementData, strings: Strings) -> Vec<ConfigRow> {
    // The authority lists profiles in its own order; the reader's arrangement
    // is a separate record, so the rows follow it when this client has read it.
    // A profile the arrangement does not name keeps its place after the ones it
    // does, which is what a stable sort over "unindexed last" gives.
    let mut ordered = data.providers.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|profile| {
        data.provider_order
            .iter()
            .find(|entry| entry.provider_profile_id == profile.id)
            .map(|entry| entry.order_index)
            .unwrap_or(i64::MAX)
    });
    let direct = ordered
        .into_iter()
        .map(|profile| {
            let enabled = profile.status == vibex_core::ProviderProfileStatus::Enabled;
            let key_count = matches!(
                profile.secret_setup_state,
                vibex_core::ProviderSecretSetupState::Available
                    | vibex_core::ProviderSecretSetupState::Referenced
            );
            ConfigRow::new(
                profile.id.as_str().to_string(),
                profile.display_name.clone(),
            )
            // A direct Provider is one group of one with one stored secret, and
            // it sits in the same columns as the gateway so the two kinds of
            // row can be read against one heading.
            .cell("1")
            .cell(if key_count { "1" } else { "0" })
            .cell(if enabled {
                strings.config_state_ready()
            } else {
                strings.config_state_off()
            })
            .cell(
                profile
                    .configured_models
                    .iter()
                    .filter(|model| model.enabled)
                    .count()
                    .to_string(),
            )
            .chip(
                if enabled {
                    strings.config_state_ready()
                } else {
                    strings.config_state_off()
                },
                if enabled {
                    ConfigTone::Ok
                } else {
                    ConfigTone::Muted
                },
            )
            .enabled(enabled)
            .detail(strings.config_field_id(), profile.id.as_str())
            .detail(strings.config_field_agent(), profile.agent_id.as_str())
            .detail(strings.config_field_kind(), profile.kind.to_string())
            .detail(
                strings.config_field_account(),
                profile.account_alias.clone().unwrap_or_default(),
            )
            .detail(
                strings.config_field_model(),
                profile.default_model.clone().unwrap_or_default(),
            )
            .detail(
                strings.config_field_models(),
                profile
                    .configured_models
                    .iter()
                    .filter(|model| model.enabled)
                    .map(|model| model.id.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
            .detail(
                strings.config_field_secret(),
                secret_label(profile.secret_setup_state, strings),
            )
            .detail(strings.config_field_updated(), stamp(profile.updated_at_ms))
            .actions(&[
                ConfigAction::Activate,
                ConfigAction::Edit,
                ConfigAction::Duplicate,
                ConfigAction::ClearSecret,
                ConfigAction::MoveUp,
                ConfigAction::MoveDown,
                ConfigAction::Secret,
                ConfigAction::Test,
                ConfigAction::FetchModels,
                ConfigAction::Delete,
            ])
        })
        .collect::<Vec<_>>();
    // The gateway leads: it is where a shared model actually comes from, and a
    // reader scanning the page should meet it before the Providers it consumes.
    let mut rows = gateway_row(data, strings).into_iter().collect::<Vec<_>>();
    rows.extend(gateway_child_rows(data, strings));
    rows.extend(direct);
    rows
}

fn server_rows(data: &ManagementData, strings: Strings) -> Vec<ConfigRow> {
    data.mcp
        .iter()
        .map(|server| {
            let enabled = server.status == vibex_core::McpServerStatus::Enabled;
            ConfigRow::new(server.id.as_str().to_string(), server.display_name.clone())
                .cell(transport_label(server.transport_kind))
                .cell(scope_label(server.scope_kind, strings))
                .cell(agents_label(&server.agent_matrix, strings))
                .cell(server.tags.join(", "))
                .chip(
                    if enabled {
                        strings.config_state_enabled()
                    } else {
                        strings.config_state_disabled()
                    },
                    tone_for_enabled(enabled),
                )
                .enabled(enabled)
                .summary(server.description.clone().unwrap_or_default())
                .detail(strings.config_field_id(), server.id.as_str())
                .detail(
                    strings.config_field_transport(),
                    transport_label(server.transport_kind),
                )
                .detail(
                    strings.config_field_scope(),
                    scope_label(server.scope_kind, strings),
                )
                .detail(strings.config_field_command(), {
                    let mut line = server.command.clone().unwrap_or_default();
                    if !server.args.is_empty() {
                        if !line.is_empty() {
                            line.push(' ');
                        }
                        line.push_str(&server.args.join(" "));
                    }
                    line
                })
                .detail(
                    strings.config_field_url(),
                    server.url.clone().unwrap_or_default(),
                )
                .detail(
                    strings.config_field_env(),
                    server
                        .env
                        .iter()
                        .map(|entry| format!("{}={}", entry.name, entry.value))
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
                .detail(
                    strings.config_field_headers(),
                    server
                        .headers
                        .iter()
                        .map(|entry| format!("{}: {}", entry.name, entry.value))
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
                .detail(
                    strings.config_field_agents(),
                    agents_label(&server.agent_matrix, strings),
                )
                .detail(strings.config_field_tags(), server.tags.join(", "))
                .detail(strings.config_field_updated(), stamp(server.updated_at_ms))
                .actions(&[
                    ConfigAction::Toggle,
                    ConfigAction::Edit,
                    ConfigAction::Matrix,
                    ConfigAction::Validate,
                    ConfigAction::Delete,
                ])
        })
        .collect()
}

fn skill_rows(data: &ManagementData, strings: Strings) -> Vec<ConfigRow> {
    data.skills
        .iter()
        .map(|skill| {
            let enabled = skill.status == vibex_core::SkillStatus::Enabled;
            ConfigRow::new(skill.id.as_str().to_string(), skill.display_name.clone())
                .cell(format!("{:?}", skill.source_kind))
                .cell(skill_scope_label(skill.scope_kind, strings))
                .cell(skill_agents_label(&skill.agent_matrix, strings))
                .cell(stamp(skill.updated_at_ms))
                .chip(
                    if enabled {
                        strings.config_state_enabled()
                    } else {
                        strings.config_state_disabled()
                    },
                    tone_for_enabled(enabled),
                )
                .enabled(enabled)
                .summary(skill.description.clone().unwrap_or_default())
                .detail(strings.config_field_id(), skill.id.as_str())
                .detail(
                    strings.config_field_scope(),
                    skill_scope_label(skill.scope_kind, strings),
                )
                .detail(
                    strings.config_field_source(),
                    skill.source_uri.clone().unwrap_or_default(),
                )
                .detail(strings.config_field_tags(), skill.tags.join(", "))
                .detail(
                    strings.config_field_preview(),
                    skill.content_preview.clone().unwrap_or_default(),
                )
                .detail(
                    strings.config_field_body(),
                    skill
                        .body
                        .as_ref()
                        .map(|body| format!("{} {}", body.lines().count(), strings.config_lines()))
                        .unwrap_or_default(),
                )
                .detail(
                    strings.config_field_agents(),
                    skill_agents_label(&skill.agent_matrix, strings),
                )
                .actions(&[
                    ConfigAction::Toggle,
                    ConfigAction::Edit,
                    ConfigAction::Matrix,
                    ConfigAction::Validate,
                    ConfigAction::Delete,
                ])
        })
        .collect()
}

pub fn market_rows(data: &ManagementData, strings: Strings) -> Vec<ConfigRow> {
    data.mcp_market
        .iter()
        .map(|entry| {
            // The registry has no idea what this machine already has, so the
            // card is matched against the stored servers by name: a card that
            // said "Install" over a server the reader already runs would be
            // offering to do what has been done.
            let installed = data
                .mcp
                .iter()
                .any(|server| server.display_name == entry.name);
            let command = entry
                .url
                .clone()
                .or_else(|| entry.command.clone())
                .unwrap_or_default();
            ConfigRow::new(entry.id.clone(), entry.name.clone())
                .cell(transport_label(entry.transport))
                .cell(entry.version.clone().unwrap_or_default())
                .cell(entry.author.clone().unwrap_or_default())
                .cell(entry.package_kind.clone().unwrap_or_default())
                // The registry's own lifecycle wins over the local answer: a
                // deprecated entry the reader happens to have is deprecated
                // first, and installed second.
                // A registry that has withdrawn the entry wins over the local
                // answer — deprecated first, installed second — but "active" is
                // not a lifecycle of its own, so a card the reader already runs
                // says so rather than repeating the registry's default.
                .chip(
                    match (lifecycle_status(entry), installed) {
                        (Some(status), _) => status,
                        (None, true) => strings.config_state_installed().to_string(),
                        (None, false) => strings.config_state_active().to_string(),
                    },
                    if lifecycle_status(entry).is_some() {
                        ConfigTone::Warn
                    } else if installed {
                        ConfigTone::Ok
                    } else {
                        ConfigTone::Muted
                    },
                )
                .summary(entry.description.clone().unwrap_or_default())
                .detail(strings.config_field_id(), entry.id.clone())
                .detail(
                    strings.config_field_transport(),
                    transport_label(entry.transport),
                )
                .detail(strings.config_field_command(), command)
                .detail(
                    strings.config_field_homepage(),
                    entry.homepage.clone().unwrap_or_default(),
                )
                .detail(
                    strings.config_field_repository(),
                    entry.repository.clone().unwrap_or_default(),
                )
                .detail(
                    strings.config_field_env(),
                    entry
                        .env
                        .iter()
                        .map(|requirement| requirement.name.clone())
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
                // How many of the variables the entry names have to be filled
                // in: a card whose variables are all optional is a different
                // decision from one that will not start without two of them.
                .detail(
                    strings.config_market_env_required(),
                    format!(
                        "{} / {}",
                        entry
                            .env
                            .iter()
                            .filter(|requirement| requirement.required)
                            .count(),
                        entry.env.len()
                    ),
                )
                .detail(
                    strings.config_field_package(),
                    entry.package_kind.clone().unwrap_or_default(),
                )
                .detail(
                    strings.config_field_updated(),
                    entry.updated_at.clone().unwrap_or_default(),
                )
                .actions(&market_actions(entry, installed))
        })
        .collect()
}

/// The lifecycle a registry entry is in, when it is in one at all.
///
/// A registry says "active" about almost everything, which is a default rather
/// than a state: only the states that change what the reader should do with the
/// card are worth the chip.
fn lifecycle_status(entry: &vibex_core::McpMarketEntry) -> Option<String> {
    match entry.status.as_deref() {
        Some("deprecated") | Some("removed") => entry.status.clone(),
        _ => None,
    }
}

/// What a market card offers: install (or reinstall), check the launcher, and
/// the entry's own page when it names one.
fn market_actions(entry: &vibex_core::McpMarketEntry, installed: bool) -> Vec<ConfigAction> {
    let mut actions = vec![
        if installed {
            ConfigAction::Reinstall
        } else {
            ConfigAction::Install
        },
        ConfigAction::Validate,
    ];
    if entry.homepage.is_some() || entry.repository.is_some() {
        actions.push(ConfigAction::OpenPage);
    }
    actions
}

/// The page a market entry names for itself, preferring its own homepage.
pub fn market_entry_url(entry: &vibex_core::McpMarketEntry) -> Option<String> {
    entry
        .homepage
        .clone()
        .or_else(|| entry.repository.clone())
        .filter(|url| !url.trim().is_empty())
}

pub fn skill_market_rows(data: &ManagementData, strings: Strings) -> Vec<ConfigRow> {
    data.skill_market
        .iter()
        .map(|entry| {
            // A Skill the reader already installed is one the registry entry
            // points at: the stored record keeps the registry identity it came
            // from, so the card can say so rather than offering a first install
            // over an installed Skill.
            let uri = market_skill_uri(entry);
            let installed = data
                .skills
                .iter()
                .any(|skill| skill.source_uri.as_deref() == Some(uri.as_str()));
            ConfigRow::new(entry.id.clone(), entry.name.clone())
                .cell(strings.config_kind_skill())
                .cell(entry.version.clone().unwrap_or_default())
                .cell(entry.owner_handle.clone().unwrap_or_default())
                .cell(skill_reach(entry))
                .chip(
                    if installed {
                        strings.config_state_installed()
                    } else {
                        strings.config_source_market()
                    },
                    if installed {
                        ConfigTone::Ok
                    } else {
                        ConfigTone::Muted
                    },
                )
                .summary(entry.summary.clone().unwrap_or_default())
                // The id is what a stored Skill records as its origin, and the
                // slug is what the registry routes by; a reader comparing the
                // card with what they have needs both.
                .detail(strings.config_field_id(), entry.id.clone())
                .detail(strings.config_field_slug(), entry.slug.clone())
                .detail(
                    strings.config_field_version(),
                    entry.version.clone().unwrap_or_default(),
                )
                .detail(
                    strings.config_field_publisher(),
                    entry.owner_handle.clone().unwrap_or_default(),
                )
                .detail(strings.config_field_stars(), entry.stars.to_string())
                .detail(
                    strings.config_field_downloads(),
                    entry.downloads.to_string(),
                )
                .detail(
                    strings.config_field_updated(),
                    entry.updated_at.map(stamp).unwrap_or_default(),
                )
                // The registry publishes whole Skills, so the card ends with
                // the endpoint its document is read from: a reader who wants
                // the source rather than the copy has somewhere to look.
                .detail(strings.config_field_source(), market_skill_uri(entry))
                .actions(&[if installed {
                    ConfigAction::Reinstall
                } else {
                    ConfigAction::Install
                }])
        })
        .collect()
}

/// Which of the roster's three groups an Agent is in, as a sort key.
///
/// Enabled first, then added but switched off, then the ones the roster does
/// not hold — the order the desktop shows them in, so the two clients list the
/// same roster the same way.
pub fn agent_group(agent: &vibex_core::AgentSnapshotEntry) -> u8 {
    match (agent.added, agent.enabled) {
        (true, true) => 0,
        (true, false) => 1,
        (false, _) => 2,
    }
}

/// The registry identity a stored Skill records when it is installed.
///
/// It is the same string the installed check compares against: one shape, so a
/// card that says "installed" and a document that records its origin cannot
/// disagree.
pub fn market_skill_uri(entry: &vibex_core::SkillMarketEntry) -> String {
    format!("market:{}", entry.id)
}

/// How far a registry Skill has reached, by the first count it publishes.
///
/// A registry fills in what it knows: a Skill that has been downloaded but
/// never installed has installs of zero, and a row that says "0" over nine
/// downloads reads as a Skill nobody wanted. The first count that is not zero
/// is the one that says something.
pub fn skill_reach(entry: &vibex_core::SkillMarketEntry) -> String {
    if entry.installs > 0 {
        entry.installs.to_string()
    } else if entry.downloads > 0 {
        entry.downloads.to_string()
    } else {
        entry.stars.to_string()
    }
}

fn mcp_source_rows(data: &ManagementData, strings: Strings) -> Vec<ConfigRow> {
    data.mcp_discoveries
        .iter()
        .map(|discovery| {
            let candidate = discovery.candidate.as_ref();
            let title = candidate
                .map(|candidate| candidate.display_name.clone())
                .unwrap_or_else(|| discovery.import_key.clone());
            let transport = candidate
                .map(|candidate| transport_label(candidate.transport_kind))
                .unwrap_or_default();
            let mut actions = Vec::new();
            if discovery.status == ResourceDiscoveryStatus::Importable {
                actions.push(ConfigAction::Import);
            }
            let mut row = ConfigRow::new(discovery.discovery_id.clone(), title)
                .cell(discovery.source_agent_id.as_str())
                .cell(discovery.source_path.clone())
                .cell(format!("{:?}", discovery.status))
                .chip(
                    match discovery.status {
                        ResourceDiscoveryStatus::Importable => strings.config_state_importable(),
                        ResourceDiscoveryStatus::AlreadyImported => strings.config_state_imported(),
                        ResourceDiscoveryStatus::Unsupported => strings.config_state_unsupported(),
                        ResourceDiscoveryStatus::Error => strings.config_state_error(),
                    },
                    discovery_tone(discovery.status),
                )
                .detail(strings.config_field_id(), discovery.discovery_id.clone())
                .detail(
                    strings.config_field_agent(),
                    discovery.source_agent_id.as_str(),
                )
                .detail(strings.config_field_path(), discovery.source_path.clone())
                .detail(strings.config_field_transport(), transport)
                .detail(
                    strings.config_field_command(),
                    candidate
                        .and_then(|candidate| candidate.command.clone())
                        .unwrap_or_default(),
                )
                .detail(
                    strings.config_field_url(),
                    candidate
                        .and_then(|candidate| candidate.url.clone())
                        .unwrap_or_default(),
                )
                .detail(
                    strings.config_field_description(),
                    candidate
                        .and_then(|candidate| candidate.description.clone())
                        .unwrap_or_default(),
                )
                .actions(&actions);
            row.enabled = discovery.status == ResourceDiscoveryStatus::Importable;
            row
        })
        .collect()
}

fn skill_source_rows(data: &ManagementData, strings: Strings) -> Vec<ConfigRow> {
    data.skill_discoveries
        .iter()
        .map(|discovery| {
            let mut actions = Vec::new();
            if discovery.status == ResourceDiscoveryStatus::Importable {
                actions.push(ConfigAction::Import);
            }
            ConfigRow::new(
                discovery.discovery_id.clone(),
                discovery.display_name.clone(),
            )
            .cell(discovery.source_agent_id.as_str())
            .cell(discovery.source_path.clone())
            .cell(format!("{:?}", discovery.status))
            .chip(
                match discovery.status {
                    ResourceDiscoveryStatus::Importable => strings.config_state_importable(),
                    ResourceDiscoveryStatus::AlreadyImported => strings.config_state_imported(),
                    ResourceDiscoveryStatus::Unsupported => strings.config_state_unsupported(),
                    ResourceDiscoveryStatus::Error => strings.config_state_error(),
                },
                discovery_tone(discovery.status),
            )
            .summary(discovery.description.clone().unwrap_or_default())
            .detail(strings.config_field_id(), discovery.discovery_id.clone())
            .detail(
                strings.config_field_agent(),
                discovery.source_agent_id.as_str(),
            )
            .detail(strings.config_field_path(), discovery.source_path.clone())
            .detail(
                strings.config_field_command(),
                discovery.command_name.clone(),
            )
            .detail(
                strings.config_field_preview(),
                discovery.content_preview.clone().unwrap_or_default(),
            )
            .actions(&actions)
        })
        .collect()
}

fn prompt_rows(data: &ManagementData, strings: Strings) -> Vec<ConfigRow> {
    data.prompts
        .iter()
        .map(|prompt| {
            let enabled = prompt.status == vibex_core::PromptStatus::Enabled;
            ConfigRow::new(prompt.id.as_str().to_string(), prompt.display_name.clone())
                .cell(prompt_kind_label(prompt.kind, strings))
                .cell(prompt_scope_label(prompt.scope_kind, strings))
                .cell(prompt.tags.join(", "))
                .cell(stamp(prompt.updated_at_ms))
                .chip(
                    match prompt.status {
                        vibex_core::PromptStatus::Enabled => strings.config_state_enabled(),
                        vibex_core::PromptStatus::Disabled => strings.config_state_disabled(),
                        vibex_core::PromptStatus::Archived => strings.config_state_archived(),
                    },
                    tone_for_enabled(enabled),
                )
                .enabled(enabled)
                .summary(prompt.description.clone().unwrap_or_default())
                .detail(strings.config_field_id(), prompt.id.as_str())
                .detail(
                    strings.config_field_kind(),
                    prompt_kind_label(prompt.kind, strings),
                )
                .detail(
                    strings.config_field_scope(),
                    prompt_scope_label(prompt.scope_kind, strings),
                )
                .detail(strings.config_field_tags(), prompt.tags.join(", "))
                .detail(
                    strings.config_field_body(),
                    format!("{} {}", prompt.body.lines().count(), strings.config_lines()),
                )
                .detail(strings.config_field_preview(), prompt.body.clone())
                .actions(&[
                    ConfigAction::Toggle,
                    ConfigAction::Edit,
                    ConfigAction::Validate,
                    ConfigAction::Delete,
                ])
        })
        .collect()
}

// ---------------------------------------------------------------------------
// forms
// ---------------------------------------------------------------------------

/// How one form field is presented and changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormFieldKind {
    /// Free text, typed in place.
    Text,
    /// Free text that may hold newlines.
    Multiline,
    /// A closed set: `←`/`→` and a click cycle it.
    Choice {
        /// Wire values, in order. Owned rather than static because a set can
        /// come from the runtime — the Agents a profile can belong to are read,
        /// not written down here.
        options: Vec<String>,
        /// What each option is called on screen, aligned with `options`.
        labels: Vec<String>,
    },
    /// Two states.
    Toggle,
}

/// One line of an editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormField {
    /// What the field means to the reducer that reads it back.
    pub key: String,
    pub label: String,
    pub value: String,
    /// The value the form opened with, so a cancel can put it back and the
    /// footer can say whether anything changed.
    pub origin: String,
    pub kind: FormFieldKind,
    pub hint: Option<String>,
    /// Whether the value is the reader's to change.
    ///
    /// A locked field is shown and written but not edited: the Agent it belongs
    /// to pins the value, and a form that let the reader type another one would
    /// be asking for something the authority refuses.
    pub locked: bool,
    /// Whether the value is a secret, drawn masked and never echoed.
    ///
    /// A requirement the registry marked secret is one the reader is pasting a
    /// credential into, and a credential on screen is a credential in whatever
    /// is behind the terminal.
    pub secret: bool,
}

impl FormField {
    fn text(key: impl Into<String>, label: impl Into<String>, value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            key: key.into(),
            label: label.into(),
            origin: value.clone(),
            value,
            kind: FormFieldKind::Text,
            hint: None,
            locked: false,
            secret: false,
        }
    }

    fn multiline(
        key: impl Into<String>,
        label: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        let value = value.into();
        Self {
            key: key.into(),
            label: label.into(),
            origin: value.clone(),
            value,
            kind: FormFieldKind::Multiline,
            hint: None,
            locked: false,
            secret: false,
        }
    }

    fn choice(
        key: impl Into<String>,
        label: impl Into<String>,
        options: Vec<&'static str>,
        labels: Vec<&'static str>,
        value: &str,
    ) -> Self {
        Self::choice_owned(
            key,
            label,
            options.into_iter().map(str::to_string).collect(),
            labels.into_iter().map(str::to_string).collect(),
            value,
        )
    }

    /// The same, for a set the runtime supplied.
    fn choice_owned(
        key: impl Into<String>,
        label: impl Into<String>,
        options: Vec<String>,
        labels: Vec<String>,
        value: &str,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            origin: value.to_string(),
            value: value.to_string(),
            kind: FormFieldKind::Choice { options, labels },
            hint: None,
            locked: false,
            secret: false,
        }
    }

    fn toggle(key: impl Into<String>, label: impl Into<String>, value: bool) -> Self {
        let value = if value { "on" } else { "off" }.to_string();
        Self {
            key: key.into(),
            label: label.into(),
            origin: value.clone(),
            value,
            kind: FormFieldKind::Toggle,
            hint: None,
            locked: false,
            secret: false,
        }
    }

    fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Pin the value: shown and written, but not editable.
    fn locked(mut self) -> Self {
        self.locked = true;
        self
    }

    /// Mark the value as a secret. It is drawn masked and never echoed.
    fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    pub fn changed(&self) -> bool {
        self.value != self.origin
    }

    /// The value as a flag, for a toggle field.
    pub fn is_on(&self) -> bool {
        self.value == "on"
    }

    /// Whether the value needs writing at all. A text field left empty is
    /// `None` rather than an empty string, so "not set" stays not set.
    pub fn optional(&self) -> Option<String> {
        let trimmed = self.value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    pub fn required(&self) -> String {
        self.value.trim().to_string()
    }

    /// Advance a choice or toggle in place. Answers whether it moved.
    pub fn step(&mut self, delta: isize) -> bool {
        match &self.kind {
            FormFieldKind::Choice { options, .. } => {
                if options.is_empty() {
                    return false;
                }
                let index = options
                    .iter()
                    .position(|option| option == &self.value)
                    .unwrap_or(0) as isize;
                let next = (index + delta).rem_euclid(options.len() as isize) as usize;
                self.value = options[next].to_string();
                true
            }
            FormFieldKind::Toggle => {
                self.value = if self.is_on() { "off" } else { "on" }.to_string();
                true
            }
            _ => false,
        }
    }

    pub fn displayed(&self) -> String {
        match &self.kind {
            FormFieldKind::Choice { options, labels } => {
                let index = options
                    .iter()
                    .position(|option| *option == self.value)
                    .unwrap_or(0);
                labels
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| self.value.clone())
            }
            FormFieldKind::Toggle => if self.is_on() { "on" } else { "off" }.to_string(),
            _ => self.value.clone(),
        }
    }
}

/// What a finished form becomes.
///
/// The purpose travels with the overlay rather than being looked up from the
/// cursor when the reader saves: a read that lands while the form is open must
/// not make the save write to a different record than the one it was opened on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormPurpose {
    McpServer {
        server_id: Option<String>,
    },
    Skill {
        skill_id: Option<String>,
    },
    Prompt {
        prompt_id: Option<String>,
    },
    ProviderProfile {
        agent_id: String,
        profile_id: String,
    },
    /// A profile that does not exist yet.
    ///
    /// The authority mints the id, so there is no record to name. `source` is
    /// the profile a copy started from, when the reader asked for one: the form
    /// does not represent the small/large model, the reasoning effort, the
    /// sandbox and permission defaults or the request options, and a copy that
    /// dropped them would not be a copy. They travel whole instead.
    NewProviderProfile {
        source: Option<Box<vibex_core::ProviderProfile>>,
    },
    /// A custom ACP Agent, from nothing.
    NewCustomAgent,
    /// A Skill from the registry, and the Agents that should receive it.
    SkillMarketInstall {
        entry_id: String,
        slug: String,
        owner_handle: Option<String>,
        version: Option<String>,
    },
    /// One Agent's sign-in method, and the variables it collects.
    AgentAuthMethod {
        agent_id: String,
        method_id: String,
    },
    /// The settings of one Model inside a profile.
    ProviderModel {
        agent_id: String,
        profile_id: String,
        model_id: String,
    },
    /// Which Agents receive one MCP server or Skill.
    McpMatrix {
        server_id: String,
    },
    SkillMatrix {
        skill_id: String,
    },
    /// One registry entry, about to become a server.
    ///
    /// The entry has no record yet, so the form carries the candidate the
    /// registry published and the values the reader fills in are written onto
    /// it: what is installed is what the form showed, rather than a second
    /// reading of the registry that could have moved on.
    McpMarketInstall {
        entry_id: String,
        candidate: Box<vibex_core::McpServerCreateRequest>,
    },
}

impl FormPurpose {
    /// Whether the form creates a record rather than changing one.
    pub fn is_new(&self) -> bool {
        matches!(
            self,
            Self::McpServer { server_id: None }
                | Self::Skill { skill_id: None }
                | Self::Prompt { prompt_id: None }
                | Self::NewProviderProfile { .. }
        )
    }
}

/// An open editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigForm {
    pub purpose: FormPurpose,
    pub title: String,
    pub fields: Vec<FormField>,
    pub selected: usize,
    /// The key of the field being typed into, with the buffer under it.
    ///
    /// The buffer is separate from the field so `Esc` inside an edit puts the
    /// old value back rather than closing the form: "undo what I am in the
    /// middle of" is the same promise everywhere else in this client.
    pub editing: Option<String>,
    pub error: Option<String>,
    pub scroll: u16,
    /// A line the form is about to write, shown read-only above the fields.
    ///
    /// A registry entry is a launcher before it is a record: what will be run
    /// is not one of the fields the reader fills in, but it is the thing they
    /// are agreeing to, so it is read back to them before anything is saved.
    pub preview: Option<String>,
}

impl ConfigForm {
    pub fn new(purpose: FormPurpose, title: impl Into<String>, fields: Vec<FormField>) -> Self {
        Self {
            purpose,
            title: title.into(),
            fields,
            selected: 0,
            editing: None,
            error: None,
            scroll: 0,
            preview: None,
        }
    }

    pub fn dirty(&self) -> bool {
        self.fields.iter().any(FormField::changed)
    }

    /// Give the form a line to read back before it is saved.
    pub fn preview(mut self, preview: impl Into<String>) -> Self {
        self.preview = Some(preview.into());
        self
    }

    pub fn field(&self, key: &str) -> Option<&FormField> {
        self.fields.iter().find(|field| field.key == key)
    }

    pub fn field_mut(&mut self, key: &str) -> Option<&mut FormField> {
        self.fields.iter_mut().find(|field| field.key == key)
    }

    pub fn value(&self, key: &str) -> Option<String> {
        self.field(key).map(FormField::required)
    }

    pub fn optional(&self, key: &str) -> Option<String> {
        self.field(key).and_then(FormField::optional)
    }

    pub fn flag(&self, key: &str) -> bool {
        self.field(key).is_some_and(FormField::is_on)
    }

    /// Every field that is on, for the matrix forms: their keys are `agent:<id>`.
    pub fn enabled_agents(&self) -> Vec<String> {
        self.fields
            .iter()
            .filter(|field| field.key.starts_with("agent:") && field.is_on())
            .map(|field| field.key.trim_start_matches("agent:").to_string())
            .collect()
    }

    pub fn selected_field(&self) -> Option<&FormField> {
        self.fields.get(self.selected)
    }

    pub fn step_field(&mut self, delta: isize) {
        if self.fields.is_empty() {
            return;
        }
        let current = self.selected.min(self.fields.len() - 1) as isize;
        let next = (current + delta).rem_euclid(self.fields.len() as isize) as usize;
        self.selected = next;
    }
}

/// What `Enter` on a row opened: the actions that row actually offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryActions {
    pub title: String,
    /// The record's stable key, so the chosen action resolves against the
    /// record rather than against whatever the cursor moved to.
    pub row_key: String,
    pub actions: Vec<ConfigAction>,
    pub selected: usize,
}

// ---------------------------------------------------------------------------
// form construction
// ---------------------------------------------------------------------------

const MCP_TRANSPORTS: [&str; 3] = ["stdio", "http", "sse"];

fn mcp_transport_options() -> Vec<&'static str> {
    MCP_TRANSPORTS.to_vec()
}

fn mcp_transport_labels() -> Vec<&'static str> {
    MCP_TRANSPORTS.to_vec()
}

fn scope_choices(strings: Strings) -> (Vec<&'static str>, Vec<&'static str>) {
    (
        vec!["user", "workspace", "global", "project"],
        vec![
            strings.config_scope_user(),
            strings.config_scope_workspace(),
            strings.config_scope_global(),
            strings.config_scope_project(),
        ],
    )
}

fn status_choices(strings: Strings) -> (Vec<&'static str>, Vec<&'static str>) {
    (
        vec!["enabled", "disabled"],
        vec![
            strings.config_state_enabled(),
            strings.config_state_disabled(),
        ],
    )
}

fn scope_wire(scope: vibex_core::McpServerScopeKind) -> &'static str {
    match scope {
        vibex_core::McpServerScopeKind::Global => "global",
        vibex_core::McpServerScopeKind::User => "user",
        vibex_core::McpServerScopeKind::Project => "project",
        vibex_core::McpServerScopeKind::Workspace => "workspace",
    }
}

fn mcp_scope_wire(scope: vibex_core::McpServerScopeKind) -> &'static str {
    scope_wire(scope)
}

fn skill_scope_wire(scope: vibex_core::SkillScopeKind) -> &'static str {
    match scope {
        vibex_core::SkillScopeKind::Global => "global",
        vibex_core::SkillScopeKind::User => "user",
        vibex_core::SkillScopeKind::Project => "project",
        vibex_core::SkillScopeKind::Workspace => "workspace",
    }
}

fn prompt_scope_wire(scope: vibex_core::PromptScopeKind) -> &'static str {
    match scope {
        vibex_core::PromptScopeKind::Global => "global",
        vibex_core::PromptScopeKind::User => "user",
        vibex_core::PromptScopeKind::Project => "project",
        vibex_core::PromptScopeKind::Workspace => "workspace",
    }
}

/// An MCP server editor, over the record or over a blank one.
pub fn mcp_form(server: Option<&vibex_core::McpServer>, strings: Strings) -> ConfigForm {
    let transport = server
        .map(|server| transport_label(server.transport_kind))
        .unwrap_or("stdio");
    let (scope_options, scope_labels) = scope_choices(strings);
    let (status_options, status_labels) = status_choices(strings);
    let env = server
        .map(|server| {
            server
                .env
                .iter()
                .map(|entry| format!("{}={}", entry.name, entry.value))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let headers = server
        .map(|server| {
            server
                .headers
                .iter()
                .map(|entry| format!("{}: {}", entry.name, entry.value))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let fields = vec![
        FormField::text(
            "name",
            strings.config_form_name(),
            server
                .map(|server| server.display_name.clone())
                .unwrap_or_default(),
        ),
        FormField::choice(
            "transport",
            strings.config_field_transport(),
            mcp_transport_options(),
            mcp_transport_labels(),
            transport,
        ),
        FormField::text(
            "command",
            strings.config_field_command(),
            server
                .and_then(|server| server.command.clone())
                .unwrap_or_default(),
        ),
        FormField::text(
            "args",
            strings.config_field_args(),
            server
                .map(|server| server.args.join(" "))
                .unwrap_or_default(),
        ),
        FormField::multiline("env", strings.config_field_env(), env)
            .hint(strings.config_hint_env()),
        FormField::text(
            "url",
            strings.config_field_url(),
            server
                .and_then(|server| server.url.clone())
                .unwrap_or_default(),
        ),
        FormField::multiline("headers", strings.config_field_headers(), headers)
            .hint(strings.config_hint_request_headers()),
        FormField::text(
            "description",
            strings.config_field_description(),
            server
                .and_then(|server| server.description.clone())
                .unwrap_or_default(),
        ),
        FormField::text(
            "tags",
            strings.config_field_tags(),
            server
                .map(|server| server.tags.join(", "))
                .unwrap_or_default(),
        ),
        FormField::choice(
            "scope",
            strings.config_field_scope(),
            scope_options,
            scope_labels,
            server
                .map(|server| mcp_scope_wire(server.scope_kind))
                .unwrap_or("user"),
        ),
        FormField::choice(
            "status",
            strings.config_col_state(),
            status_options,
            status_labels,
            server
                .map(|server| {
                    if server.status == vibex_core::McpServerStatus::Enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                })
                .unwrap_or("enabled"),
        ),
    ];
    let purpose = FormPurpose::McpServer {
        server_id: server.map(|server| server.id.as_str().to_string()),
    };
    let title = if server.is_some() {
        strings.config_form_edit_server()
    } else {
        strings.config_form_new_server()
    };
    ConfigForm::new(purpose, title, fields)
}

/// A Skill editor.
pub fn skill_form(skill: Option<&vibex_core::Skill>, strings: Strings) -> ConfigForm {
    let (scope_options, scope_labels) = scope_choices(strings);
    let (status_options, status_labels) = status_choices(strings);
    let source_kind = skill
        .map(|skill| match skill.source_kind {
            vibex_core::SkillSourceKind::Manual => "manual",
            vibex_core::SkillSourceKind::GitRepo => "git_repo",
            vibex_core::SkillSourceKind::LocalFolder => "local_folder",
            vibex_core::SkillSourceKind::Marketplace => "marketplace",
        })
        .unwrap_or("manual");
    let fields = vec![
        FormField::text(
            "name",
            strings.config_form_name(),
            skill
                .map(|skill| skill.display_name.clone())
                .unwrap_or_default(),
        ),
        FormField::multiline(
            "description",
            strings.config_field_description(),
            skill
                .and_then(|skill| skill.description.clone())
                .unwrap_or_default(),
        ),
        FormField::multiline(
            "body",
            strings.config_field_body(),
            skill
                .and_then(|skill| skill.body.clone())
                .unwrap_or_default(),
        ),
        FormField::text(
            "tags",
            strings.config_field_tags(),
            skill.map(|skill| skill.tags.join(", ")).unwrap_or_default(),
        ),
        FormField::choice(
            "source_kind",
            strings.config_field_source(),
            vec!["manual", "local_folder", "git_repo"],
            vec![
                strings.config_source_manual(),
                strings.config_source_folder(),
                strings.config_source_git(),
            ],
            source_kind,
        ),
        FormField::text(
            "source_uri",
            strings.config_field_path(),
            skill
                .and_then(|skill| skill.source_uri.clone())
                .unwrap_or_default(),
        ),
        FormField::choice(
            "scope",
            strings.config_field_scope(),
            scope_options,
            scope_labels,
            skill
                .map(|skill| skill_scope_wire(skill.scope_kind))
                .unwrap_or("user"),
        ),
        FormField::choice(
            "status",
            strings.config_col_state(),
            status_options,
            status_labels,
            skill
                .map(|skill| {
                    if skill.status == vibex_core::SkillStatus::Enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                })
                .unwrap_or("enabled"),
        ),
    ];
    let purpose = FormPurpose::Skill {
        skill_id: skill.map(|skill| skill.id.as_str().to_string()),
    };
    let title = if skill.is_some() {
        strings.config_form_edit_skill()
    } else {
        strings.config_form_new_skill()
    };
    ConfigForm::new(purpose, title, fields)
}

/// A Prompt editor.
pub fn prompt_form(prompt: Option<&vibex_core::Prompt>, strings: Strings) -> ConfigForm {
    let (scope_options, scope_labels) = scope_choices(strings);
    let kind = prompt
        .map(|prompt| match prompt.kind {
            PromptKind::ReusablePrompt => "reusable_prompt",
            PromptKind::SlashCommand => "slash_command",
            PromptKind::SystemSnippet => "system_snippet",
        })
        .unwrap_or("reusable_prompt");
    let status = prompt
        .map(|prompt| match prompt.status {
            vibex_core::PromptStatus::Enabled => "enabled",
            vibex_core::PromptStatus::Disabled => "disabled",
            vibex_core::PromptStatus::Archived => "archived",
        })
        .unwrap_or("enabled");
    let fields = vec![
        FormField::text(
            "name",
            strings.config_form_name(),
            prompt
                .map(|prompt| prompt.display_name.clone())
                .unwrap_or_default(),
        ),
        FormField::choice(
            "kind",
            strings.config_field_kind(),
            vec!["reusable_prompt", "slash_command", "system_snippet"],
            vec![
                strings.config_prompt_kind_reusable(),
                strings.config_prompt_kind_slash(),
                strings.config_prompt_kind_snippet(),
            ],
            kind,
        ),
        FormField::multiline(
            "description",
            strings.config_field_description(),
            prompt
                .and_then(|prompt| prompt.description.clone())
                .unwrap_or_default(),
        ),
        FormField::multiline(
            "body",
            strings.config_field_body(),
            prompt.map(|prompt| prompt.body.clone()).unwrap_or_default(),
        ),
        FormField::text(
            "tags",
            strings.config_field_tags(),
            prompt
                .map(|prompt| prompt.tags.join(", "))
                .unwrap_or_default(),
        ),
        FormField::choice(
            "scope",
            strings.config_field_scope(),
            scope_options,
            scope_labels,
            prompt
                .map(|prompt| prompt_scope_wire(prompt.scope_kind))
                .unwrap_or("user"),
        ),
        FormField::choice(
            "status",
            strings.config_col_state(),
            vec!["enabled", "disabled", "archived"],
            vec![
                strings.config_state_enabled(),
                strings.config_state_disabled(),
                strings.config_state_archived(),
            ],
            status,
        ),
    ];
    let purpose = FormPurpose::Prompt {
        prompt_id: prompt.map(|prompt| prompt.id.as_str().to_string()),
    };
    let title = if prompt.is_some() {
        strings.config_form_edit_prompt()
    } else {
        strings.config_form_new_prompt()
    };
    ConfigForm::new(purpose, title, fields)
}

/// A model-provider profile editor.
pub fn provider_form(
    profile: &vibex_core::ProviderProfileSummary,
    detail: Option<&vibex_core::ProviderProfile>,
    strings: Strings,
) -> ConfigForm {
    // The endpoint is not in the summary, so it is shown when this client has
    // read the profile and left empty when it has not: the field writes what it
    // holds, and an empty one leaves what is stored alone.
    let pinned = fixed_endpoint_for(&profile.agent_id);
    let base_url = pinned
        .clone()
        .or_else(|| detail.and_then(|detail| detail.base_url.clone()))
        .unwrap_or_default();
    let options = detail
        .map(|detail| ProviderOptionsDraft::read(&detail.provider_options))
        .unwrap_or_default();
    let fields = vec![
        FormField::text(
            "name",
            strings.config_form_name(),
            profile.display_name.clone(),
        ),
        base_url_field(base_url, pinned.as_deref(), strings),
        FormField::text(
            "default_model",
            strings.config_field_model(),
            profile.default_model.clone().unwrap_or_default(),
        ),
        FormField::text("website", strings.config_field_website(), options.website)
            .hint(strings.config_hint_endpoint()),
        FormField::multiline(
            "headers",
            strings.config_field_request_headers(),
            options
                .headers
                .iter()
                .map(|(name, value)| format!("{name}: {}", value.displayed()))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .hint(strings.config_hint_headers()),
        FormField::multiline(
            "protocol_urls",
            strings.config_field_protocol_urls(),
            options
                .protocol_urls
                .iter()
                .map(|(wire, url)| format!("{}={url}", wire_api_label(*wire)))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .hint(strings.config_hint_protocol_urls()),
    ];
    ConfigForm::new(
        FormPurpose::ProviderProfile {
            agent_id: profile.agent_id.as_str().to_string(),
            profile_id: profile.id.as_str().to_string(),
        },
        strings.config_form_edit_provider(),
        fields,
    )
    // The headers as JSON: what `c` copies, and what the reader checks their
    // `Name: value` lines against.
    .preview(write_header_override(&options.headers))
}

/// The form that creates a profile, or starts a copy of one.
///
/// A copy opens here rather than being written straight out. This client reads
/// profiles through the summary the authority publishes, and that summary
/// carries the name, the account and the Models but not the endpoint or the
/// request headers: a duplicate written from the summary alone would be a
/// profile that launches nowhere, so the reader is shown what the copy starts
/// with and asked for the rest rather than handed a record that is quietly
/// missing half of what it was copied from.
pub fn provider_create_form(
    agents: &[vibex_core::AgentSnapshotEntry],
    source: Option<&vibex_core::ProviderProfileSummary>,
    detail: Option<&vibex_core::ProviderProfile>,
    strings: Strings,
) -> ConfigForm {
    let options = agents
        .iter()
        .map(|agent| agent.id.as_str().to_string())
        .collect::<Vec<_>>();
    let labels = agents
        .iter()
        .map(|agent| agent.label.clone())
        .collect::<Vec<_>>();
    let selected = source
        .map(|profile| profile.agent_id.as_str().to_string())
        .filter(|id| options.iter().any(|option| option == id))
        .or_else(|| options.first().cloned())
        .unwrap_or_default();
    let pinned = options
        .iter()
        .find(|option| *option == &selected)
        .and_then(|id| vibex_core::AgentId::parse(id.clone()).ok())
        .and_then(|agent_id| fixed_endpoint_for(&agent_id));
    let name = source
        .map(|profile| format!("{} {}", profile.display_name, strings.config_copy_suffix()))
        .unwrap_or_default();
    let models = source
        .map(|profile| {
            profile
                .configured_models
                .iter()
                .map(|model| model.id.clone())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let fields = vec![
        FormField::choice_owned(
            "agent",
            strings.config_field_agent(),
            options,
            labels,
            &selected,
        ),
        FormField::text("name", strings.config_form_name(), name),
        FormField::text(
            "account_alias",
            strings.config_field_account(),
            source
                .and_then(|profile| profile.account_alias.clone())
                .unwrap_or_default(),
        ),
        base_url_field(
            pinned
                .clone()
                .or_else(|| detail.and_then(|detail| detail.base_url.clone()))
                .unwrap_or_default(),
            pinned.as_deref(),
            strings,
        ),
        FormField::text(
            "default_model",
            strings.config_field_model(),
            source
                .and_then(|profile| profile.default_model.clone())
                .unwrap_or_default(),
        ),
        FormField::multiline("models", strings.config_field_models(), models)
            .hint(strings.config_hint_models()),
    ];
    ConfigForm::new(
        FormPurpose::NewProviderProfile {
            source: detail.map(|detail| Box::new(detail.clone())),
        },
        strings.config_form_new_provider(),
        fields,
    )
}

/// The per-Agent switches for one MCP server.
pub fn mcp_matrix_form(
    server: &vibex_core::McpServer,
    agents: &[vibex_core::AgentSnapshotEntry],
    strings: Strings,
) -> ConfigForm {
    let fields = agents
        .iter()
        .map(|agent| {
            let enabled = server
                .agent_matrix
                .iter()
                .find(|entry| entry.agent_id == agent.id)
                .is_some_and(|entry| entry.enabled);
            FormField::toggle(
                format!("agent:{}", agent.id.as_str()),
                agent.label.clone(),
                enabled,
            )
        })
        .collect();
    ConfigForm::new(
        FormPurpose::McpMatrix {
            server_id: server.id.as_str().to_string(),
        },
        format!(
            "{} · {}",
            strings.config_action_matrix(),
            server.display_name
        ),
        fields,
    )
}

/// The per-Agent switches for one Skill.
pub fn skill_matrix_form(
    skill: &vibex_core::Skill,
    agents: &[vibex_core::AgentSnapshotEntry],
    strings: Strings,
) -> ConfigForm {
    let fields = agents
        .iter()
        .map(|agent| {
            let enabled = skill
                .agent_matrix
                .iter()
                .find(|entry| entry.agent_id == agent.id)
                .is_some_and(|entry| entry.enabled);
            FormField::toggle(
                format!("agent:{}", agent.id.as_str()),
                agent.label.clone(),
                enabled,
            )
        })
        .collect();
    ConfigForm::new(
        FormPurpose::SkillMatrix {
            skill_id: skill.id.as_str().to_string(),
        },
        format!(
            "{} · {}",
            strings.config_action_matrix(),
            skill.display_name
        ),
        fields,
    )
}

/// One registry entry as the record installing it would create.
///
/// The registry publishes a launcher, not a record: the scope, the status and
/// the tag that says where it came from are this client's decisions, and both
/// "install" and "check it first" have to make them the same way or a check
/// would answer about a different server than the install writes.
pub fn mcp_market_candidate(
    entry: &vibex_core::McpMarketEntry,
) -> vibex_core::McpServerCreateRequest {
    vibex_core::McpServerCreateRequest {
        display_name: entry.name.clone(),
        transport_kind: entry.transport,
        status: vibex_core::McpServerStatus::Enabled,
        scope_kind: vibex_core::McpServerScopeKind::User,
        project_id: None,
        workspace_id: None,
        command: entry.command.clone(),
        args: entry.args.clone(),
        env: entry
            .env
            .iter()
            .map(|requirement| vibex_core::McpServerEnvEntry {
                name: requirement.name.clone(),
                value: requirement.default_value.clone().unwrap_or_default(),
            })
            .collect(),
        url: entry.url.clone(),
        headers: Vec::new(),
        description: entry.description.clone(),
        tags: vec!["market".to_string()],
        secret_references: Vec::new(),
        provider_matrix: Vec::new(),
    }
}

/// The launcher a candidate would run or connect to, as one line.
pub fn mcp_candidate_command_line(candidate: &vibex_core::McpServerCreateRequest) -> String {
    match candidate.transport_kind {
        vibex_core::McpServerTransportKind::Stdio => {
            let mut line = candidate.command.clone().unwrap_or_default();
            if !candidate.args.is_empty() {
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(&candidate.args.join(" "));
            }
            line
        }
        _ => candidate.url.clone().unwrap_or_default(),
    }
}

/// The form that turns one registry entry into a server.
///
/// The registry publishes a launcher and the variables it needs; what it cannot
/// publish is the reader's own token or which Agents should receive the server.
/// Both are asked for here, and the command that will be saved is read back
/// above them: a reader agreeing to an entry is agreeing to what it runs, and
/// that is the one thing the name of the entry does not say.
pub fn mcp_market_install_form(
    entry: &vibex_core::McpMarketEntry,
    agents: &[vibex_core::AgentSnapshotEntry],
    strings: Strings,
) -> ConfigForm {
    let candidate = mcp_market_candidate(entry);
    let mut fields = Vec::new();
    for requirement in &entry.env {
        let mut field = FormField::text(
            format!("env:{}", requirement.name),
            requirement.name.clone(),
            requirement.default_value.clone().unwrap_or_default(),
        );
        let hint = match (
            requirement
                .description
                .clone()
                .or_else(|| requirement.placeholder.clone()),
            requirement.required,
        ) {
            (Some(description), true) => {
                format!("{description} · {}", strings.config_form_required())
            }
            (Some(description), false) => description,
            (None, true) => strings.config_form_required().to_string(),
            (None, false) => String::new(),
        };
        if !hint.is_empty() {
            field = field.hint(hint);
        }
        if requirement.secret {
            field = field.secret();
        }
        fields.push(field);
    }
    // Every added Agent starts switched on, which is what the desktop does and
    // what "install this" almost always means; the reader who wants it narrower
    // turns one off here rather than discovering it afterwards.
    for agent in agents {
        fields.push(FormField::toggle(
            format!("agent:{}", agent.id.as_str()),
            agent.label.clone(),
            true,
        ));
    }
    ConfigForm::new(
        FormPurpose::McpMarketInstall {
            entry_id: entry.id.clone(),
            candidate: Box::new(candidate.clone()),
        },
        format!("{} · {}", strings.config_action_install(), entry.name),
        fields,
    )
    .preview(mcp_candidate_command_line(&candidate))
}

/// One Model as the profile editor lists it.
///
/// The list is the union of what the profile declares and what the endpoint last
/// answered with, because those are two different things: a catalogue entry the
/// profile does not offer is a Model the reader might want to turn on, and a
/// declared Model the catalogue has forgotten is one they may still be using.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    pub id: String,
    pub label: String,
    /// Whether the profile offers it to the Agent.
    pub enabled: bool,
    /// Whether the profile declares it at all. A Model that only the catalogue
    /// knows is offered but not yet part of the profile.
    pub declared: bool,
    /// Short facts for the row's second line.
    pub detail: Vec<(String, String)>,
}

impl ModelRow {
    /// What the row is called, in the reader's language.
    pub fn chip(&self, strings: Strings) -> (&'static str, ConfigTone) {
        match (self.declared, self.enabled) {
            (false, _) => (strings.config_state_available(), ConfigTone::Muted),
            (true, true) => (strings.config_state_enabled(), ConfigTone::Ok),
            (true, false) => (strings.config_state_disabled(), ConfigTone::Muted),
        }
    }
}

/// The Models one profile offers, declared first and catalogue-only after.
pub fn model_rows(
    profile: &vibex_core::ProviderProfileSummary,
    catalogue: &[vibex_core::ProviderConfiguredModel],
    strings: Strings,
) -> Vec<ModelRow> {
    let mut rows = Vec::new();
    for model in &profile.configured_models {
        let known = catalogue.iter().find(|entry| entry.id == model.id);
        rows.push(ModelRow {
            id: model.id.clone(),
            label: model
                .display_name
                .clone()
                .or_else(|| known.and_then(|entry| entry.display_name.clone()))
                .unwrap_or_else(|| model.id.clone()),
            enabled: model.enabled,
            declared: true,
            detail: model_detail(model.wire_api, &model.capabilities, strings),
        });
    }
    for entry in catalogue {
        if profile
            .configured_models
            .iter()
            .any(|model| model.id == entry.id)
        {
            continue;
        }
        rows.push(ModelRow {
            id: entry.id.clone(),
            label: entry
                .display_name
                .clone()
                .unwrap_or_else(|| entry.id.clone()),
            enabled: false,
            declared: false,
            detail: model_detail(entry.wire_api, &entry.capabilities, strings),
        });
    }
    rows
}

/// What a Model's second line says: the wire protocol it is called over and the
/// limits it declares, when it declares any.
fn model_detail(
    wire_api: Option<vibex_core::ProviderModelWireApi>,
    capabilities: &vibex_core::ProviderModelCapabilities,
    strings: Strings,
) -> Vec<(String, String)> {
    let mut detail = Vec::new();
    if let Some(wire_api) = wire_api {
        detail.push((
            strings.config_field_wire_api().to_string(),
            wire_api_label(wire_api).to_string(),
        ));
    }
    if let Some(tokens) = capabilities.context_tokens {
        detail.push((
            strings.config_field_context_tokens().to_string(),
            tokens.to_string(),
        ));
    }
    if let Some(tokens) = capabilities.output_tokens {
        detail.push((
            strings.config_field_output_tokens().to_string(),
            tokens.to_string(),
        ));
    }
    detail
}

/// One wire protocol as a form value, and back.
///
/// The value is the registry's own spelling so a form the reader opened and
/// saved without touching the field writes back what it read.
pub fn wire_api_from_value(value: &str) -> Option<vibex_core::ProviderModelWireApi> {
    use vibex_core::ProviderModelWireApi as Api;
    Some(match value {
        "openai_responses" => Api::OpenaiResponses,
        "openai_chat_completions" => Api::OpenaiChatCompletions,
        "anthropic_messages" => Api::AnthropicMessages,
        "mistral_conversations" => Api::MistralConversations,
        "google_generative_ai" => Api::GoogleGenerativeAi,
        "google_vertex" => Api::GoogleVertex,
        "aws_bedrock_converse" => Api::AwsBedrockConverse,
        _ => return None,
    })
}

/// Every protocol a Model may be called over, with "leave it to the Agent"
/// first: a profile that names no protocol is the common case.
pub fn wire_api_choices() -> (Vec<String>, Vec<String>) {
    let mut options = vec![INHERIT.to_string()];
    let mut labels = vec!["inherit".to_string()];
    for api in [
        vibex_core::ProviderModelWireApi::OpenaiResponses,
        vibex_core::ProviderModelWireApi::OpenaiChatCompletions,
        vibex_core::ProviderModelWireApi::AnthropicMessages,
        vibex_core::ProviderModelWireApi::MistralConversations,
        vibex_core::ProviderModelWireApi::GoogleGenerativeAi,
        vibex_core::ProviderModelWireApi::GoogleVertex,
        vibex_core::ProviderModelWireApi::AwsBedrockConverse,
    ] {
        options.push(wire_api_label(api).to_string());
        labels.push(wire_api_label(api).to_string());
    }
    (options, labels)
}

/// The value a choice field carries when the answer is "whatever the Agent
/// does", which is not the same as an empty answer.
pub const INHERIT: &str = "inherit";

/// The settings of one Model inside a profile.
///
/// A Model is not a record, so neither is its editor: the purpose names the
/// profile and the Model together, and the save writes the profile's whole Model
/// list back with this one replaced.
pub fn provider_model_form(
    agent_id: &str,
    profile_id: &str,
    model: &vibex_core::ProviderConfiguredModel,
    strings: Strings,
) -> ConfigForm {
    let (wire_options, wire_labels) = wire_api_choices();
    let wire = model
        .wire_api
        .map(wire_api_label)
        .unwrap_or(INHERIT)
        .to_string();
    let reasoning = match model.capabilities.reasoning {
        None => INHERIT,
        Some(true) => "reasoning",
        Some(false) => "non_reasoning",
    };
    let efforts = model
        .capabilities
        .reasoning_efforts
        .as_ref()
        .map(|efforts| {
            efforts
                .declared_levels()
                .map(|(level, wire)| match wire {
                    Some(wire) if wire != level.as_str() => format!("{level}={wire}"),
                    _ => level.as_str().to_string(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let fields = vec![
        FormField::text(
            "display_name",
            strings.config_form_name(),
            model.display_name.clone().unwrap_or_default(),
        ),
        FormField::choice_owned(
            "wire_api",
            strings.config_field_wire_api(),
            wire_options,
            wire_labels,
            &wire,
        ),
        FormField::choice(
            "reasoning",
            strings.config_field_reasoning(),
            vec![INHERIT, "reasoning", "non_reasoning"],
            vec![
                strings.config_hint_inherit(),
                strings.config_choice_reasoning(),
                strings.config_choice_non_reasoning(),
            ],
            reasoning,
        ),
        FormField::text(
            "context_tokens",
            strings.config_field_context_tokens(),
            model
                .capabilities
                .context_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_default(),
        ),
        FormField::text(
            "output_tokens",
            strings.config_field_output_tokens(),
            model
                .capabilities
                .output_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_default(),
        ),
        FormField::text(
            "reasoning_efforts",
            strings.config_field_reasoning_efforts(),
            efforts,
        )
        .hint(strings.config_hint_reasoning_efforts()),
    ];
    ConfigForm::new(
        FormPurpose::ProviderModel {
            agent_id: agent_id.to_string(),
            profile_id: profile_id.to_string(),
            model_id: model.id.clone(),
        },
        format!("{} · {}", strings.config_form_edit_model(), model.id),
        fields,
    )
}

/// The options the form's three fields describe, as a draft.
///
/// The header field is the one place a value may be a variable, so it is the one
/// place the parse can fail on something the reader typed: a `{{name}}` that is
/// not a variable the runtime knows is refused rather than sent as a header
/// whose value is the literal text of a placeholder.
pub fn provider_options_draft(
    form: &ConfigForm,
    strings: Strings,
) -> Result<ProviderOptionsDraft, &'static str> {
    let mut draft = ProviderOptionsDraft {
        website: form.value("website").unwrap_or_default(),
        ..ProviderOptionsDraft::default()
    };
    for line in form.value("headers").unwrap_or_default().lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(strings.config_form_error_header());
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() {
            return Err(strings.config_form_error_header());
        }
        let value = match value
            .strip_prefix("{{")
            .and_then(|rest| rest.strip_suffix("}}"))
        {
            Some(id) => {
                let id = id.trim();
                let Some(variable) = vibex_core::ModelProviderHeaderVariable::ALL
                    .into_iter()
                    .find(|variable| variable.id() == id)
                else {
                    return Err(strings.config_form_error_header_variable());
                };
                ProviderHeaderValue::Variable(variable)
            }
            None => ProviderHeaderValue::Literal(value.to_string()),
        };
        if draft
            .headers
            .iter()
            .any(|(existing, _)| existing.eq_ignore_ascii_case(name))
        {
            return Err(strings.config_form_error_header_duplicate());
        }
        draft.headers.push((name.to_string(), value));
    }
    for line in form.value("protocol_urls").unwrap_or_default().lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((wire, url)) = line.split_once('=') else {
            return Err(strings.config_form_error_protocol_url());
        };
        let Some(wire) = wire_api_from_value(wire.trim()) else {
            return Err(strings.config_form_error_protocol_url());
        };
        draft.protocol_urls.push((wire, url.trim().to_string()));
    }
    Ok(draft)
}

/// What one wire protocol is called on screen.
pub const fn wire_api_label(wire_api: vibex_core::ProviderModelWireApi) -> &'static str {
    use vibex_core::ProviderModelWireApi as Api;
    match wire_api {
        Api::OpenaiResponses => "openai_responses",
        Api::OpenaiChatCompletions => "openai_chat_completions",
        Api::AnthropicMessages => "anthropic_messages",
        Api::MistralConversations => "mistral_conversations",
        Api::GoogleGenerativeAi => "google_generative_ai",
        Api::GoogleVertex => "google_vertex",
        Api::AwsBedrockConverse => "aws_bedrock_converse",
    }
}

/// What other CLI tools hold, as the import picker draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeImportRow {
    pub item_id: String,
    pub label: String,
    pub source: String,
    pub agent: String,
    pub chip: String,
    pub tone: ConfigTone,
    /// Whether this one can be imported at all.
    pub importable: bool,
    pub detail: Vec<(String, String)>,
}

/// The imports one preview offers, in the order it listed them.
pub fn native_import_rows(
    preview: Option<&vibex_core::ProviderNativeImportPreview>,
    strings: Strings,
) -> Vec<NativeImportRow> {
    let Some(preview) = preview else {
        return Vec::new();
    };
    preview
        .items
        .iter()
        .map(|item| {
            let (chip, tone, importable) = match item.status {
                vibex_core::ProviderNativeImportItemStatus::Importable => {
                    (strings.config_state_available(), ConfigTone::Ok, true)
                }
                vibex_core::ProviderNativeImportItemStatus::NeedsSecretSetup => {
                    (strings.config_state_needs_secret(), ConfigTone::Warn, true)
                }
                vibex_core::ProviderNativeImportItemStatus::Partial => {
                    (strings.config_state_partial(), ConfigTone::Warn, true)
                }
                vibex_core::ProviderNativeImportItemStatus::BlockedByParseError => {
                    (strings.config_state_blocked(), ConfigTone::Bad, false)
                }
            };
            let mut detail = Vec::new();
            if let Some(base_url) = &item.base_url {
                detail.push((strings.config_field_url().to_string(), base_url.clone()));
            }
            if let Some(model) = &item.default_model {
                detail.push((strings.config_field_model().to_string(), model.clone()));
            }
            for diagnostic in &item.diagnostics {
                detail.push((
                    strings.config_field_description().to_string(),
                    diagnostic.message.clone(),
                ));
            }
            NativeImportRow {
                item_id: item.import_item_id.as_str().to_string(),
                label: item.display_name.clone(),
                source: import_source_label(item.source, strings).to_string(),
                agent: item
                    .agent_id
                    .as_ref()
                    .map(|agent| agent.as_str().to_string())
                    .unwrap_or_default(),
                chip: chip.to_string(),
                tone,
                importable,
                detail,
            }
        })
        .collect()
}

/// What a native source is called on screen.
pub const fn import_source_label(
    source: vibex_core::ProviderNativeImportSource,
    strings: Strings,
) -> &'static str {
    use vibex_core::ProviderNativeImportSource as Source;
    match source {
        Source::Codex => strings.config_import_source_codex(),
        Source::Claude => strings.config_import_source_claude(),
        Source::CcSwitch => strings.config_import_source_cc_switch(),
    }
}

/// The sources the import picker asks about.
///
/// The same three the desktop asks for: the two CLI tools whose own settings a
/// reader is most likely to have, and the switch tool that holds both.
pub fn native_import_sources() -> Vec<vibex_core::ProviderNativeImportSource> {
    use vibex_core::ProviderNativeImportSource as Source;
    vec![Source::Codex, Source::Claude, Source::CcSwitch]
}

/// One line of the Agent account panel: a way in, or something to do with the
/// account the Agent holds.
///
/// They are one list because they are one decision — "what can I do about this
/// account" — and a second key for the verbs would be a second thing to learn
/// for the same question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAuthRow {
    /// What `Enter` runs. `None` for a method that cannot be used as reported.
    pub action: Option<AgentAuthAction>,
    pub label: String,
    pub chip: String,
    pub tone: ConfigTone,
    pub detail: Vec<(String, String)>,
    pub hint: Option<String>,
}

/// What one line of the account panel does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentAuthAction {
    /// Sign in with this method.
    Method {
        method_id: String,
        kind: vibex_core::AgentAuthMethodKind,
        /// The variables the method collects, in the order it declares them.
        environment: Vec<vibex_core::AgentAuthEnvironmentVariable>,
    },
    RefreshMethods,
    Verify,
    RefreshModels,
    SignOut,
    /// Call off the sign-in the authority is still running.
    Cancel,
    /// Open the page the method named for getting credentials.
    Credentials {
        url: String,
    },
}

/// One registry Skill as the preview draws it: the manifest's own words, the
/// bundle beside it, and what the registry refused to send.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SkillPreviewLines {
    /// The banner, when the document is one the runtime would refuse to write.
    pub banner: Option<String>,
    /// The bundle's own summary: how many files and how many bytes.
    pub bundle: Option<String>,
    /// Each bundled file, in write order.
    pub files: Vec<String>,
    /// Files the bundle carried that will not be written, with the reason.
    pub skipped: Vec<String>,
    /// The instructions, one line each.
    pub body: Vec<String>,
}

impl SkillPreviewLines {
    /// Everything the reader scrolls through, in the order it is drawn.
    pub fn len(&self) -> usize {
        self.files.len() + self.skipped.len() + self.body.len() + 4
    }

    pub fn is_empty(&self) -> bool {
        self.len() <= 4
    }
}

pub fn skill_preview_lines(
    document: Option<&vibex_core::SkillMarketDocument>,
    strings: Strings,
) -> SkillPreviewLines {
    let Some(document) = document else {
        return SkillPreviewLines::default();
    };
    SkillPreviewLines {
        banner: document.too_large.then(|| {
            strings
                .config_preview_too_large()
                .replace("{bytes}", &document.bytes.to_string())
        }),
        bundle: Some(
            strings
                .config_preview_bundle()
                .replace("{files}", &document.files.len().to_string())
                .replace("{bytes}", &document.bundle_bytes.to_string()),
        ),
        files: document
            .files
            .iter()
            .map(|file| format!("{} · {} bytes", file.path, file.bytes))
            .collect(),
        skipped: document.skipped_files.clone(),
        body: document.body.lines().map(ToString::to_string).collect(),
    }
}

/// An Agent whose install the reader is watching, and the tick that watches it.
///
/// The desktop polls every 500ms; this client's tick is 200ms, so it asks every
/// third one. The count is kept here rather than in the app so the clock stops
/// when the watch does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallWatch {
    pub agent_id: vibex_core::AgentId,
    pub label: String,
    /// What the authority last said it was doing.
    pub phase: Option<vibex_core::AgentManagedInstallStatus>,
    /// What the authority said when it gave up.
    pub error: Option<String>,
    /// The version the authority is moving to, when it named one. An upgrade
    /// that says what it is upgrading to is a different wait from one that does
    /// not.
    pub version: Option<String>,
    ticks: u8,
}

/// How many ticks between two questions about an install.
pub const INSTALL_POLL_TICKS: u8 = 3;

impl InstallWatch {
    pub fn new(agent_id: vibex_core::AgentId, label: String) -> Self {
        Self {
            agent_id,
            label,
            phase: None,
            error: None,
            version: None,
            ticks: 0,
        }
    }

    /// Whether the authority is still working on this Agent.
    pub fn running(&self) -> bool {
        matches!(
            self.phase,
            Some(
                vibex_core::AgentManagedInstallStatus::Installing
                    | vibex_core::AgentManagedInstallStatus::Upgrading
                    | vibex_core::AgentManagedInstallStatus::Uninstalling
            )
        )
    }

    /// What the phase is called on screen.
    pub fn phase_label(&self, strings: Strings) -> &'static str {
        match self.phase {
            Some(vibex_core::AgentManagedInstallStatus::Upgrading) => {
                strings.config_install_phase_upgrading()
            }
            Some(vibex_core::AgentManagedInstallStatus::Uninstalling) => {
                strings.config_install_phase_uninstalling()
            }
            _ => strings.config_install_phase_installing(),
        }
    }

    /// Count one tick towards asking again. Answers whether the question is due.
    pub fn poll(&mut self) -> bool {
        if self.ticks > 0 {
            self.ticks -= 1;
            return false;
        }
        self.ticks = INSTALL_POLL_TICKS;
        true
    }
}

/// Which registry Skill the preview is on, and how far down it has been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPreviewView {
    pub entry_id: String,
    pub title: String,
    /// Index of the first line drawn, so the body scrolls rather than stopping
    /// at the height of the window.
    pub scroll: usize,
    pub loading: bool,
    pub notice: Option<(String, ConfigTone)>,
}

/// The form a custom ACP Agent is registered with.
///
/// It is the one record in this centre that has nothing behind it: the Agent
/// does not exist until the authority writes it, so every field is the
/// reader's and the validation is the desktop's own — a lowercase kebab-case
/// id, a display name and a command that are not empty, and an environment in
/// `KEY=value` lines.
pub fn custom_agent_form(strings: Strings) -> ConfigForm {
    ConfigForm::new(
        FormPurpose::NewCustomAgent,
        strings.config_form_new_agent(),
        vec![
            FormField::text("agent_id", strings.config_field_agent_id(), String::new())
                .hint(strings.config_hint_agent_id()),
            FormField::text("label", strings.config_field_display_name(), String::new()),
            FormField::text(
                "description",
                strings.config_field_description(),
                String::new(),
            ),
            FormField::text("command", strings.config_field_command(), String::new()),
            FormField::text("args", strings.config_field_arguments(), String::new())
                .hint(strings.config_hint_arguments()),
            FormField::multiline("env", strings.config_field_env_pairs(), String::new())
                .hint(strings.config_hint_env_pairs()),
        ],
    )
}

/// The request a custom Agent form describes, or the reason it cannot be made.
pub fn custom_agent_request(
    form: &ConfigForm,
    strings: Strings,
) -> Result<vibex_core::CustomAgentCreateRequest, &'static str> {
    let id = form.value("agent_id").unwrap_or_default();
    let label = form.value("label").unwrap_or_default();
    let command = form.value("command").unwrap_or_default();
    // The id is the Agent's identity: everything that names it later — a
    // profile, a session, a config file — is keyed by it, so it is checked
    // here rather than left for the authority to refuse.
    let Ok(agent_id) = vibex_core::AgentId::parse(id) else {
        return Err(strings.config_form_error_agent_id());
    };
    if label.is_empty() || command.is_empty() {
        return Err(strings.config_form_error_agent_required());
    }
    let mut env = std::collections::BTreeMap::new();
    for line in form.value("env").unwrap_or_default().lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(strings.config_form_error_env_pairs());
        };
        let key = key.trim();
        let named = key.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
        });
        if key.is_empty() || !named {
            return Err(strings.config_form_error_env_name());
        }
        env.insert(key.to_string(), value.trim().to_string());
    }
    let description = form.value("description").unwrap_or_default();
    Ok(vibex_core::CustomAgentCreateRequest {
        agent_id,
        label,
        description: (!description.is_empty()).then_some(description),
        command: vibex_core::AgentCommandConfig {
            command,
            // Arguments are what the reader typed, split the way a shell would
            // read them; a command that needs quoting is a command this form
            // cannot express, and the reader can put it in a wrapper script.
            args: form
                .value("args")
                .unwrap_or_default()
                .split_whitespace()
                .map(ToString::to_string)
                .collect(),
        },
        env,
    })
}

/// The form a Skill install asks for.
///
/// A Skill is stored once and enabled per Agent, so the install is a question
/// about who receives it rather than a press: the desktop asks the same
/// question, and an install that quietly enabled it for nobody would be an
/// install the reader cannot find afterwards.
pub fn skill_market_install_form(
    entry: &vibex_core::SkillMarketEntry,
    agents: &[vibex_core::AgentSnapshotEntry],
    strings: Strings,
) -> ConfigForm {
    let mut fields = Vec::new();
    for agent in agents {
        fields.push(FormField::toggle(
            format!("agent:{}", agent.id.as_str()),
            agent.label.clone(),
            true,
        ));
    }
    ConfigForm::new(
        FormPurpose::SkillMarketInstall {
            entry_id: entry.id.clone(),
            slug: entry.slug.clone(),
            owner_handle: entry.owner_handle.clone(),
            version: entry.version.clone(),
        },
        format!("{} · {}", strings.config_action_install(), entry.name),
        fields,
    )
}

/// The form one sign-in method asks for.
///
/// The runtime never hands a stored value back, so the fields open empty and
/// say what is already set instead: a blank field preserves what is there,
/// which is why taking one away is a switch of its own rather than an empty
/// answer.
pub fn agent_auth_method_form(
    agent_id: &str,
    method_id: &str,
    method_name: &str,
    environment: &[vibex_core::AgentAuthEnvironmentVariable],
    strings: Strings,
) -> ConfigForm {
    let mut fields = Vec::new();
    for variable in environment {
        let label = variable
            .label
            .clone()
            .unwrap_or_else(|| variable.name.clone());
        let state = if variable.configured {
            strings.config_auth_configured()
        } else if variable.optional {
            strings.config_auth_optional()
        } else {
            strings.config_form_required()
        };
        let mut field = FormField::text(format!("value:{}", variable.name), label, String::new())
            .hint(state.to_string());
        if variable.secret {
            field = field.secret();
        }
        fields.push(field);
        // Only a variable the profile already owns can be taken away; a switch
        // on one that holds nothing would be a switch that does nothing.
        if variable.configured {
            fields.push(FormField::toggle(
                format!("clear:{}", variable.name),
                format!(
                    "{} · {}",
                    strings.config_action_clear_secret(),
                    variable.name
                ),
                false,
            ));
        }
    }
    ConfigForm::new(
        FormPurpose::AgentAuthMethod {
            agent_id: agent_id.to_string(),
            method_id: method_id.to_string(),
        },
        format!("{} · {}", strings.config_action_sign_in(), method_name),
        fields,
    )
}

/// The values one sign-in form collected.
///
/// A field left blank is `None` rather than an empty string: the runtime reads
/// that as "keep what is stored", and an empty string as a value the reader
/// meant to be empty.
pub fn agent_auth_method_values(
    form: &ConfigForm,
    environment: &[vibex_core::AgentAuthEnvironmentVariable],
    strings: Strings,
) -> Result<Vec<vibex_core::AgentAuthEnvironmentValue>, &'static str> {
    let mut values = Vec::new();
    for variable in environment {
        let typed = form.optional(&format!("value:{}", variable.name));
        let clear = form
            .value(&format!("clear:{}", variable.name))
            .is_some_and(|value| value == "on");
        if clear {
            values.push(vibex_core::AgentAuthEnvironmentValue {
                name: variable.name.clone(),
                value: None,
                secret: variable.secret,
                optional: variable.optional,
                clear: true,
            });
            continue;
        }
        if typed.is_none() && !variable.optional && !variable.configured {
            // A required variable with nothing stored and nothing typed is the
            // one thing the authority would refuse, so it is refused here with
            // the field named rather than after a round trip.
            return Err(strings.config_auth_missing_value());
        }
        values.push(vibex_core::AgentAuthEnvironmentValue {
            name: variable.name.clone(),
            value: typed,
            secret: variable.secret,
            optional: variable.optional,
            clear: false,
        });
    }
    Ok(values)
}

/// The account panel's lines.
pub fn agent_auth_rows(
    catalog: Option<&vibex_core::AgentAuthCatalog>,
    strings: Strings,
) -> Vec<AgentAuthRow> {
    agent_auth_rows_with_sign_in(catalog, false, strings)
}

/// The account panel's lines, knowing whether a sign-in is still running.
///
/// Kept as a second entry point rather than a field on the catalogue because the
/// operation is the client's to know: the catalogue is a read the authority
/// answered, and whether a sign-in it started is still open is what this client
/// is holding, not what the Agent advertised.
pub fn agent_auth_rows_with_sign_in(
    catalog: Option<&vibex_core::AgentAuthCatalog>,
    pending_sign_in: bool,
    strings: Strings,
) -> Vec<AgentAuthRow> {
    let Some(catalog) = catalog else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for method in &catalog.methods {
        // A method that needs a provider profile is one this client cannot
        // finish: the profile is chosen in the Providers area, so the row says
        // so rather than opening a form whose answer the authority refuses.
        let needs_profile =
            method.effect == vibex_core::AgentAuthMethodEffect::RequiresProviderProfile;
        // One shared rule for both clients: a terminal method is always
        // drivable (the client attaches to the PTY), while an Agent-managed one
        // needs either an advertised logout — a credential store the Agent can
        // rewrite — or a verified browser flow for this exact Agent and method.
        // The latter is what keeps Cursor, Grok's `grok.com`, Gemini's
        // `oauth-personal`, Cline, CodeBuddy and Antigravity's OAuth methods
        // offered: they start a browser sign-in but advertise no logout.
        let drivable = method.is_client_drivable(&catalog.agent_id, catalog.supports_logout);
        let (chip, tone) = if needs_profile {
            (strings.config_auth_needs_profile(), ConfigTone::Muted)
        } else if !drivable {
            (strings.config_auth_external(), ConfigTone::Muted)
        } else {
            match method.kind {
                vibex_core::AgentAuthMethodKind::Agent => {
                    (strings.config_auth_method_agent(), ConfigTone::Ok)
                }
                vibex_core::AgentAuthMethodKind::Terminal => {
                    (strings.config_auth_method_terminal(), ConfigTone::Ok)
                }
                vibex_core::AgentAuthMethodKind::Environment => {
                    (strings.config_auth_method_environment(), ConfigTone::Muted)
                }
            }
        };
        let mut detail = Vec::new();
        if let Some(description) = &method.description {
            detail.push((String::new(), description.clone()));
        }
        for variable in &method.environment {
            let label = variable
                .label
                .clone()
                .unwrap_or_else(|| variable.name.clone());
            let state = if variable.configured {
                strings.config_auth_configured()
            } else if variable.optional {
                strings.config_auth_optional()
            } else {
                strings.config_form_required()
            };
            detail.push((label, state.to_string()));
        }
        // A method that names where its credentials come from gets a line for
        // that page: a reader who does not have a key yet is one press from the
        // place that issues one.
        if let Some(url) = method.credential_link.clone() {
            rows.push(AgentAuthRow {
                action: Some(AgentAuthAction::Credentials { url }),
                label: strings.config_auth_action_credentials().to_string(),
                chip: String::new(),
                tone: ConfigTone::Muted,
                detail: vec![(String::new(), method.name.clone())],
                hint: None,
            });
        }
        rows.push(AgentAuthRow {
            action: (drivable && !needs_profile).then(|| AgentAuthAction::Method {
                method_id: method.id.clone(),
                kind: method.kind,
                environment: method.environment.clone(),
            }),
            label: method.name.clone(),
            chip: chip.to_string(),
            tone,
            detail,
            hint: method.credential_link.clone(),
        });
    }
    // A sign-in the authority is still running is a process: it opened a
    // browser, or a terminal the client may have closed, and it is holding a
    // context revision. Offering to call it off is the only way the reader can
    // stop waiting on one that went nowhere.
    if pending_sign_in {
        rows.push(AgentAuthRow {
            action: Some(AgentAuthAction::Cancel),
            label: strings.config_auth_action_cancel().to_string(),
            chip: strings.config_auth_waiting_eyebrow().to_string(),
            tone: ConfigTone::Warn,
            detail: Vec::new(),
            hint: None,
        });
    }
    rows.push(AgentAuthRow {
        action: Some(AgentAuthAction::RefreshMethods),
        label: strings.config_auth_action_refresh().to_string(),
        chip: String::new(),
        tone: ConfigTone::Muted,
        detail: Vec::new(),
        hint: None,
    });
    rows.push(AgentAuthRow {
        action: Some(AgentAuthAction::Verify),
        label: strings.config_auth_action_verify().to_string(),
        chip: String::new(),
        tone: ConfigTone::Muted,
        detail: Vec::new(),
        hint: None,
    });
    // The verbs are offered only where they mean something: refreshing a model
    // catalogue needs an account, and signing out needs a runtime that can.
    if catalog.status == vibex_core::AgentAuthStatus::Authenticated {
        rows.push(AgentAuthRow {
            action: Some(AgentAuthAction::RefreshModels),
            label: strings.config_auth_action_models().to_string(),
            chip: String::new(),
            tone: ConfigTone::Muted,
            detail: Vec::new(),
            hint: None,
        });
    }
    if catalog.supports_logout {
        rows.push(AgentAuthRow {
            action: Some(AgentAuthAction::SignOut),
            label: strings.config_auth_action_sign_out().to_string(),
            chip: String::new(),
            tone: ConfigTone::Warn,
            detail: Vec::new(),
            hint: None,
        });
    }
    rows
}

/// What the account panel says about the account itself.
pub fn agent_auth_status(
    catalog: Option<&vibex_core::AgentAuthCatalog>,
    strings: Strings,
) -> (&'static str, ConfigTone) {
    match catalog.map(|catalog| catalog.status) {
        Some(vibex_core::AgentAuthStatus::Authenticated) => {
            (strings.config_auth_signed_in(), ConfigTone::Ok)
        }
        Some(vibex_core::AgentAuthStatus::AuthenticationRequired) => {
            (strings.config_auth_required(), ConfigTone::Warn)
        }
        _ => (strings.config_auth_unknown(), ConfigTone::Muted),
    }
}

/// Fold the account the authority holds into the catalogue it advertised.
///
/// The catalogue answers what an Agent *can* sign in with; the context answers
/// whether it *is* signed in. They are two reads, and a panel that only kept the
/// first said "Not checked" over an account the reader had just signed into —
/// and hid the verbs that need an account, because the panel gates them on the
/// status this returns. The context is the authority's own word, so it wins.
///
/// `None` while the catalogue belongs to another Agent, which is how a panel
/// left open across a navigation avoids wearing the previous Agent's methods.
pub fn merge_agent_auth_context(
    catalog: &mut vibex_core::AgentAuthCatalog,
    contexts: &[vibex_core::AgentAuthContext],
) {
    let mine = contexts
        .iter()
        .filter(|context| context.agent_id == catalog.agent_id);
    let mut authenticated = false;
    let mut required = false;
    for context in mine {
        match context.status {
            vibex_core::AgentAuthContextStatus::Authenticated => authenticated = true,
            vibex_core::AgentAuthContextStatus::AuthenticationRequired => required = true,
            // A verification in flight, or an account the Agent's own CLI
            // manages, is not evidence either way: leaving the catalogue alone
            // keeps what the last settled read said.
            vibex_core::AgentAuthContextStatus::Unverified
            | vibex_core::AgentAuthContextStatus::Verifying
            | vibex_core::AgentAuthContextStatus::Unavailable => {}
        }
    }
    catalog.status = if authenticated {
        vibex_core::AgentAuthStatus::Authenticated
    } else if required {
        vibex_core::AgentAuthStatus::AuthenticationRequired
    } else {
        vibex_core::AgentAuthStatus::Unknown
    };
}

/// Which Agent's account the panel is on, and what it last said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAuthView {
    pub agent_id: String,
    pub title: String,
    pub selected: usize,
    /// Whether the catalogue is still being read.
    pub loading: bool,
    /// Whether the sign-out has been asked about and is waiting for its answer.
    /// The impact is read out before the second press, because a sign-out that
    /// also ends the reader's sessions is not a decision to make blind.
    pub confirming_sign_out: bool,
    pub notice: Option<(String, ConfigTone)>,
    /// The line the verb that opened the panel was about, applied once the
    /// catalogue arrives — the lines are the catalogue's, so where they are is
    /// not known until it lands.
    pub want_line: AgentAuthLine,
    /// The sign-in the authority is still running, if one is.
    ///
    /// It is the client's own memory of a request it started: the answer that
    /// opened the browser or the terminal came back with an operation, and
    /// nothing else will say whether it is still open. Holding it here is what
    /// lets the panel offer to call it off.
    pub pending_sign_in: Option<vibex_core::AgentAuthenticationOperation>,
}

impl AgentAuthView {
    /// The lines to draw, with the one the panel asked for already selected.
    pub fn rows(
        &self,
        catalog: Option<&vibex_core::AgentAuthCatalog>,
        strings: Strings,
    ) -> Vec<AgentAuthRow> {
        agent_auth_rows_with_sign_in(catalog, self.pending_sign_in.is_some(), strings)
    }
}

/// The line a verb asked the account panel to land on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAuthLine {
    /// The row's `Auth`: the first line, which is the first way in.
    First,
    /// The row's `Logout`: the sign-out line, wherever it ended up.
    SignOut,
}

impl AgentAuthLine {
    /// The index to move the cursor to, once the lines are known.
    pub fn index(self, rows: &[AgentAuthRow]) -> usize {
        match self {
            Self::First => 0,
            Self::SignOut => rows
                .iter()
                .position(|row| row.action == Some(AgentAuthAction::SignOut))
                .unwrap_or(0),
        }
    }
}

/// Where a native export writes, in the order the picker cycles them.
///
/// The first is the Agent the profile itself runs, which is the only answer
/// that needs a word rather than a name: every other entry is a product, and a
/// product is called what it is called in every language.
pub const fn native_export_sources() -> [vibex_core::ProviderNativeExportSource; 14] {
    use vibex_core::ProviderNativeExportSource as Source;
    [
        Source::AgentDefault,
        Source::Codex,
        Source::Claude,
        Source::Cursor,
        Source::Grok,
        Source::Gemini,
        Source::Kimi,
        Source::OpenCode,
        Source::QwenCode,
        Source::Cline,
        Source::CodeBuddy,
        Source::Copilot,
        Source::OpenClaw,
        Source::Hermes,
    ]
}

pub fn native_export_source_label(
    source: vibex_core::ProviderNativeExportSource,
    strings: Strings,
) -> &'static str {
    use vibex_core::ProviderNativeExportSource as Source;
    match source {
        Source::AgentDefault => strings.config_export_source_agent(),
        Source::Codex => "Codex",
        Source::Claude => "Claude",
        Source::Cursor => "Cursor",
        Source::Grok => "Grok Build",
        Source::Gemini => "Gemini",
        Source::Kimi => "Kimi",
        Source::OpenCode => "OpenCode",
        Source::QwenCode => "Qwen Code",
        Source::Cline => "Cline",
        Source::CodeBuddy => "CodeBuddy",
        Source::Copilot => "Copilot",
        Source::OpenClaw => "OpenClaw",
        Source::Hermes => "Hermes",
    }
}

/// What a native export writes, in the order the picker cycles them.
pub const fn native_export_modes() -> [vibex_core::ProviderNativeExportMode; 5] {
    use vibex_core::ProviderNativeExportMode as Mode;
    [
        Mode::ProviderProfile,
        Mode::Combined,
        Mode::Mcp,
        Mode::Skills,
        Mode::Prompts,
    ]
}

pub fn native_export_mode_label(
    mode: vibex_core::ProviderNativeExportMode,
    strings: Strings,
) -> &'static str {
    use vibex_core::ProviderNativeExportMode as Mode;
    match mode {
        Mode::ProviderProfile => strings.config_export_mode_profile(),
        Mode::Combined => strings.config_export_mode_combined(),
        Mode::Mcp => strings.config_export_mode_mcp(),
        Mode::Skills => strings.config_export_mode_skills(),
        Mode::Prompts => strings.config_export_mode_prompts(),
    }
}

/// The next entry of a cycle, given the list it walks.
pub fn cycle<T: PartialEq + Copy>(all: &[T], current: T) -> T {
    let index = all.iter().position(|entry| *entry == current).unwrap_or(0);
    all[(index + 1) % all.len()]
}

/// What one export writes and whether it landed, as the picker lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeExportFileRow {
    pub label: String,
    pub chip: String,
    pub tone: ConfigTone,
    /// The redacted difference the reader is agreeing to.
    pub diff: String,
}

/// The files one preview would write.
pub fn native_export_file_rows(
    preview: Option<&vibex_core::ProviderNativeExportPreview>,
    strings: Strings,
) -> Vec<NativeExportFileRow> {
    let Some(preview) = preview else {
        return Vec::new();
    };
    preview
        .files
        .iter()
        .map(|file| {
            use vibex_core::ProviderNativeExportFileStatus as Status;
            let (chip, tone) = match file.status {
                Status::Ready => (strings.config_state_available(), ConfigTone::Ok),
                Status::NoOp => (strings.config_export_unchanged(), ConfigTone::Muted),
                Status::Blocked => (strings.config_state_blocked(), ConfigTone::Bad),
                Status::Failed => (strings.config_state_error(), ConfigTone::Bad),
                Status::Applied => (strings.config_export_applied(), ConfigTone::Ok),
                Status::Restored => (strings.config_export_rolled_back(), ConfigTone::Muted),
            };
            NativeExportFileRow {
                label: file.target_path.clone(),
                chip: chip.to_string(),
                tone,
                diff: file.redacted_diff.clone(),
            }
        })
        .collect()
}

/// One line of the export history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeExportRecordRow {
    pub export_id: String,
    pub source: String,
    pub mode: String,
    pub status: String,
    /// Whether this one can still be rolled back.
    pub rollbackable: bool,
}

pub fn native_export_record_rows(
    history: &[vibex_core::ProviderNativeExportRecordSummary],
    strings: Strings,
) -> Vec<NativeExportRecordRow> {
    history
        .iter()
        .map(|record| NativeExportRecordRow {
            export_id: record.export_id.as_str().to_string(),
            source: native_export_source_label(record.source, strings).to_string(),
            mode: native_export_mode_label(record.mode, strings).to_string(),
            status: record.status.clone(),
            rollbackable: record.applied_at_ms.is_some() && record.rolled_back_at_ms.is_none(),
        })
        .collect()
}

/// Which export the picker is on, and what it last answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeExportView {
    pub agent_id: String,
    pub profile_id: String,
    pub title: String,
    pub source: vibex_core::ProviderNativeExportSource,
    pub mode: vibex_core::ProviderNativeExportMode,
    /// The preview the reader asked for, which is what Apply names.
    pub preview: Option<Box<vibex_core::ProviderNativeExportPreview>>,
    pub history: Vec<vibex_core::ProviderNativeExportRecordSummary>,
    /// Which history record the cursor is on.
    pub selected: usize,
    pub notice: Option<(String, ConfigTone)>,
}

/// Which import the picker is on, and what it last said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeImportView {
    pub selected: usize,
    /// Whether the scan that fills the list is still running.
    pub loading: bool,
    pub notice: Option<(String, ConfigTone)>,
}

impl Default for NativeImportView {
    fn default() -> Self {
        Self {
            selected: 0,
            loading: true,
            notice: None,
        }
    }
}

/// Which profile's Models the editor is on.
///
/// The rows are projected from the records every frame rather than held here: a
/// Model is written through the authority and comes back as a re-read, and an
/// editor holding its own copy would drift from the profile it is editing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelListView {
    pub agent_id: String,
    pub profile_id: String,
    /// The profile as the editor last named it, for the title and for the edit
    /// that follows a re-read.
    pub title: String,
    pub selected: usize,
    pub notice: Option<(String, ConfigTone)>,
}

/// The address an Agent always calls, when its projection pins the endpoint.
///
/// Some Agents reach a fixed service whatever a profile says — their bridge
/// reads the address from its own environment — so the projection reports the
/// address rather than letting a profile name one the request never carries.
pub fn fixed_endpoint_for(agent_id: &vibex_core::AgentId) -> Option<String> {
    vibex_core::catalog_projection_descriptors()
        .ok()?
        .into_iter()
        .find(|descriptor| descriptor.route.agent_id == *agent_id)
        .and_then(|descriptor| match descriptor.provider_control {
            vibex_core::AgentProviderControl::FixedEndpoint { base_url } => Some(base_url),
            _ => None,
        })
}

/// The reason a pinned address cannot be changed, for the field's hint.
fn fixed_endpoint_hint(url: &str, strings: Strings) -> String {
    strings.config_hint_fixed_endpoint().replace("{url}", url)
}

/// The endpoint field: editable, or pinned to the address its Agent calls.
pub fn base_url_field(value: String, pinned: Option<&str>, strings: Strings) -> FormField {
    match pinned {
        Some(url) => FormField::text("base_url", strings.config_field_url(), url.to_string())
            .hint(fixed_endpoint_hint(url, strings))
            .locked(),
        None => FormField::text("base_url", strings.config_field_url(), value)
            .hint(strings.config_hint_endpoint()),
    }
}

/// Re-evaluate the endpoint field for the Agent the form now names.
///
/// The Agent is a choice, so the pin follows it: a form that locked the field
/// for the Agent it opened on would keep the lock after the reader moved to
/// another one.
pub fn reaim_base_url_field(form: &mut ConfigForm, strings: Strings) {
    let Some(agent) = form.value("agent") else {
        return;
    };
    let Ok(agent_id) = vibex_core::AgentId::parse(agent) else {
        return;
    };
    let pinned = fixed_endpoint_for(&agent_id);
    let Some(field) = form.field_mut("base_url") else {
        return;
    };
    match pinned {
        Some(url) => {
            field.value = url.clone();
            field.origin = url.clone();
            field.hint = Some(fixed_endpoint_hint(&url, strings));
            field.locked = true;
        }
        None => {
            // Unpinning gives the reader a blank field rather than the address
            // the last Agent pinned: the field is theirs again.
            if field.locked {
                field.value = String::new();
                field.origin = String::new();
            }
            field.hint = Some(strings.config_hint_endpoint().to_string());
            field.locked = false;
        }
    }
}

/// The Provider option that carries the publisher's website.
const PROVIDER_OPTION_WEBSITE: &str = "ccSwitchWebsiteUrl";
/// The Provider option that carries the reader's request headers.
///
/// A JSON object keyed by header name: a string is a literal value, and
/// `{"variable": "session_id"}` is a value the runtime fills from the session
/// serving the Agent process.
const PROVIDER_OPTION_HEADERS: &str = "headerOverrideJson";
/// The prefix of the option that carries one protocol's own request address.
const PROVIDER_OPTION_PROTOCOL_URL: &str = "protocolBaseUrl.";

/// What the provider form makes of the options it edits.
///
/// The options are a key/value bag the profile shares with other writers —
/// where the profile was imported from, which switch app type it came from —
/// so the form owns three keys and leaves every other entry exactly as it
/// found it. A field that wrote the whole bag would delete a record it never
/// showed the reader.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderOptionsDraft {
    /// The publisher's website, as typed.
    pub website: String,
    /// Request headers, one `Name: value` per line. A value of `{{id}}` is a
    /// variable the runtime fills from the session; anything else is literal.
    pub headers: Vec<(String, ProviderHeaderValue)>,
    /// One address per protocol, as typed: `wire https://…` per line.
    pub protocol_urls: Vec<(vibex_core::ProviderModelWireApi, String)>,
}

/// One request header's value, in the shape the override stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderHeaderValue {
    Literal(String),
    Variable(vibex_core::ModelProviderHeaderVariable),
}

impl ProviderHeaderValue {
    /// The value as the form shows it.
    pub fn displayed(&self) -> String {
        match self {
            Self::Literal(value) => value.clone(),
            Self::Variable(variable) => format!("{{{{{}}}}}", variable.id()),
        }
    }
}

impl ProviderOptionsDraft {
    /// What the form makes of a profile's options.
    ///
    /// Anything that is not the shape this editor writes is skipped rather than
    /// guessed at, so a hand-edited option never turns into a header the reader
    /// never configured — and because the draft is applied back over the
    /// profile's own options, a skipped entry is left exactly as it was.
    pub fn read(options: &vibex_core::ProviderOptions) -> Self {
        let mut draft = Self::default();
        for entry in &options.entries {
            if entry.key == PROVIDER_OPTION_WEBSITE {
                draft.website = entry.value.clone();
            } else if entry.key == PROVIDER_OPTION_HEADERS {
                draft.headers = parse_header_override(&entry.value);
            } else if let Some(wire) = entry
                .key
                .strip_prefix(PROVIDER_OPTION_PROTOCOL_URL)
                .and_then(wire_api_from_value)
            {
                draft.protocol_urls.push((wire, entry.value.clone()));
            }
        }
        draft
    }

    /// The options a profile holds, with the three keys this form owns replaced
    /// and every other entry left where it was.
    pub fn apply(&self, options: &vibex_core::ProviderOptions) -> vibex_core::ProviderOptions {
        let owned = |key: &str| {
            key == PROVIDER_OPTION_WEBSITE
                || key == PROVIDER_OPTION_HEADERS
                || key.starts_with(PROVIDER_OPTION_PROTOCOL_URL)
        };
        let mut entries = options
            .entries
            .iter()
            .filter(|entry| !owned(&entry.key))
            .cloned()
            .collect::<Vec<_>>();
        if !self.website.trim().is_empty() {
            entries.push(vibex_core::ProviderBindingMetadata {
                key: PROVIDER_OPTION_WEBSITE.to_string(),
                value: self.website.trim().to_string(),
            });
        }
        if !self.headers.is_empty() {
            entries.push(vibex_core::ProviderBindingMetadata {
                key: PROVIDER_OPTION_HEADERS.to_string(),
                value: write_header_override(&self.headers),
            });
        }
        for (wire, url) in &self.protocol_urls {
            if url.trim().is_empty() {
                continue;
            }
            entries.push(vibex_core::ProviderBindingMetadata {
                key: format!("{PROVIDER_OPTION_PROTOCOL_URL}{}", wire_api_label(*wire)),
                value: url.trim().to_string(),
            });
        }
        vibex_core::ProviderOptions {
            schema_version: options.schema_version,
            entries,
        }
    }

    /// Whether any of the addresses names a gateway that wants a session header.
    ///
    /// One subscription gateway routes by a request header carrying the session,
    /// and a profile pointed at it that does not send one reaches the wrong
    /// bucket. The editor adds the header rather than leaving the reader to find
    /// out from the gateway's answer.
    pub fn wants_session_header(&self, base_url: Option<&str>) -> bool {
        let zen = |url: &str| url.contains("opencode.ai/zen/go");
        base_url.is_some_and(zen) || self.protocol_urls.iter().any(|(_, url)| zen(url))
    }

    /// Add the session header, when it is not already declared.
    pub fn with_session_header(&mut self) {
        let name = "x-opencode-session";
        if self
            .headers
            .iter()
            .any(|(header, _)| header.eq_ignore_ascii_case(name))
        {
            return;
        }
        self.headers.push((
            name.to_string(),
            ProviderHeaderValue::Variable(vibex_core::ModelProviderHeaderVariable::SessionId),
        ));
    }
}

/// Read the header override, skipping anything that is not the shape it writes.
fn parse_header_override(raw: &str) -> Vec<(String, ProviderHeaderValue)> {
    let Some(object) = serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
    else {
        return Vec::new();
    };
    object
        .into_iter()
        .filter_map(|(name, value)| {
            let value = match value {
                serde_json::Value::String(value) => ProviderHeaderValue::Literal(value),
                serde_json::Value::Object(object) => {
                    let id = object.get("variable")?.as_str()?;
                    let variable = vibex_core::ModelProviderHeaderVariable::ALL
                        .into_iter()
                        .find(|variable| variable.id() == id)?;
                    ProviderHeaderValue::Variable(variable)
                }
                _ => return None,
            };
            Some((name, value))
        })
        .collect()
}

/// Write the header override in the shape the runtime reads.
fn write_header_override(headers: &[(String, ProviderHeaderValue)]) -> String {
    let mut object = serde_json::Map::new();
    for (name, value) in headers {
        object.insert(
            name.clone(),
            match value {
                ProviderHeaderValue::Literal(value) => serde_json::Value::String(value.clone()),
                ProviderHeaderValue::Variable(variable) => {
                    serde_json::json!({ "variable": variable.id() })
                }
            },
        );
    }
    serde_json::Value::Object(object).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row projections are pure, so the rules they carry can be checked
    /// without a terminal: what a reader sees is decided here, and the frame is
    /// only the way it is drawn.
    fn strings() -> Strings {
        Strings::for_locale(crate::locale::Locale::En)
    }

    fn agent(id: &str, added: bool, enabled: bool, order: i64) -> vibex_core::AgentSnapshotEntry {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "label": id,
            "runtimeKind": "acp",
            "sourceKind": "builtin",
            "added": added,
            "enabled": enabled,
            "orderIndex": order,
            "installed": true,
            "configured": true,
            "installStatus": "installed",
            "managedInstall": {"managed": true, "status": "installed"},
            "configStatus": "configured",
            "runtimeStatus": "ready",
            "models": [],
            "modes": [],
            "capabilityHints": [],
            "env": {},
            "params": {},
            "diagnostics": [],
            "nativeConfigPaths": []
        }))
        .expect("agent fixture")
    }

    /// The roster is three groups, in the order the desktop shows them.
    #[test]
    fn the_roster_sorts_by_group_then_place_then_id() {
        let data = ManagementData {
            agents: vec![
                agent("gemini", false, false, 0),
                agent("kimi", true, false, 5),
                agent("cursor", true, true, 2),
                agent("codex", true, true, 1),
                // Two records claiming one place still have an order.
                agent("claude", true, true, 1),
            ],
            ..Default::default()
        };
        let rows = agent_rows(&data, strings());
        assert_eq!(
            rows.iter().map(|row| row.key.as_str()).collect::<Vec<_>>(),
            vec!["claude", "codex", "cursor", "kimi", "gemini"],
            "the roster is not grouped, ordered and tie-broken"
        );
    }

    /// The group is a function of the record, and it is what the sort and the
    /// chip both read.
    #[test]
    fn the_group_is_the_roster_s_own_three() {
        assert_eq!(agent_group(&agent("codex", true, true, 0)), 0);
        assert_eq!(agent_group(&agent("codex", true, false, 0)), 1);
        assert_eq!(agent_group(&agent("codex", false, true, 0)), 2);
        assert_eq!(
            agent_group(&agent("codex", false, false, 0)),
            2,
            "an Agent the roster does not hold is in the third group whether or not it is switched on"
        );
    }

    /// The origin a stored Skill records is the origin the card checks for:
    /// one shape, so a card that says "installed" and a document that records
    /// where it came from cannot disagree.
    #[test]
    fn a_stored_skill_records_the_origin_the_card_looks_for() {
        let entry: vibex_core::SkillMarketEntry = serde_json::from_value(serde_json::json!({
            "id": "awspace/pdf",
            "slug": "pdf",
            "name": "PDF",
            "installs": 1,
            "downloads": 0,
            "stars": 0
        }))
        .expect("entry");
        assert_eq!(market_skill_uri(&entry), "market:awspace/pdf");

        let skill: vibex_core::Skill = serde_json::from_value(serde_json::json!({
            "id": "skill_pdf",
            "displayName": "PDF",
            "sourceKind": "marketplace",
            "status": "enabled",
            "scopeKind": "user",
            "sourceUri": market_skill_uri(&entry),
            "description": "",
            "instructions": "",
            "tags": [],
            "providerMatrix": [],
            "agentMatrix": [],
            "createdAtMs": 1,
            "updatedAtMs": 1
        }))
        .expect("skill");
        let data = ManagementData {
            skill_market: vec![entry],
            skills: vec![skill],
            ..Default::default()
        };
        let rows = skill_market_rows(&data, strings());
        assert_eq!(rows[0].chip, "Installed");
    }

    /// A card's count is the first one the registry published rather than a
    /// zero it filled in.
    #[test]
    fn a_skill_card_counts_what_the_registry_published() {
        let mut entry: vibex_core::SkillMarketEntry = serde_json::from_value(serde_json::json!({
            "id": "o/demo",
            "slug": "demo",
            "name": "Demo",
            "installs": 0,
            "downloads": 0,
            "stars": 0
        }))
        .expect("entry");
        assert_eq!(skill_reach(&entry), "0");
        entry.downloads = 9;
        assert_eq!(skill_reach(&entry), "9", "downloads did not stand in");
        entry.installs = 3;
        assert_eq!(skill_reach(&entry), "3", "installs did not win");
    }

    /// The header options round-trip through the runtime's own JSON shape:
    /// a variable stays a variable and a literal stays a literal.
    #[test]
    fn the_header_override_round_trips() {
        let written = write_header_override(&[
            (
                "X-Trace".to_string(),
                ProviderHeaderValue::Literal("on".to_string()),
            ),
            (
                "x-opencode-session".to_string(),
                ProviderHeaderValue::Variable(vibex_core::ModelProviderHeaderVariable::SessionId),
            ),
        ]);
        let read = parse_header_override(&written);
        assert_eq!(read.len(), 2);
        assert!(read.contains(&(
            "X-Trace".to_string(),
            ProviderHeaderValue::Literal("on".to_string())
        )));
        assert!(read.contains(&(
            "x-opencode-session".to_string(),
            ProviderHeaderValue::Variable(vibex_core::ModelProviderHeaderVariable::SessionId)
        )));

        // Anything that is not the shape it writes is skipped rather than
        // guessed at, so a hand-edited option cannot become a header.
        assert!(parse_header_override("not json").is_empty());
        assert!(parse_header_override("{\"X\": 7}").is_empty());
        assert!(parse_header_override("{\"X\": {\"variable\": \"nope\"}}").is_empty());
    }

    /// The form owns three option keys and leaves every other entry where it
    /// was: a field that wrote the whole bag would delete a record it never
    /// showed the reader.
    #[test]
    fn the_form_replaces_its_own_options_and_keeps_the_rest() {
        let stored = vibex_core::ProviderOptions {
            schema_version: 1,
            entries: vec![
                vibex_core::ProviderBindingMetadata {
                    key: "nativeSource".to_string(),
                    value: "cc-switch".to_string(),
                },
                vibex_core::ProviderBindingMetadata {
                    key: PROVIDER_OPTION_WEBSITE.to_string(),
                    value: "https://old".to_string(),
                },
                vibex_core::ProviderBindingMetadata {
                    key: "protocolBaseUrl.anthropic_messages".to_string(),
                    value: "https://old-anthropic".to_string(),
                },
            ],
        };
        let draft = ProviderOptionsDraft {
            website: "https://new".to_string(),
            headers: vec![(
                "X-Trace".to_string(),
                ProviderHeaderValue::Literal("on".to_string()),
            )],
            protocol_urls: vec![],
        };
        let applied = draft.apply(&stored);
        let entry = |key: &str| {
            applied
                .entries
                .iter()
                .find(|entry| entry.key == key)
                .map(|entry| entry.value.as_str())
        };
        assert_eq!(
            entry("nativeSource"),
            Some("cc-switch"),
            "an unowned key went"
        );
        assert_eq!(entry(PROVIDER_OPTION_WEBSITE), Some("https://new"));
        assert_eq!(
            entry("protocolBaseUrl.anthropic_messages"),
            None,
            "a protocol address the form no longer names was left behind"
        );
        assert!(entry(PROVIDER_OPTION_HEADERS).is_some());
    }

    /// The gateway that routes by session gets the header it reads, and a
    /// header that is already there is not added twice.
    #[test]
    fn the_zen_gateway_gets_its_session_header_once() {
        let mut draft = ProviderOptionsDraft::default();
        assert!(!draft.wants_session_header(Some("https://api.example/v1")));
        assert!(draft.wants_session_header(Some("https://opencode.ai/zen/go/v1")));
        draft.with_session_header();
        draft.with_session_header();
        assert_eq!(
            draft.headers.len(),
            1,
            "the session header was declared twice"
        );
        assert_eq!(draft.headers[0].1.displayed(), "{{session_id}}");
    }

    /// A cycle walks its list and comes back to the start.
    #[test]
    fn a_cycle_walks_every_entry_and_wraps() {
        let modes = native_export_modes();
        let mut current = modes[0];
        for expected in modes.iter().skip(1) {
            current = cycle(&modes, current);
            assert_eq!(current, *expected);
        }
        assert_eq!(cycle(&modes, current), modes[0], "the cycle did not wrap");
    }

    /// The custom Agent form is refused before the write, with the rule it
    /// broke, and the request it makes is what the reader typed.
    #[test]
    fn the_custom_agent_form_checks_before_it_writes() {
        let strings = strings();
        let fill = |id: &str, label: &str, command: &str, env: &str| {
            let mut form = custom_agent_form(strings);
            form.field_mut("agent_id").unwrap().value = id.to_string();
            form.field_mut("label").unwrap().value = label.to_string();
            form.field_mut("command").unwrap().value = command.to_string();
            form.field_mut("args").unwrap().value = "  --acp   --verbose ".to_string();
            form.field_mut("env").unwrap().value = env.to_string();
            form
        };

        assert!(custom_agent_request(&fill("My Agent", "A", "/bin/a", ""), strings).is_err());
        assert!(custom_agent_request(&fill("my-agent", "", "/bin/a", ""), strings).is_err());
        assert!(
            custom_agent_request(&fill("my-agent", "A", "", ""), strings).is_err(),
            "an empty command was accepted"
        );
        assert!(custom_agent_request(&fill("my-agent", "A", "/bin/a", "TOKEN"), strings).is_err());
        assert!(
            custom_agent_request(&fill("my-agent", "A", "/bin/a", "1TOKEN=x"), strings).is_err(),
            "a variable name that is not one was accepted"
        );

        let request = custom_agent_request(
            &fill("my-agent", "A", "/bin/a", " TOKEN = secret \n\nX_Y=1\n"),
            strings,
        )
        .expect("a well-formed form");
        assert_eq!(request.agent_id.as_str(), "my-agent");
        assert_eq!(request.command.command, "/bin/a");
        assert_eq!(
            request.command.args,
            vec!["--acp", "--verbose"],
            "the arguments were not split the way a shell reads them"
        );
        assert_eq!(request.env.get("TOKEN").map(String::as_str), Some("secret"));
        assert_eq!(request.env.get("X_Y").map(String::as_str), Some("1"));
    }

    /// An Agent's install page is the authority's own answer first, and the
    /// vendor table only for the Agents whose entry carries none.
    #[test]
    fn an_agents_install_page_prefers_the_snapshot() {
        let mut published = agent("opencode", true, true, 0);
        published.params = serde_json::json!({"installUrl": "https://example.test/opencode"});
        assert_eq!(
            vibex_core::agent_install_url(&published),
            Some("https://example.test/opencode")
        );

        let mut cline = agent("cline", true, true, 0);
        cline.label = "Cline".to_string();
        assert_eq!(
            vibex_core::agent_install_url(&cline),
            Some("https://cline.bot/cli")
        );

        let mut unknown = agent("something-nobody-knows", true, true, 0);
        unknown.label = "Unlisted".to_string();
        assert_eq!(vibex_core::agent_install_url(&unknown), None);

        // An empty published value falls through to the table rather than
        // sending the reader to a blank page.
        cline.params = serde_json::json!({"installUrl": "  "});
        assert_eq!(
            vibex_core::agent_install_url(&cline),
            Some("https://cline.bot/cli")
        );
    }

    /// The three panes are the ones the surface draws, in reading order, and
    /// the walk wraps.
    #[test]
    fn the_zone_walk_covers_every_pane_and_wraps() {
        let mut center = ConfigCenter::default();
        assert_eq!(center.zone, ConfigZone::Rows);
        let mut walked = vec![center.zone];
        for _ in 0..ConfigZone::ALL.len() {
            center.step_zone(1);
            walked.push(center.zone);
        }
        assert_eq!(walked.len(), ConfigZone::ALL.len() + 1);
        // Every pane is reached, whatever order the walk starts in.
        for zone in ConfigZone::ALL {
            assert!(
                walked.contains(&zone),
                "{zone:?} was never reached by the walk"
            );
        }
        assert_eq!(
            walked[0],
            walked[ConfigZone::ALL.len()],
            "the walk did not wrap"
        );

        center.step_zone(-1);
        assert_eq!(
            center.zone,
            ConfigZone::ALL[ConfigZone::ALL.len() - 2],
            "the walk backwards is not the reverse"
        );
    }
}

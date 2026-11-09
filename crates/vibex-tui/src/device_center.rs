//! The devices page: pairing and the devices already paired, on one surface.
//!
//! Pairing and the trust store used to be two destinations reached by two
//! palette verbs — a dialog that showed one code and a page that listed what
//! came of it. They are one job: the reader opens this page to let a phone or a
//! second computer in, and the list of what is already in is how they check
//! that it worked. So the page keeps the desktop's two halves and separates
//! them with a tab strip rather than a keypress, the way the configuration
//! centre separates its modules.
//!
//! What the pairing half offers is not the reader's choice, it is the runtime's:
//! a phone can always scan an offer, a window can be advertised on the local
//! network when the runtime can reach Direct, and a second desktop can only be
//! handed a code or a link by a headless runtime — a desktop shell pairs
//! phones, it does not accept another desktop as a client. The row that cannot
//! work is absent rather than disabled: a page that lists a verb the runtime
//! will refuse teaches the reader to distrust the page.
//!
//! Everything here is pure: the projections answer rows, the state machine
//! answers where the cursor is and what is open, and neither performs I/O. The
//! reducer turns a row into an effect and the renderer draws the rows the
//! projection answered, which is what keeps the mouse and the keyboard acting
//! on the same object.

use vibex_core::{
    RemoteDevicePermissionLevel, RemoteLanPairingRequestState, RemoteLanPairingWindowSnapshot,
    RemoteServerKind,
};

use crate::app::Availability;
use crate::locale::Strings;

/// Which half of the page is open.
///
/// The pairing half comes first because it is what the page is for; a reader
/// who came back to check a device is one `→` away, and the module they were on
/// is remembered while they are there. The publishing half is last because it is
/// the only half that never asks the runtime for anything: it is where a reader
/// goes when pairing is not offering what they expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, PartialOrd, Ord)]
pub enum DeviceTab {
    #[default]
    Pair,
    Devices,
    /// How this runtime publishes itself: Tailscale Serve, Direct, Relay.
    ///
    /// The tab exists only where the runtime can answer: a peer that predates
    /// the read has no rows to draw, and a page that lists methods it cannot
    /// confirm would be inventing them.
    Publishing,
}

impl DeviceTab {
    /// The halves that always exist, in the order they are drawn.
    pub const ALWAYS: [Self; 2] = [Self::Pair, Self::Devices];

    pub const fn key(self) -> &'static str {
        match self {
            Self::Pair => "pair",
            Self::Devices => "devices",
            Self::Publishing => "publishing",
        }
    }

    pub fn label(self, strings: Strings) -> &'static str {
        match self {
            Self::Pair => strings.devices_tab_pair(),
            Self::Devices => strings.devices_title(),
            Self::Publishing => strings.devices_tab_publishing(),
        }
    }
}

/// One way to pair, as the pairing tab lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PairMethod {
    /// Show an offer as a QR code: the phone reads it with its camera.
    QrCode,
    /// Advertise the runtime on the local network: the phone finds it and asks,
    /// and the request is approved here.
    LocalNetwork,
    /// Show the offer as a one-time code and a link: a second client opens the
    /// link to pair.
    CodeOrLink,
}

impl PairMethod {
    pub const fn key(self) -> &'static str {
        match self {
            Self::QrCode => "qr",
            Self::LocalNetwork => "lan",
            Self::CodeOrLink => "code",
        }
    }

    /// The row's title: the reader's action, not the mechanism.
    pub fn label(self, strings: Strings) -> &'static str {
        match self {
            Self::QrCode => strings.devices_pair_qr(),
            Self::LocalNetwork => strings.devices_pair_lan(),
            Self::CodeOrLink => strings.devices_pair_code(),
        }
    }

    /// What the row is for: who can use it, and what they need.
    pub fn scope(self, strings: Strings) -> &'static str {
        match self {
            Self::QrCode => strings.devices_pair_qr_scope(),
            Self::LocalNetwork => strings.devices_pair_lan_scope(),
            Self::CodeOrLink => strings.devices_pair_code_scope(),
        }
    }

    /// The effect that opens this row's panel.
    pub const fn opening(self) -> Opening {
        match self {
            Self::QrCode | Self::CodeOrLink => Opening::Offer,
            Self::LocalNetwork => Opening::Window,
        }
    }
}

/// What opening a pairing row starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opening {
    /// Mint a pairing offer.
    Offer,
    /// Open a LAN pairing window.
    Window,
}

/// What one press on a pairing row did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    /// The row's panel was opened, and this is what it needs.
    Opened(Opening),
    /// The row's panel was closed, and this is what has to come down with it.
    Closed { release: Release },
}

/// What taking a panel down gives up.
///
/// A LAN window and an offer are both advertisements of the same runtime: one is
/// found by a device on the network, the other is read off the screen. Leaving
/// either live behind a closed panel is the same mistake, so the state machine
/// answers which one it was holding rather than a boolean per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    /// Nothing was advertised, so the press asks the runtime for nothing.
    Nothing,
    /// A minted offer the runtime is still accepting.
    Offer,
    /// An advertisement on the local network.
    Window,
}

/// What is known about the Direct route a LAN window has to stand on.
///
/// A local-network window is refused outright when no Direct route is online
/// (`remote_lan_direct_unavailable`), so the row is a verb the runtime would
/// reject unless a route is up. `Unknown` is a runtime that does not report its
/// routes at all: nothing is known, so nothing is hidden on the strength of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanPrecondition {
    Unknown,
    Reachable,
    Unreachable,
}

/// The pairing rows this runtime can actually offer, in the order they are
/// drawn.
///
/// A row is present only when it can work here. The runtime's kind decides the
/// desktop row — only a headless runtime accepts a desktop client — the
/// capabilities decide the rest, and the LAN row also needs the Direct route its
/// advertisement is built on.
pub fn pair_methods(
    runtime_kind: RemoteServerKind,
    pairing: Availability,
    lan_pairing: Availability,
    lan_precondition: LanPrecondition,
) -> Vec<PairMethod> {
    if pairing != Availability::Available {
        return Vec::new();
    }
    let mut methods = vec![PairMethod::QrCode];
    if lan_pairing == Availability::Available
        && runtime_kind == RemoteServerKind::Desktop
        && lan_precondition != LanPrecondition::Unreachable
    {
        methods.push(PairMethod::LocalNetwork);
    }
    if runtime_kind == RemoteServerKind::Headless {
        methods.push(PairMethod::CodeOrLink);
    }
    methods
}

/// One published route an offer can be carried over.
///
/// The runtime publishes a route per method, and the offer carries all of them:
/// the difference between the entries is the transport named in the launch link,
/// because the fragment after it is route-independent. `origin` is the address a
/// device would dial, which is what lets a reader recognise the route their phone
/// is actually on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingRoute {
    pub transport: vibex_core::RemotePairingTransport,
    pub origin: String,
}

/// The transport preference the desktop uses, applied to one route list.
///
/// Tailnet first, then Direct, then the relay: the same order the desktop's
/// segmented control draws, so "the route this client picked for you" is the one
/// the desktop would have picked too.
fn route_index(transport: vibex_core::RemotePairingTransport) -> usize {
    match transport {
        vibex_core::RemotePairingTransport::Tailnet => 0,
        vibex_core::RemotePairingTransport::Direct => 1,
        vibex_core::RemotePairingTransport::SelfHostedRelay => 2,
        vibex_core::RemotePairingTransport::Unknown => 3,
    }
}

/// One offer, as the panel draws it.
///
/// The code and the link are the same offer seen twice: the code is what the
/// reader reads out, the link is what the QR encodes and what a desktop pastes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingOffer {
    /// What a cancel or a status read addresses. The code is shown once and
    /// never stored, but the id has to be kept: leaving the panel is what takes
    /// the offer down, and the runtime can only take down what it can name.
    pub offer_id: vibex_core::RequestId,
    pub code: String,
    /// The launch fragment the runtime answered with, without its `#`.
    ///
    /// It is the same for every route — only the transport segment in front of
    /// it changes — which is why switching routes costs nothing but a re-encode.
    pub fragment: String,
    pub permission: RemoteDevicePermissionLevel,
    /// When the runtime stops accepting the code. An offer that is drawn past
    /// its deadline is a code the reader will type into a phone that refuses
    /// it, so the page stops drawing one.
    pub expires_at_ms: i64,
    /// Every route the runtime published for this offer, in preference order.
    pub routes: Vec<PairingRoute>,
    /// Which of them the link below the code carries.
    pub selected: usize,
    /// Whether a device claimed it. A claimed offer is spent: the code in front
    /// of the reader is no longer a code, so the panel says the device arrived
    /// rather than counting down to an expiry that no longer matters.
    pub claimed: bool,
}

impl PairingOffer {
    /// The offer the runtime answered with, as the panel shows it.
    pub fn from_response(response: &vibex_core::RemoteCreatePairingOfferResponse) -> Self {
        let summary = &response.offer.summary;
        let mut routes: Vec<PairingRoute> = Vec::new();
        for candidate in summary
            .direct_candidates
            .iter()
            .chain(summary.relay_candidate.iter())
        {
            if candidate.transport == vibex_core::RemotePairingTransport::Unknown {
                continue;
            }
            // One route per method: the runtime may publish several Direct
            // candidates, and a reader choosing between two identical labels is
            // being asked a question the link cannot answer.
            if routes
                .iter()
                .any(|route| route.transport == candidate.transport)
            {
                continue;
            }
            routes.push(PairingRoute {
                transport: candidate.transport,
                origin: candidate.url.clone(),
            });
        }
        routes.sort_by_key(|route| route_index(route.transport));
        Self {
            offer_id: summary.offer_id.clone(),
            code: response.offer.one_time_challenge.clone(),
            fragment: response
                .launch_fragment
                .strip_prefix('#')
                .unwrap_or(&response.launch_fragment)
                .to_string(),
            permission: summary.permission_level,
            expires_at_ms: summary.expires_at_ms,
            routes,
            selected: 0,
            claimed: false,
        }
    }

    /// The route the code and the link carry, when the offer has one.
    pub fn selected_route(&self) -> Option<&PairingRoute> {
        self.routes.get(self.selected)
    }

    /// How many routes there are to choose between.
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }

    /// Whether the reader has a choice to make. One route is not a choice, and
    /// a control that offers a single option is a control that does nothing.
    pub fn route_is_chosen(&self) -> bool {
        self.routes.len() > 1
    }

    /// The link a phone opens, and the one a second desktop opens to pair.
    ///
    /// The runtime answers with a bare `#/pair/…` fragment and leaves the
    /// transport to the client, which is the side that knows which of the
    /// offer's routes it is showing. The `vibex://open/…` form around it is not
    /// decoration: the mobile scanner only accepts a `vibex://` link, so a
    /// fragment on its own is a code no camera would act on.
    pub fn url(&self) -> String {
        let transport = self
            .selected_route()
            .map(|route| route.transport)
            .unwrap_or(vibex_core::RemotePairingTransport::Direct);
        format!(
            "vibex://open/{}#{}",
            pairing_transport_name(transport),
            self.fragment
        )
    }

    /// Moves the selection by `delta`, wrapping. Answers whether it moved, so a
    /// key press with nothing to choose between repaints nothing.
    pub fn cycle_route(&mut self, delta: i64) -> bool {
        if !self.route_is_chosen() {
            return false;
        }
        let count = self.routes.len() as i64;
        let next = (self.selected as i64 + delta).rem_euclid(count) as usize;
        if next == self.selected {
            return false;
        }
        self.selected = next;
        true
    }
}

/// The path segment that tells the mobile app which route the offer carries.
pub const fn pairing_transport_name(transport: vibex_core::RemotePairingTransport) -> &'static str {
    match transport {
        vibex_core::RemotePairingTransport::Direct => "direct",
        vibex_core::RemotePairingTransport::Tailnet => "tailnet",
        vibex_core::RemotePairingTransport::SelfHostedRelay => "self_hosted_relay",
        vibex_core::RemotePairingTransport::Unknown => "direct",
    }
}

/// A pairing action that failed, filed under the row it was taken from.
///
/// The code travels with the message because the recovery copy is chosen from
/// it: "no Direct route" and "you lack permission" are the same sentence only
/// to a reader who has not been told which one happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingError {
    pub method: PairMethod,
    pub code: String,
    pub message: String,
}

/// The live facts behind the pairing tab.
#[derive(Debug, Clone, Default)]
pub struct Pairing {
    /// The method whose request is in flight. Only one at a time: two windows,
    /// or an offer and a window, would be two advertisements of the same
    /// runtime and the reader would not know which one the phone answered.
    pub busy: Option<PairMethod>,
    /// The offer the QR and the link are drawn from, held until the reader
    /// leaves the tab.
    pub offer: Option<PairingOffer>,
    /// The window this runtime is advertising, re-read while the panel is open.
    pub window: Option<RemoteLanPairingWindowSnapshot>,
    pub error: Option<PairingError>,
}

impl Pairing {
    /// The request the reader is being asked about: the oldest one still
    /// waiting.
    ///
    /// The coordinator accepts one device per window, so a panel that showed
    /// every pending request at once would be offering a choice that does not
    /// exist. The oldest is the one the phone has been waiting on longest.
    pub fn pending(&self) -> Option<&vibex_core::RemoteLanPairingPendingRequestSummary> {
        self.window.as_ref().and_then(|window| {
            window
                .pending_requests
                .iter()
                .find(|request| request.state == RemoteLanPairingRequestState::Pending)
        })
    }

    /// Whether a request the reader made is still on its way.
    ///
    /// The open row draws a spinner for as long as this is true, and that
    /// spinner's frame comes from the animation phase: the clock has to keep
    /// stepping while the answer is out, or the row stands on one frame and the
    /// reader concludes the press went nowhere. The state is set only while a
    /// request is in flight and cleared by the answer — or by the panel closing,
    /// which is what keeps the clock from running for a surface nobody has open.
    pub fn is_busy(&self) -> bool {
        self.busy.is_some()
    }

    /// How many requests are waiting, including the one [`Self::pending`] names.
    pub fn waiting(&self) -> usize {
        self.window
            .as_ref()
            .map(|window| {
                window
                    .pending_requests
                    .iter()
                    .filter(|request| request.state == RemoteLanPairingRequestState::Pending)
                    .count()
            })
            .unwrap_or(0)
    }

    /// Whether the window has run out, which the panel says rather than
    /// pretending a dead advertisement is still up.
    pub fn expired(&self, now_ms: i64) -> bool {
        self.window
            .as_ref()
            .is_some_and(|window| window.discovery.expires_at_ms <= now_ms)
    }

    /// The offer, while the runtime would still accept it.
    pub fn live_offer(&self, now_ms: i64) -> Option<&PairingOffer> {
        self.offer
            .as_ref()
            .filter(|offer| !offer.claimed && offer.expires_at_ms > now_ms)
    }

    /// Whether the code in front of the reader was already used.
    ///
    /// Kept apart from [`Self::live_offer`] because the two draw different
    /// things: a spent code is not an expired one, and telling a reader their
    /// code expired when a phone just paired is the wrong sentence.
    pub fn offer_was_claimed(&self) -> bool {
        self.offer.as_ref().is_some_and(|offer| offer.claimed)
    }

    /// Whether the open row has nothing left to show: no offer, one the
    /// runtime has already stopped accepting, or a window that has closed.
    ///
    /// The press that finds nothing live is the press that asks again, which is
    /// why this is a question the reducer asks rather than a state the panel
    /// draws.
    pub fn spent(&self, method: PairMethod, now_ms: i64) -> bool {
        match method {
            PairMethod::LocalNetwork => self.window.is_none() || self.expired(now_ms),
            PairMethod::QrCode | PairMethod::CodeOrLink => self.live_offer(now_ms).is_none(),
        }
    }

    /// What closing the panel behind `method` gives up.
    ///
    /// The answer is read from what is actually held rather than from the row
    /// that was open: a row whose request never answered was never advertising
    /// anything, so closing it asks the runtime for nothing.
    pub fn release(&self, method: PairMethod) -> Release {
        match method {
            PairMethod::LocalNetwork if self.window.is_some() => Release::Window,
            // A claimed offer is already spent — the runtime closed it when the
            // device claimed it — so there is nothing left to take down, and
            // asking anyway would turn a successful pairing into an error toast
            // on the way out.
            PairMethod::QrCode | PairMethod::CodeOrLink
                if self.offer.as_ref().is_some_and(|offer| !offer.claimed) =>
            {
                Release::Offer
            }
            _ => Release::Nothing,
        }
    }

    /// Folds a status read into the offer it names.
    ///
    /// An answer about a different offer is dropped rather than applied: the
    /// reader has moved on to a newer code, and the old read says nothing about
    /// the one on screen.
    pub fn apply_offer_status(&mut self, summary: &vibex_core::RemotePairingOfferSummary) -> bool {
        let Some(offer) = self.offer.as_mut() else {
            return false;
        };
        if offer.offer_id != summary.offer_id {
            return false;
        }
        let claimed = summary.claimed_device_id.is_some();
        let moved = claimed != offer.claimed;
        offer.claimed = claimed;
        moved
    }

    /// Everything the page learned from a fresh read or a finished action.
    pub fn settle(&mut self, window: Option<RemoteLanPairingWindowSnapshot>) {
        self.busy = None;
        self.error = None;
        self.window = window;
    }

    /// Files a failure under the row it came from and stops the spinner there.
    pub fn fail(&mut self, error: PairingError) {
        self.busy = None;
        self.error = Some(error);
    }

    /// Gives up everything held for the pairing tab: the offer is shown once,
    /// on this surface, and leaving it is what "once" means.
    pub fn clear(&mut self) {
        self.busy = None;
        self.offer = None;
        self.window = None;
        self.error = None;
    }
}

/// The ways this runtime publishes itself, as the third half of the page reads
/// them.
///
/// Nothing here is a verb. Switching a route on moves a Tailscale Serve config,
/// binds a port, or enrolls a relay, and confirming a port is a decision about a
/// machine the reader may not be sitting in front of — the desktop Remote Access
/// page owns all of it. What a terminal can honestly do is say which routes are
/// up, which are broken, and what an operator has to do about it.
#[derive(Debug, Clone, Default)]
pub struct Publications {
    /// Whether a read has answered. The difference between "nothing is
    /// published" and "nobody has asked yet" is the difference between a
    /// conclusion and a blank page.
    pub loaded: bool,
    pub rows: Vec<vibex_core::RemotePairingPublicationSummary>,
    /// The error code the last read failed with, if it failed.
    pub error: Option<String>,
}

impl Publications {
    /// Whether a route a device could actually reach is up.
    pub fn any_reachable(&self) -> bool {
        self.rows.iter().any(|row| row.state.is_reachable())
    }

    /// The row the cursor is on.
    pub fn row(&self, index: usize) -> Option<&vibex_core::RemotePairingPublicationSummary> {
        self.rows.get(index)
    }

    /// Whether the Direct route a LAN window needs is up.
    ///
    /// Only a reachable `Direct` entry counts: a Tailnet route also travels as a
    /// Direct candidate, but the window is opened against the Direct method
    /// specifically, so Tailnet is not a substitute for it.
    pub fn lan_precondition(&self) -> LanPrecondition {
        if !self.loaded {
            return LanPrecondition::Unknown;
        }
        if self.error.is_some() {
            return LanPrecondition::Unknown;
        }
        let direct = self
            .rows
            .iter()
            .find(|row| row.method == vibex_core::RemotePairingPublicationMethod::Direct);
        match direct {
            Some(row) if row.state.is_reachable() => LanPrecondition::Reachable,
            Some(_) => LanPrecondition::Unreachable,
            // A runtime that does not list the method is not a runtime that has
            // said the method is down.
            None => LanPrecondition::Unknown,
        }
    }

    /// Folds one answer in. The rows are replaced whole: this is a status read,
    /// so the last answer is the truth rather than a merge of two snapshots.
    pub fn settle(
        &mut self,
        result: Result<Vec<vibex_core::RemotePairingPublicationSummary>, &str>,
    ) {
        self.loaded = true;
        match result {
            Ok(rows) => {
                self.rows = rows;
                self.error = None;
            }
            Err(code) => {
                self.rows.clear();
                self.error = Some(code.to_string());
            }
        }
    }
}

/// The audit trail, as the read-only view prints it.
///
/// One block per record: when it happened, what the runtime decided, the
/// sentence the runtime already wrote about it, and the identifiers a support
/// conversation needs to line the row up with its logs.
///
/// The action and the outcome are the reader's words rather than the wire's.
/// An audit trail is read by the person who owns the machine, and a column of
/// `DeviceRevoked` / `Allowed` is a column of Rust variant names in a client
/// that translates everything else. The summary beside them is the runtime's own
/// sentence, shown as it stands: it is the one part of a record that says what
/// actually happened, and it is already redacted where it was written.
pub fn audit_report(records: &[vibex_core::RemoteAuditRecord], strings: Strings) -> String {
    if records.is_empty() {
        return strings.audit_empty().to_string();
    }
    let mut lines: Vec<String> = Vec::new();
    for record in records {
        lines.push(format!(
            "{}  {}  {}",
            crate::text::format_clock(record.created_at_ms, strings.locale),
            audit_outcome_label(record.outcome, strings),
            audit_action_label(record.action, strings),
        ));
        let summary = record.redacted_summary.trim();
        if !summary.is_empty() {
            lines.push(format!("    {summary}"));
        }
        let mut identifiers: Vec<String> = Vec::new();
        if let Some(device_id) = record.device_id.as_ref() {
            identifiers.push(format!("{} {device_id}", strings.audit_field_device()));
        }
        if let Some(target_id) = record.target_id.as_ref() {
            identifiers.push(format!(
                "{} {}:{target_id}",
                strings.audit_field_target(),
                audit_target_kind_id(record.target_kind)
            ));
        }
        if let Some(request_id) = record.request_id.as_ref() {
            identifiers.push(format!("{} {request_id}", strings.audit_field_request()));
        }
        if let Some(correlation_id) = record.correlation_id.as_ref() {
            identifiers.push(format!(
                "{} {correlation_id}",
                strings.audit_field_correlation()
            ));
        }
        if !identifiers.is_empty() {
            lines.push(format!("    {}", identifiers.join(" · ")));
        }
        lines.push(String::new());
    }
    lines.join("\n").trim_end().to_string()
}

fn audit_action_label(action: vibex_core::RemoteAuditAction, strings: Strings) -> &'static str {
    use vibex_core::RemoteAuditAction as Action;
    match action {
        Action::PairingCodeCreated => strings.audit_action_pairing_code_created(),
        Action::PairingCodeClaimed => strings.audit_action_pairing_code_claimed(),
        Action::PairingCodeRejected => strings.audit_action_pairing_code_rejected(),
        Action::PairingOfferCreated => strings.audit_action_pairing_offer_created(),
        Action::PairingOfferClaimed => strings.audit_action_pairing_offer_claimed(),
        Action::PairingOfferCanceled => strings.audit_action_pairing_offer_canceled(),
        Action::PairingOfferRejected => strings.audit_action_pairing_offer_rejected(),
        Action::DeviceAuthenticated => strings.audit_action_device_authenticated(),
        Action::DeviceAuthFailed => strings.audit_action_device_auth_failed(),
        Action::DeviceRevoked => strings.audit_action_device_revoked(),
        Action::DeviceRestored => strings.audit_action_device_restored(),
        Action::DeviceDeleted => strings.audit_action_device_deleted(),
        Action::DeviceRenamed => strings.audit_action_device_renamed(),
        Action::RuntimeRenamed => strings.audit_action_runtime_renamed(),
        Action::PermissionAllowed => strings.audit_action_permission_allowed(),
        Action::PermissionDenied => strings.audit_action_permission_denied(),
        Action::MutationAllowed => strings.audit_action_mutation_allowed(),
        Action::MutationDenied => strings.audit_action_mutation_denied(),
    }
}

fn audit_outcome_label(outcome: vibex_core::RemoteAuditOutcome, strings: Strings) -> &'static str {
    use vibex_core::RemoteAuditOutcome as Outcome;
    match outcome {
        Outcome::Allowed => strings.audit_outcome_allowed(),
        Outcome::Denied => strings.audit_outcome_denied(),
        Outcome::Failed => strings.audit_outcome_failed(),
        Outcome::Created => strings.audit_outcome_created(),
        Outcome::Revoked => strings.audit_outcome_revoked(),
    }
}

/// What a record's target is, as the stable word its own id is filed under.
///
/// Not translated on purpose: this is the half of the identifier line a reader
/// greps for, like an error code, rather than copy to read.
const fn audit_target_kind_id(target: vibex_core::RemoteAuditTargetKind) -> &'static str {
    use vibex_core::RemoteAuditTargetKind as Target;
    match target {
        Target::PairingCode => "pairing_code",
        Target::PairingOffer => "pairing_offer",
        Target::Device => "device",
        Target::Permission => "permission",
        Target::Elicitation => "elicitation",
        Target::AgentSession => "agent_session",
        Target::AgentAuthentication => "agent_authentication",
        Target::WorkspaceFile => "workspace_file",
        Target::Git => "git",
        Target::Terminal => "terminal",
        Target::ProviderSettings => "provider_settings",
        Target::System => "system",
    }
}

/// The page's own navigation: which tab is open, which row the keyboard is on,
/// and which pairing row has its panel open.
#[derive(Debug, Clone, Default)]
pub struct DeviceCenter {
    pub tab: DeviceTab,
    /// The row cursor, one per tab, so leaving a tab and coming back lands
    /// where the reader left rather than at the top.
    cursor: std::collections::BTreeMap<&'static str, usize>,
    /// The pairing row whose panel is open, if any. One at a time: the panels
    /// are built from the same window and the same offer, and two open at once
    /// would be the same facts drawn twice.
    pub open: Option<PairMethod>,
    pub pairing: Pairing,
    pub publications: Publications,
}

impl DeviceCenter {
    pub fn cursor(&self, tab: DeviceTab) -> usize {
        self.cursor.get(tab.key()).copied().unwrap_or(0)
    }

    pub fn set_cursor(&mut self, tab: DeviceTab, row: usize) {
        self.cursor.insert(tab.key(), row);
    }

    /// How many rows the tab draws.
    pub fn row_count(&self, tab: DeviceTab, pair_rows: usize, devices: usize) -> usize {
        match tab {
            DeviceTab::Pair => pair_rows,
            DeviceTab::Devices => devices,
            DeviceTab::Publishing => self.publications.rows.len(),
        }
    }

    /// The row the keyboard is on, clamped to the rows that exist.
    ///
    /// A page whose rows shrank under the cursor — a window that closed, a
    /// runtime that lost a capability — keeps the cursor on a row rather than
    /// on nothing.
    pub fn clamped_cursor(&self, pair_rows: usize, devices: usize) -> usize {
        let count = self.row_count(self.tab, pair_rows, devices);
        if count == 0 {
            return 0;
        }
        self.cursor(self.tab).min(count - 1)
    }

    /// Switch halves. Answers what has to come down: the window and the offer
    /// both belong to the pairing rows, so leaving that half closes its panel
    /// and gives up what it was advertising — a code shown again after the
    /// reader walked away is shown twice, and one left claimable is worse.
    pub fn select_tab(&mut self, tab: DeviceTab) -> Release {
        if self.tab == tab {
            return Release::Nothing;
        }
        self.tab = tab;
        let release = self
            .open
            .take()
            .map_or(Release::Nothing, |method| self.pairing.release(method));
        self.pairing.clear();
        release
    }

    pub fn step_tab(&mut self, delta: i64, tabs: &[DeviceTab]) -> Release {
        let tabs = if tabs.is_empty() {
            &DeviceTab::ALWAYS[..]
        } else {
            tabs
        };
        let current = tabs.iter().position(|tab| *tab == self.tab).unwrap_or(0) as i64;
        let next = (current + delta).rem_euclid(tabs.len() as i64) as usize;
        self.select_tab(tabs[next])
    }

    /// Opens or closes one pairing row. The answer says what the press asks
    /// for, so the reducer owns the decision to perform I/O.
    pub fn toggle(&mut self, method: PairMethod) -> Activation {
        if self.open == Some(method) {
            self.open = None;
            return Activation::Closed {
                release: self.pairing.release(method),
            };
        }
        self.open = Some(method);
        Activation::Opened(method.opening())
    }

    /// Whether the panel behind the LAN row is up.
    pub fn lan_panel_open(&self) -> bool {
        self.tab == DeviceTab::Pair && self.open == Some(PairMethod::LocalNetwork)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const AVAILABLE: Availability = Availability::Available;
    const UNSUPPORTED: Availability = Availability::Unsupported;
    const UNKNOWN_ROUTE: LanPrecondition = LanPrecondition::Unknown;

    fn rows(kind: RemoteServerKind, pairing: Availability, lan: Availability) -> Vec<PairMethod> {
        pair_methods(kind, pairing, lan, UNKNOWN_ROUTE)
    }

    fn rows_with_direct(
        kind: RemoteServerKind,
        pairing: Availability,
        lan: Availability,
        direct: LanPrecondition,
    ) -> Vec<PairMethod> {
        pair_methods(kind, pairing, lan, direct)
    }

    fn offer() -> PairingOffer {
        PairingOffer {
            offer_id: vibex_core::RequestId::new(),
            code: "1234".to_string(),
            fragment: "/pair/x".to_string(),
            permission: RemoteDevicePermissionLevel::FullControl,
            expires_at_ms: i64::MAX,
            routes: vec![PairingRoute {
                transport: vibex_core::RemotePairingTransport::Direct,
                origin: "https://localhost:1428".to_string(),
            }],
            selected: 0,
            claimed: false,
        }
    }

    #[test]
    fn a_desktop_runtime_pairs_phones_and_a_headless_one_also_pairs_desktops() {
        assert_eq!(
            rows(RemoteServerKind::Desktop, AVAILABLE, AVAILABLE),
            vec![PairMethod::QrCode, PairMethod::LocalNetwork]
        );
        assert_eq!(
            rows(RemoteServerKind::Headless, AVAILABLE, AVAILABLE),
            vec![PairMethod::QrCode, PairMethod::CodeOrLink]
        );
    }

    #[test]
    fn a_runtime_without_lan_pairing_does_not_offer_the_row() {
        assert_eq!(
            rows(RemoteServerKind::Desktop, AVAILABLE, UNSUPPORTED),
            vec![PairMethod::QrCode]
        );
    }

    #[test]
    fn a_runtime_that_cannot_pair_offers_nothing() {
        assert!(rows(RemoteServerKind::Desktop, UNSUPPORTED, AVAILABLE).is_empty());
        // A peer that predates the kind still gets the offer: the QR is the one
        // route that never depended on knowing what the runtime is.
        assert_eq!(
            rows(RemoteServerKind::Unknown, AVAILABLE, AVAILABLE),
            vec![PairMethod::QrCode]
        );
        // And the local-network row is a desktop fact, so a peer of unknown
        // kind does not get it however available the capability looks.
        assert!(
            !rows(RemoteServerKind::Unknown, AVAILABLE, AVAILABLE)
                .contains(&PairMethod::LocalNetwork)
        );
    }

    #[test]
    fn the_audit_report_says_what_happened_rather_than_naming_variants() {
        use crate::locale::Locale;
        let strings = Strings::for_locale(Locale::En);
        assert_eq!(audit_report(&[], strings), strings.audit_empty());

        let record = vibex_core::RemoteAuditRecord {
            audit_id: vibex_core::RequestId::new(),
            device_id: Some(vibex_core::DeviceId::parse("device_phone").unwrap()),
            action: vibex_core::RemoteAuditAction::PairingOfferClaimed,
            target_kind: vibex_core::RemoteAuditTargetKind::PairingOffer,
            target_id: Some("request_offer".to_string()),
            outcome: vibex_core::RemoteAuditOutcome::Allowed,
            redacted_summary: "Device 'Pixel 8' paired".to_string(),
            request_id: Some(vibex_core::RequestId::parse("request_one").unwrap()),
            correlation_id: Some(vibex_core::CorrelationId::parse("correlation_one").unwrap()),
            created_at_ms: 1_772_000_000_000,
        };
        let report = audit_report(std::slice::from_ref(&record), strings);
        // The reader's words, not the payload's variant names.
        assert!(report.contains("Pairing offer claimed"), "{report}");
        assert!(report.contains("Allowed"), "{report}");
        assert!(!report.contains("PairingOfferClaimed"), "{report}");
        assert!(!report.contains("RemoteAuditAction"), "{report}");
        // The runtime's own sentence is the one part that says what happened,
        // and it used to be thrown away.
        assert!(report.contains("Device 'Pixel 8' paired"), "{report}");
        // Every identifier a support conversation needs is on the block.
        assert!(report.contains("device_phone"), "{report}");
        assert!(report.contains("pairing_offer:request_offer"), "{report}");
        assert!(report.contains("request_one"), "{report}");
        assert!(report.contains("correlation_one"), "{report}");
        // A record with nothing optional still renders its one line.
        let bare = vibex_core::RemoteAuditRecord {
            device_id: None,
            target_id: None,
            redacted_summary: String::new(),
            request_id: None,
            correlation_id: None,
            ..record
        };
        let report = audit_report(std::slice::from_ref(&bare), strings);
        assert_eq!(report.lines().count(), 1, "{report}");

        // The Chinese interfaces read the same record in Chinese.
        let zh = audit_report(
            std::slice::from_ref(&bare),
            Strings::for_locale(Locale::ZhCn),
        );
        assert!(zh.contains("配对邀请已被领取"), "{zh}");
        assert!(zh.contains("已允许"), "{zh}");
    }

    #[test]
    fn the_lan_row_is_absent_when_the_route_it_stands_on_is_down() {
        // The window is opened against the Direct method specifically, so a
        // runtime the reader can tell has no Direct route is not offered a verb
        // that would be refused.
        assert_eq!(
            rows_with_direct(
                RemoteServerKind::Desktop,
                AVAILABLE,
                AVAILABLE,
                LanPrecondition::Unreachable
            ),
            vec![PairMethod::QrCode]
        );
        // A route that is up keeps the row, and a runtime that does not report
        // its routes is not treated as if it had said "down".
        assert_eq!(
            rows_with_direct(
                RemoteServerKind::Desktop,
                AVAILABLE,
                AVAILABLE,
                LanPrecondition::Reachable
            ),
            vec![PairMethod::QrCode, PairMethod::LocalNetwork]
        );
        assert_eq!(
            rows_with_direct(
                RemoteServerKind::Desktop,
                AVAILABLE,
                AVAILABLE,
                LanPrecondition::Unknown
            ),
            vec![PairMethod::QrCode, PairMethod::LocalNetwork]
        );
    }

    #[test]
    fn the_lan_precondition_reads_only_a_reachable_direct_entry() {
        let mut publications = Publications::default();
        // Nothing has answered yet: not knowing is not a "no".
        assert_eq!(publications.lan_precondition(), LanPrecondition::Unknown);
        publications.settle(Ok(vec![publication(
            vibex_core::RemotePairingPublicationMethod::TailscaleServe,
            vibex_core::RemotePairingPublicationState::Online,
        )]));
        // A Tailnet route travels as a Direct candidate but is not the Direct
        // method the window is opened against.
        assert_eq!(publications.lan_precondition(), LanPrecondition::Unknown);
        publications.settle(Ok(vec![publication(
            vibex_core::RemotePairingPublicationMethod::Direct,
            vibex_core::RemotePairingPublicationState::Degraded,
        )]));
        assert_eq!(publications.lan_precondition(), LanPrecondition::Reachable);
        publications.settle(Ok(vec![publication(
            vibex_core::RemotePairingPublicationMethod::Direct,
            vibex_core::RemotePairingPublicationState::RepairRequired,
        )]));
        assert_eq!(
            publications.lan_precondition(),
            LanPrecondition::Unreachable
        );
        // A failed read knows nothing, so nothing is hidden on the strength of
        // it.
        publications.settle(Err("remote_pairing_publications_unavailable"));
        assert_eq!(publications.lan_precondition(), LanPrecondition::Unknown);
        assert!(publications.error.is_some());
    }

    fn publication(
        method: vibex_core::RemotePairingPublicationMethod,
        state: vibex_core::RemotePairingPublicationState,
    ) -> vibex_core::RemotePairingPublicationSummary {
        vibex_core::RemotePairingPublicationSummary {
            method,
            desired_enabled: true,
            state,
            origin: None,
            recovery_action: vibex_core::RemotePairingPublicationRecovery::None,
            error_code: None,
        }
    }

    #[test]
    fn leaving_the_pairing_tab_closes_its_panel_and_drops_the_offer() {
        let mut center = DeviceCenter::default();
        assert_eq!(
            center.toggle(PairMethod::QrCode),
            Activation::Opened(Opening::Offer)
        );
        center.pairing.offer = Some(offer());
        // The offer is handed back to the runtime rather than only forgotten:
        // a code the reader walked away from is still a code until it is taken
        // down.
        assert_eq!(center.select_tab(DeviceTab::Devices), Release::Offer);
        assert!(center.open.is_none());
        assert!(center.pairing.offer.is_none());
        center.select_tab(DeviceTab::Pair);
        assert_eq!(center.cursor(DeviceTab::Pair), 0);
        // A second hop has nothing left to give up.
        assert_eq!(center.select_tab(DeviceTab::Devices), Release::Nothing);
    }

    #[test]
    fn closing_the_window_panel_asks_for_the_advertisement_to_come_down() {
        let mut center = DeviceCenter::default();
        assert_eq!(
            center.toggle(PairMethod::LocalNetwork),
            Activation::Opened(Opening::Window)
        );
        assert!(center.lan_panel_open());
        // The row is open but no window has been opened yet, so there is
        // nothing to take down.
        assert_eq!(
            center.toggle(PairMethod::LocalNetwork),
            Activation::Closed {
                release: Release::Nothing
            }
        );
        assert!(!center.lan_panel_open());
        center.pairing.window = Some(window(Vec::new()));
        center.toggle(PairMethod::LocalNetwork);
        assert_eq!(
            center.toggle(PairMethod::LocalNetwork),
            Activation::Closed {
                release: Release::Window
            }
        );
        // The QR panel is not an advertisement, so a closed one with no offer
        // behind it asks for nothing.
        assert_eq!(
            center.toggle(PairMethod::QrCode),
            Activation::Opened(Opening::Offer)
        );
        assert_eq!(
            center.toggle(PairMethod::QrCode),
            Activation::Closed {
                release: Release::Nothing
            }
        );
    }

    #[test]
    fn an_offer_carries_every_published_route_and_defaults_to_the_desktops_pick() {
        let response = offer_response(vec![
            candidate(
                vibex_core::RemotePairingTransport::Direct,
                "https://lan:8443",
            ),
            candidate(
                vibex_core::RemotePairingTransport::Tailnet,
                "https://mac.ts.net",
            ),
            candidate(vibex_core::RemotePairingTransport::Unknown, "https://?"),
            candidate(
                vibex_core::RemotePairingTransport::Direct,
                "https://dup:8443",
            ),
        ]);
        let offer = PairingOffer::from_response(&response);
        // The order is the desktop's, and a method appears once however many
        // candidates carry it.
        assert_eq!(
            offer
                .routes
                .iter()
                .map(|route| route.transport)
                .collect::<Vec<_>>(),
            vec![
                vibex_core::RemotePairingTransport::Tailnet,
                vibex_core::RemotePairingTransport::Direct,
            ]
        );
        assert_eq!(offer.route_count(), 2);
        assert!(offer.route_is_chosen());
        // Tailnet first is the desktop's preference, so the client picks what
        // the desktop would have picked.
        assert_eq!(offer.selected, 0);
        assert_eq!(offer.url(), "vibex://open/tailnet#/pair/x");
        let mut offer = offer;
        assert!(offer.cycle_route(1));
        assert_eq!(offer.url(), "vibex://open/direct#/pair/x");
        assert!(offer.cycle_route(1));
        assert_eq!(offer.selected, 0);
        assert!(offer.cycle_route(-1));
        assert_eq!(offer.selected, 1);
    }

    #[test]
    fn a_single_route_offer_has_nothing_to_choose_between() {
        let response = offer_response(vec![candidate(
            vibex_core::RemotePairingTransport::SelfHostedRelay,
            "https://relay.example",
        )]);
        let mut offer = PairingOffer::from_response(&response);
        assert!(!offer.route_is_chosen());
        assert!(!offer.cycle_route(1));
        assert_eq!(offer.selected, 0);
        assert_eq!(offer.url(), "vibex://open/self_hosted_relay#/pair/x");
    }

    #[test]
    fn an_offer_with_no_route_still_names_one_the_phone_can_parse() {
        // A peer that published nothing gets the Direct segment: the link has to
        // stay a `vibex://` link the scanner accepts, and the runtime is the
        // side that decides whether it can be claimed.
        let offer = PairingOffer::from_response(&offer_response(Vec::new()));
        assert_eq!(offer.route_count(), 0);
        assert_eq!(offer.url(), "vibex://open/direct#/pair/x");
    }

    #[test]
    fn a_claimed_offer_is_spent_and_says_so() {
        let mut pairing = Pairing::default();
        let offer = offer();
        let offer_id = offer.offer_id.clone();
        pairing.offer = Some(offer);
        assert!(pairing.live_offer(0).is_some());
        assert!(!pairing.offer_was_claimed());
        // An answer about another offer says nothing about this one.
        assert!(!pairing.apply_offer_status(&summary(vibex_core::RequestId::new(), true)));
        assert!(pairing.live_offer(0).is_some());
        assert!(pairing.apply_offer_status(&summary(offer_id.clone(), true)));
        assert!(pairing.offer_was_claimed());
        // A claimed offer is not a code any more, so the panel stops drawing it
        // even though its deadline has not passed.
        assert!(pairing.live_offer(0).is_none());
        assert!(pairing.spent(PairMethod::QrCode, 0));
        // A claimed offer is already spent on the runtime, so leaving the half
        // has nothing left to take down.
        assert_eq!(pairing.release(PairMethod::QrCode), Release::Nothing);
        // Folding the same answer twice is not news.
        assert!(!pairing.apply_offer_status(&summary(offer_id, true)));
    }

    fn summary(
        offer_id: vibex_core::RequestId,
        claimed: bool,
    ) -> vibex_core::RemotePairingOfferSummary {
        vibex_core::RemotePairingOfferSummary {
            format_version: 2,
            protocol_range: vibex_core::RemoteProtocolVersionRange::v2(),
            server_id: "server".to_string(),
            server_identity_public_key: "key".to_string(),
            offer_id,
            expires_at_ms: i64::MAX,
            direct_candidates: Vec::new(),
            relay_candidate: None,
            permission_level: RemoteDevicePermissionLevel::FullControl,
            granted_permissions: Vec::new(),
            canceled: false,
            claimed_device_id: claimed.then(|| vibex_core::DeviceId::parse("device_one").unwrap()),
        }
    }

    fn candidate(
        transport: vibex_core::RemotePairingTransport,
        url: &str,
    ) -> vibex_core::RemotePairingCandidate {
        vibex_core::RemotePairingCandidate {
            transport,
            url: url.to_string(),
            relay_room_id: None,
            relay_pc_peer_id: None,
            relay_pc_public_key: None,
        }
    }

    fn offer_response(
        candidates: Vec<vibex_core::RemotePairingCandidate>,
    ) -> vibex_core::RemoteCreatePairingOfferResponse {
        let mut summary = summary(vibex_core::RequestId::new(), false);
        summary.direct_candidates = candidates
            .iter()
            .filter(|candidate| {
                candidate.transport != vibex_core::RemotePairingTransport::SelfHostedRelay
            })
            .cloned()
            .collect();
        summary.relay_candidate = candidates.into_iter().find(|candidate| {
            candidate.transport == vibex_core::RemotePairingTransport::SelfHostedRelay
        });
        vibex_core::RemoteCreatePairingOfferResponse {
            offer: vibex_core::RemotePairingOffer {
                summary,
                one_time_challenge: "1234".to_string(),
            },
            launch_fragment: "#/pair/x".to_string(),
        }
    }

    #[test]
    fn the_pending_request_is_the_oldest_one_still_waiting() {
        let mut pairing = Pairing::default();
        assert!(pairing.pending().is_none());
        assert_eq!(pairing.waiting(), 0);
        pairing.window = Some(window(vec![
            ("older", RemoteLanPairingRequestState::Pending),
            ("newer", RemoteLanPairingRequestState::Pending),
            ("settled", RemoteLanPairingRequestState::Approved),
        ]));
        assert_eq!(
            pairing
                .pending()
                .map(|request| request.display_name.as_str()),
            Some("older")
        );
        assert_eq!(pairing.waiting(), 2);
    }

    fn window(
        requests: Vec<(&str, RemoteLanPairingRequestState)>,
    ) -> RemoteLanPairingWindowSnapshot {
        RemoteLanPairingWindowSnapshot {
            discovery: vibex_core::RemoteLanPairingDiscoverySummary {
                schema_version: "v1".to_string(),
                window_id: vibex_core::RequestId::new(),
                server_id: "server".to_string(),
                server_identity_public_key: "key".to_string(),
                protocol_range: vibex_core::RemoteProtocolVersionRange::v2(),
                permission_level: RemoteDevicePermissionLevel::FullControl,
                expires_at_ms: 1_000,
            },
            advertisement: vibex_core::RemoteLanPairingAdvertisement {
                advertisement_id: "ad".to_string(),
                service_instance: "instance".to_string(),
                display_name: "runtime".to_string(),
                direct_origin: "https://localhost:1428".to_string(),
                protocol_min: 2,
                protocol_max: 2,
            },
            pending_requests: requests
                .into_iter()
                .map(
                    |(name, state)| vibex_core::RemoteLanPairingPendingRequestSummary {
                        request_id: vibex_core::RequestId::new(),
                        display_name: name.to_string(),
                        device_fingerprint: "A1B2C3".to_string(),
                        verification_code: "4821".to_string(),
                        state,
                        expires_at_ms: 1_000,
                    },
                )
                .collect(),
        }
    }
}

use std::{collections::BTreeSet, future::Future, sync::Arc, time::Duration};

use gpui::{
    Anchor, AnyElement, App, ClipboardItem, Context, Entity, IntoElement, KeyDownEvent, Render,
    RenderImage, Role, SharedString, Subscription, Task, WeakEntity, Window, div, img, prelude::*,
    px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, Size, StyledExt as _, Theme,
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogAction, DialogButtonProps, DialogClose, DialogFooter},
    empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle},
    h_flex,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    pagination::Pagination,
    tab::{Tab, TabBar},
    tag::Tag,
    v_flex,
};
use image::{Frame, Rgba, RgbaImage};
use qrcode::{Color as QrColor, EcLevel, QrCode};
#[cfg(feature = "e2e-test-support")]
use serde::{Deserialize, Serialize};
use url::Url;
use vibex_core::{
    DeviceId, RemoteAuditListRequest, RemoteCreatePairingOfferResponse, RemoteDeleteDeviceRequest,
    RemoteDeviceDetail, RemoteDevicePermissionLevel, RemoteDeviceStatus,
    RemoteLanPairingRequestState, RemoteLanPairingWindowSnapshot, RemotePairingOfferSummary,
    RemotePairingTransport, RemoteRenameDeviceRequest, RemoteRestoreDeviceRequest,
    RemoteRevokeDeviceRequest, RequestId, VibexError, VibexResult, unix_timestamp_ms,
};
use vibex_desktop_runtime::{
    DesktopRuntime, RemoteConnectivityController, RemoteConnectivityMethod,
    RemoteConnectivitySnapshot, RemoteHandle, RemoteMethodState, RemoteRecoveryAction,
    normalize_https_origin,
};

use crate::{
    gpui_ext::{DOCS_REMOTE_MOBILE_URL, docs_help_button},
    locale, spinner::Spinner, theme,
};

const PAIRING_OFFER_TTL_MS: u32 = 90_000;
const OFFER_POLL_INTERVAL: Duration = Duration::from_millis(500);
const QR_QUIET_ZONE_MODULES: usize = 4;
const QR_MODULE_SCALE: usize = 2;
const DIALOG_MAX_WIDTH: f32 = 760.0;
/// The trust store clamps an audit read to this many records, so the count the
/// dialog shows is a floor rather than a total once it is reached.
const DEVICE_AUDIT_COUNT_LIMIT: u32 = 500;
/// Paired devices one page holds. The trust store has no paged read, so the
/// dialog loads the registry once and windows it here.
const DEVICE_PAGE_SIZE: usize = 6;
/// How often the device list re-reads which devices are connected. Presence is
/// a live fact that changes without any trust-store write, so the list polls it
/// while it is open rather than showing the state it saw when the dialog opened.
const DEVICE_PRESENCE_POLL_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteAccessMutation {
    Enable(RemoteConnectivityMethod),
    Disable(RemoteConnectivityMethod),
    Repair(RemoteConnectivityMethod),
    DisableAll,
    CreateOffer,
    RegenerateOffer,
    CancelOffer,
    CancelLanPairing,
    ApproveLanPairing,
    RejectLanPairing,
    StartZeroConfigPairing,
    CancelZeroConfigPairing,
    ApproveZeroConfigPairing,
    RejectZeroConfigPairing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteAccessEntry {
    TailscaleServe,
    Direct,
    SelfHostedRelay,
    LocalNetwork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteAccessPage {
    Setup,
    Devices,
    Pairing,
}

impl RemoteAccessEntry {
    fn from_remote_method(method: RemoteConnectivityMethod) -> Self {
        match method {
            RemoteConnectivityMethod::TailscaleServe => Self::TailscaleServe,
            RemoteConnectivityMethod::Direct => Self::Direct,
            RemoteConnectivityMethod::SelfHostedRelay => Self::SelfHostedRelay,
        }
    }

    fn remote_method(self) -> Option<RemoteConnectivityMethod> {
        match self {
            Self::TailscaleServe => Some(RemoteConnectivityMethod::TailscaleServe),
            Self::Direct => Some(RemoteConnectivityMethod::Direct),
            Self::SelfHostedRelay => Some(RemoteConnectivityMethod::SelfHostedRelay),
            Self::LocalNetwork => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteAccessAction {
    Refresh,
    SelectConnectionEntry(RemoteAccessEntry),
    EnableMethod(RemoteConnectivityMethod),
    ConfirmTailscalePort(u16),
    DisableMethod(RemoteConnectivityMethod),
    RepairMethod(RemoteConnectivityMethod),
    DisableAll,
    SetPermission(RemoteDevicePermissionLevel),
    SetZeroConfigPermission(RemoteDevicePermissionLevel),
    CreateOffer,
    RegenerateOffer,
    CancelOffer,
    SelectEntry(RemoteConnectivityMethod),
    CopyLink,
    CancelLanPairing,
    ApproveLanPairing(RequestId),
    RejectLanPairing(RequestId),
    StartZeroConfigPairing,
    CancelZeroConfigPairing,
    ApproveZeroConfigPairing(RequestId),
    RejectZeroConfigPairing(RequestId),
    ShowSetup,
    ShowDevices,
    ShowPairing,
    RefreshDevices,
    SelectDevicePage(usize),
    RevokeDevice(String),
    RestoreDevice(String),
    DeleteDevice(String),
    RenameDevice {
        device_id: String,
        display_name: String,
    },
}

#[derive(Clone, PartialEq, Eq)]
struct PairingEntry {
    method: RemoteConnectivityMethod,
}

struct PrivateOfferMaterial {
    launch_fragment: String,
    launch_url: Url,
    qr_image: Arc<RenderImage>,
    qr_size_px: u32,
}

struct ActivePairingOffer {
    summary: RemotePairingOfferSummary,
    entries: Vec<PairingEntry>,
    selected_entry: RemoteConnectivityMethod,
    qr_size_px: u32,
    private: Option<PrivateOfferMaterial>,
}

impl ActivePairingOffer {
    fn from_response(
        response: RemoteCreatePairingOfferResponse,
        preferred_entry: Option<RemoteConnectivityMethod>,
    ) -> VibexResult<Self> {
        let entries = pairing_entries(&response.offer.summary);
        let selected_entry =
            preferred_pairing_entry(&entries, preferred_entry).ok_or_else(|| {
                VibexError::capability(
                    "remote_pairing_routes_unavailable",
                    "pairing offer has no usable mobile entry",
                )
            })?;
        let private = compose_private_offer(selected_entry, response.launch_fragment)?;
        let qr_size_px = private.qr_size_px;
        Ok(Self {
            summary: response.offer.summary,
            entries,
            selected_entry,
            qr_size_px,
            private: Some(private),
        })
    }

    fn offer_id(&self) -> &RequestId {
        &self.summary.offer_id
    }

    fn select_entry(&mut self, method: RemoteConnectivityMethod) -> VibexResult<()> {
        if self.selected_entry == method {
            return Ok(());
        }
        if !self.entries.iter().any(|entry| entry.method == method) {
            return Err(VibexError::validation(
                "remote_pairing_entry_not_offered",
                "selected pairing entry is not part of the offer",
            ));
        }
        let launch_fragment = self
            .private
            .as_ref()
            .map(|private| private.launch_fragment.clone())
            .ok_or_else(|| {
                VibexError::conflict(
                    "remote_pairing_offer_unavailable",
                    "pairing offer is no longer available",
                )
            })?;
        let private = compose_private_offer(method, launch_fragment)?;
        self.qr_size_px = private.qr_size_px;
        self.private = Some(private);
        self.selected_entry = method;
        Ok(())
    }

    fn apply_status(&mut self, summary: RemotePairingOfferSummary, now_ms: i64) {
        if summary.offer_id != self.summary.offer_id {
            return;
        }
        self.summary = summary;
        if self.is_terminal(now_ms) {
            self.private = None;
        }
    }

    fn is_expired(&self, now_ms: i64) -> bool {
        now_ms >= self.summary.expires_at_ms
    }

    fn is_terminal(&self, now_ms: i64) -> bool {
        self.summary.canceled || self.summary.claimed_device_id.is_some() || self.is_expired(now_ms)
    }

    fn remaining_seconds(&self, now_ms: i64) -> u64 {
        self.summary
            .expires_at_ms
            .saturating_sub(now_ms)
            .saturating_add(999)
            .div_euclid(1_000) as u64
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteAccessSafeSnapshot {
    has_connectivity_snapshot: bool,
    method_count: usize,
    available_entry_count: usize,
    selected_method: RemoteConnectivityMethod,
    permission: RemoteDevicePermissionLevel,
    pending: Option<RemoteAccessMutation>,
    has_offer: bool,
    has_qr: bool,
    offer_claimed: bool,
    offer_canceled: bool,
    error_code: Option<String>,
}

#[cfg(feature = "e2e-test-support")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAccessPairingE2eMethodSnapshot {
    pub method: RemoteConnectivityMethod,
    pub desired_enabled: bool,
    pub state: RemoteMethodState,
    pub candidate_available: bool,
    pub https_port: Option<u16>,
    pub recovery_action: RemoteRecoveryAction,
    pub error_code: Option<String>,
}

#[cfg(feature = "e2e-test-support")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAccessPairingE2eOfferStatus {
    None,
    Active,
    Claimed,
    Canceled,
    Expired,
}

#[cfg(feature = "e2e-test-support")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAccessPairingE2eSnapshot {
    pub schema_version: &'static str,
    pub has_connectivity_snapshot: bool,
    pub desired_enabled: bool,
    pub running: bool,
    pub generation: u64,
    pub methods: Vec<RemoteAccessPairingE2eMethodSnapshot>,
    pub active_route: Option<RemoteConnectivityMethod>,
    pub selected_method: RemoteConnectivityMethod,
    pub selected_entry: Option<RemoteConnectivityMethod>,
    pub permission: RemoteDevicePermissionLevel,
    pub pending_action: Option<&'static str>,
    pub available_entry_count: usize,
    pub offer_status: RemoteAccessPairingE2eOfferStatus,
    pub has_qr: bool,
    pub proposed_tailscale_port: Option<u16>,
    pub error_code: Option<String>,
}

#[cfg(feature = "e2e-test-support")]
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RemoteAccessPairingE2eAction {
    Refresh,
    SelectMethod {
        method: RemoteConnectivityMethod,
    },
    ConfigureOrigin {
        method: RemoteConnectivityMethod,
        origin: String,
    },
    EnableMethod {
        method: RemoteConnectivityMethod,
    },
    ConfirmTailscalePort {
        port: u16,
    },
    DisableMethod {
        method: RemoteConnectivityMethod,
    },
    RepairMethod {
        method: RemoteConnectivityMethod,
    },
    DisableAll,
    SetPermission {
        permission: RemoteDevicePermissionLevel,
    },
    CreateOffer,
    RegenerateOffer,
    CancelOffer,
    SelectEntry {
        method: RemoteConnectivityMethod,
    },
}

struct PairingViewState {
    connectivity: Option<RemoteConnectivitySnapshot>,
    page: RemoteAccessPage,
    selected_method: RemoteConnectivityMethod,
    selected_entry: RemoteAccessEntry,
    permission: RemoteDevicePermissionLevel,
    zero_config_permission: RemoteDevicePermissionLevel,
    active_offer: Option<ActivePairingOffer>,
    active_lan_window: Option<RemoteLanPairingWindowSnapshot>,
    active_zero_config_window: Option<RemoteLanPairingWindowSnapshot>,
    pending: Option<RemoteAccessMutation>,
    error_code: Option<String>,
    notice: Option<RemoteAccessNotice>,
    devices: Vec<RemoteDeviceDetail>,
    devices_loaded: bool,
    devices_error: Option<String>,
    device_page: usize,
    audit_count: usize,
    audit_count_capped: bool,
    revoking_device: Option<String>,
    restoring_device: Option<String>,
    deleting_device: Option<String>,
    renaming_device: Option<String>,
    /// Device ids with a live connection, refreshed by the presence poll while
    /// the device list is open.
    connected_devices: BTreeSet<String>,
}

/// One light hint the Remote Access page has to show.
///
/// The hint is an answer to something the user just did — a pairing link copied,
/// a device paired, a pairing stopped — so it is announced on the notification
/// layer instead of occupying a banner in the page until the next action clears
/// it. The tone is kept because a failed clipboard write is not a success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RemoteAccessNotice {
    message: &'static str,
    tone: RemoteAccessNoticeTone,
}

impl RemoteAccessNotice {
    fn success(message: &'static str) -> Self {
        Self {
            message,
            tone: RemoteAccessNoticeTone::Success,
        }
    }

    fn error(message: &'static str) -> Self {
        Self {
            message,
            tone: RemoteAccessNoticeTone::Error,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteAccessNoticeTone {
    Success,
    Error,
}

/// Names the Remote Access hint on the notification layer, so a newer hint
/// replaces the one still on screen instead of stacking behind it.
struct RemoteAccessNoticeNotification;

impl Default for PairingViewState {
    fn default() -> Self {
        Self {
            connectivity: None,
            page: RemoteAccessPage::Setup,
            selected_method: RemoteConnectivityMethod::TailscaleServe,
            selected_entry: RemoteAccessEntry::TailscaleServe,
            permission: RemoteDevicePermissionLevel::ReadOnly,
            zero_config_permission: RemoteDevicePermissionLevel::ReadOnly,
            active_offer: None,
            active_lan_window: None,
            active_zero_config_window: None,
            pending: None,
            error_code: None,
            notice: None,
            devices: Vec::new(),
            devices_loaded: false,
            devices_error: None,
            device_page: 1,
            audit_count: 0,
            audit_count_capped: false,
            revoking_device: None,
            restoring_device: None,
            deleting_device: None,
            renaming_device: None,
            connected_devices: BTreeSet::new(),
        }
    }
}

impl PairingViewState {
    fn apply_connectivity(&mut self, snapshot: RemoteConnectivitySnapshot) {
        self.connectivity = Some(snapshot);
    }

    fn show_setup(&mut self) {
        self.page = RemoteAccessPage::Setup;
        self.error_code = None;
    }

    fn show_pairing(&mut self) {
        self.page = RemoteAccessPage::Pairing;
        self.error_code = None;
    }

    fn show_devices(&mut self) {
        self.page = RemoteAccessPage::Devices;
        self.error_code = None;
    }

    /// Total pages the paired-device list is windowed into, never below one so
    /// the pager keeps a stable single-page state instead of disappearing.
    fn device_page_count(&self) -> usize {
        self.devices.len().div_ceil(DEVICE_PAGE_SIZE).max(1)
    }

    /// The slice of the registry the current page shows.
    fn device_page_slice(&self) -> &[RemoteDeviceDetail] {
        let start = self
            .device_page
            .saturating_sub(1)
            .saturating_mul(DEVICE_PAGE_SIZE);
        let end = start
            .saturating_add(DEVICE_PAGE_SIZE)
            .min(self.devices.len());
        self.devices.get(start..end).unwrap_or_default()
    }

    /// Selects a page, clamped so a shrinking list can never strand the pager
    /// on a page that no longer holds rows.
    fn select_device_page(&mut self, page: usize) {
        self.device_page = page.clamp(1, self.device_page_count());
    }

    /// Whether a device mutation is in flight. The list locks every per-row
    /// action while one runs, so a delete cannot race the revoke it depends on.
    fn device_mutation_pending(&self) -> bool {
        self.revoking_device.is_some()
            || self.restoring_device.is_some()
            || self.deleting_device.is_some()
            || self.renaming_device.is_some()
    }

    fn select_connection_entry(&mut self, entry: RemoteAccessEntry) {
        if let Some(method) = entry.remote_method() {
            self.selected_method = method;
        }
        self.selected_entry = entry;
        self.error_code = None;
    }

    fn preferred_entry(&self) -> Option<RemoteConnectivityMethod> {
        self.connectivity
            .as_ref()
            .and_then(|snapshot| snapshot.last_successful_pairing_entry)
    }

    fn install_offer(&mut self, response: RemoteCreatePairingOfferResponse) -> VibexResult<()> {
        self.active_offer = None;
        let offer = ActivePairingOffer::from_response(response, self.preferred_entry())?;
        self.permission = offer.summary.permission_level;
        self.active_offer = Some(offer);
        self.notice = None;
        Ok(())
    }

    fn can_regenerate_offer(&self) -> bool {
        self.active_offer.is_some() && self.pending.is_none()
    }

    fn can_start_zero_config_pairing(&self) -> bool {
        self.pending.is_none()
            && self.active_offer.is_none()
            && self.active_lan_window.is_none()
            && self.active_zero_config_window.is_none()
    }

    fn safe_snapshot(&self) -> RemoteAccessSafeSnapshot {
        let offer = self.active_offer.as_ref();
        RemoteAccessSafeSnapshot {
            has_connectivity_snapshot: self.connectivity.is_some(),
            method_count: self
                .connectivity
                .as_ref()
                .map_or(0, |snapshot| snapshot.methods.len()),
            available_entry_count: self.connectivity.as_ref().map_or(0, |snapshot| {
                snapshot
                    .methods
                    .iter()
                    .filter(|method| method.candidate_available)
                    .count()
            }),
            selected_method: self.selected_method,
            permission: self.permission,
            pending: self.pending,
            has_offer: offer.is_some(),
            has_qr: offer.and_then(|offer| offer.private.as_ref()).is_some(),
            offer_claimed: offer.is_some_and(|offer| offer.summary.claimed_device_id.is_some()),
            offer_canceled: offer.is_some_and(|offer| offer.summary.canceled),
            error_code: self.error_code.clone(),
        }
    }

    #[cfg(feature = "e2e-test-support")]
    fn e2e_snapshot(&self) -> RemoteAccessPairingE2eSnapshot {
        let safe = self.safe_snapshot();
        let now_ms = unix_timestamp_ms();
        let connectivity = self.connectivity.as_ref();
        let offer = self.active_offer.as_ref();
        let offer_status = match offer {
            None => RemoteAccessPairingE2eOfferStatus::None,
            Some(offer) if offer.summary.claimed_device_id.is_some() => {
                RemoteAccessPairingE2eOfferStatus::Claimed
            }
            Some(offer) if offer.summary.canceled => RemoteAccessPairingE2eOfferStatus::Canceled,
            Some(offer) if offer.is_expired(now_ms) => RemoteAccessPairingE2eOfferStatus::Expired,
            Some(_) => RemoteAccessPairingE2eOfferStatus::Active,
        };
        let proposed_tailscale_port = connectivity
            .and_then(|snapshot| snapshot.method(RemoteConnectivityMethod::TailscaleServe))
            .filter(|snapshot| snapshot.recovery_action == RemoteRecoveryAction::ConfirmPort)
            .and_then(|snapshot| snapshot.https_port);
        RemoteAccessPairingE2eSnapshot {
            schema_version: "remote-access-pairing-e2e.v1",
            has_connectivity_snapshot: safe.has_connectivity_snapshot,
            desired_enabled: connectivity.is_some_and(|snapshot| snapshot.desired_enabled),
            running: connectivity.is_some_and(|snapshot| snapshot.running),
            generation: connectivity.map_or(0, |snapshot| snapshot.generation),
            methods: connectivity
                .map(|snapshot| {
                    snapshot
                        .methods
                        .iter()
                        .map(|method| RemoteAccessPairingE2eMethodSnapshot {
                            method: method.method,
                            desired_enabled: method.desired_enabled,
                            state: method.state,
                            candidate_available: method.candidate_available,
                            https_port: method.https_port,
                            recovery_action: method.recovery_action,
                            error_code: method.error_code.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            active_route: connectivity.and_then(|snapshot| snapshot.active_route),
            selected_method: safe.selected_method,
            selected_entry: offer.map(|offer| offer.selected_entry),
            permission: safe.permission,
            pending_action: safe.pending.map(remote_access_mutation_name),
            available_entry_count: safe.available_entry_count,
            offer_status,
            has_qr: safe.has_qr,
            proposed_tailscale_port,
            error_code: safe.error_code,
        }
    }
}

enum RemoteAccessMutationOutcome {
    Connectivity(RemoteConnectivitySnapshot),
    DisabledAll(RemoteConnectivitySnapshot),
    OfferCreated(RemoteCreatePairingOfferResponse),
    OfferCreationFailed(VibexError),
    OfferCanceled,
    LanWindow(RemoteLanPairingWindowSnapshot),
    LanCanceled,
    ZeroConfigWindow(RemoteLanPairingWindowSnapshot),
    ZeroConfigCanceled,
}

struct OfferPollOutcome {
    summary: RemotePairingOfferSummary,
}

pub(crate) struct RemoteAccessPairing {
    controller: RemoteConnectivityController,
    remote: RemoteHandle,
    state: PairingViewState,
    direct_origin: Entity<InputState>,
    relay_origin: Entity<InputState>,
    refresh_task: Option<Task<()>>,
    devices_task: Option<Task<()>>,
    revoke_task: Option<Task<()>>,
    restore_task: Option<Task<()>>,
    delete_task: Option<Task<()>>,
    rename_task: Option<Task<()>>,
    presence_poll_task: Option<Task<()>>,
    mutation_task: Option<Task<()>>,
    offer_poll_task: Option<Task<()>>,
    lan_poll_task: Option<Task<()>>,
    zero_config_poll_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl RemoteAccessPairing {
    fn new(runtime: Arc<DesktopRuntime>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let direct_origin = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://desktop.example")
                .submit_on_enter(true)
        });
        let relay_origin = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://relay.example")
                .submit_on_enter(true)
        });
        let subscriptions = vec![
            cx.subscribe_in(
                &direct_origin,
                window,
                |this, _, event, _, cx| match event {
                    InputEvent::PressEnter { shift: false, .. } => this.dispatch_action(
                        RemoteAccessAction::EnableMethod(RemoteConnectivityMethod::Direct),
                        cx,
                    ),
                    InputEvent::Change
                    | InputEvent::Focus
                    | InputEvent::Blur
                    | InputEvent::PressEnter { shift: true, .. } => cx.notify(),
                },
            ),
            cx.subscribe_in(&relay_origin, window, |this, _, event, _, cx| match event {
                InputEvent::PressEnter { shift: false, .. } => this.dispatch_action(
                    RemoteAccessAction::EnableMethod(RemoteConnectivityMethod::SelfHostedRelay),
                    cx,
                ),
                InputEvent::Change
                | InputEvent::Focus
                | InputEvent::Blur
                | InputEvent::PressEnter { shift: true, .. } => cx.notify(),
            }),
        ];
        Self {
            controller: runtime.remote_connectivity(),
            remote: runtime.management().remote(),
            state: PairingViewState::default(),
            direct_origin,
            relay_origin,
            refresh_task: None,
            devices_task: None,
            revoke_task: None,
            restore_task: None,
            delete_task: None,
            rename_task: None,
            presence_poll_task: None,
            mutation_task: None,
            offer_poll_task: None,
            lan_poll_task: None,
            zero_config_poll_task: None,
            _subscriptions: subscriptions,
        }
    }

    fn dispatch_action(&mut self, action: RemoteAccessAction, cx: &mut Context<Self>) {
        match action {
            RemoteAccessAction::Refresh => self.refresh(cx),
            RemoteAccessAction::ShowSetup => {
                self.state.show_setup();
                self.stop_presence_poll();
                cx.notify();
            }
            RemoteAccessAction::ShowPairing => {
                self.state.show_pairing();
                self.stop_presence_poll();
                cx.notify();
            }
            RemoteAccessAction::ShowDevices => {
                self.state.show_devices();
                // The list may have been opened after the device connected, and
                // presence is not part of the stored registry.
                self.refresh_devices(cx);
                self.schedule_presence_poll(cx);
                cx.notify();
            }
            RemoteAccessAction::RefreshDevices => self.refresh_devices(cx),
            RemoteAccessAction::SelectDevicePage(page) => {
                self.state.select_device_page(page);
                cx.notify();
            }
            RemoteAccessAction::RevokeDevice(device_id) => self.revoke_device(device_id, cx),
            RemoteAccessAction::RestoreDevice(device_id) => self.restore_device(device_id, cx),
            RemoteAccessAction::DeleteDevice(device_id) => self.delete_device(device_id, cx),
            RemoteAccessAction::RenameDevice {
                device_id,
                display_name,
            } => self.rename_device(device_id, display_name, cx),
            RemoteAccessAction::SelectConnectionEntry(entry) => {
                self.state.select_connection_entry(entry);
                cx.notify();
            }
            RemoteAccessAction::EnableMethod(method) => self.enable_method(method, cx),
            RemoteAccessAction::ConfirmTailscalePort(port) => self.confirm_tailscale_port(port, cx),
            RemoteAccessAction::DisableMethod(method) => self.disable_method(method, cx),
            RemoteAccessAction::RepairMethod(method) => self.repair_method(method, cx),
            RemoteAccessAction::DisableAll => self.disable_all(cx),
            RemoteAccessAction::SetPermission(permission) => self.set_permission(permission, cx),
            RemoteAccessAction::SetZeroConfigPermission(permission) => {
                self.set_zero_config_permission(permission, cx)
            }
            RemoteAccessAction::CreateOffer => self.create_offer(cx),
            RemoteAccessAction::RegenerateOffer => self.regenerate_offer(self.state.permission, cx),
            RemoteAccessAction::CancelOffer => self.cancel_offer(cx),
            RemoteAccessAction::SelectEntry(method) => self.select_entry(method, cx),
            RemoteAccessAction::CopyLink => self.copy_pairing_link(cx),
            RemoteAccessAction::CancelLanPairing => self.cancel_lan_pairing(cx),
            RemoteAccessAction::ApproveLanPairing(request_id) => {
                self.approve_lan_pairing(request_id, cx)
            }
            RemoteAccessAction::RejectLanPairing(request_id) => {
                self.reject_lan_pairing(request_id, cx)
            }
            RemoteAccessAction::StartZeroConfigPairing => self.start_zero_config_pairing(cx),
            RemoteAccessAction::CancelZeroConfigPairing => self.cancel_zero_config_pairing(cx),
            RemoteAccessAction::ApproveZeroConfigPairing(request_id) => {
                self.approve_zero_config_pairing(request_id, cx)
            }
            RemoteAccessAction::RejectZeroConfigPairing(request_id) => {
                self.reject_zero_config_pairing(request_id, cx)
            }
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_devices(cx);
        let controller = self.controller.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move { controller.snapshot().await });
        self.refresh_task = Some(cx.spawn(
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let outcome = runner.await;
                let _ = entity.update(cx, |this, cx| {
                    this.refresh_task = None;
                    match outcome {
                        Ok(snapshot) => this.state.apply_connectivity(snapshot),
                        Err(_) => {
                            this.state.error_code =
                                Some("remote_connectivity_snapshot_failed".to_string())
                        }
                    }
                    cx.notify();
                });
            },
        ));
    }

    /// Reads the paired-device registry out of the runtime trust store.
    ///
    /// This dialog owns both halves of remote access, so the device list is
    /// loaded by the same refresh that fetches connectivity and again whenever
    /// a pairing or a revoke changes the registry.
    fn refresh_devices(&mut self, cx: &mut Context<Self>) {
        let remote = self.remote.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            let devices = remote.list_devices();
            let audit_count = remote
                .list_audit(RemoteAuditListRequest {
                    device_id: None,
                    limit: Some(DEVICE_AUDIT_COUNT_LIMIT),
                })
                .map(|records| records.len())
                .unwrap_or_default();
            let connected = remote.connected_device_ids();
            (devices, audit_count, connected)
        });
        self.devices_task = Some(cx.spawn(
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let outcome = runner.await;
                let _ = entity.update(cx, |this, cx| {
                    this.devices_task = None;
                    match outcome {
                        Ok((Ok(devices), audit_count, connected)) => {
                            this.state.devices = devices;
                            this.state.audit_count = audit_count;
                            this.state.audit_count_capped =
                                audit_count >= DEVICE_AUDIT_COUNT_LIMIT as usize;
                            this.state.connected_devices = connected_device_keys(connected);
                            this.state.devices_loaded = true;
                            this.state.devices_error = None;
                            // A revoke can empty the page the pager was on.
                            this.state.select_device_page(this.state.device_page);
                        }
                        Ok((Err(error), _, _)) => this.state.devices_error = Some(error.code),
                        Err(_) => {
                            this.state.devices_error =
                                Some("remote_device_list_task_failed".to_string())
                        }
                    }
                    cx.notify();
                });
            },
        ));
    }

    /// Refreshes only the presence set while the device list stays open.
    ///
    /// Presence is a connection-registry fact, not a stored one, so it is read
    /// on a timer instead of being written to the trust store. The loop stops
    /// with the dialog: [`Self::dismiss`] drops the task, and the page guard
    /// keeps a task that already woke from rescheduling.
    fn schedule_presence_poll(&mut self, cx: &mut Context<Self>) {
        if self.state.page != RemoteAccessPage::Devices {
            return;
        }
        let remote = self.remote.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            tokio::time::sleep(DEVICE_PRESENCE_POLL_INTERVAL).await;
            remote.connected_device_ids()
        });
        self.presence_poll_task = Some(cx.spawn(
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let outcome = runner.await;
                let _ = entity.update(cx, |this, cx| {
                    this.presence_poll_task = None;
                    if let Ok(connected) = outcome {
                        let connected = connected_device_keys(connected);
                        if connected != this.state.connected_devices {
                            this.state.connected_devices = connected;
                            cx.notify();
                        }
                    }
                    this.schedule_presence_poll(cx);
                });
            },
        ));
    }

    /// Stops the presence loop by dropping its task.
    ///
    /// The device list owns the only reason to poll, so leaving the page — or
    /// closing the dialog — drops the pending timer instead of letting it
    /// reschedule.
    fn stop_presence_poll(&mut self) {
        self.presence_poll_task = None;
    }

    fn revoke_device(&mut self, device_id: String, cx: &mut Context<Self>) {
        let Ok(device_id_value) = DeviceId::parse(device_id.clone()) else {
            self.state.devices_error = Some("remote_device_id_invalid".to_string());
            cx.notify();
            return;
        };
        let remote = self.remote.clone();
        self.state.revoking_device = Some(device_id);
        self.state.error_code = None;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            remote.revoke_device(RemoteRevokeDeviceRequest {
                device_id: device_id_value,
                reason: Some("revoked from the mobile pairing dialog".to_string()),
            })
        });
        self.revoke_task = Some(cx.spawn(
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let outcome = runner.await;
                let _ = entity.update(cx, |this, cx| {
                    this.revoke_task = None;
                    this.state.revoking_device = None;
                    match outcome {
                        Ok(Ok(_)) => {
                            this.state.notice = Some(RemoteAccessNotice::success(locale::text(
                                "Device access revoked",
                                "已撤销设备访问权限",
                                "已撤銷裝置存取權限",
                            )));
                            this.refresh_devices(cx);
                        }
                        Ok(Err(error)) => {
                            this.state.notice = Some(RemoteAccessNotice::error(locale::text(
                                "The device could not be revoked",
                                "撤销设备失败",
                                "撤銷裝置失敗",
                            )));
                            this.state.devices_error = Some(error.code);
                        }
                        Err(_) => {
                            this.state.devices_error =
                                Some("remote_device_revoke_task_failed".to_string())
                        }
                    }
                    cx.notify();
                });
            },
        ));
    }

    /// Returns a revoked device to service.
    ///
    /// The grant it was paired with is kept, so the phone reconnects with the
    /// credential it already holds instead of pairing again.
    fn restore_device(&mut self, device_id: String, cx: &mut Context<Self>) {
        let Ok(device_id_value) = DeviceId::parse(device_id.clone()) else {
            self.state.devices_error = Some("remote_device_id_invalid".to_string());
            cx.notify();
            return;
        };
        let remote = self.remote.clone();
        self.state.restoring_device = Some(device_id);
        self.state.error_code = None;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            remote.restore_device(RemoteRestoreDeviceRequest {
                device_id: device_id_value,
                reason: Some("restored from the mobile pairing dialog".to_string()),
            })
        });
        self.restore_task = Some(cx.spawn(
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let outcome = runner.await;
                let _ = entity.update(cx, |this, cx| {
                    this.restore_task = None;
                    this.state.restoring_device = None;
                    match outcome {
                        Ok(Ok(_)) => {
                            this.state.notice = Some(RemoteAccessNotice::success(locale::text(
                                "Device access restored",
                                "已恢复设备访问权限",
                                "已恢復裝置存取權限",
                            )));
                            this.refresh_devices(cx);
                        }
                        Ok(Err(error)) => {
                            this.state.notice = Some(RemoteAccessNotice::error(locale::text(
                                "The device access could not be restored",
                                "恢复设备访问权限失败",
                                "恢復裝置存取權限失敗",
                            )));
                            this.state.devices_error = Some(error.code);
                        }
                        Err(_) => {
                            this.state.devices_error =
                                Some("remote_device_restore_task_failed".to_string())
                        }
                    }
                    cx.notify();
                });
            },
        ));
    }

    /// Forgets one trust-store record.
    ///
    /// An active record still carries a live grant, so the runtime revokes it
    /// and disconnects the client before the row goes away; a revoked record is
    /// only removed. Audit history survives both.
    fn delete_device(&mut self, device_id: String, cx: &mut Context<Self>) {
        let Ok(device_id_value) = DeviceId::parse(device_id.clone()) else {
            self.state.devices_error = Some("remote_device_id_invalid".to_string());
            cx.notify();
            return;
        };
        let remote = self.remote.clone();
        self.state.deleting_device = Some(device_id);
        self.state.error_code = None;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            remote.delete_device(RemoteDeleteDeviceRequest {
                device_id: device_id_value,
                reason: Some("deleted from the mobile pairing dialog".to_string()),
            })
        });
        self.delete_task = Some(cx.spawn(
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let outcome = runner.await;
                let _ = entity.update(cx, |this, cx| {
                    this.delete_task = None;
                    this.state.deleting_device = None;
                    match outcome {
                        Ok(Ok(_)) => {
                            this.state.notice = Some(RemoteAccessNotice::success(locale::text(
                                "Device record deleted",
                                "已删除设备记录",
                                "已刪除裝置記錄",
                            )));
                            this.refresh_devices(cx);
                        }
                        Ok(Err(error)) => {
                            this.state.notice = Some(RemoteAccessNotice::error(locale::text(
                                "The device record could not be deleted",
                                "删除设备记录失败",
                                "刪除裝置記錄失敗",
                            )));
                            this.state.devices_error = Some(error.code);
                        }
                        Err(_) => {
                            this.state.devices_error =
                                Some("remote_device_delete_task_failed".to_string())
                        }
                    }
                    cx.notify();
                });
            },
        ));
    }

    /// Renames one paired device in the runtime trust store.
    ///
    /// The runtime owns the name, so the rename is stored with the authority
    /// and the device reads it back from its next handshake instead of keeping
    /// a second copy that could drift.
    fn rename_device(&mut self, device_id: String, display_name: String, cx: &mut Context<Self>) {
        let Ok(device_id_value) = DeviceId::parse(device_id.clone()) else {
            self.state.devices_error = Some("remote_device_id_invalid".to_string());
            cx.notify();
            return;
        };
        let remote = self.remote.clone();
        self.state.renaming_device = Some(device_id);
        self.state.error_code = None;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            remote.rename_device(RemoteRenameDeviceRequest {
                device_id: device_id_value,
                display_name,
            })
        });
        self.rename_task = Some(cx.spawn(
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let outcome = runner.await;
                let _ = entity.update(cx, |this, cx| {
                    this.rename_task = None;
                    this.state.renaming_device = None;
                    match outcome {
                        Ok(Ok(_)) => {
                            this.state.notice = Some(RemoteAccessNotice::success(locale::text(
                                "Device renamed",
                                "已重命名设备",
                                "已重新命名裝置",
                            )));
                            this.refresh_devices(cx);
                        }
                        Ok(Err(error)) => {
                            this.state.notice = Some(RemoteAccessNotice::error(locale::text(
                                "The device could not be renamed",
                                "重命名设备失败",
                                "重新命名裝置失敗",
                            )));
                            this.state.devices_error = Some(error.code);
                        }
                        Err(_) => {
                            this.state.devices_error =
                                Some("remote_device_rename_task_failed".to_string())
                        }
                    }
                    cx.notify();
                });
            },
        ));
    }

    /// Opens the rename dialog with the device's current name selected.
    fn confirm_rename_device(
        &mut self,
        device_id: String,
        device_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.weak_entity();
        let title = match locale::current_locale() {
            locale::ResolvedLocale::En => format!("Rename \"{device_name}\"?"),
            locale::ResolvedLocale::ZhCn => format!("重命名“{device_name}”？"),
            locale::ResolvedLocale::ZhTw => format!("重新命名「{device_name}」？"),
        };
        // Keep the input outside the dialog builder. A builder is evaluated
        // again on every repaint, so an input created inside one would be
        // replaced — along with its focus handle and its text — while the user
        // types.
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(device_name.clone())
                .placeholder(locale::text("Device name", "设备名称", "裝置名稱"))
        });
        let input_for_focus = input.clone();
        let selection_end = device_name.len();
        window.open_dialog(cx, move |dialog, _window, cx| {
            let device_input = input.clone();
            let device_id = device_id.clone();
            let entity = entity.clone();
            dialog
                .title(title.clone())
                .child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .child(div().text_sm().child(locale::text(
                            "Device name",
                            "设备名称",
                            "裝置名稱",
                        )))
                        .child(Input::new(&input)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(locale::text(
                            "The runtime stores this name and publishes it to the device.",
                            "该名称保存在运行时上，并会同步到对应设备。",
                            "此名稱儲存在執行階段上，並會同步到對應裝置。",
                        )),
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new().child(
                                Button::new("cancel-device-rename")
                                    .outline()
                                    .label(locale::text("Cancel", "取消", "取消")),
                            ),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("confirm-device-rename")
                                    .label(locale::text("Save", "保存", "儲存")),
                            ),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let display_name = device_input.read(cx).value().trim().to_string();
                    let _ = entity.update(cx, |this, cx| {
                        this.dispatch_action(
                            RemoteAccessAction::RenameDevice {
                                device_id: device_id.clone(),
                                display_name,
                            },
                            cx,
                        )
                    });
                    true
                })
        });
        // The dialog's focus trap claims focus while it mounts; request the
        // field on the next frame and select the current name so typing
        // replaces it.
        window.on_next_frame(move |window, cx| {
            input_for_focus.update(cx, |input, cx| {
                input.set_selected_range(0..selection_end, cx);
                input.focus(window, cx);
            });
        });
    }

    fn confirm_delete_device(
        &mut self,
        device_id: String,
        device_name: String,
        revoked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.weak_entity();
        // Only the consequence the user cannot see from the row is worth
        // stating: an active row still holds a grant that has to go first.
        let description = match (locale::current_locale(), revoked) {
            (locale::ResolvedLocale::En, true) => {
                format!("\"{device_name}\" is removed from this list. Its audit history is kept.")
            }
            (locale::ResolvedLocale::En, false) => format!(
                "\"{device_name}\" still holds access: deleting revokes the grant, disconnects it, and then removes it. Its audit history is kept."
            ),
            (locale::ResolvedLocale::ZhCn, true) => {
                format!("“{device_name}”的记录将从列表中移除，审计记录会保留。")
            }
            (locale::ResolvedLocale::ZhCn, false) => format!(
                "“{device_name}”仍持有访问授权：删除会先撤销授权并断开连接，然后移除记录。审计记录会保留。"
            ),
            (locale::ResolvedLocale::ZhTw, true) => {
                format!("「{device_name}」的記錄將從清單中移除，稽核記錄會保留。")
            }
            (locale::ResolvedLocale::ZhTw, false) => format!(
                "「{device_name}」仍持有存取授權：刪除會先撤銷授權並中斷連線，然後移除記錄。稽核記錄會保留。"
            ),
        };
        let title = match locale::current_locale() {
            locale::ResolvedLocale::En => format!("Delete \"{device_name}\"?"),
            locale::ResolvedLocale::ZhCn => format!("删除“{device_name}”？"),
            locale::ResolvedLocale::ZhTw => format!("刪除「{device_name}」？"),
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            let device_id = device_id.clone();
            let description = description.clone();
            let title = title.clone();
            dialog
                .title(title)
                .child(description)
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new().child(
                                Button::new("cancel-device-delete")
                                    .outline()
                                    .label(locale::text("Cancel", "取消", "取消")),
                            ),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("confirm-device-delete")
                                    .danger()
                                    .label(locale::text("Delete", "删除", "刪除")),
                            ),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.dispatch_action(
                            RemoteAccessAction::DeleteDevice(device_id.clone()),
                            cx,
                        )
                    });
                    true
                })
        });
    }

    /// Puts a revoked device back in service.
    ///
    /// Restoring is the deliberate undo of a revocation, so it is confirmed the
    /// same way: the decision-critical fact is that the phone does not have to
    /// pair again.
    fn confirm_restore_device(
        &mut self,
        device_id: String,
        device_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.weak_entity();
        let description = match locale::current_locale() {
            locale::ResolvedLocale::En => format!(
                "\"{device_name}\" can connect again with the credential it already holds, without pairing again."
            ),
            locale::ResolvedLocale::ZhCn => {
                format!("“{device_name}”无需重新配对，用已有凭据即可再次连接。")
            }
            locale::ResolvedLocale::ZhTw => {
                format!("「{device_name}」無需重新配對，用已有憑證即可再次連線。")
            }
        };
        let title = match locale::current_locale() {
            locale::ResolvedLocale::En => format!("Restore access for \"{device_name}\"?"),
            locale::ResolvedLocale::ZhCn => format!("恢复“{device_name}”的访问权限？"),
            locale::ResolvedLocale::ZhTw => format!("恢復「{device_name}」的存取權限？"),
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            let device_id = device_id.clone();
            let description = description.clone();
            let title = title.clone();
            dialog
                .title(title)
                .child(description)
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new().child(
                                Button::new("cancel-device-restore")
                                    .outline()
                                    .label(locale::text("Cancel", "取消", "取消")),
                            ),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("confirm-device-restore")
                                    .label(locale::text("Restore", "恢复", "恢復")),
                            ),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.dispatch_action(
                            RemoteAccessAction::RestoreDevice(device_id.clone()),
                            cx,
                        )
                    });
                    true
                })
        });
    }

    fn confirm_revoke_device(
        &mut self,
        device_id: String,
        device_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.weak_entity();
        let description = match locale::current_locale() {
            locale::ResolvedLocale::En => format!(
                "\"{device_name}\" loses access immediately and is disconnected. This action is audited."
            ),
            locale::ResolvedLocale::ZhCn => {
                format!("“{device_name}”将立即失去访问权限并断开连接。此操作会写入审计记录。")
            }
            locale::ResolvedLocale::ZhTw => {
                format!("「{device_name}」將立即失去存取權限並中斷連線。此操作會寫入稽核記錄。")
            }
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            let device_id = device_id.clone();
            let description = description.clone();
            dialog
                .title(locale::text(
                    "Revoke device access?",
                    "撤销设备访问权限？",
                    "撤銷裝置存取權限？",
                ))
                .child(description)
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new().child(
                                Button::new("cancel-device-revoke")
                                    .outline()
                                    .label(locale::text("Cancel", "取消", "取消")),
                            ),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("confirm-device-revoke")
                                    .danger()
                                    .label(locale::text("Revoke", "撤销", "撤銷")),
                            ),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.dispatch_action(
                            RemoteAccessAction::RevokeDevice(device_id.clone()),
                            cx,
                        )
                    });
                    true
                })
        });
    }

    fn begin_mutation<F>(
        &mut self,
        mutation: RemoteAccessMutation,
        cx: &mut Context<Self>,
        future: F,
    ) where
        F: Future<Output = VibexResult<RemoteAccessMutationOutcome>> + Send + 'static,
    {
        if self.state.pending.is_some() {
            return;
        }
        self.state.pending = Some(mutation);
        self.state.error_code = None;
        self.state.notice = None;
        let runner = gpui_tokio::Tokio::spawn(cx, future);
        self.mutation_task = Some(cx.spawn(
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let outcome = runner.await;
                let _ = entity.update(cx, |this, cx| {
                    this.mutation_task = None;
                    this.state.pending = None;
                    match outcome {
                        Ok(Ok(RemoteAccessMutationOutcome::Connectivity(snapshot))) => {
                            this.state.apply_connectivity(snapshot);
                        }
                        Ok(Ok(RemoteAccessMutationOutcome::DisabledAll(snapshot))) => {
                            this.state.apply_connectivity(snapshot);
                            this.clear_offer();
                            this.clear_lan_window();
                            this.clear_zero_config_window();
                            this.state.notice = Some(RemoteAccessNotice::success(locale::text(
                                "Remote access disabled",
                                "远程访问已停用",
                                "遠端存取已停用",
                            )));
                        }
                        Ok(Ok(RemoteAccessMutationOutcome::OfferCreated(response))) => {
                            if let Err(error) = this.state.install_offer(response) {
                                this.state.error_code = Some(error.code);
                            } else {
                                this.state.page = RemoteAccessPage::Pairing;
                                this.schedule_offer_poll(cx);
                            }
                        }
                        Ok(Ok(RemoteAccessMutationOutcome::OfferCreationFailed(error))) => {
                            this.clear_offer();
                            this.state.error_code = Some(error.code);
                        }
                        Ok(Ok(RemoteAccessMutationOutcome::OfferCanceled)) => {
                            this.clear_offer();
                            this.state.notice = Some(RemoteAccessNotice::success(locale::text(
                                "Pairing offer canceled",
                                "配对请求已取消",
                                "配對請求已取消",
                            )));
                        }
                        Ok(Ok(RemoteAccessMutationOutcome::LanWindow(snapshot))) => {
                            this.state.selected_entry = RemoteAccessEntry::Direct;
                            this.state.active_lan_window = Some(snapshot);
                            this.schedule_lan_poll(cx);
                        }
                        Ok(Ok(RemoteAccessMutationOutcome::LanCanceled)) => {
                            this.clear_lan_window();
                            this.state.notice = Some(RemoteAccessNotice::success(locale::text(
                                "Nearby pairing stopped",
                                "附近配对已停止",
                                "附近配對已停止",
                            )));
                        }
                        Ok(Ok(RemoteAccessMutationOutcome::ZeroConfigWindow(snapshot))) => {
                            this.state.selected_entry = RemoteAccessEntry::LocalNetwork;
                            this.state.active_zero_config_window = Some(snapshot);
                            this.schedule_zero_config_poll(cx);
                        }
                        Ok(Ok(RemoteAccessMutationOutcome::ZeroConfigCanceled)) => {
                            this.clear_zero_config_window();
                            this.state.notice = Some(RemoteAccessNotice::success(locale::text(
                                "Local pairing stopped",
                                "局域网配对已停止",
                                "區域網路配對已停止",
                            )));
                        }
                        Ok(Err(error)) => this.state.error_code = Some(error.code),
                        Err(_) => {
                            this.state.error_code = Some("remote_access_task_failed".to_string())
                        }
                    }
                    cx.notify();
                });
            },
        ));
        cx.notify();
    }

    fn enable_method(&mut self, method: RemoteConnectivityMethod, cx: &mut Context<Self>) {
        let controller = self.controller.clone();
        match method {
            RemoteConnectivityMethod::TailscaleServe => {
                self.begin_mutation(RemoteAccessMutation::Enable(method), cx, async move {
                    controller
                        .enable_tailscale(None)
                        .await
                        .map(RemoteAccessMutationOutcome::Connectivity)
                })
            }
            RemoteConnectivityMethod::Direct => {
                let origin = match self.configured_origin(method, cx) {
                    Ok(origin) => origin,
                    Err(error) => {
                        self.state.error_code = Some(error.code);
                        cx.notify();
                        return;
                    }
                };
                self.begin_mutation(RemoteAccessMutation::Enable(method), cx, async move {
                    controller
                        .enable_direct(origin)
                        .await
                        .map(RemoteAccessMutationOutcome::Connectivity)
                });
            }
            RemoteConnectivityMethod::SelfHostedRelay => {
                let origin = match self.configured_origin(method, cx) {
                    Ok(origin) => origin,
                    Err(error) => {
                        self.state.error_code = Some(error.code);
                        cx.notify();
                        return;
                    }
                };
                self.begin_mutation(RemoteAccessMutation::Enable(method), cx, async move {
                    controller
                        .enable_relay(origin)
                        .await
                        .map(RemoteAccessMutationOutcome::Connectivity)
                });
            }
        }
    }

    fn confirm_tailscale_port(&mut self, port: u16, cx: &mut Context<Self>) {
        let controller = self.controller.clone();
        self.begin_mutation(
            RemoteAccessMutation::Enable(RemoteConnectivityMethod::TailscaleServe),
            cx,
            async move {
                controller
                    .enable_tailscale(Some(port))
                    .await
                    .map(RemoteAccessMutationOutcome::Connectivity)
            },
        );
    }

    fn disable_method(&mut self, method: RemoteConnectivityMethod, cx: &mut Context<Self>) {
        let controller = self.controller.clone();
        self.begin_mutation(RemoteAccessMutation::Disable(method), cx, async move {
            controller
                .disable_method(method)
                .await
                .map(RemoteAccessMutationOutcome::Connectivity)
        });
    }

    fn repair_method(&mut self, method: RemoteConnectivityMethod, cx: &mut Context<Self>) {
        let controller = self.controller.clone();
        self.begin_mutation(RemoteAccessMutation::Repair(method), cx, async move {
            controller
                .repair_method(method)
                .await
                .map(RemoteAccessMutationOutcome::Connectivity)
        });
    }

    fn disable_all(&mut self, cx: &mut Context<Self>) {
        let controller = self.controller.clone();
        let offer_id = self
            .state
            .active_offer
            .as_ref()
            .map(|offer| offer.offer_id().clone());
        self.begin_mutation(RemoteAccessMutation::DisableAll, cx, async move {
            if let Some(offer_id) = offer_id {
                let _ = controller.cancel_pairing_offer(offer_id);
            }
            let _ = controller.cancel_lan_pairing_window();
            let _ = controller.cancel_zero_config_lan_pairing().await;
            controller
                .disable_all()
                .await
                .map(RemoteAccessMutationOutcome::DisabledAll)
        });
    }

    fn create_offer(&mut self, cx: &mut Context<Self>) {
        if self.state.active_zero_config_window.is_some() {
            return;
        }
        self.state.selected_entry =
            RemoteAccessEntry::from_remote_method(self.state.selected_method);
        let controller = self.controller.clone();
        let permission = self.state.permission;
        self.begin_mutation(RemoteAccessMutation::CreateOffer, cx, async move {
            controller
                .create_pairing_offer(permission, PAIRING_OFFER_TTL_MS)
                .map(RemoteAccessMutationOutcome::OfferCreated)
        });
    }

    fn cancel_lan_pairing(&mut self, cx: &mut Context<Self>) {
        if self.state.active_lan_window.is_none() {
            return;
        }
        let controller = self.controller.clone();
        self.begin_mutation(RemoteAccessMutation::CancelLanPairing, cx, async move {
            controller
                .cancel_lan_pairing_window()
                .map(|_| RemoteAccessMutationOutcome::LanCanceled)
        });
    }

    fn approve_lan_pairing(&mut self, request_id: RequestId, cx: &mut Context<Self>) {
        let controller = self.controller.clone();
        self.begin_mutation(RemoteAccessMutation::ApproveLanPairing, cx, async move {
            controller
                .approve_lan_pairing_request(&request_id)
                .map(RemoteAccessMutationOutcome::LanWindow)
        });
    }

    fn reject_lan_pairing(&mut self, request_id: RequestId, cx: &mut Context<Self>) {
        let controller = self.controller.clone();
        self.begin_mutation(RemoteAccessMutation::RejectLanPairing, cx, async move {
            controller
                .reject_lan_pairing_request(&request_id)
                .map(RemoteAccessMutationOutcome::LanWindow)
        });
    }

    fn start_zero_config_pairing(&mut self, cx: &mut Context<Self>) {
        if !self.state.can_start_zero_config_pairing() {
            return;
        }
        self.state.selected_entry = RemoteAccessEntry::LocalNetwork;
        let controller = self.controller.clone();
        let permission = self.state.zero_config_permission;
        self.begin_mutation(
            RemoteAccessMutation::StartZeroConfigPairing,
            cx,
            async move {
                controller
                    .start_zero_config_lan_pairing(permission, PAIRING_OFFER_TTL_MS)
                    .await
                    .map(RemoteAccessMutationOutcome::ZeroConfigWindow)
            },
        );
    }

    fn cancel_zero_config_pairing(&mut self, cx: &mut Context<Self>) {
        if self.state.active_zero_config_window.is_none() {
            return;
        }
        let controller = self.controller.clone();
        self.begin_mutation(
            RemoteAccessMutation::CancelZeroConfigPairing,
            cx,
            async move {
                controller
                    .cancel_zero_config_lan_pairing()
                    .await
                    .map(|_| RemoteAccessMutationOutcome::ZeroConfigCanceled)
            },
        );
    }

    fn approve_zero_config_pairing(&mut self, request_id: RequestId, cx: &mut Context<Self>) {
        let controller = self.controller.clone();
        self.begin_mutation(
            RemoteAccessMutation::ApproveZeroConfigPairing,
            cx,
            async move {
                controller
                    .approve_zero_config_lan_pairing_request(&request_id)
                    .map(RemoteAccessMutationOutcome::ZeroConfigWindow)
            },
        );
    }

    fn reject_zero_config_pairing(&mut self, request_id: RequestId, cx: &mut Context<Self>) {
        let controller = self.controller.clone();
        self.begin_mutation(
            RemoteAccessMutation::RejectZeroConfigPairing,
            cx,
            async move {
                controller
                    .reject_zero_config_lan_pairing_request(&request_id)
                    .map(RemoteAccessMutationOutcome::ZeroConfigWindow)
            },
        );
    }

    fn regenerate_offer(
        &mut self,
        permission: RemoteDevicePermissionLevel,
        cx: &mut Context<Self>,
    ) {
        let controller = self.controller.clone();
        let offer_id = self
            .state
            .active_offer
            .as_ref()
            .map(|offer| offer.offer_id().clone());
        self.begin_mutation(RemoteAccessMutation::RegenerateOffer, cx, async move {
            if let Some(offer_id) = offer_id {
                match controller.cancel_pairing_offer(offer_id) {
                    Ok(_) => {}
                    Err(error) if offer_cancel_error_allows_replacement(&error.code) => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(
                match controller.create_pairing_offer(permission, PAIRING_OFFER_TTL_MS) {
                    Ok(response) => RemoteAccessMutationOutcome::OfferCreated(response),
                    Err(error) => RemoteAccessMutationOutcome::OfferCreationFailed(error),
                },
            )
        });
    }

    fn cancel_offer(&mut self, cx: &mut Context<Self>) {
        let Some(offer_id) = self
            .state
            .active_offer
            .as_ref()
            .map(|offer| offer.offer_id().clone())
        else {
            return;
        };
        let controller = self.controller.clone();
        self.begin_mutation(RemoteAccessMutation::CancelOffer, cx, async move {
            controller
                .cancel_pairing_offer(offer_id)
                .map(|_| RemoteAccessMutationOutcome::OfferCanceled)
        });
    }

    fn select_entry(&mut self, method: RemoteConnectivityMethod, cx: &mut Context<Self>) {
        if self.state.pending.is_some()
            || self
                .state
                .active_offer
                .as_ref()
                .is_some_and(|offer| offer.is_terminal(unix_timestamp_ms()))
        {
            return;
        }
        let Some(offer) = self.state.active_offer.as_mut() else {
            return;
        };
        match offer.select_entry(method) {
            Ok(()) => {
                self.state.error_code = None;
                self.state.notice = None;
            }
            Err(error) => self.state.error_code = Some(error.code),
        }
        cx.notify();
    }

    fn set_permission(&mut self, permission: RemoteDevicePermissionLevel, cx: &mut Context<Self>) {
        if self.state.permission == permission
            || self.state.pending.is_some()
            || self.state.active_lan_window.is_some()
            || self.state.active_zero_config_window.is_some()
            || self
                .state
                .active_offer
                .as_ref()
                .is_some_and(|offer| offer.summary.claimed_device_id.is_some())
        {
            return;
        }
        if self.state.active_offer.is_some() {
            self.regenerate_offer(permission, cx);
        } else {
            self.state.permission = permission;
            self.state.error_code = None;
            cx.notify();
        }
    }

    fn set_zero_config_permission(
        &mut self,
        permission: RemoteDevicePermissionLevel,
        cx: &mut Context<Self>,
    ) {
        if self.state.zero_config_permission == permission
            || self.state.pending.is_some()
            || self.state.active_offer.is_some()
            || self.state.active_lan_window.is_some()
            || self.state.active_zero_config_window.is_some()
        {
            return;
        }
        self.state.zero_config_permission = permission;
        self.state.error_code = None;
        cx.notify();
    }

    fn copy_pairing_link(&mut self, cx: &mut Context<Self>) {
        let Some(value) = self
            .state
            .active_offer
            .as_ref()
            .and_then(|offer| offer.private.as_ref())
            .map(|private| private.launch_url.as_str().to_string())
        else {
            self.state.error_code = Some("remote_pairing_offer_unavailable".to_string());
            cx.notify();
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(value.clone()));
        let verified = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .is_some_and(|clipboard| clipboard == value);
        self.state.notice = Some(if verified {
            RemoteAccessNotice::success(locale::text(
                "Pairing link copied",
                "配对链接已复制",
                "配對連結已複製",
            ))
        } else {
            RemoteAccessNotice::error(locale::text(
                "Clipboard write failed",
                "无法写入剪贴板",
                "無法寫入剪貼簿",
            ))
        });
        cx.notify();
    }

    fn schedule_offer_poll(&mut self, cx: &mut Context<Self>) {
        let Some(offer_id) = self
            .state
            .active_offer
            .as_ref()
            .map(|offer| offer.offer_id().clone())
        else {
            return;
        };
        let controller = self.controller.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            tokio::time::sleep(OFFER_POLL_INTERVAL).await;
            let summary = controller.pairing_offer_status(&offer_id)?;
            Ok::<_, VibexError>(OfferPollOutcome { summary })
        });
        self.offer_poll_task = Some(cx.spawn(
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let outcome = runner.await;
                let _ = entity.update(cx, |this, cx| {
                    this.offer_poll_task = None;
                    let mut continue_polling = false;
                    let mut claimed_entry = None;
                    match outcome {
                        Ok(Ok(outcome)) => {
                            if let Some(offer) = this.state.active_offer.as_mut()
                                && offer.offer_id() == &outcome.summary.offer_id
                            {
                                if outcome.summary.claimed_device_id.is_some() {
                                    claimed_entry =
                                        Some((offer.offer_id().clone(), offer.selected_entry));
                                }
                                offer.apply_status(outcome.summary, unix_timestamp_ms());
                                if offer.summary.claimed_device_id.is_some() {
                                    this.state.notice = Some(RemoteAccessNotice::success(
                                        locale::text("Device paired", "设备已配对", "裝置已配對"),
                                    ));
                                }
                                continue_polling = !offer.is_terminal(unix_timestamp_ms());
                            }
                        }
                        Ok(Err(error)) => {
                            this.state.error_code = Some(error.code);
                            continue_polling = this
                                .state
                                .active_offer
                                .as_ref()
                                .is_some_and(|offer| !offer.is_terminal(unix_timestamp_ms()));
                        }
                        Err(_) => {
                            this.state.error_code =
                                Some("remote_pairing_status_task_failed".to_string());
                        }
                    }
                    if let Some((offer_id, method)) = claimed_entry {
                        this.record_claimed_entry(offer_id, method, cx);
                        this.refresh_devices(cx);
                    }
                    if continue_polling {
                        this.schedule_offer_poll(cx);
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn schedule_lan_poll(&mut self, cx: &mut Context<Self>) {
        if self.state.active_lan_window.is_none() {
            return;
        }
        let controller = self.controller.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            tokio::time::sleep(OFFER_POLL_INTERVAL).await;
            controller.lan_pairing_window_status()
        });
        self.lan_poll_task = Some(cx.spawn(
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let outcome = runner.await;
                let _ = entity.update(cx, |this, cx| {
                    this.lan_poll_task = None;
                    match outcome {
                        Ok(Ok(snapshot)) => {
                            let active = snapshot.discovery.expires_at_ms > unix_timestamp_ms();
                            this.state.active_lan_window = active.then_some(snapshot);
                            if active {
                                this.schedule_lan_poll(cx);
                            }
                        }
                        Ok(Err(error))
                            if matches!(
                                error.code.as_str(),
                                "remote_lan_pairing_window_unavailable"
                                    | "remote_pairing_offer_already_claimed"
                                    | "remote_pairing_offer_expired"
                            ) =>
                        {
                            let had_approved_request =
                                this.state.active_lan_window.as_ref().is_some_and(|window| {
                                    window.pending_requests.iter().any(|request| {
                                        request.state == RemoteLanPairingRequestState::Approved
                                    })
                                });
                            this.clear_lan_window();
                            if had_approved_request {
                                this.refresh_devices(cx);
                            }
                            this.state.notice = Some(if had_approved_request {
                                RemoteAccessNotice::success(locale::text(
                                    "Device paired",
                                    "设备已配对",
                                    "裝置已配對",
                                ))
                            } else {
                                RemoteAccessNotice::success(locale::text(
                                    "Nearby pairing ended",
                                    "附近配对已结束",
                                    "附近配對已結束",
                                ))
                            });
                        }
                        Ok(Err(error)) => {
                            this.state.error_code = Some(error.code);
                            this.schedule_lan_poll(cx);
                        }
                        Err(_) => {
                            this.state.error_code =
                                Some("remote_lan_pairing_status_task_failed".to_string());
                        }
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn schedule_zero_config_poll(&mut self, cx: &mut Context<Self>) {
        if self.state.active_zero_config_window.is_none() {
            return;
        }
        let controller = self.controller.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            tokio::time::sleep(OFFER_POLL_INTERVAL).await;
            controller.zero_config_lan_pairing_window_status()
        });
        self.zero_config_poll_task = Some(cx.spawn(
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let outcome = runner.await;
                let _ = entity.update(cx, |this, cx| {
                    this.zero_config_poll_task = None;
                    match outcome {
                        Ok(Ok(snapshot)) => {
                            let active = snapshot.discovery.expires_at_ms > unix_timestamp_ms();
                            this.state.active_zero_config_window = active.then_some(snapshot);
                            if active {
                                this.schedule_zero_config_poll(cx);
                            }
                        }
                        Ok(Err(error))
                            if matches!(
                                error.code.as_str(),
                                "remote_lan_pairing_window_unavailable"
                                    | "remote_pairing_offer_already_claimed"
                                    | "remote_pairing_offer_expired"
                            ) =>
                        {
                            let had_approved_request =
                                this.state.active_zero_config_window.as_ref().is_some_and(
                                    |window| {
                                        window.pending_requests.iter().any(|request| {
                                            request.state == RemoteLanPairingRequestState::Approved
                                        })
                                    },
                                );
                            this.clear_zero_config_window();
                            if had_approved_request {
                                this.refresh_devices(cx);
                            }
                            let controller = this.controller.clone();
                            gpui_tokio::Tokio::spawn(cx, async move {
                                let _ = controller.cancel_zero_config_lan_pairing().await;
                            })
                            .detach();
                            this.state.notice = Some(if had_approved_request {
                                RemoteAccessNotice::success(locale::text(
                                    "Device paired",
                                    "设备已配对",
                                    "裝置已配對",
                                ))
                            } else {
                                RemoteAccessNotice::success(locale::text(
                                    "Local pairing ended",
                                    "局域网配对已结束",
                                    "區域網路配對已結束",
                                ))
                            });
                        }
                        Ok(Err(error)) => {
                            this.state.error_code = Some(error.code);
                            this.schedule_zero_config_poll(cx);
                        }
                        Err(_) => {
                            this.state.error_code =
                                Some("remote_zero_config_pairing_status_task_failed".to_string());
                        }
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn record_claimed_entry(
        &mut self,
        offer_id: RequestId,
        method: RemoteConnectivityMethod,
        cx: &mut Context<Self>,
    ) {
        let controller = self.controller.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            controller
                .record_claimed_pairing_entry(&offer_id, method)
                .await
        });
        cx.spawn(
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let outcome = runner.await;
                let _ = entity.update(cx, |this, cx| {
                    match outcome {
                        Ok(Ok(snapshot)) => this.state.apply_connectivity(snapshot),
                        Ok(Err(error)) => this.state.error_code = Some(error.code),
                        Err(_) => {
                            this.state.error_code =
                                Some("remote_pairing_preference_task_failed".to_string())
                        }
                    }
                    cx.notify();
                });
            },
        )
        .detach();
    }

    fn clear_offer(&mut self) {
        self.offer_poll_task = None;
        self.state.active_offer = None;
    }

    fn clear_lan_window(&mut self) {
        self.lan_poll_task = None;
        self.state.active_lan_window = None;
    }

    fn clear_zero_config_window(&mut self) {
        self.zero_config_poll_task = None;
        self.state.active_zero_config_window = None;
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        let offer_id = self
            .state
            .active_offer
            .as_ref()
            .filter(|offer| !offer.is_terminal(unix_timestamp_ms()))
            .map(|offer| offer.offer_id().clone());
        let had_lan_window = self.state.active_lan_window.is_some();
        let had_zero_config_window = self.state.active_zero_config_window.is_some();
        self.clear_offer();
        self.clear_lan_window();
        self.clear_zero_config_window();
        self.stop_presence_poll();
        if let Some(offer_id) = offer_id {
            let controller = self.controller.clone();
            gpui_tokio::Tokio::spawn(cx, async move {
                let _ = controller.cancel_pairing_offer(offer_id);
            })
            .detach();
        }
        if had_lan_window {
            let controller = self.controller.clone();
            gpui_tokio::Tokio::spawn(cx, async move {
                let _ = controller.cancel_lan_pairing_window();
            })
            .detach();
        }
        if had_zero_config_window {
            let controller = self.controller.clone();
            gpui_tokio::Tokio::spawn(cx, async move {
                let _ = controller.cancel_zero_config_lan_pairing().await;
            })
            .detach();
        }
    }

    fn configured_origin(&self, method: RemoteConnectivityMethod, cx: &App) -> VibexResult<String> {
        let typed = match method {
            RemoteConnectivityMethod::Direct => self.direct_origin.read(cx).value().to_string(),
            RemoteConnectivityMethod::SelfHostedRelay => {
                self.relay_origin.read(cx).value().to_string()
            }
            RemoteConnectivityMethod::TailscaleServe => String::new(),
        };
        let value = if typed.trim().is_empty() {
            self.state
                .connectivity
                .as_ref()
                .and_then(|snapshot| snapshot.method(method))
                .and_then(|snapshot| snapshot.origin.clone())
                .unwrap_or_default()
        } else {
            typed
        };
        if value.trim().is_empty() {
            return Err(VibexError::validation(
                match method {
                    RemoteConnectivityMethod::Direct => "remote_direct_origin_missing",
                    RemoteConnectivityMethod::SelfHostedRelay => "relay_origin_missing",
                    RemoteConnectivityMethod::TailscaleServe => "tailscale_origin_unavailable",
                },
                "remote access origin is missing",
            ));
        }
        normalize_https_origin(&value)
    }

    fn present_disable_all_confirmation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            alert
                .title(locale::text(
                    "Disable remote access?",
                    "停用远程访问？",
                    "停用遠端存取？",
                ))
                .description(locale::text(
                    "Paired devices stay trusted and can reconnect after access is enabled again.",
                    "已配对设备仍受信任，重新启用后可以继续连接。",
                    "已配對裝置仍受信任，重新啟用後可以繼續連線。",
                ))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(locale::text("Disable", "停用", "停用"))
                        .cancel_text(locale::text("Cancel", "取消", "取消"))
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.dispatch_action(RemoteAccessAction::DisableAll, cx)
                    });
                    true
                })
        });
    }

    fn present_tailscale_confirmation(
        &mut self,
        port: u16,
        origin: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.weak_entity();
        let description = match locale::current_locale() {
            locale::ResolvedLocale::En => {
                format!(
                    "Port 443 is already in use. Publish Vibex at {origin} without changing the existing route?"
                )
            }
            locale::ResolvedLocale::ZhCn => {
                format!("端口 443 已被占用。是否在 {origin} 发布 Vibex，并保留现有路由？")
            }
            locale::ResolvedLocale::ZhTw => {
                format!("連接埠 443 已被占用。是否在 {origin} 發布 Vibex，並保留現有路由？")
            }
        };
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            alert
                .title(locale::text(
                    "Use alternate Tailscale port?",
                    "使用其他 Tailscale 端口？",
                    "使用其他 Tailscale 連接埠？",
                ))
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(locale::text("Publish", "发布", "發布"))
                        .cancel_text(locale::text("Cancel", "取消", "取消"))
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.dispatch_action(RemoteAccessAction::ConfirmTailscalePort(port), cx)
                    });
                    true
                })
        });
    }

    fn render_hero_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let (status, color) = self.remote_access_status(cx);
        h_flex()
            .w_full()
            .min_w_0()
            .items_start()
            .justify_between()
            .gap_3()
            .child(
                h_flex()
                    .min_w_0()
                    .items_center()
                    .gap_3()
                    .child(icon_tile(IconName::SquareTerminal, px(40.0), px(20.0), cx))
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_lg().font_semibold().child(locale::text(
                                "Connect a mobile device",
                                "连接移动设备",
                                "連接行動裝置",
                            )))
                            .child(
                                div()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(locale::text(
                                        "Pair this computer with the Vibex mobile app",
                                        "将此电脑与 Vibex 移动应用配对",
                                        "將此電腦與 Vibex 行動應用程式配對",
                                    )),
                            ),
                    ),
            )
            .child(status_pill(status, color))
            .into_any_element()
    }

    fn remote_access_status(&self, cx: &App) -> (SharedString, gpui::Hsla) {
        let Some(snapshot) = self.state.connectivity.as_ref() else {
            return (
                locale::text("Checking…", "检查中…", "檢查中…").into(),
                cx.theme().muted_foreground,
            );
        };
        let online = snapshot
            .methods
            .iter()
            .filter(|method| method.candidate_available)
            .count();
        if online > 0 {
            let label = match locale::current_locale() {
                locale::ResolvedLocale::En => format!("{online} online"),
                locale::ResolvedLocale::ZhCn => format!("{online} 项在线"),
                locale::ResolvedLocale::ZhTw => format!("{online} 項上線"),
            };
            return (label.into(), cx.theme().success);
        }
        if snapshot.desired_enabled {
            return (
                locale::text("Checking…", "检查中…", "檢查中…").into(),
                cx.theme().warning,
            );
        }
        (
            locale::text("Not enabled", "未启用", "未啟用").into(),
            cx.theme().muted_foreground,
        )
    }

    fn render_pairing_resume_strip(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.weak_entity();
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(cx.theme().primary.opacity(0.35))
            .bg(cx.theme().primary.opacity(0.06))
            .px_3()
            .py_2()
            .child(
                h_flex()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Info)
                            .size(px(14.0))
                            .text_color(cx.theme().primary),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(locale::text(
                                "A pairing session is already in progress",
                                "已有配对会话正在进行",
                                "已有配對工作階段進行中",
                            )),
                    ),
            )
            .child(
                Button::new("resume-active-pairing")
                    .small()
                    .outline()
                    .label(locale::text("Resume", "继续配对", "繼續配對"))
                    .disabled(self.state.pending.is_some())
                    .on_click(move |_, _, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            this.dispatch_action(RemoteAccessAction::ShowPairing, cx)
                        });
                    }),
            )
            .into_any_element()
    }

    /// The dialog's two modes: pairing a new device, and managing the devices
    /// that already hold a grant on this computer.
    fn render_mode_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let paired = self
            .state
            .devices
            .iter()
            .filter(|device| device.status != RemoteDeviceStatus::Revoked)
            .count();
        let devices_label = match locale::current_locale() {
            locale::ResolvedLocale::En => format!("Paired devices ({paired})"),
            locale::ResolvedLocale::ZhCn => format!("已配对设备 ({paired})"),
            locale::ResolvedLocale::ZhTw => format!("已配對裝置 ({paired})"),
        };
        let selected_index = match self.state.page {
            RemoteAccessPage::Devices => 1,
            _ => 0,
        };
        TabBar::new("remote-access-mode")
            .segmented()
            .selected_index(selected_index)
            .children([
                Tab::new()
                    .flex_1()
                    .label(locale::text("Pair", "配对", "配對")),
                Tab::new().flex_1().label(devices_label),
            ])
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                let action = if *index == 1 {
                    RemoteAccessAction::ShowDevices
                } else {
                    RemoteAccessAction::ShowSetup
                };
                this.dispatch_action(action, cx);
            }))
            .into_any_element()
    }

    fn render_devices_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let paired = self
            .state
            .devices
            .iter()
            .filter(|device| device.status != RemoteDeviceStatus::Revoked)
            .count();
        let revoked = self.state.devices.len().saturating_sub(paired);
        let summary = device_management_summary(
            paired,
            revoked,
            self.state.audit_count,
            self.state.audit_count_capped,
        );
        let refresh_entity = cx.weak_entity();
        let pending = self.state.device_mutation_pending();
        let mut column = v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(summary),
                    )
                    .child(
                        Button::new("refresh-paired-devices")
                            .small()
                            .ghost()
                            .compact()
                            .size(px(28.0))
                            .px_0()
                            .tooltip(locale::text(
                                "Refresh devices",
                                "刷新设备",
                                "重新整理裝置",
                            ))
                            .disabled(pending)
                            .child(Icon::new(IconName::Redo2).size(px(15.0)))
                            .on_click(move |_, _, cx| {
                                let _ = refresh_entity.update(cx, |this, cx| {
                                    this.dispatch_action(RemoteAccessAction::RefreshDevices, cx)
                                });
                            }),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(locale::text(
                        "Devices listed here hold a grant on this computer. Revoking one disconnects it immediately.",
                        "这里的设备已获得本机访问授权。撤销后该设备会立即断开连接。",
                        "這裡的裝置已取得本機存取授權。撤銷後該裝置會立即中斷連線。",
                    )),
            );

        if !self.state.devices_loaded && self.state.devices_error.is_none() {
            column = column.child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .py(px(32.0))
                    .child(
                        Spinner::new()
                            .with_size(Size::Size(px(18.0)))
                            .color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(locale::text(
                                "Reading paired devices…",
                                "正在读取已配对设备…",
                                "正在讀取已配對裝置…",
                            )),
                    ),
            );
            return column.into_any_element();
        }

        if let Some(error) = self.state.devices_error.clone() {
            column = column.child(self.render_status_banner(
                IconName::TriangleAlert,
                cx.theme().danger,
                remote_error_label(&error),
                cx,
            ));
        }
        if self.state.devices.is_empty() {
            if self.state.devices_error.is_none() {
                column = column.child(device_list_empty_state(cx));
            }
            return column.into_any_element();
        }

        let mut rows = v_flex().w_full().gap_2();
        for device in self.state.device_page_slice().to_vec() {
            rows = rows.child(self.render_device_row(device, cx));
        }
        column = column.child(rows);

        if self.state.device_page_count() > 1 {
            column = column.child(self.render_device_pager(cx));
        }
        column.into_any_element()
    }

    /// Windows the registry so a long trust store cannot push the dialog past
    /// the viewport. The range caption keeps the page's slice explicit even
    /// when the pager collapses it into an ellipsis.
    fn render_device_pager(&self, cx: &mut Context<Self>) -> AnyElement {
        let total = self.state.devices.len();
        let page = self.state.device_page;
        let first = (page - 1) * DEVICE_PAGE_SIZE + 1;
        let last = (first + DEVICE_PAGE_SIZE - 1).min(total);
        let range = match locale::current_locale() {
            locale::ResolvedLocale::En => format!("{first}–{last} of {total}"),
            locale::ResolvedLocale::ZhCn => format!("第 {first}–{last} 台，共 {total} 台"),
            locale::ResolvedLocale::ZhTw => format!("第 {first}–{last} 台，共 {total} 台"),
        };
        let entity = cx.weak_entity();
        h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .justify_between()
            .gap_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .pt_2()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(range),
            )
            .child(
                Pagination::new("paired-device-pagination")
                    .small()
                    // The row already supplies the separation from the list.
                    .py_0()
                    .current_page(page)
                    .total_pages(self.state.device_page_count())
                    .visible_pages(5)
                    .on_click(move |page, _, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            this.dispatch_action(RemoteAccessAction::SelectDevicePage(*page), cx)
                        });
                    }),
            )
            .into_any_element()
    }

    fn render_device_row(&self, device: RemoteDeviceDetail, cx: &mut Context<Self>) -> AnyElement {
        let revoked = device.status == RemoteDeviceStatus::Revoked;
        let pending = self.state.device_mutation_pending();
        let revoking = self.state.revoking_device.as_deref() == Some(device.device_id.as_str());
        let restoring = self.state.restoring_device.as_deref() == Some(device.device_id.as_str());
        let deleting = self.state.deleting_device.as_deref() == Some(device.device_id.as_str());
        let renaming = self.state.renaming_device.as_deref() == Some(device.device_id.as_str());
        let status_color = device_status_color(device.status, cx);
        let device_key = device.device_id.as_str().to_string();
        // Presence is a live connection, so it replaces the stored last-seen
        // sentence rather than sitting beside a stale one.
        let online = !revoked && self.state.connected_devices.contains(&device_key);
        let activity = if online {
            locale::text("Online now", "在线", "線上").to_string()
        } else {
            device_activity_label(&device)
        };
        let detail = format!(
            "{} · {}",
            permission_label(device.permission_level),
            activity
        );
        let detail_color = if online {
            cx.theme().success
        } else {
            cx.theme().muted_foreground
        };
        let device_id = device_key;
        let device_name = device.display_name.clone();
        let restore_device_id = device_id.clone();
        let restore_device_name = device_name.clone();
        let delete_device_id = device_id.clone();
        let delete_device_name = device_name.clone();
        let rename_device_id = device_id.clone();
        let rename_device_name = device_name.clone();
        let entity = cx.weak_entity();
        let restore_entity = entity.clone();
        let delete_entity = entity.clone();
        let rename_entity = entity.clone();

        h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background.opacity(0.6))
            .px_3()
            .py_2()
            // The identity block is dimmed for a revoked row; the actions stay
            // at full strength so the row's only remaining command still reads
            // as available.
            .child(
                h_flex()
                    .min_w_0()
                    .flex_1()
                    .items_center()
                    .gap_3()
                    .when(revoked, |block| block.opacity(0.62))
                    .child(icon_tile(IconName::CircleUser, px(32.0), px(18.0), cx))
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .gap_1()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_sm()
                                            .font_semibold()
                                            .child(device.display_name.clone()),
                                    )
                                    .child(status_pill(
                                        device_status_label(device.status),
                                        status_color,
                                    )),
                            )
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .items_center()
                                    .gap_1()
                                    .when(online, |line| {
                                        line.child(
                                            div()
                                                .size(px(6.0))
                                                .flex_none()
                                                .rounded_full()
                                                .bg(detail_color),
                                        )
                                    })
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .text_color(detail_color)
                                            .child(detail),
                                    ),
                            ),
                    ),
            )
            .when(!revoked, |row| {
                row.child(
                    Button::new(SharedString::from(format!("revoke-device-{device_id}")))
                        .small()
                        .danger()
                        .label(locale::text("Revoke", "撤销", "撤銷"))
                        .loading(revoking)
                        .disabled(pending)
                        .on_click(move |_, window, cx| {
                            let _ = entity.update(cx, |this, cx| {
                                this.confirm_revoke_device(
                                    device_id.clone(),
                                    device_name.clone(),
                                    window,
                                    cx,
                                )
                            });
                        }),
                )
            })
            .when(revoked, |row| {
                row.child(
                    Button::new(SharedString::from(format!(
                        "restore-device-{restore_device_id}"
                    )))
                    .small()
                    .label(locale::text("Restore", "恢复", "恢復"))
                    .loading(restoring)
                    .disabled(pending)
                    .on_click(move |_, window, cx| {
                        let _ = restore_entity.update(cx, |this, cx| {
                            this.confirm_restore_device(
                                restore_device_id.clone(),
                                restore_device_name.clone(),
                                window,
                                cx,
                            )
                        });
                    }),
                )
            })
            .child(
                Button::new(SharedString::from(format!(
                    "rename-device-{rename_device_id}"
                )))
                .small()
                .ghost()
                .label(locale::text("Rename", "重命名", "重新命名"))
                .loading(renaming)
                .disabled(pending)
                .tooltip(locale::text(
                    "Rename this device",
                    "重命名该设备",
                    "重新命名該裝置",
                ))
                .on_click(move |_, window, cx| {
                    let _ = rename_entity.update(cx, |this, cx| {
                        this.confirm_rename_device(
                            rename_device_id.clone(),
                            rename_device_name.clone(),
                            window,
                            cx,
                        )
                    });
                }),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "delete-device-{delete_device_id}"
                )))
                .small()
                .ghost()
                .label(locale::text("Delete", "删除", "刪除"))
                .loading(deleting)
                .disabled(pending)
                .tooltip(locale::text(
                    "Delete this device record",
                    "删除该设备记录",
                    "刪除該裝置記錄",
                ))
                .on_click(move |_, window, cx| {
                    let _ = delete_entity.update(cx, |this, cx| {
                        this.confirm_delete_device(
                            delete_device_id.clone(),
                            delete_device_name.clone(),
                            revoked,
                            window,
                            cx,
                        )
                    });
                }),
            )
            .into_any_element()
    }

    fn render_connection_list(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .gap_2()
            .child(self.render_connection_row(RemoteAccessEntry::TailscaleServe, cx))
            .child(self.render_connection_row(RemoteAccessEntry::Direct, cx))
            .child(self.render_connection_row(RemoteAccessEntry::SelfHostedRelay, cx))
            .child(self.render_connection_row(RemoteAccessEntry::LocalNetwork, cx))
            .into_any_element()
    }

    fn render_connection_row(
        &self,
        entry: RemoteAccessEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.state.selected_entry == entry;
        let disabled = self.has_active_pairing();
        let entity = cx.weak_entity();
        let keyboard_entity = entity.clone();
        let action = RemoteAccessAction::SelectConnectionEntry(entry);
        let keyboard_action = action.clone();
        let (status, status_color) = self.connection_entry_status(entry, cx);
        let row_id: SharedString = connection_entry_id(entry).into();
        let accessibility_label = format!(
            "{}: {}",
            connection_entry_name(entry),
            connection_entry_description(entry)
        );

        h_flex()
            .id(row_id)
            .w_full()
            .min_w_0()
            .items_center()
            .gap_3()
            .rounded(px(10.0))
            .border_1()
            .border_color(if selected {
                cx.theme().primary.opacity(0.55)
            } else {
                cx.theme().border.opacity(0.72)
            })
            .bg(if selected {
                cx.theme().primary.opacity(0.07)
            } else {
                cx.theme().background.opacity(0.4)
            })
            .px_3()
            .py_2()
            .cursor_pointer()
            .focusable()
            .tab_stop(!disabled)
            .role(Role::Button)
            .aria_label(accessibility_label)
            .aria_selected(selected)
            .hover(|style| {
                style.bg(if selected {
                    cx.theme().primary.opacity(0.10)
                } else {
                    cx.theme().accent
                })
            })
            .focus_visible(|style| {
                style.shadow(vec![
                    gpui::BoxShadow::new(px(0.0), px(0.0), cx.theme().ring).spread_radius(px(2.0)),
                ])
            })
            .when(disabled, |row| row.opacity(0.62).cursor_default())
            .child(radio_dot(selected, cx))
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_1()
                    .child(
                        h_flex()
                            .min_w_0()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .text_sm()
                                    .font_semibold()
                                    .child(connection_entry_name(entry)),
                            )
                            .when(entry == RemoteAccessEntry::TailscaleServe, |row| {
                                row.child(status_pill(
                                    locale::text("Recommended", "推荐", "建議"),
                                    cx.theme().primary,
                                ))
                            }),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(connection_entry_description(entry)),
                    ),
            )
            .child(status_pill(status, status_color))
            .on_click(move |_, _, cx| {
                if disabled {
                    return;
                }
                let _ = entity.update(cx, |this, cx| {
                    this.dispatch_action(action.clone(), cx);
                });
            })
            .on_key_down(move |event: &KeyDownEvent, _, cx| {
                if disabled {
                    return;
                }
                if event.keystroke.key == "enter" || event.keystroke.key == "space" {
                    let _ = keyboard_entity.update(cx, |this, cx| {
                        this.dispatch_action(keyboard_action.clone(), cx);
                    });
                    cx.stop_propagation();
                }
            })
            .into_any_element()
    }

    fn render_status_banner(
        &self,
        icon: IconName,
        color: gpui::Hsla,
        text: impl IntoElement,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .rounded(px(6.0))
            .bg(color.opacity(0.08))
            .px_3()
            .py_2()
            .child(Icon::new(icon).size(px(14.0)).text_color(color))
            .child(div().min_w_0().text_xs().text_color(color).child(text))
            .into_any_element()
    }

    fn has_active_pairing(&self) -> bool {
        self.state.pending.is_some()
            || self.state.active_lan_window.is_some()
            || self.state.active_zero_config_window.is_some()
            || self
                .state
                .active_offer
                .as_ref()
                .is_some_and(|offer| !offer.is_terminal(unix_timestamp_ms()))
    }

    fn connection_entry_status(
        &self,
        entry: RemoteAccessEntry,
        cx: &App,
    ) -> (&'static str, gpui::Hsla) {
        if entry == RemoteAccessEntry::LocalNetwork {
            if self.state.active_zero_config_window.is_some() {
                return (
                    locale::text("Discovering", "发现中", "發現中"),
                    cx.theme().success,
                );
            }
            return (
                locale::text("On demand", "按需开启", "按需開啟"),
                cx.theme().muted_foreground,
            );
        }
        let Some(method) = entry.remote_method() else {
            return (
                method_state_label(RemoteMethodState::Disabled),
                cx.theme().muted_foreground,
            );
        };
        let state = self
            .state
            .connectivity
            .as_ref()
            .and_then(|connectivity| connectivity.method(method))
            .map_or(RemoteMethodState::Disabled, |snapshot| snapshot.state);
        (method_state_label(state), method_state_color(state, cx))
    }

    fn render_connection_detail_panel(
        &self,
        entry: RemoteAccessEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if entry == RemoteAccessEntry::LocalNetwork {
            return self.render_local_network_panel(cx);
        }
        let method = match entry.remote_method() {
            Some(method) => method,
            None => return self.render_local_network_panel(cx),
        };
        let snapshot = self
            .state
            .connectivity
            .as_ref()
            .and_then(|connectivity| connectivity.method(method));
        let pending = self.state.pending.is_some();
        let desired_enabled = snapshot.is_some_and(|snapshot| snapshot.desired_enabled);
        let state = snapshot.map_or(RemoteMethodState::Disabled, |snapshot| snapshot.state);
        let status_color = method_state_color(state, cx);
        let recovery = snapshot.map_or(RemoteRecoveryAction::None, |snapshot| {
            snapshot.recovery_action
        });
        let origin = snapshot.and_then(|snapshot| snapshot.origin.clone());
        let entity = cx.weak_entity();

        let mut panel = v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .rounded(px(10.0))
            .border_1()
            .border_color(cx.theme().primary.opacity(0.30))
            .bg(cx.theme().muted.opacity(0.14))
            .p_4()
            .child(
                h_flex()
                    .w_full()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(icon_tile(method_icon(method), px(28.0), px(15.0), cx))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_semibold()
                                    .child(method_short_name(method)),
                            )
                            .child(status_pill(method_state_label(state), status_color)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(method_description(method)),
                    ),
            )
            .when_some(origin.clone(), |column, origin| {
                column.child(
                    div()
                        .w_full()
                        .truncate()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(origin),
                )
            });

        if method == RemoteConnectivityMethod::Direct {
            panel = panel.child(origin_editor(
                "remote-direct-origin",
                locale::text(
                    "Operator-managed HTTPS origin",
                    "自管 HTTPS 地址",
                    "自管 HTTPS 位址",
                ),
                &self.direct_origin,
                pending,
                entity.clone(),
                method,
                cx,
            ));
        } else if method == RemoteConnectivityMethod::SelfHostedRelay {
            panel = panel.child(origin_editor(
                "remote-relay-origin",
                locale::text(
                    "Self-hosted Relay origin",
                    "自建 Relay 地址",
                    "自建 Relay 位址",
                ),
                &self.relay_origin,
                pending,
                entity.clone(),
                method,
                cx,
            ));
        }

        if recovery == RemoteRecoveryAction::ConfirmPort {
            let port = snapshot
                .and_then(|snapshot| snapshot.https_port)
                .unwrap_or_default();
            let proposed_origin = origin.unwrap_or_else(|| format!("HTTPS port {port}"));
            let confirm_entity = entity.clone();
            panel = panel.child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .rounded(px(8.0))
                    .bg(cx.theme().warning.opacity(0.10))
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().warning)
                            .child(proposed_origin.clone()),
                    )
                    .child(
                        Button::new("confirm-tailscale-port")
                            .small()
                            .outline()
                            .icon(IconName::CircleCheck)
                            .label(locale::text("Review", "确认", "確認"))
                            .disabled(pending || port == 0)
                            .on_click(move |_, window, cx| {
                                let origin = proposed_origin.clone();
                                let _ = confirm_entity.update(cx, |this, cx| {
                                    this.present_tailscale_confirmation(port, origin, window, cx)
                                });
                            }),
                    ),
            );
        } else if !matches!(
            recovery,
            RemoteRecoveryAction::None | RemoteRecoveryAction::Configure
        ) {
            let repair_entity = entity.clone();
            panel = panel.child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .rounded(px(8.0))
                    .bg(cx.theme().danger.opacity(0.08))
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child(locale::text(
                                "This entry needs attention",
                                "该入口需要处理",
                                "該入口需要處理",
                            )),
                    )
                    .child(
                        Button::new("repair-remote-method")
                            .small()
                            .outline()
                            .icon(IconName::Redo2)
                            .label(recovery_label(recovery))
                            .disabled(pending)
                            .on_click(move |_, _, cx| {
                                let _ = repair_entity.update(cx, |this, cx| {
                                    this.dispatch_action(
                                        RemoteAccessAction::RepairMethod(method),
                                        cx,
                                    )
                                });
                            }),
                    ),
            );
        }

        if desired_enabled {
            let disable_entity = entity.clone();
            panel = panel.child(
                Button::new(SharedString::from(format!(
                    "disable-remote-method-{}",
                    method.wire_name()
                )))
                .small()
                .outline()
                .icon(IconName::Pause)
                .label(locale::text("Disable", "停用", "停用"))
                .disabled(pending)
                .on_click(move |_, _, cx| {
                    let _ = disable_entity.update(cx, |this, cx| {
                        this.dispatch_action(RemoteAccessAction::DisableMethod(method), cx)
                    });
                }),
            );
        } else if recovery != RemoteRecoveryAction::ConfirmPort
            && (method == RemoteConnectivityMethod::TailscaleServe
                || method == RemoteConnectivityMethod::SelfHostedRelay)
        {
            let enable_entity = entity.clone();
            panel = panel.child(
                Button::new(SharedString::from(format!(
                    "enable-remote-method-{}",
                    method.wire_name()
                )))
                .small()
                .primary()
                .icon(IconName::Play)
                .label(locale::text("Enable", "启用", "啟用"))
                .disabled(pending)
                .on_click(move |_, _, cx| {
                    let _ = enable_entity.update(cx, |this, cx| {
                        this.dispatch_action(RemoteAccessAction::EnableMethod(method), cx)
                    });
                }),
            );
        }
        panel.into_any_element()
    }

    fn render_local_network_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(window) = self.state.active_zero_config_window.as_ref() {
            return self.render_lan_window(window, true, cx);
        }

        let entity = cx.weak_entity();
        let can_start = self.state.can_start_zero_config_pairing();
        let pending_start = matches!(
            self.state.pending,
            Some(RemoteAccessMutation::StartZeroConfigPairing)
        );
        let (_, status_color) = self.connection_entry_status(RemoteAccessEntry::LocalNetwork, cx);

        v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .rounded(px(10.0))
            .border_1()
            .border_color(cx.theme().primary.opacity(0.30))
            .bg(cx.theme().muted.opacity(0.14))
            .p_4()
            .child(
                h_flex()
                    .w_full()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(icon_tile(IconName::Map, px(28.0), px(15.0), cx))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_semibold()
                                    .child(connection_entry_name(RemoteAccessEntry::LocalNetwork)),
                            )
                            .child(status_pill(
                                locale::text("On demand", "按需开启", "按需開啟"),
                                status_color,
                            )),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(connection_entry_description(
                                RemoteAccessEntry::LocalNetwork,
                            )),
                    ),
            )
            .child(self.render_permission_selector(true, cx))
            .child(
                Button::new("start-zero-config-pairing")
                    .primary()
                    .w_full()
                    .icon(IconName::Play)
                    .label(locale::text(
                        "Start local pairing",
                        "开始局域网配对",
                        "開始區域網路配對",
                    ))
                    .loading(pending_start)
                    .disabled(!can_start)
                    .on_click(move |_, _, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            this.dispatch_action(RemoteAccessAction::StartZeroConfigPairing, cx)
                        });
                    }),
            )
            .into_any_element()
    }

    fn render_permission_selector(&self, zero_config: bool, cx: &mut Context<Self>) -> AnyElement {
        let permission = if zero_config {
            self.state.zero_config_permission
        } else {
            self.state.permission
        };
        let selected = permission_index(permission);
        let disabled = self.state.pending.is_some()
            || self.state.active_lan_window.is_some()
            || self.state.active_zero_config_window.is_some()
            || (zero_config && self.state.active_offer.is_some())
            || self
                .state
                .active_offer
                .as_ref()
                .is_some_and(|offer| offer.summary.claimed_device_id.is_some());
        let entity = cx.weak_entity();
        v_flex()
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(div().text_xs().font_medium().child(locale::text(
                        "Device permission",
                        "设备权限",
                        "裝置權限",
                    )))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(permission_description(permission)),
                    ),
            )
            .child(
                TabBar::new(if zero_config {
                    "zero-config-pairing-permission"
                } else {
                    "remote-pairing-permission"
                })
                .segmented()
                .selected_index(selected)
                .children(permission_options().into_iter().map(|permission| {
                    Tab::new()
                        .flex_1()
                        .label(permission_label(permission))
                        .disabled(disabled)
                }))
                .on_click(move |index, _, cx| {
                    let permission = permission_options()
                        .get(*index)
                        .copied()
                        .unwrap_or(RemoteDevicePermissionLevel::ReadOnly);
                    let _ = entity.update(cx, |this, cx| {
                        this.dispatch_action(
                            if zero_config {
                                RemoteAccessAction::SetZeroConfigPermission(permission)
                            } else {
                                RemoteAccessAction::SetPermission(permission)
                            },
                            cx,
                        )
                    });
                }),
            )
            .into_any_element()
    }

    fn render_pairing_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.weak_entity();
        if let Some(offer) = self.state.active_offer.as_ref() {
            return self.render_offer_sheet(offer, cx);
        }
        if let Some(window) = self.state.active_lan_window.as_ref() {
            return self.render_lan_window(window, false, cx);
        }
        if let Some(window) = self.state.active_zero_config_window.as_ref() {
            return self.render_lan_window(window, true, cx);
        }
        if self.state.pending.is_some() {
            return v_flex()
                .w_full()
                .items_center()
                .justify_center()
                .gap_3()
                .py(px(48.0))
                .child(
                    Spinner::new()
                        .with_size(Size::Size(px(20.0)))
                        .color(cx.theme().muted_foreground),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(locale::text(
                            "Preparing the pairing window…",
                            "正在准备配对窗口…",
                            "正在準備配對視窗…",
                        )),
                )
                .into_any_element();
        }
        Empty::new()
            .gap_3()
            .py(px(48.0))
            .header(
                EmptyHeader::new().title(EmptyTitle::new().child(locale::text(
                    "Pairing has ended",
                    "配对已结束",
                    "配對已結束",
                ))),
            )
            .content(
                EmptyContent::new().child(
                    Button::new("pairing-ended-back")
                        .small()
                        .outline()
                        .label(locale::text("Back", "返回", "返回"))
                        .on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |this, cx| {
                                this.dispatch_action(RemoteAccessAction::ShowSetup, cx)
                            });
                        }),
                ),
            )
            .into_any_element()
    }

    fn render_offer_sheet(&self, offer: &ActivePairingOffer, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.state.pending.is_some();
        let now_ms = unix_timestamp_ms();
        let terminal = offer.is_terminal(now_ms);
        let claimed = offer.summary.claimed_device_id.is_some();
        let expired = offer.is_expired(now_ms);
        let remaining = offer.remaining_seconds(now_ms);
        let qr = offer
            .private
            .as_ref()
            .map(|private| private.qr_image.clone());
        let qr_size = px(offer.qr_size_px as f32);
        let selected_index = offer
            .entries
            .iter()
            .position(|entry| entry.method == offer.selected_entry)
            .unwrap_or(0);
        let entry_methods = offer
            .entries
            .iter()
            .map(|entry| entry.method)
            .collect::<Vec<_>>();
        let entity = cx.weak_entity();
        let entry_entity = entity.clone();
        let copy_entity = entity.clone();
        let regenerate_entity = entity.clone();
        let cancel_entity = entity.clone();
        let status_label = if claimed {
            locale::text("Device paired", "设备已配对", "裝置已配對")
        } else if offer.summary.canceled {
            locale::text("Offer canceled", "配对请求已取消", "配對請求已取消")
        } else if expired {
            locale::text("Offer expired", "配对请求已过期", "配對請求已過期")
        } else {
            locale::text("Scan to pair", "扫码即可配对", "掃描即可配對")
        };
        let status_color = if claimed {
            cx.theme().success
        } else if terminal {
            cx.theme().muted_foreground
        } else {
            cx.theme().primary
        };

        let entry_selector = TabBar::new("pairing-entry-selector")
            .segmented()
            .selected_index(selected_index)
            .children(offer.entries.iter().map(|entry| {
                Tab::new()
                    .flex_1()
                    .label(method_short_label(entry.method))
                    .disabled(pending || terminal)
            }))
            .on_click(move |index, _, cx| {
                if let Some(method) = entry_methods.get(*index).copied() {
                    let _ = entry_entity.update(cx, |this, cx| {
                        this.dispatch_action(RemoteAccessAction::SelectEntry(method), cx)
                    });
                }
            });

        let qr_visual = div()
            .id("pairing-qr-secret-region")
            .size(qr_size + px(16.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(12.0))
            .border_1()
            .border_color(cx.theme().border)
            .bg(gpui::white())
            .child(if terminal {
                div()
                    .size(qr_size)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(if claimed {
                            IconName::CircleCheck
                        } else {
                            IconName::CircleX
                        })
                        .size(px(36.0))
                        .text_color(if claimed {
                            cx.theme().success
                        } else {
                            cx.theme().muted_foreground
                        }),
                    )
                    .into_any_element()
            } else if let Some(qr) = qr {
                img(qr).size(qr_size).flex_none().into_any_element()
            } else {
                div()
                    .size(qr_size)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Spinner::new()
                            .with_size(Size::Large)
                            .color(cx.theme().muted_foreground),
                    )
                    .into_any_element()
            });

        let controls = v_flex()
            .min_w(px(240.0))
            .flex_1()
            .min_w_0()
            .gap_3()
            .child(
                v_flex()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .min_w_0()
                            .flex_wrap()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .text_base()
                                    .font_semibold()
                                    .text_color(status_color)
                                    .child(status_label),
                            )
                            .when(!terminal, |row| {
                                row.child(
                                    div()
                                        .flex_none()
                                        .rounded_full()
                                        .bg(cx.theme().warning.opacity(0.14))
                                        .px_2()
                                        .py(px(2.0))
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_xs()
                                        .font_medium()
                                        .text_color(cx.theme().warning)
                                        .child(countdown_label(remaining as i64)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if claimed {
                                locale::text(
                                    "The mobile device is now connected",
                                    "移动设备已连接",
                                    "行動裝置已連線",
                                )
                            } else if terminal {
                                locale::text(
                                    "Generate a new QR code to continue",
                                    "生成新的二维码即可继续",
                                    "產生新的 QR Code 即可繼續",
                                )
                            } else {
                                locale::text(
                                    "Open the Vibex mobile app and scan this code",
                                    "打开 Vibex 移动应用扫描此二维码",
                                    "開啟 Vibex 行動應用程式掃描此 QR Code",
                                )
                            }),
                    ),
            )
            .child(self.render_permission_selector(false, cx))
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(div().text_xs().font_medium().child(locale::text(
                        "QR code entry",
                        "二维码使用的入口",
                        "QR Code 使用的入口",
                    )))
                    .child(entry_selector),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("copy-pairing-link")
                            .primary()
                            .icon(IconName::Copy)
                            .label(locale::text("Copy link", "复制链接", "複製連結"))
                            .disabled(pending || terminal)
                            .on_click(move |_, _, cx| {
                                let _ = copy_entity.update(cx, |this, cx| {
                                    this.dispatch_action(RemoteAccessAction::CopyLink, cx)
                                });
                            }),
                    )
                    .child(
                        Button::new("regenerate-pairing-offer")
                            .outline()
                            .icon(IconName::Redo2)
                            .label(locale::text("Regenerate", "重新生成", "重新產生"))
                            .loading(matches!(
                                self.state.pending,
                                Some(RemoteAccessMutation::RegenerateOffer)
                            ))
                            .disabled(!self.state.can_regenerate_offer())
                            .on_click(move |_, _, cx| {
                                let _ = regenerate_entity.update(cx, |this, cx| {
                                    this.dispatch_action(RemoteAccessAction::RegenerateOffer, cx)
                                });
                            }),
                    )
                    .child(
                        Button::new("cancel-pairing-offer")
                            .outline()
                            .icon(IconName::Close)
                            .label(locale::text("Cancel", "取消", "取消"))
                            .loading(matches!(
                                self.state.pending,
                                Some(RemoteAccessMutation::CancelOffer)
                            ))
                            .disabled(pending || terminal)
                            .on_click(move |_, _, cx| {
                                let _ = cancel_entity.update(cx, |this, cx| {
                                    this.dispatch_action(RemoteAccessAction::CancelOffer, cx)
                                });
                            }),
                    ),
            );

        h_flex()
            .w_full()
            .min_w_0()
            .flex_wrap()
            .items_start()
            .gap_4()
            .child(qr_visual)
            .child(controls)
            .into_any_element()
    }

    fn render_lan_window(
        &self,
        window: &RemoteLanPairingWindowSnapshot,
        zero_config: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pending = self.state.pending.is_some();
        let remaining = window
            .discovery
            .expires_at_ms
            .saturating_sub(unix_timestamp_ms())
            .saturating_add(999)
            .div_euclid(1_000)
            .max(0);
        let cancel_entity = cx.weak_entity();
        let cancel_action = if zero_config {
            RemoteAccessAction::CancelZeroConfigPairing
        } else {
            RemoteAccessAction::CancelLanPairing
        };
        let cancel_mutation = if zero_config {
            RemoteAccessMutation::CancelZeroConfigPairing
        } else {
            RemoteAccessMutation::CancelLanPairing
        };
        let pairing_title = if zero_config {
            locale::text(
                "Local network pairing is active",
                "局域网配对进行中",
                "區域網路配對進行中",
            )
        } else {
            locale::text(
                "Nearby pairing is active",
                "附近设备配对进行中",
                "附近裝置配對進行中",
            )
        };

        let mut column = v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .rounded(px(10.0))
            .border_1()
            .border_color(cx.theme().success.opacity(0.45))
            .bg(cx.theme().success.opacity(0.05))
            .p_4()
            .child(
                h_flex()
                    .w_full()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(icon_tile(IconName::Map, px(28.0), px(15.0), cx))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_sm()
                                            .font_semibold()
                                            .child(pairing_title),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(window.advertisement.display_name.clone()),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .rounded_full()
                                    .bg(cx.theme().warning.opacity(0.14))
                                    .px_2()
                                    .py(px(2.0))
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_xs()
                                    .font_medium()
                                    .text_color(cx.theme().warning)
                                    .child(countdown_label(remaining)),
                            )
                            .child(
                                Button::new(if zero_config {
                                    "cancel-zero-config-pairing"
                                } else {
                                    "cancel-lan-pairing"
                                })
                                .small()
                                .outline()
                                .icon(IconName::Pause)
                                .label(locale::text("Stop", "停止", "停止"))
                                .loading(matches!(
                                    self.state.pending,
                                    Some(pending_mutation) if pending_mutation == cancel_mutation
                                ))
                                .disabled(pending)
                                .on_click(move |_, _, cx| {
                                    let _ = cancel_entity.update(cx, |this, cx| {
                                        this.dispatch_action(cancel_action.clone(), cx)
                                    });
                                }),
                            ),
                    ),
            )
            .child(self.render_permission_selector(zero_config, cx));

        if window.pending_requests.is_empty() {
            column = column.child(
                Empty::new()
                    .flex_none()
                    .gap_2()
                    .rounded(px(8.0))
                    .bg(cx.theme().background.opacity(0.5))
                    .px_3()
                    .py(px(28.0))
                    .header(
                        EmptyHeader::new()
                            .media(
                                EmptyMedia::new().mb_0().child(
                                    Icon::new(IconName::Eye)
                                        .size(px(20.0))
                                        .text_color(cx.theme().muted_foreground),
                                ),
                            )
                            .description(EmptyDescription::new().text_xs().child(locale::text(
                                "Waiting for a nearby device",
                                "正在等待附近设备",
                                "正在等待附近裝置",
                            ))),
                    ),
            );
        }
        for request in &window.pending_requests {
            let request_id = request.request_id.clone();
            let reject_id = request.request_id.clone();
            let approve_action = if zero_config {
                RemoteAccessAction::ApproveZeroConfigPairing(request_id.clone())
            } else {
                RemoteAccessAction::ApproveLanPairing(request_id.clone())
            };
            let reject_action = if zero_config {
                RemoteAccessAction::RejectZeroConfigPairing(reject_id.clone())
            } else {
                RemoteAccessAction::RejectLanPairing(reject_id.clone())
            };
            let approve_entity = cx.weak_entity();
            let reject_entity = approve_entity.clone();
            let request_pending = request.state == RemoteLanPairingRequestState::Pending;
            let (state_label, state_color) = match request.state {
                RemoteLanPairingRequestState::Pending => (
                    locale::text("Awaiting confirmation", "等待确认", "等待確認"),
                    cx.theme().warning,
                ),
                RemoteLanPairingRequestState::Approved => (
                    locale::text("Approved", "已允许", "已允許"),
                    cx.theme().success,
                ),
                RemoteLanPairingRequestState::Rejected => (
                    locale::text("Rejected", "已拒绝", "已拒絕"),
                    cx.theme().danger,
                ),
                RemoteLanPairingRequestState::Expired => (
                    locale::text("Expired", "已过期", "已過期"),
                    cx.theme().muted_foreground,
                ),
                RemoteLanPairingRequestState::Claimed => (
                    locale::text("Paired", "已配对", "已配對"),
                    cx.theme().success,
                ),
                RemoteLanPairingRequestState::Unknown => (
                    locale::text("Unavailable", "不可用", "不可用"),
                    cx.theme().muted_foreground,
                ),
            };
            column = column.child(
                v_flex()
                    .w_full()
                    .gap_3()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background.opacity(0.6))
                    .p_3()
                    .child(
                        h_flex()
                            .w_full()
                            .items_start()
                            .justify_between()
                            .gap_3()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .items_center()
                                    .gap_2()
                                    .child(icon_tile(IconName::CircleUser, px(32.0), px(18.0), cx))
                                    .child(
                                        v_flex()
                                            .min_w_0()
                                            .gap_1()
                                            .child(
                                                h_flex()
                                                    .min_w_0()
                                                    .flex_wrap()
                                                    .items_center()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .min_w_0()
                                                            .truncate()
                                                            .text_sm()
                                                            .font_semibold()
                                                            .child(request.display_name.clone()),
                                                    )
                                                    .child(status_pill(state_label, state_color)),
                                            )
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .truncate()
                                                    .font_family(
                                                        cx.theme().mono_font_family.clone(),
                                                    )
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(request.device_fingerprint.clone()),
                                            ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .flex_none()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        div()
                                            .rounded(px(8.0))
                                            .border_1()
                                            .border_color(cx.theme().border)
                                            .bg(gpui::white())
                                            .px_3()
                                            .py_1()
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_xl()
                                            .font_semibold()
                                            .text_color(gpui::black())
                                            .child(format_verification_code(
                                                &request.verification_code,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(locale::text(
                                                "Verification code",
                                                "核对代码",
                                                "核對代碼",
                                            )),
                                    ),
                            ),
                    )
                    .when(request_pending, |card| {
                        card.child(
                            h_flex()
                                .w_full()
                                .flex_wrap()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new(format!(
                                        "approve-{}-pairing-{}",
                                        if zero_config { "zero-config" } else { "lan" },
                                        request.request_id.as_str()
                                    ))
                                    .small()
                                    .primary()
                                    .icon(IconName::Check)
                                    .label(locale::text(
                                        "Code matches, allow",
                                        "代码一致，允许",
                                        "代碼一致，允許",
                                    ))
                                    .disabled(pending)
                                    .on_click(
                                        move |_, _, cx| {
                                            let _ = approve_entity.update(cx, |this, cx| {
                                                this.dispatch_action(approve_action.clone(), cx)
                                            });
                                        },
                                    ),
                                )
                                .child(
                                    Button::new(format!(
                                        "reject-{}-pairing-{}",
                                        if zero_config { "zero-config" } else { "lan" },
                                        request.request_id.as_str()
                                    ))
                                    .small()
                                    .outline()
                                    .icon(IconName::Close)
                                    .label(locale::text("Reject", "拒绝", "拒絕"))
                                    .disabled(pending)
                                    .on_click(
                                        move |_, _, cx| {
                                            let _ = reject_entity.update(cx, |this, cx| {
                                                this.dispatch_action(reject_action.clone(), cx)
                                            });
                                        },
                                    ),
                                ),
                        )
                    }),
            );
        }
        column.into_any_element()
    }

    fn render_setup_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.state.pending.is_some();
        let route_available = self.state.connectivity.as_ref().is_some_and(|snapshot| {
            snapshot
                .methods
                .iter()
                .any(|method| method.candidate_available)
        });
        let desired_enabled = self
            .state
            .connectivity
            .as_ref()
            .is_some_and(|snapshot| snapshot.desired_enabled);
        let create_entity = cx.weak_entity();
        let disable_entity = create_entity.clone();

        v_flex()
            .w_full()
            .gap_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .pt_4()
            .child(self.render_permission_selector(false, cx))
            .child(
                Button::new("create-pairing-offer")
                    .primary()
                    .w_full()
                    .icon(IconName::SquareTerminal)
                    .label(locale::text(
                        "Generate pairing QR code",
                        "生成配对二维码",
                        "產生配對 QR Code",
                    ))
                    .loading(matches!(
                        self.state.pending,
                        Some(RemoteAccessMutation::CreateOffer)
                    ))
                    .disabled(pending || !route_available)
                    .on_click(move |_, _, cx| {
                        let _ = create_entity.update(cx, |this, cx| {
                            this.dispatch_action(RemoteAccessAction::CreateOffer, cx)
                        });
                    }),
            )
            .when(!route_available, |column| {
                column.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .child(
                            Icon::new(IconName::Info)
                                .size(px(14.0))
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(locale::text(
                                    "No validated remote entry is online",
                                    "当前没有已验证的远程入口",
                                    "目前沒有已驗證的遠端入口",
                                )),
                        ),
                )
            })
            .when(desired_enabled, |column| {
                column.child(
                    h_flex().w_full().justify_end().child(
                        Button::new("disable-all-remote-access")
                            .small()
                            .ghost()
                            .icon(IconName::Pause)
                            .label(locale::text("Disable all", "全部停用", "全部停用"))
                            .disabled(pending)
                            .on_click(move |_, window, cx| {
                                let _ = disable_entity.update(cx, |this, cx| {
                                    this.present_disable_all_confirmation(window, cx)
                                });
                            }),
                    ),
                )
            })
            .into_any_element()
    }

    /// Announces the pending light hint on the notification layer.
    ///
    /// The hint answers an action the user just took, so it is shown through the
    /// kit's `Notification` rather than a page banner: the banner sat in the
    /// layout until the next action happened to clear it, and it pushed the
    /// connection list down while it was there.
    fn present_notice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(notice) = self.state.notice.take() else {
            return;
        };
        window.defer(cx, move |window, cx| {
            Theme::global_mut(cx).notification.placement = Anchor::TopCenter;
            let notification = match notice.tone {
                RemoteAccessNoticeTone::Success => Notification::success(notice.message),
                RemoteAccessNoticeTone::Error => Notification::error(notice.message),
            };
            window.push_notification(
                notification
                    .id::<RemoteAccessNoticeNotification>()
                    .autohide(true)
                    .on_click(|_, _, _| {}),
                cx,
            );
        });
    }
}

#[cfg(feature = "e2e-test-support")]
pub struct RemoteAccessPairingE2eDriver {
    pairing: Entity<RemoteAccessPairing>,
}

#[cfg(feature = "e2e-test-support")]
impl RemoteAccessPairingE2eDriver {
    pub fn new(runtime: Arc<DesktopRuntime>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let pairing = cx.new(|cx| RemoteAccessPairing::new(runtime, window, cx));
        pairing.update(cx, |pairing, cx| {
            pairing.dispatch_action(RemoteAccessAction::Refresh, cx)
        });
        Self { pairing }
    }

    pub fn dispatch(
        &mut self,
        action: RemoteAccessPairingE2eAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> VibexResult<()> {
        let action = match action {
            RemoteAccessPairingE2eAction::Refresh => RemoteAccessAction::Refresh,
            RemoteAccessPairingE2eAction::SelectMethod { method } => {
                RemoteAccessAction::SelectConnectionEntry(RemoteAccessEntry::from_remote_method(
                    method,
                ))
            }
            RemoteAccessPairingE2eAction::ConfigureOrigin { method, origin } => {
                if !matches!(
                    method,
                    RemoteConnectivityMethod::Direct | RemoteConnectivityMethod::SelfHostedRelay
                ) {
                    return Err(VibexError::validation(
                        "remote_e2e_origin_method_invalid",
                        "the selected method does not accept an operator origin",
                    ));
                }
                let origin = normalize_https_origin(&origin)?;
                let input = match method {
                    RemoteConnectivityMethod::Direct => self.pairing.read(cx).direct_origin.clone(),
                    RemoteConnectivityMethod::SelfHostedRelay => {
                        self.pairing.read(cx).relay_origin.clone()
                    }
                    RemoteConnectivityMethod::TailscaleServe => unreachable!(),
                };
                input.update(cx, |input, cx| input.set_value(origin, window, cx));
                RemoteAccessAction::SelectConnectionEntry(RemoteAccessEntry::from_remote_method(
                    method,
                ))
            }
            RemoteAccessPairingE2eAction::EnableMethod { method } => {
                RemoteAccessAction::EnableMethod(method)
            }
            RemoteAccessPairingE2eAction::ConfirmTailscalePort { port } => {
                RemoteAccessAction::ConfirmTailscalePort(port)
            }
            RemoteAccessPairingE2eAction::DisableMethod { method } => {
                RemoteAccessAction::DisableMethod(method)
            }
            RemoteAccessPairingE2eAction::RepairMethod { method } => {
                RemoteAccessAction::RepairMethod(method)
            }
            RemoteAccessPairingE2eAction::DisableAll => RemoteAccessAction::DisableAll,
            RemoteAccessPairingE2eAction::SetPermission { permission } => {
                RemoteAccessAction::SetPermission(permission)
            }
            RemoteAccessPairingE2eAction::CreateOffer => RemoteAccessAction::CreateOffer,
            RemoteAccessPairingE2eAction::RegenerateOffer => RemoteAccessAction::RegenerateOffer,
            RemoteAccessPairingE2eAction::CancelOffer => RemoteAccessAction::CancelOffer,
            RemoteAccessPairingE2eAction::SelectEntry { method } => {
                RemoteAccessAction::SelectEntry(method)
            }
        };
        self.pairing
            .update(cx, |pairing, cx| pairing.dispatch_action(action, cx));
        Ok(())
    }

    pub fn snapshot(&self, cx: &App) -> RemoteAccessPairingE2eSnapshot {
        self.pairing.read(cx).state.e2e_snapshot()
    }

    pub fn copy_pairing_link_once(&mut self, cx: &mut Context<Self>) -> VibexResult<String> {
        cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
        self.pairing.update(cx, |pairing, cx| {
            pairing.dispatch_action(RemoteAccessAction::CopyLink, cx)
        });
        let value = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .filter(|value| !value.is_empty() && value.len() <= 16 * 1024)
            .ok_or_else(|| {
                VibexError::capability(
                    "remote_e2e_clipboard_unavailable",
                    "the product pairing action did not produce a bounded clipboard value",
                )
            })?;
        cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
        Ok(value)
    }
}

#[cfg(feature = "e2e-test-support")]
impl Render for RemoteAccessPairingE2eDriver {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.pairing.clone()
    }
}

impl Render for RemoteAccessPairing {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.present_notice(window, cx);
        let page = self.state.page;
        let error = self.state.error_code.as_deref().map(remote_error_label);
        let _safe_snapshot = self.state.safe_snapshot();
        let is_dark = cx.theme().is_dark();
        let popover = theme::semantic_color("popover", is_dark);
        let popover_foreground = theme::semantic_color("popover-foreground", is_dark);

        let page_content = match page {
            RemoteAccessPage::Setup => {
                let mut column = v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_4()
                    .child(self.render_hero_row(cx))
                    .when(self.has_active_pairing(), |column| {
                        column.child(self.render_pairing_resume_strip(cx))
                    })
                    .child(self.render_connection_list(cx))
                    .child(self.render_connection_detail_panel(self.state.selected_entry, cx))
                    .when(
                        self.state.selected_entry != RemoteAccessEntry::LocalNetwork,
                        |column| column.child(self.render_setup_footer(cx)),
                    );
                if let Some(error) = error {
                    column = column.child(self.render_status_banner(
                        IconName::TriangleAlert,
                        cx.theme().danger,
                        error,
                        cx,
                    ));
                }
                column.into_any_element()
            }
            RemoteAccessPage::Devices => self.render_devices_page(cx),
            RemoteAccessPage::Pairing => {
                let back_entity = cx.weak_entity();
                let mut column = v_flex().w_full().min_w_0().gap_3().child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("remote-access-pairing-back")
                                .small()
                                .ghost()
                                .compact()
                                .size(px(28.0))
                                .px_0()
                                .tooltip(locale::text("Back", "返回", "返回"))
                                .child(Icon::new(IconName::ArrowLeft).size(px(17.0)))
                                .on_click(move |_, _, cx| {
                                    let _ = back_entity.update(cx, |this, cx| {
                                        this.dispatch_action(RemoteAccessAction::ShowSetup, cx)
                                    });
                                }),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_sm()
                                .font_semibold()
                                .child(locale::text(
                                    "Pair your mobile device",
                                    "配对移动设备",
                                    "配對行動裝置",
                                )),
                        ),
                );
                if let Some(error) = error {
                    column = column.child(self.render_status_banner(
                        IconName::TriangleAlert,
                        cx.theme().danger,
                        error,
                        cx,
                    ));
                }
                column
                    .child(self.render_pairing_page(cx))
                    .into_any_element()
            }
        };

        let mode_tabs = matches!(page, RemoteAccessPage::Setup | RemoteAccessPage::Devices)
            .then(|| self.render_mode_tabs(cx));

        v_flex()
            .id("remote-access-pairing")
            .w_full()
            .min_h_0()
            .gap_4()
            .bg(popover)
            .text_color(popover_foreground)
            .overflow_y_scroll()
            .pt_2()
            .pr_1()
            .pb_1()
            .when_some(mode_tabs, |column, tabs| column.child(tabs))
            .child(page_content)
    }
}

pub(crate) fn open_remote_access_pairing(
    runtime: Arc<DesktopRuntime>,
    window: &mut Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }
    let view = cx.new(|cx| RemoteAccessPairing::new(runtime, window, cx));
    view.update(cx, |view, cx| {
        view.dispatch_action(RemoteAccessAction::Refresh, cx)
    });
    let close_view = view.clone();
    let viewport = window.viewport_size();
    let dialog_width = (f32::from(viewport.width) - 32.0).clamp(280.0, DIALOG_MAX_WIDTH);
    let dialog_max_height = pairing_dialog_max_height(f32::from(viewport.height));
    window.open_dialog(cx, move |dialog, _, cx| {
        let close_view = close_view.clone();
        let is_dark = cx.theme().is_dark();
        let popover = theme::semantic_color("popover", is_dark);
        let popover_foreground = theme::semantic_color("popover-foreground", is_dark);
        dialog
            // The title and its help glyph are one header row: the glyph opens
            // the remote-development documentation, which is where the pairing
            // and permission choices this dialog offers are explained.
            .title(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(locale::text(
                        "Connect a mobile device",
                        "连接移动设备",
                        "連接行動裝置",
                    ))
                    .child(docs_help_button(
                        "connect-mobile-device-docs",
                        locale::text(
                            "Mobile connection documentation",
                            "移动设备连接文档",
                            "行動裝置連線文件",
                        ),
                        DOCS_REMOTE_MOBILE_URL,
                    )),
            )
            .w(px(dialog_width))
            .max_w(px(dialog_width))
            .h_auto()
            .max_h(px(dialog_max_height))
            .rounded(px(14.0))
            .bg(popover)
            .text_color(popover_foreground)
            .border_color(popover_foreground.opacity(0.10))
            .overlay(true)
            .overlay_closable(true)
            .keyboard(true)
            .child(view.clone())
            .on_close(move |_, _, cx| {
                close_view.update(cx, |view, cx| view.dismiss(cx));
            })
    });
}

fn pairing_dialog_max_height(viewport_height: f32) -> f32 {
    (viewport_height - 32.0).max(1.0)
}

fn compose_private_offer(
    method: RemoteConnectivityMethod,
    launch_fragment: String,
) -> VibexResult<PrivateOfferMaterial> {
    let mut launch_url = Url::parse(&format!("vibex://open/{}", pairing_transport_name(method)))
        .map_err(|_| {
            VibexError::validation(
                "remote_pairing_entry_invalid",
                "mobile pairing entry is invalid",
            )
        })?;
    let fragment = launch_fragment.strip_prefix('#').ok_or_else(|| {
        VibexError::validation(
            "remote_pairing_launch_fragment_invalid",
            "pairing launch fragment is invalid",
        )
    })?;
    launch_url.set_fragment(Some(fragment));
    let (qr_image, qr_size_px) = render_qr(launch_url.as_str())?;
    Ok(PrivateOfferMaterial {
        launch_fragment,
        launch_url,
        qr_image,
        qr_size_px,
    })
}

fn pairing_transport_name(method: RemoteConnectivityMethod) -> &'static str {
    match method {
        RemoteConnectivityMethod::TailscaleServe => "tailnet",
        RemoteConnectivityMethod::Direct => "direct",
        RemoteConnectivityMethod::SelfHostedRelay => "self_hosted_relay",
    }
}

fn format_verification_code(value: &str) -> String {
    if value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_digit()) {
        format!("{} {}", &value[..3], &value[3..])
    } else {
        "--- ---".to_string()
    }
}

fn render_qr(value: &str) -> VibexResult<(Arc<RenderImage>, u32)> {
    let code = QrCode::with_error_correction_level(value.as_bytes(), EcLevel::L).map_err(|_| {
        VibexError::validation(
            "remote_pairing_qr_encode_failed",
            "pairing QR could not be encoded",
        )
    })?;
    let modules = code.width();
    let image_modules = modules.saturating_add(QR_QUIET_ZONE_MODULES * 2);
    let image_size = image_modules.saturating_mul(QR_MODULE_SCALE);
    let image_size = u32::try_from(image_size).map_err(|_| {
        VibexError::validation(
            "remote_pairing_qr_size_invalid",
            "pairing QR dimensions are invalid",
        )
    })?;
    let mut pixels = RgbaImage::from_pixel(image_size, image_size, Rgba([255, 255, 255, 255]));
    for y in 0..modules {
        for x in 0..modules {
            if code[(x, y)] != QrColor::Dark {
                continue;
            }
            let left = (x + QR_QUIET_ZONE_MODULES) * QR_MODULE_SCALE;
            let top = (y + QR_QUIET_ZONE_MODULES) * QR_MODULE_SCALE;
            for offset_y in 0..QR_MODULE_SCALE {
                for offset_x in 0..QR_MODULE_SCALE {
                    pixels.put_pixel(
                        u32::try_from(left + offset_x).unwrap_or_default(),
                        u32::try_from(top + offset_y).unwrap_or_default(),
                        Rgba([0, 0, 0, 255]),
                    );
                }
            }
        }
    }
    Ok((
        Arc::new(RenderImage::new(vec![Frame::new(pixels)])),
        image_size,
    ))
}

fn pairing_entries(summary: &RemotePairingOfferSummary) -> Vec<PairingEntry> {
    let mut entries = Vec::new();
    for candidate in &summary.direct_candidates {
        let method = match candidate.transport {
            RemotePairingTransport::Tailnet => RemoteConnectivityMethod::TailscaleServe,
            RemotePairingTransport::Direct => RemoteConnectivityMethod::Direct,
            RemotePairingTransport::SelfHostedRelay | RemotePairingTransport::Unknown => continue,
        };
        if !entries
            .iter()
            .any(|entry: &PairingEntry| entry.method == method)
        {
            entries.push(PairingEntry { method });
        }
    }
    if summary.relay_candidate.is_some() {
        entries.push(PairingEntry {
            method: RemoteConnectivityMethod::SelfHostedRelay,
        });
    }
    entries.sort_by_key(|entry| method_index(entry.method));
    entries
}

fn preferred_pairing_entry(
    entries: &[PairingEntry],
    preferred: Option<RemoteConnectivityMethod>,
) -> Option<RemoteConnectivityMethod> {
    preferred
        .filter(|method| entries.iter().any(|entry| entry.method == *method))
        .or_else(|| {
            entries
                .iter()
                .any(|entry| entry.method == RemoteConnectivityMethod::TailscaleServe)
                .then_some(RemoteConnectivityMethod::TailscaleServe)
        })
        .or_else(|| entries.first().map(|entry| entry.method))
}

fn origin_editor(
    id: &'static str,
    label: &'static str,
    input: &Entity<InputState>,
    pending: bool,
    entity: WeakEntity<RemoteAccessPairing>,
    method: RemoteConnectivityMethod,
    cx: &App,
) -> AnyElement {
    h_flex()
        .w_full()
        .min_w_0()
        .flex_wrap()
        .items_end()
        .gap_2()
        .child(
            v_flex()
                .min_w(px(220.0))
                .flex_1()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .child(Input::new(input).w_full().disabled(pending)),
        )
        .child(
            Button::new(id)
                .small()
                .primary()
                .icon(IconName::CircleCheck)
                .label(locale::text("Validate", "验证并启用", "驗證並啟用"))
                .disabled(pending)
                .on_click(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.dispatch_action(RemoteAccessAction::EnableMethod(method), cx)
                    });
                }),
        )
        .into_any_element()
}

fn icon_tile(icon: IconName, tile: gpui::Pixels, glyph: gpui::Pixels, cx: &App) -> gpui::Div {
    div()
        .size(tile)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.0))
        .bg(cx.theme().primary.opacity(0.10))
        .child(Icon::new(icon).size(glyph).text_color(cx.theme().primary))
}

fn status_pill(label: impl Into<SharedString>, color: gpui::Hsla) -> gpui::AnyElement {
    let surface = color.opacity(0.12);
    Tag::custom(surface, color, surface)
        .xsmall()
        .rounded_full()
        .px_2()
        .py(px(2.0))
        .font_medium()
        .child(label.into())
        .into_any_element()
}

fn radio_dot(selected: bool, cx: &App) -> gpui::Div {
    div()
        .size(px(16.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(if selected {
            cx.theme().primary
        } else {
            cx.theme().border
        })
        .when(selected, |dot| {
            dot.child(div().size(px(8.0)).rounded_full().bg(cx.theme().primary))
        })
}

fn countdown_label(seconds: i64) -> String {
    let seconds = seconds.max(0) as u64;
    match locale::current_locale() {
        locale::ResolvedLocale::En => format!("{seconds}s"),
        locale::ResolvedLocale::ZhCn | locale::ResolvedLocale::ZhTw => {
            format!("{seconds} 秒")
        }
    }
}

fn method_short_name(method: RemoteConnectivityMethod) -> &'static str {
    match method {
        RemoteConnectivityMethod::TailscaleServe => "Tailscale Serve",
        RemoteConnectivityMethod::Direct => {
            locale::text("Direct HTTPS", "自管 Direct HTTPS", "自管 Direct HTTPS")
        }
        RemoteConnectivityMethod::SelfHostedRelay => {
            locale::text("Self-hosted Relay", "自建 Relay", "自建 Relay")
        }
    }
}

fn connection_entry_name(entry: RemoteAccessEntry) -> &'static str {
    match entry.remote_method() {
        Some(method) => method_short_name(method),
        None => locale::text("Local network pairing", "局域网配对", "區域網路配對"),
    }
}

fn connection_entry_description(entry: RemoteAccessEntry) -> &'static str {
    match entry.remote_method() {
        Some(method) => method_description(method),
        None => locale::text(
            "Discover this computer on the same network",
            "在同一网络中发现此电脑",
            "在同一網路中探索此電腦",
        ),
    }
}

fn connection_entry_id(entry: RemoteAccessEntry) -> String {
    match entry.remote_method() {
        Some(method) => format!("remote-access-method-{}", method.wire_name()),
        None => "remote-access-method-local-network".to_string(),
    }
}

fn permission_options() -> [RemoteDevicePermissionLevel; 3] {
    [
        RemoteDevicePermissionLevel::ReadOnly,
        RemoteDevicePermissionLevel::ApproveOnly,
        RemoteDevicePermissionLevel::FullControl,
    ]
}

#[cfg(feature = "e2e-test-support")]
fn remote_access_mutation_name(mutation: RemoteAccessMutation) -> &'static str {
    match mutation {
        RemoteAccessMutation::Enable(_) => "enable_method",
        RemoteAccessMutation::Disable(_) => "disable_method",
        RemoteAccessMutation::Repair(_) => "repair_method",
        RemoteAccessMutation::DisableAll => "disable_all",
        RemoteAccessMutation::CreateOffer => "create_offer",
        RemoteAccessMutation::RegenerateOffer => "regenerate_offer",
        RemoteAccessMutation::CancelOffer => "cancel_offer",
        RemoteAccessMutation::CancelLanPairing => "cancel_lan_pairing",
        RemoteAccessMutation::ApproveLanPairing => "approve_lan_pairing",
        RemoteAccessMutation::RejectLanPairing => "reject_lan_pairing",
        RemoteAccessMutation::StartZeroConfigPairing => "start_zero_config_pairing",
        RemoteAccessMutation::CancelZeroConfigPairing => "cancel_zero_config_pairing",
        RemoteAccessMutation::ApproveZeroConfigPairing => "approve_zero_config_pairing",
        RemoteAccessMutation::RejectZeroConfigPairing => "reject_zero_config_pairing",
    }
}

fn method_index(method: RemoteConnectivityMethod) -> usize {
    match method {
        RemoteConnectivityMethod::TailscaleServe => 0,
        RemoteConnectivityMethod::Direct => 1,
        RemoteConnectivityMethod::SelfHostedRelay => 2,
    }
}

fn permission_index(permission: RemoteDevicePermissionLevel) -> usize {
    permission_options()
        .iter()
        .position(|candidate| *candidate == permission)
        .unwrap_or(0)
}

fn method_icon(method: RemoteConnectivityMethod) -> IconName {
    match method {
        RemoteConnectivityMethod::TailscaleServe => IconName::Network,
        RemoteConnectivityMethod::Direct => IconName::Globe,
        RemoteConnectivityMethod::SelfHostedRelay => IconName::Building2,
    }
}

fn method_description(method: RemoteConnectivityMethod) -> &'static str {
    match method {
        RemoteConnectivityMethod::TailscaleServe => locale::text(
            "Private remote access through your Tailnet",
            "通过 Tailnet 建立私有远程连接",
            "透過 Tailnet 建立私人遠端連線",
        ),
        RemoteConnectivityMethod::Direct => locale::text(
            "Connect through an operator-managed HTTPS endpoint",
            "通过自管 HTTPS 地址直接连接",
            "透過自管 HTTPS 位址直接連線",
        ),
        RemoteConnectivityMethod::SelfHostedRelay => locale::text(
            "Connect through a self-hosted encrypted relay",
            "通过自建加密 Relay 连接",
            "透過自建加密 Relay 連線",
        ),
    }
}

fn method_short_label(method: RemoteConnectivityMethod) -> &'static str {
    match method {
        RemoteConnectivityMethod::TailscaleServe => "Tailnet",
        RemoteConnectivityMethod::Direct => "Direct",
        RemoteConnectivityMethod::SelfHostedRelay => "Relay",
    }
}

fn permission_label(permission: RemoteDevicePermissionLevel) -> &'static str {
    match permission {
        RemoteDevicePermissionLevel::ReadOnly => locale::text("Read only", "只读", "唯讀"),
        RemoteDevicePermissionLevel::ApproveOnly => {
            locale::text("Approve only", "仅审批", "僅核准")
        }
        RemoteDevicePermissionLevel::FullControl => {
            locale::text("Full control", "完全控制", "完整控制")
        }
    }
}

fn permission_description(permission: RemoteDevicePermissionLevel) -> &'static str {
    match permission {
        RemoteDevicePermissionLevel::ReadOnly => {
            locale::text("View only", "仅查看内容", "僅檢視內容")
        }
        RemoteDevicePermissionLevel::ApproveOnly => {
            locale::text("View and approve", "查看并审批", "檢視並核准")
        }
        RemoteDevicePermissionLevel::FullControl => {
            locale::text("All remote actions", "允许全部远程操作", "允許所有遠端操作")
        }
    }
}

/// The presence set the device list compares against.
///
/// Device ids are compared as their stored text so the set can be held without
/// cloning a [`DeviceId`] per row on every poll.
fn connected_device_keys(device_ids: Vec<DeviceId>) -> BTreeSet<String> {
    device_ids
        .into_iter()
        .map(|device_id| device_id.as_str().to_string())
        .collect()
}

fn device_status_label(status: RemoteDeviceStatus) -> &'static str {
    match status {
        RemoteDeviceStatus::Pending => locale::text("Pending", "待确认", "待確認"),
        RemoteDeviceStatus::Active => locale::text("Active", "已启用", "已啟用"),
        RemoteDeviceStatus::Revoked => locale::text("Revoked", "已撤销", "已撤銷"),
    }
}

fn device_status_color(status: RemoteDeviceStatus, cx: &App) -> gpui::Hsla {
    match status {
        RemoteDeviceStatus::Pending => cx.theme().warning,
        RemoteDeviceStatus::Active => cx.theme().success,
        RemoteDeviceStatus::Revoked => cx.theme().muted_foreground,
    }
}

/// The "when" half of a device row: last connection while the grant is live,
/// revocation time once it is gone, and an explicit empty state before the
/// device has ever connected.
fn device_activity_label(device: &RemoteDeviceDetail) -> String {
    match device.status {
        RemoteDeviceStatus::Revoked => match device.revoked_at_ms {
            Some(at) => format!(
                "{} {}",
                locale::text("revoked", "撤销于", "撤銷於"),
                crate::app::relative_time_label(at)
            ),
            None => locale::text("access revoked", "已撤销访问", "已撤銷存取").to_string(),
        },
        RemoteDeviceStatus::Pending => {
            locale::text("waiting for approval", "等待确认", "等待確認").to_string()
        }
        RemoteDeviceStatus::Active => match device.last_seen_at_ms {
            Some(at) => format!(
                "{} {}",
                locale::text("last seen", "最后在线", "最後上線"),
                crate::app::relative_time_label(at)
            ),
            None => locale::text("never connected", "尚未连接", "尚未連線").to_string(),
        },
    }
}

fn device_management_summary(
    paired: usize,
    revoked: usize,
    audit_count: usize,
    audit_count_capped: bool,
) -> String {
    let audit = if audit_count_capped {
        format!("{audit_count}+")
    } else {
        audit_count.to_string()
    };
    match locale::current_locale() {
        locale::ResolvedLocale::En => {
            format!("{paired} paired · {revoked} revoked · {audit} audit records")
        }
        locale::ResolvedLocale::ZhCn => {
            format!("{paired} 台已配对 · {revoked} 台已撤销 · {audit} 条审计记录")
        }
        locale::ResolvedLocale::ZhTw => {
            format!("{paired} 台已配對 · {revoked} 台已撤銷 · {audit} 條稽核記錄")
        }
    }
}

fn device_list_empty_state(cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .gap_1()
        .rounded(px(8.0))
        .border_1()
        .border_color(cx.theme().border.opacity(0.72))
        .bg(cx.theme().background.opacity(0.4))
        .px_3()
        .py_3()
        .child(
            div()
                .text_sm()
                .font_medium()
                .child(locale::text("No paired devices", "暂无配对设备", "暫無配對裝置")),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(locale::text(
                    "Generate a pairing QR code under Pair, then scan it with the Vibex mobile app. The device appears here once it is paired.",
                    "在「配对」中生成二维码，用 Vibex 移动应用扫描后，设备会显示在这里。",
                    "在「配對」中產生 QR Code，用 Vibex 行動應用程式掃描後，裝置會顯示在這裡。",
                )),
        )
        .into_any_element()
}

fn method_state_label(state: RemoteMethodState) -> &'static str {
    match state {
        RemoteMethodState::Disabled => locale::text("Off", "未启用", "未啟用"),
        RemoteMethodState::Checking => locale::text("Checking", "检查中", "檢查中"),
        RemoteMethodState::ConfirmationNeeded => locale::text("Confirm", "需要确认", "需要確認"),
        RemoteMethodState::Enabling => locale::text("Starting", "启动中", "啟動中"),
        RemoteMethodState::Online => locale::text("Online", "在线", "上線"),
        RemoteMethodState::Degraded => locale::text("Degraded", "连接异常", "連線異常"),
        RemoteMethodState::RepairRequired => locale::text("Repair", "需要修复", "需要修復"),
        RemoteMethodState::Conflict => locale::text("Conflict", "存在冲突", "存在衝突"),
        RemoteMethodState::Stopping => locale::text("Stopping", "停止中", "停止中"),
        RemoteMethodState::Error => locale::text("Error", "错误", "錯誤"),
    }
}

fn method_state_color(state: RemoteMethodState, cx: &App) -> gpui::Hsla {
    match state {
        RemoteMethodState::Online => cx.theme().success,
        RemoteMethodState::Checking
        | RemoteMethodState::ConfirmationNeeded
        | RemoteMethodState::Enabling
        | RemoteMethodState::Stopping => cx.theme().warning,
        RemoteMethodState::Disabled => cx.theme().muted_foreground,
        RemoteMethodState::Degraded
        | RemoteMethodState::RepairRequired
        | RemoteMethodState::Conflict
        | RemoteMethodState::Error => cx.theme().danger,
    }
}

fn recovery_label(action: RemoteRecoveryAction) -> &'static str {
    match action {
        RemoteRecoveryAction::Retry => locale::text("Retry", "重试", "重試"),
        RemoteRecoveryAction::RepairRoute => locale::text("Repair route", "修复路由", "修復路由"),
        RemoteRecoveryAction::ManualCommand => {
            locale::text("Check service", "检查服务", "檢查服務")
        }
        RemoteRecoveryAction::RePair => locale::text("Pair again", "重新配对", "重新配對"),
        RemoteRecoveryAction::None
        | RemoteRecoveryAction::ConfirmPort
        | RemoteRecoveryAction::Configure => locale::text("Repair", "修复", "修復"),
    }
}

fn remote_error_label(code: &str) -> &'static str {
    match code {
        "remote_pairing_routes_unavailable" | "remote_zero_config_pairing_routes_unavailable" => {
            locale::text(
                "Enable and validate at least one remote method",
                "请先启用并验证一种远程连接方式",
                "請先啟用並驗證一種遠端連線方式",
            )
        }
        "remote_direct_origin_missing" | "relay_origin_missing" => locale::text(
            "Enter a valid HTTPS origin",
            "请输入有效的 HTTPS 地址",
            "請輸入有效的 HTTPS 位址",
        ),
        "tailscale_not_found" | "tailscale_daemon_offline" | "tailscale_dns_unavailable" => {
            locale::text(
                "Tailscale is unavailable on this device",
                "此设备上的 Tailscale 不可用",
                "此裝置上的 Tailscale 不可用",
            )
        }
        "remote_direct_probe_client_unavailable" => locale::text(
            "The direct network check could not start. Restart Vibex and try again",
            "无法启动直连网络检查，请重启 Vibex 后重试",
            "無法啟動直連網路檢查，請重新啟動 Vibex 後再試",
        ),
        "remote_direct_probe_direct_failed" => locale::text(
            "The private remote entry could not be reached directly. Check Tailscale Serve or the local firewall",
            "无法直连私有远程入口，请检查 Tailscale Serve 或本机防火墙",
            "無法直連私人遠端入口，請檢查 Tailscale Serve 或本機防火牆",
        ),
        "remote_direct_probe_failed" => locale::text(
            "The remote entry could not be verified through the current network or proxy",
            "无法通过当前网络或代理验证远程入口",
            "無法透過目前網路或代理驗證遠端入口",
        ),
        "remote_pairing_offer_expired" | "remote_pairing_offer_unavailable" => locale::text(
            "The pairing offer is no longer active",
            "配对请求已失效",
            "配對請求已失效",
        ),
        "remote_pairing_qr_encode_failed" | "remote_pairing_qr_size_invalid" => locale::text(
            "The pairing QR could not be generated",
            "无法生成配对二维码",
            "無法產生配對 QR Code",
        ),
        "remote_device_list_failed" | "remote_device_list_task_failed" => locale::text(
            "The paired-device list could not be read",
            "无法读取已配对设备列表",
            "無法讀取已配對裝置清單",
        ),
        "remote_device_revoke_failed"
        | "remote_device_revoke_task_failed"
        | "remote_device_unknown"
        | "remote_device_id_invalid" => locale::text(
            "The device could not be revoked. Refresh the list and try again",
            "无法撤销该设备，请刷新列表后重试",
            "無法撤銷該裝置，請重新整理清單後再試",
        ),
        _ => locale::text(
            "Remote access action failed",
            "远程访问操作失败",
            "遠端存取操作失敗",
        ),
    }
}

fn offer_cancel_error_allows_replacement(code: &str) -> bool {
    matches!(
        code,
        "remote_pairing_offer_expired"
            | "remote_pairing_offer_canceled"
            | "remote_pairing_offer_unknown"
            | "remote_pairing_offer_already_claimed"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use vibex_core::{
        DeviceId, RemotePairingCandidate, RemotePairingOffer, RemoteProtocolVersionRange,
        remote_permissions_for_level,
    };
    use vibex_desktop_runtime::{
        REMOTE_CONNECTIVITY_SCHEMA_VERSION, RemoteMethodSnapshot, RemoteRecoveryAction,
        RemoteRouteOwnership,
    };

    fn connectivity(
        last_successful: Option<RemoteConnectivityMethod>,
    ) -> RemoteConnectivitySnapshot {
        RemoteConnectivitySnapshot {
            schema_version: REMOTE_CONNECTIVITY_SCHEMA_VERSION,
            desired_enabled: true,
            running: true,
            generation: 1,
            methods: RemoteConnectivityMethod::ALL
                .into_iter()
                .map(|method| RemoteMethodSnapshot {
                    method,
                    desired_enabled: true,
                    state: RemoteMethodState::Online,
                    origin: Some(match method {
                        RemoteConnectivityMethod::TailscaleServe => {
                            "https://desktop.tailnet.example".to_string()
                        }
                        RemoteConnectivityMethod::Direct => "https://desktop.example".to_string(),
                        RemoteConnectivityMethod::SelfHostedRelay => {
                            "https://relay.example".to_string()
                        }
                    }),
                    https_port: None,
                    candidate_available: true,
                    last_validated_at_ms: Some(1),
                    ownership: RemoteRouteOwnership::External,
                    error_code: None,
                    recovery_action: RemoteRecoveryAction::None,
                })
                .collect(),
            active_route: Some(RemoteConnectivityMethod::TailscaleServe),
            last_successful_pairing_entry: last_successful,
            direct_route_count: 2,
            relay_connected: true,
            gateway_running: true,
            gateway_bound_addr: None,
        }
    }

    #[test]
    fn pairing_hints_use_top_light_notifications_instead_of_a_page_banner() {
        let source = include_str!("remote_access_pairing.rs");
        let presenter = source
            .split_once("    fn present_notice(")
            .and_then(|(_, tail)| tail.split_once("\n    }\n}"))
            .map(|(body, _)| body)
            .expect("pairing notice presenter should remain inspectable");
        assert!(presenter.contains("self.state.notice.take()"));
        assert!(presenter.contains("Notification::success(notice.message)"));
        assert!(presenter.contains("Notification::error(notice.message)"));
        assert!(presenter.contains(".id::<RemoteAccessNoticeNotification>()"));
        assert!(presenter.contains(".autohide(true)"));
        assert!(presenter.contains(".on_click(|_, _, _| {})"));
        assert!(presenter.contains("Anchor::TopCenter"));

        let render = source
            .split_once("impl Render for RemoteAccessPairing {")
            .and_then(|(_, tail)| tail.split_once("\n}\n"))
            .map(|(body, _)| body)
            .expect("pairing renderer should remain inspectable");
        assert!(render.contains("self.present_notice(window, cx);"));
        // The page keeps the actionable error banner; the light hint is the
        // layer's job now.
        assert!(!render.contains("IconName::CircleCheck"));
        assert!(render.contains("IconName::TriangleAlert"));
    }

    #[test]
    fn remote_probe_errors_explain_proxy_and_private_route_failures() {
        assert_eq!(
            remote_error_label("remote_direct_probe_client_unavailable"),
            locale::text(
                "The direct network check could not start. Restart Vibex and try again",
                "无法启动直连网络检查，请重启 Vibex 后重试",
                "無法啟動直連網路檢查，請重新啟動 Vibex 後再試",
            )
        );
        assert_eq!(
            remote_error_label("remote_direct_probe_failed"),
            locale::text(
                "The remote entry could not be verified through the current network or proxy",
                "无法通过当前网络或代理验证远程入口",
                "無法透過目前網路或代理驗證遠端入口",
            )
        );
        assert_eq!(
            remote_error_label("remote_direct_probe_direct_failed"),
            locale::text(
                "The private remote entry could not be reached directly. Check Tailscale Serve or the local firewall",
                "无法直连私有远程入口，请检查 Tailscale Serve 或本机防火墙",
                "無法直連私人遠端入口，請檢查 Tailscale Serve 或本機防火牆",
            )
        );
    }

    fn offer_response() -> RemoteCreatePairingOfferResponse {
        let summary = RemotePairingOfferSummary {
            format_version: 1,
            protocol_range: RemoteProtocolVersionRange::v2(),
            server_id: "server".to_string(),
            server_identity_public_key: "server-public".to_string(),
            offer_id: RequestId::new(),
            expires_at_ms: unix_timestamp_ms() + i64::from(PAIRING_OFFER_TTL_MS),
            direct_candidates: vec![
                RemotePairingCandidate {
                    transport: RemotePairingTransport::Direct,
                    url: "https://desktop.example".to_string(),
                    relay_room_id: None,
                    relay_pc_peer_id: None,
                    relay_pc_public_key: None,
                },
                RemotePairingCandidate {
                    transport: RemotePairingTransport::Tailnet,
                    url: "https://desktop.tailnet.example".to_string(),
                    relay_room_id: None,
                    relay_pc_peer_id: None,
                    relay_pc_public_key: None,
                },
            ],
            relay_candidate: Some(RemotePairingCandidate {
                transport: RemotePairingTransport::SelfHostedRelay,
                url: "https://relay.example".to_string(),
                relay_room_id: Some(vibex_core::RelayRoomId::new()),
                relay_pc_peer_id: Some(vibex_core::RelayPeerId::new()),
                relay_pc_public_key: Some("relay-public".to_string()),
            }),
            permission_level: RemoteDevicePermissionLevel::ReadOnly,
            granted_permissions: remote_permissions_for_level(
                RemoteDevicePermissionLevel::ReadOnly,
            ),
            canceled: false,
            claimed_device_id: None,
        };
        RemoteCreatePairingOfferResponse {
            offer: vibex_core::RemotePairingOffer {
                summary,
                one_time_challenge: "private-challenge-sentinel".to_string(),
            },
            launch_fragment: "#/pair/private-fragment-sentinel".to_string(),
        }
    }

    #[test]
    fn remote_access_pairing_defaults_to_read_only_and_tailnet() {
        let mut state = PairingViewState::default();
        state.apply_connectivity(connectivity(None));
        state.install_offer(offer_response()).unwrap();

        assert_eq!(state.permission, RemoteDevicePermissionLevel::ReadOnly);
        assert_eq!(
            state.active_offer.as_ref().unwrap().selected_entry,
            RemoteConnectivityMethod::TailscaleServe
        );
    }

    #[test]
    fn remote_access_pairing_uses_only_a_healthy_last_successful_entry() {
        let mut state = PairingViewState::default();
        state.apply_connectivity(connectivity(Some(
            RemoteConnectivityMethod::SelfHostedRelay,
        )));
        state.install_offer(offer_response()).unwrap();
        assert_eq!(
            state.active_offer.as_ref().unwrap().selected_entry,
            RemoteConnectivityMethod::SelfHostedRelay
        );

        let entries = vec![PairingEntry {
            method: RemoteConnectivityMethod::Direct,
        }];
        assert_eq!(
            preferred_pairing_entry(&entries, Some(RemoteConnectivityMethod::SelfHostedRelay)),
            Some(RemoteConnectivityMethod::Direct)
        );
    }

    #[test]
    fn remote_access_pairing_switches_entry_without_changing_offer() {
        let mut active = ActivePairingOffer::from_response(offer_response(), None).unwrap();
        let offer_id = active.offer_id().clone();
        active
            .select_entry(RemoteConnectivityMethod::SelfHostedRelay)
            .unwrap();

        assert_eq!(active.offer_id(), &offer_id);
        assert_eq!(
            active.selected_entry,
            RemoteConnectivityMethod::SelfHostedRelay
        );
        assert_eq!(
            active.summary.permission_level,
            RemoteDevicePermissionLevel::ReadOnly
        );
    }

    #[test]
    fn failed_offer_install_drops_previous_private_material() {
        let mut state = PairingViewState::default();
        state.apply_connectivity(connectivity(None));
        state.install_offer(offer_response()).unwrap();
        let mut invalid = offer_response();
        invalid.launch_fragment = "not-a-fragment".to_string();

        assert!(state.install_offer(invalid).is_err());
        assert!(state.active_offer.is_none());
        assert_eq!(state.permission, RemoteDevicePermissionLevel::ReadOnly);
    }

    #[test]
    fn remote_access_safe_snapshot_never_contains_link_or_qr_payload() {
        let mut state = PairingViewState::default();
        state.apply_connectivity(connectivity(None));
        state.install_offer(offer_response()).unwrap();

        let debug = format!("{:?}", state.safe_snapshot());
        for secret in [
            "private-fragment-sentinel",
            "private-challenge-sentinel",
            "#/pair/",
        ] {
            assert!(!debug.contains(secret));
        }
        assert!(state.safe_snapshot().has_qr);
    }

    #[cfg(feature = "e2e-test-support")]
    #[test]
    fn e2e_snapshot_is_serializable_and_excludes_private_pairing_material() {
        let mut state = PairingViewState::default();
        state.apply_connectivity(connectivity(None));
        state.install_offer(offer_response()).unwrap();

        let serialized = serde_json::to_string(&state.e2e_snapshot()).unwrap();
        for forbidden in [
            "private-fragment-sentinel",
            "private-challenge-sentinel",
            "desktop.example",
            "tailnet.example",
            "relay.example",
            "offerId",
            "deviceId",
            "server-public",
            "#/pair/",
        ] {
            assert!(!serialized.contains(forbidden), "leaked {forbidden}");
        }
        assert!(serialized.contains("remote-access-pairing-e2e.v1"));
        assert!(serialized.contains("\"offerStatus\":\"active\""));
        assert!(serialized.contains("\"permission\":\"read_only\""));
        assert!(serialized.len() < 4_096);
    }

    #[test]
    fn claimed_or_expired_offer_drops_private_material() {
        let mut active = ActivePairingOffer::from_response(offer_response(), None).unwrap();
        let mut claimed = active.summary.clone();
        claimed.claimed_device_id = Some(DeviceId::new());
        active.apply_status(claimed, unix_timestamp_ms());
        assert!(active.private.is_none());

        let mut active = ActivePairingOffer::from_response(offer_response(), None).unwrap();
        let expires_at_ms = active.summary.expires_at_ms;
        active.apply_status(active.summary.clone(), expires_at_ms);
        assert!(active.private.is_none());
    }

    #[test]
    fn claimed_offer_remains_regeneratable_when_idle() {
        let mut state = PairingViewState::default();
        state.apply_connectivity(connectivity(None));
        state.install_offer(offer_response()).unwrap();
        let active = state.active_offer.as_mut().unwrap();
        let mut claimed = active.summary.clone();
        claimed.claimed_device_id = Some(DeviceId::new());
        active.apply_status(claimed, unix_timestamp_ms());

        assert!(state.can_regenerate_offer());
        state.pending = Some(RemoteAccessMutation::RegenerateOffer);
        assert!(!state.can_regenerate_offer());
    }

    #[test]
    fn zero_config_pairing_does_not_require_a_remote_route() {
        let mut state = PairingViewState::default();
        assert!(state.can_start_zero_config_pairing());

        let mut snapshot = connectivity(None);
        snapshot.desired_enabled = false;
        snapshot.running = false;
        snapshot.active_route = None;
        for method in &mut snapshot.methods {
            method.desired_enabled = false;
            method.state = RemoteMethodState::Disabled;
            method.candidate_available = false;
        }
        state.apply_connectivity(snapshot);

        assert!(state.can_start_zero_config_pairing());
        state.pending = Some(RemoteAccessMutation::StartZeroConfigPairing);
        assert!(!state.can_start_zero_config_pairing());
    }

    #[test]
    fn claimed_offer_cancel_error_allows_replacement() {
        for code in [
            "remote_pairing_offer_expired",
            "remote_pairing_offer_canceled",
            "remote_pairing_offer_unknown",
            "remote_pairing_offer_already_claimed",
        ] {
            assert!(offer_cancel_error_allows_replacement(code), "{code}");
        }
        assert!(!offer_cancel_error_allows_replacement(
            "remote_pairing_server_identity_mismatch"
        ));
    }

    #[test]
    fn pairing_dialog_width_and_adaptive_height_fit_the_viewport() {
        let narrow = (360.0_f32 - 32.0).clamp(280.0, DIALOG_MAX_WIDTH);
        let wide = (1_440.0_f32 - 32.0).clamp(280.0, DIALOG_MAX_WIDTH);
        assert_eq!(narrow, 328.0);
        assert_eq!(wide, DIALOG_MAX_WIDTH);
        assert_eq!(pairing_dialog_max_height(900.0), 868.0);
        assert_eq!(pairing_dialog_max_height(24.0), 1.0);
    }

    #[test]
    fn connection_entry_selection_keeps_remote_and_lan_methods_in_one_list() {
        let mut state = PairingViewState::default();
        assert_eq!(state.page, RemoteAccessPage::Setup);
        assert_eq!(
            state.selected_entry,
            RemoteAccessEntry::TailscaleServe,
            "default selection mirrors the first radio row"
        );

        state.select_connection_entry(RemoteAccessEntry::Direct);
        assert_eq!(state.selected_method, RemoteConnectivityMethod::Direct);
        assert_eq!(state.selected_entry, RemoteAccessEntry::Direct);
        assert_eq!(state.page, RemoteAccessPage::Setup);

        state.select_connection_entry(RemoteAccessEntry::LocalNetwork);
        assert_eq!(
            state.selected_method,
            RemoteConnectivityMethod::Direct,
            "LAN rows keep the previously selected remote method intact"
        );
        assert_eq!(state.selected_entry, RemoteAccessEntry::LocalNetwork);
        assert_eq!(state.page, RemoteAccessPage::Setup);

        state.select_connection_entry(RemoteAccessEntry::SelfHostedRelay);
        assert_eq!(
            state.selected_method,
            RemoteConnectivityMethod::SelfHostedRelay
        );
        assert_eq!(state.selected_entry, RemoteAccessEntry::SelfHostedRelay);
        assert_eq!(state.page, RemoteAccessPage::Setup);

        state.show_pairing();
        assert_eq!(state.page, RemoteAccessPage::Pairing);
        state.show_setup();
        assert_eq!(state.page, RemoteAccessPage::Setup);
    }

    #[test]
    fn realistic_pairing_qr_uses_integer_module_pixels_at_display_size() {
        let offer = RemotePairingOffer {
            summary: RemotePairingOfferSummary {
                format_version: 1,
                protocol_range: RemoteProtocolVersionRange::v2(),
                server_id: "server_0123456789abcdef0123456789abcdef".to_string(),
                server_identity_public_key: "A".repeat(43),
                offer_id: RequestId::parse("request_0123456789abcdef0123456789abcdef").unwrap(),
                expires_at_ms: unix_timestamp_ms() + i64::from(PAIRING_OFFER_TTL_MS),
                direct_candidates: vec![RemotePairingCandidate {
                    transport: RemotePairingTransport::Tailnet,
                    url: "https://desktop-name.tail123456.ts.net:8444".to_string(),
                    relay_room_id: None,
                    relay_pc_peer_id: None,
                    relay_pc_public_key: None,
                }],
                relay_candidate: None,
                permission_level: RemoteDevicePermissionLevel::FullControl,
                granted_permissions: remote_permissions_for_level(
                    RemoteDevicePermissionLevel::FullControl,
                ),
                canceled: false,
                claimed_device_id: None,
            },
            one_time_challenge: format!("pair-{}", "B".repeat(43)),
        };
        let launch_fragment = format!(
            "#/pair/{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&offer).unwrap())
        );
        let private =
            compose_private_offer(RemoteConnectivityMethod::TailscaleServe, launch_fragment)
                .unwrap();
        let code =
            QrCode::with_error_correction_level(private.launch_url.as_str().as_bytes(), EcLevel::L)
                .unwrap();
        let expected_size = (code.width() + QR_QUIET_ZONE_MODULES * 2) * QR_MODULE_SCALE;

        assert!(private.launch_url.as_str().len() > 900);
        assert!(
            private
                .launch_url
                .as_str()
                .starts_with("vibex://open/tailnet#/pair/")
        );
        assert!(!private.launch_url.as_str().contains("desktop-name"));
        assert!(
            expected_size <= 560,
            "realistic pairing QR grew beyond the desktop dialog: {expected_size}px"
        );
        assert_eq!(private.qr_size_px as usize, expected_size);
        assert_eq!(
            private.qr_image.size(0).width.0,
            i32::try_from(expected_size).unwrap()
        );
        assert_eq!(
            private.qr_image.size(0).height.0,
            i32::try_from(expected_size).unwrap()
        );
        assert_eq!(private.qr_size_px as usize % QR_MODULE_SCALE, 0);
    }

    fn device_detail(
        status: RemoteDeviceStatus,
        last_seen_at_ms: Option<i64>,
        revoked_at_ms: Option<i64>,
    ) -> RemoteDeviceDetail {
        RemoteDeviceDetail {
            device_id: DeviceId::parse("device_0123456789abcdef0123456789abcdef").unwrap(),
            display_name: "Pixel 8".to_string(),
            public_key: None,
            grant_revision: 1,
            permission_level: RemoteDevicePermissionLevel::FullControl,
            status,
            paired_at_ms: Some(1),
            last_seen_at_ms,
            revoked_at_ms,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn device_activity_prefers_last_seen_and_revocation_over_a_bare_status() {
        let now = unix_timestamp_ms();
        let active = device_detail(RemoteDeviceStatus::Active, Some(now - 120_000), None);
        assert!(device_activity_label(&active).contains(locale::text(
            "last seen",
            "最后在线",
            "最後上線"
        )));

        let never_connected = device_detail(RemoteDeviceStatus::Active, None, None);
        assert_eq!(
            device_activity_label(&never_connected),
            locale::text("never connected", "尚未连接", "尚未連線")
        );

        let revoked = device_detail(
            RemoteDeviceStatus::Revoked,
            Some(now - 300_000),
            Some(now - 60_000),
        );
        assert!(device_activity_label(&revoked).contains(locale::text(
            "revoked",
            "撤销于",
            "撤銷於"
        )));

        let revoked_without_time = device_detail(RemoteDeviceStatus::Revoked, None, None);
        assert_eq!(
            device_activity_label(&revoked_without_time),
            locale::text("access revoked", "已撤销访问", "已撤銷存取")
        );
    }

    #[test]
    fn device_summary_marks_a_capped_audit_total() {
        let summary = device_management_summary(3, 1, 12, false);
        assert!(summary.contains('3'));
        assert!(summary.contains("12"));
        assert!(!summary.contains('+'));
        assert_eq!(summary.matches('·').count(), 2);

        let capped = device_management_summary(3, 1, DEVICE_AUDIT_COUNT_LIMIT as usize, true);
        assert!(capped.contains("500+"));
    }

    fn device_registry(count: usize) -> PairingViewState {
        PairingViewState {
            devices: (0..count)
                .map(|_| device_detail(RemoteDeviceStatus::Revoked, None, Some(1)))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn device_paging_windows_the_registry_and_clamps_the_page() {
        // A registry that fits one page never grows a pager.
        let single = device_registry(DEVICE_PAGE_SIZE);
        assert_eq!(single.device_page_count(), 1);
        assert_eq!(single.device_page_slice().len(), DEVICE_PAGE_SIZE);

        // A longer one pages in fixed slices, with a short final page.
        let mut state = device_registry(DEVICE_PAGE_SIZE * 2 + 3);
        assert_eq!(state.device_page_count(), 3);
        assert_eq!(state.device_page_slice().len(), DEVICE_PAGE_SIZE);

        state.select_device_page(3);
        assert_eq!(state.device_page, 3);
        assert_eq!(state.device_page_slice().len(), 3);

        // Selecting past the end lands on the last page rather than an empty one.
        state.select_device_page(99);
        assert_eq!(state.device_page, 3);
        assert_eq!(state.device_page_slice().len(), 3);

        // Page zero is not a page.
        state.select_device_page(0);
        assert_eq!(state.device_page, 1);
        assert_eq!(state.device_page_slice().len(), DEVICE_PAGE_SIZE);
    }

    #[test]
    fn device_paging_survives_a_registry_that_shrinks_or_empties() {
        let mut state = device_registry(DEVICE_PAGE_SIZE * 2);
        state.select_device_page(2);
        assert_eq!(state.device_page_slice().len(), DEVICE_PAGE_SIZE);

        // Revoking rows away drops the pager back onto a page that still holds
        // devices instead of stranding it on an empty slice.
        state.devices.truncate(DEVICE_PAGE_SIZE);
        state.select_device_page(state.device_page);
        assert_eq!(state.device_page, 1);
        assert_eq!(state.device_page_slice().len(), DEVICE_PAGE_SIZE);

        let empty = device_registry(0);
        assert_eq!(empty.device_page_count(), 1);
        assert!(empty.device_page_slice().is_empty());
    }

    #[test]
    fn device_pager_uses_the_kit_pagination_component() {
        let source = include_str!("remote_access_pairing.rs");
        let pager = source
            .split_once("    fn render_device_pager(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_device_row("))
            .map(|(body, _)| body)
            .expect("device pager should remain inspectable");
        assert!(pager.contains("Pagination::new("));
        assert!(pager.contains(".current_page(page)"));
        assert!(pager.contains(".total_pages(self.state.device_page_count())"));
        assert!(pager.contains("RemoteAccessAction::SelectDevicePage"));

        let devices_page = source
            .split_once("    fn render_devices_page(")
            .and_then(|(_, tail)| tail.split_once("\n    /// Windows the registry"))
            .map(|(body, _)| body)
            .expect("device page should remain inspectable");
        // The pager is only worth its row when the registry actually pages.
        assert!(devices_page.contains("if self.state.device_page_count() > 1 {"));
        assert!(devices_page.contains("self.state.device_page_slice()"));
        assert!(!devices_page.contains("self.state.devices.clone()"));
    }

    /// The dialog title carries the help glyph beside the surface it explains:
    /// pairing, transports, and permissions are the remote-development page's
    /// subject, and the title row is where the dialog names itself.
    #[test]
    fn the_pairing_dialog_title_links_to_the_remote_mobile_docs() {
        let source = include_str!("remote_access_pairing.rs");
        let opener = source
            .split_once("pub(crate) fn open_remote_access_pairing(")
            .and_then(|(_, tail)| tail.split_once("\nfn pairing_dialog_max_height("))
            .map(|(body, _)| body)
            .expect("the pairing dialog opener should remain inspectable");
        assert!(opener.contains(".title("));
        assert!(opener.contains("docs_help_button("));
        assert!(
            opener.contains("DOCS_REMOTE_MOBILE_URL"),
            "the title's help glyph must open the remote development page"
        );
    }

    #[test]
    fn pairing_dialog_renders_device_management_beside_pairing() {
        let source = include_str!("remote_access_pairing.rs");
        let devices_page = source
            .split_once("    fn render_devices_page(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_device_row("))
            .map(|(body, _)| body)
            .expect("device page should remain inspectable");
        assert!(devices_page.contains("RemoteAccessAction::RefreshDevices"));
        assert!(devices_page.contains("device_list_empty_state(cx)"));

        let device_row = source
            .split_once("    fn render_device_row(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_connection_list("))
            .map(|(body, _)| body)
            .expect("device row should remain inspectable");
        assert!(device_row.contains("confirm_revoke_device("));
        assert!(device_row.contains(".danger()"));
        assert!(device_row.contains("revoke-device-"));
        assert!(device_row.contains("confirm_delete_device("));
        assert!(device_row.contains("delete-device-"));

        let renderer = source
            .split_once("impl Render for RemoteAccessPairing {")
            .and_then(|(_, tail)| tail.split_once("\n}\n"))
            .map(|(body, _)| body)
            .expect("pairing renderer should remain inspectable");
        assert!(renderer.contains("RemoteAccessPage::Devices => self.render_devices_page(cx)"));
        assert!(renderer.contains("self.render_mode_tabs(cx)"));
    }

    /// Deleting a record is available in both states, and the confirmation
    /// names the extra consequence an active row carries.
    #[test]
    fn deleting_a_device_record_covers_revoked_and_active_rows() {
        let source = include_str!("remote_access_pairing.rs");
        let confirmation = source
            .split_once("    fn confirm_delete_device(")
            .and_then(|(_, tail)| tail.split_once("\n    fn confirm_revoke_device("))
            .map(|(body, _)| body)
            .expect("device delete confirmation should remain inspectable");
        assert!(confirmation.contains("still holds access"));
        assert!(confirmation.contains("revokes the grant"));
        assert!(confirmation.contains("audit history is kept"));
        assert!(confirmation.contains("RemoteAccessAction::DeleteDevice("));

        let deletion = source
            .split_once("    fn delete_device(")
            .and_then(|(_, tail)| tail.split_once("\n    fn confirm_delete_device("))
            .map(|(body, _)| body)
            .expect("device deletion should remain inspectable");
        assert!(deletion.contains("remote.delete_device(RemoteDeleteDeviceRequest {"));
        assert!(deletion.contains("this.refresh_devices(cx);"));

        // The row locks both commands while either one runs, so a delete can
        // never race the revoke it depends on.
        let state = source
            .split_once("    fn device_mutation_pending(")
            .and_then(|(_, tail)| tail.split_once("\n    fn select_connection_entry("))
            .map(|(body, _)| body)
            .expect("device mutation state should remain inspectable");
        assert!(state.contains("revoking_device.is_some()"));
        assert!(state.contains("deleting_device.is_some()"));
        assert!(state.contains("restoring_device.is_some()"));
    }

    /// Restoring is offered exactly where a record is revoked, and the presence
    /// poll follows the device page instead of the whole dialog.
    #[test]
    fn revoked_devices_offer_restore_and_presence_follows_the_device_page() {
        let source = include_str!("remote_access_pairing.rs");

        let row = source
            .split_once("    fn render_device_row(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_connection_list("))
            .map(|(body, _)| body)
            .expect("device row should remain inspectable");
        assert!(row.contains("confirm_restore_device("));
        assert!(row.contains("restore-device-"));
        assert!(row.contains("connected_devices.contains("));
        // Presence replaces the stored sentence rather than contradicting it.
        assert!(row.contains("device_activity_label(&device)"));

        let restore = source
            .split_once("    fn confirm_restore_device(")
            .and_then(|(_, tail)| tail.split_once("\n    fn confirm_revoke_device("))
            .map(|(body, _)| body)
            .expect("device restore confirmation should remain inspectable");
        assert!(restore.contains("RemoteAccessAction::RestoreDevice("));
        assert!(restore.contains("without pairing again"));

        let restoration = source
            .split_once("    fn restore_device(")
            .and_then(|(_, tail)| tail.split_once("\n    fn confirm_restore_device("))
            .map(|(body, _)| body)
            .expect("device restore should remain inspectable");
        assert!(restoration.contains("remote.restore_device(RemoteRestoreDeviceRequest {"));
        assert!(restoration.contains("this.refresh_devices(cx);"));

        let dispatcher = source
            .split_once("    fn dispatch_action(")
            .and_then(|(_, tail)| tail.split_once("\n    fn refresh("))
            .map(|(body, _)| body)
            .expect("action dispatcher should remain inspectable");
        assert!(dispatcher.contains("self.schedule_presence_poll(cx);"));
        assert!(dispatcher.contains("self.stop_presence_poll();"));

        let dismiss = source
            .split_once("    fn dismiss(")
            .and_then(|(_, tail)| tail.split_once("\n    fn "))
            .map(|(body, _)| body)
            .expect("dialog dismissal should remain inspectable");
        assert!(dismiss.contains("self.stop_presence_poll();"));
    }

    /// A dialog builder is evaluated again on every repaint, so an input created
    /// inside one is replaced — focus handle and text included — as soon as the
    /// user types, which makes the rename field impossible to edit.
    #[test]
    fn device_rename_dialog_keeps_one_input_entity_across_repaints() {
        let source = include_str!("remote_access_pairing.rs");
        let dialog = source
            .split_once("    fn confirm_rename_device(")
            .and_then(|(_, tail)| tail.split_once("\n    fn confirm_delete_device("))
            .map(|(body, _)| body)
            .expect("the device rename dialog should remain inspectable");

        let input_creation = dialog
            .find("let input = cx.new(")
            .expect("the device name input should be created once");
        let dialog_open = dialog
            .find("window.open_dialog(cx")
            .expect("the rename dialog should open after its input exists");
        assert!(
            input_creation < dialog_open,
            "the input entity must be created outside the dialog builder"
        );
        assert!(
            !dialog[dialog_open..].contains("cx.new("),
            "the dialog builder must not replace the input entity on every repaint"
        );
        assert!(
            dialog.contains("window.on_next_frame(move |window, cx|"),
            "the input should take focus after the dialog's focus trap mounts"
        );
        assert!(
            dialog.contains("input.set_selected_range(0..selection_end, cx)"),
            "the current name should be selected so typing replaces it"
        );
    }
}

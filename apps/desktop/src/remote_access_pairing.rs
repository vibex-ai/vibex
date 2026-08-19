use std::{future::Future, sync::Arc, time::Duration};

use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, IntoElement, KeyDownEvent, Render,
    RenderImage, Role, SharedString, Subscription, Task, WeakEntity, Window, div, img, prelude::*,
    px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::DialogButtonProps,
    h_flex,
    input::{Input, InputEvent, InputState},
    switch::Switch,
    tab::{Tab, TabBar},
    v_flex,
};
use image::{Frame, Rgba, RgbaImage};
use qrcode::{Color as QrColor, EcLevel, QrCode};
#[cfg(feature = "e2e-test-support")]
use serde::{Deserialize, Serialize};
use url::Url;
use vibex_core::{
    RemoteCreatePairingOfferResponse, RemoteDevicePermissionLevel, RemoteLanPairingRequestState,
    RemoteLanPairingWindowSnapshot, RemotePairingOfferSummary, RemotePairingTransport, RequestId,
    VibexError, VibexResult, unix_timestamp_ms,
};
use vibex_desktop_runtime::{
    DesktopRuntime, RemoteConnectivityController, RemoteConnectivityMethod,
    RemoteConnectivitySnapshot, RemoteMethodState, RemoteRecoveryAction, normalize_https_origin,
};

use crate::locale;

const PAIRING_OFFER_TTL_MS: u32 = 90_000;
const OFFER_POLL_INTERVAL: Duration = Duration::from_millis(500);
const QR_QUIET_ZONE_MODULES: usize = 4;
const QR_MODULE_SCALE: usize = 4;
const DIALOG_MAX_WIDTH: f32 = 760.0;
const DIALOG_MAX_HEIGHT: f32 = 720.0;

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

impl RemoteAccessEntry {
    const ALL: [Self; 4] = [
        Self::TailscaleServe,
        Self::Direct,
        Self::SelfHostedRelay,
        Self::LocalNetwork,
    ];

    fn remote_method(self) -> Option<RemoteConnectivityMethod> {
        match self {
            Self::TailscaleServe => Some(RemoteConnectivityMethod::TailscaleServe),
            Self::Direct => Some(RemoteConnectivityMethod::Direct),
            Self::SelfHostedRelay => Some(RemoteConnectivityMethod::SelfHostedRelay),
            Self::LocalNetwork => None,
        }
    }

    fn from_remote_method(method: RemoteConnectivityMethod) -> Self {
        match method {
            RemoteConnectivityMethod::TailscaleServe => Self::TailscaleServe,
            RemoteConnectivityMethod::Direct => Self::Direct,
            RemoteConnectivityMethod::SelfHostedRelay => Self::SelfHostedRelay,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteAccessAction {
    Refresh,
    SelectMethod(RemoteConnectivityMethod),
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
    selected_method: RemoteConnectivityMethod,
    selected_entry: RemoteAccessEntry,
    permission: RemoteDevicePermissionLevel,
    zero_config_permission: RemoteDevicePermissionLevel,
    active_offer: Option<ActivePairingOffer>,
    active_lan_window: Option<RemoteLanPairingWindowSnapshot>,
    active_zero_config_window: Option<RemoteLanPairingWindowSnapshot>,
    pending: Option<RemoteAccessMutation>,
    error_code: Option<String>,
    notice: Option<&'static str>,
}

impl Default for PairingViewState {
    fn default() -> Self {
        Self {
            connectivity: None,
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
        }
    }
}

impl PairingViewState {
    fn apply_connectivity(&mut self, snapshot: RemoteConnectivitySnapshot) {
        self.connectivity = Some(snapshot);
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
    state: PairingViewState,
    direct_origin: Entity<InputState>,
    relay_origin: Entity<InputState>,
    refresh_task: Option<Task<()>>,
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
            state: PairingViewState::default(),
            direct_origin,
            relay_origin,
            refresh_task: None,
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
            RemoteAccessAction::SelectMethod(method) => {
                self.state.selected_method = method;
                self.state.selected_entry = RemoteAccessEntry::from_remote_method(method);
                self.state.error_code = None;
                cx.notify();
            }
            RemoteAccessAction::SelectConnectionEntry(entry) => {
                self.state.selected_entry = entry;
                if let Some(method) = entry.remote_method() {
                    self.state.selected_method = method;
                }
                self.state.error_code = None;
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
                            this.state.notice = Some(locale::text(
                                "Remote access disabled",
                                "远程访问已停用",
                                "遠端存取已停用",
                            ));
                        }
                        Ok(Ok(RemoteAccessMutationOutcome::OfferCreated(response))) => {
                            if let Err(error) = this.state.install_offer(response) {
                                this.state.error_code = Some(error.code);
                            } else {
                                this.schedule_offer_poll(cx);
                            }
                        }
                        Ok(Ok(RemoteAccessMutationOutcome::OfferCreationFailed(error))) => {
                            this.clear_offer();
                            this.state.error_code = Some(error.code);
                        }
                        Ok(Ok(RemoteAccessMutationOutcome::OfferCanceled)) => {
                            this.clear_offer();
                            this.state.notice = Some(locale::text(
                                "Pairing offer canceled",
                                "配对请求已取消",
                                "配對請求已取消",
                            ));
                        }
                        Ok(Ok(RemoteAccessMutationOutcome::LanWindow(snapshot))) => {
                            this.state.selected_entry = RemoteAccessEntry::Direct;
                            this.state.active_lan_window = Some(snapshot);
                            this.schedule_lan_poll(cx);
                        }
                        Ok(Ok(RemoteAccessMutationOutcome::LanCanceled)) => {
                            this.clear_lan_window();
                            this.state.notice = Some(locale::text(
                                "Nearby pairing stopped",
                                "附近配对已停止",
                                "附近配對已停止",
                            ));
                        }
                        Ok(Ok(RemoteAccessMutationOutcome::ZeroConfigWindow(snapshot))) => {
                            this.state.selected_entry = RemoteAccessEntry::LocalNetwork;
                            this.state.active_zero_config_window = Some(snapshot);
                            this.schedule_zero_config_poll(cx);
                        }
                        Ok(Ok(RemoteAccessMutationOutcome::ZeroConfigCanceled)) => {
                            this.clear_zero_config_window();
                            this.state.notice = Some(locale::text(
                                "Local pairing stopped",
                                "局域网配对已停止",
                                "區域網路配對已停止",
                            ));
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
            locale::text("Pairing link copied", "配对链接已复制", "配對連結已複製")
        } else {
            locale::text("Clipboard write failed", "无法写入剪贴板", "無法寫入剪貼簿")
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
                                    this.state.notice = Some(locale::text(
                                        "Device paired",
                                        "设备已配对",
                                        "裝置已配對",
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
                            this.state.notice = Some(if had_approved_request {
                                locale::text("Device paired", "设备已配对", "裝置已配對")
                            } else {
                                locale::text(
                                    "Nearby pairing ended",
                                    "附近配对已结束",
                                    "附近配對已結束",
                                )
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
                            let controller = this.controller.clone();
                            gpui_tokio::Tokio::spawn(cx, async move {
                                let _ = controller.cancel_zero_config_lan_pairing().await;
                            })
                            .detach();
                            this.state.notice = Some(if had_approved_request {
                                locale::text("Device paired", "设备已配对", "裝置已配對")
                            } else {
                                locale::text(
                                    "Local pairing ended",
                                    "局域网配对已结束",
                                    "區域網路配對已結束",
                                )
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

    fn render_connection_entry_selector(&self, cx: &mut Context<Self>) -> AnyElement {
        let disabled = self.has_active_pairing();
        v_flex()
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .child(self.render_connection_entry_card(
                        RemoteAccessEntry::TailscaleServe,
                        disabled,
                        cx,
                    ))
                    .child(self.render_connection_entry_card(
                        RemoteAccessEntry::Direct,
                        disabled,
                        cx,
                    )),
            )
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .child(self.render_connection_entry_card(
                        RemoteAccessEntry::SelfHostedRelay,
                        disabled,
                        cx,
                    ))
                    .child(self.render_connection_entry_card(
                        RemoteAccessEntry::LocalNetwork,
                        disabled,
                        cx,
                    )),
            )
            .into_any_element()
    }

    fn render_connection_entry_card(
        &self,
        entry: RemoteAccessEntry,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.state.selected_entry == entry;
        let entity = cx.weak_entity();
        let keyboard_entity = entity.clone();
        let action = entry
            .remote_method()
            .map(RemoteAccessAction::SelectMethod)
            .unwrap_or(RemoteAccessAction::SelectConnectionEntry(entry));
        let keyboard_action = action.clone();
        let (status, status_color) = match entry {
            RemoteAccessEntry::LocalNetwork => {
                if self.state.active_zero_config_window.is_some() {
                    (
                        locale::text("Discovering", "发现中", "探索中"),
                        cx.theme().success,
                    )
                } else {
                    (
                        locale::text("Ready", "待开启", "待開啟"),
                        cx.theme().muted_foreground,
                    )
                }
            }
            RemoteAccessEntry::TailscaleServe
            | RemoteAccessEntry::Direct
            | RemoteAccessEntry::SelfHostedRelay => {
                let method = entry.remote_method().expect("remote entry has a method");
                let snapshot = self
                    .state
                    .connectivity
                    .as_ref()
                    .and_then(|connectivity| connectivity.method(method));
                let state = snapshot.map_or(RemoteMethodState::Disabled, |snapshot| snapshot.state);
                (method_state_label(state), method_state_color(state, cx))
            }
        };
        let icon = match entry {
            RemoteAccessEntry::LocalNetwork => IconName::Network,
            RemoteAccessEntry::TailscaleServe
            | RemoteAccessEntry::Direct
            | RemoteAccessEntry::SelfHostedRelay => {
                method_icon(entry.remote_method().expect("remote entry has a method"))
            }
        };
        let label = connection_entry_label(entry);
        let description = connection_entry_description(entry);
        let accessibility_label = format!("{label}: {description}");

        v_flex()
            .id(SharedString::from(format!(
                "remote-access-entry-{}",
                connection_entry_index(entry)
            )))
            .flex_1()
            .min_w_0()
            .min_h(px(92.0))
            .justify_between()
            .gap_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(if selected {
                cx.theme().primary.opacity(0.72)
            } else {
                cx.theme().border.opacity(0.72)
            })
            .bg(if selected {
                cx.theme().primary.opacity(0.08)
            } else {
                cx.theme().background.opacity(0.55)
            })
            .p_3()
            .cursor_pointer()
            .focusable()
            .tab_stop(!disabled)
            .role(Role::Button)
            .aria_label(accessibility_label)
            .aria_selected(selected)
            .hover(|style| {
                style.bg(if selected {
                    cx.theme().primary.opacity(0.12)
                } else {
                    cx.theme().accent
                })
            })
            .focus_visible(|style| {
                style.shadow(vec![
                    gpui::BoxShadow::new(px(0.0), px(0.0), cx.theme().ring).spread_radius(px(2.0)),
                ])
            })
            .when(disabled, |card| card.opacity(0.62).cursor_default())
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .size(px(28.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.0))
                            .bg(if selected {
                                cx.theme().primary.opacity(0.14)
                            } else {
                                cx.theme().muted.opacity(0.32)
                            })
                            .child(Icon::new(icon).size(px(15.0)).text_color(if selected {
                                cx.theme().primary
                            } else {
                                cx.theme().muted_foreground
                            })),
                    )
                    .child(connection_entry_card_copy(label, description, cx)),
            )
            .child(
                div()
                    .text_xs()
                    .font_medium()
                    .text_color(status_color)
                    .child(status),
            )
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

    fn render_connection_entry_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.state.selected_entry {
            RemoteAccessEntry::TailscaleServe => {
                self.render_method_panel(RemoteConnectivityMethod::TailscaleServe, cx)
            }
            RemoteAccessEntry::Direct => {
                self.render_method_panel(RemoteConnectivityMethod::Direct, cx)
            }
            RemoteAccessEntry::SelfHostedRelay => {
                self.render_method_panel(RemoteConnectivityMethod::SelfHostedRelay, cx)
            }
            RemoteAccessEntry::LocalNetwork => self.render_local_network_panel(cx),
        }
    }

    fn render_method_panel(
        &self,
        method: RemoteConnectivityMethod,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let snapshot = self
            .state
            .connectivity
            .as_ref()
            .and_then(|connectivity| connectivity.method(method));
        let pending = self.state.pending.is_some();
        let desired_enabled = snapshot.is_some_and(|snapshot| snapshot.desired_enabled);
        let state = snapshot.map_or(RemoteMethodState::Disabled, |snapshot| snapshot.state);
        let entity = cx.weak_entity();
        let toggle_entity = entity.clone();
        let status_color = method_state_color(state, cx);
        let recovery = snapshot.map_or(RemoteRecoveryAction::None, |snapshot| {
            snapshot.recovery_action
        });
        let origin = snapshot.and_then(|snapshot| snapshot.origin.clone());

        let mut panel = v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(cx.theme().border.opacity(0.72))
            .bg(cx.theme().muted.opacity(0.12))
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
                            .child(Icon::new(method_icon(method)).size(px(16.0)))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        h_flex()
                                            .flex_wrap()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_semibold()
                                                    .child(method_label(method)),
                                            )
                                            .child(
                                                div()
                                                    .rounded(px(4.0))
                                                    .bg(status_color.opacity(0.12))
                                                    .px_1()
                                                    .py(px(1.0))
                                                    .text_xs()
                                                    .text_color(status_color)
                                                    .child(method_state_label(state)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(method_description(method)),
                                    )
                                    .when_some(origin.clone(), |column, origin| {
                                        column.child(
                                            div()
                                                .max_w(px(500.0))
                                                .truncate()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(origin),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        Switch::new(SharedString::from(format!(
                            "remote-method-toggle-{}",
                            method.wire_name()
                        )))
                        .checked(desired_enabled)
                        .disabled(pending)
                        .label(locale::text("Enabled", "启用", "啟用"))
                        .on_click(move |checked, _, cx| {
                            let _ = toggle_entity.update(cx, |this, cx| {
                                if *checked {
                                    this.dispatch_action(
                                        RemoteAccessAction::EnableMethod(method),
                                        cx,
                                    );
                                } else {
                                    this.dispatch_action(
                                        RemoteAccessAction::DisableMethod(method),
                                        cx,
                                    );
                                }
                            });
                        }),
                    ),
            );

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
                    .rounded(px(6.0))
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
            panel = panel.child(
                Button::new("repair-remote-method")
                    .small()
                    .outline()
                    .icon(IconName::Redo2)
                    .label(recovery_label(recovery))
                    .disabled(pending)
                    .on_click(move |_, _, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            this.dispatch_action(RemoteAccessAction::RepairMethod(method), cx)
                        });
                    }),
            );
        }
        panel.into_any_element()
    }

    fn render_local_network_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.state.active_zero_config_window.is_some();
        let status_color = if active {
            cx.theme().success
        } else {
            cx.theme().muted_foreground
        };
        let status = if active {
            locale::text("Discovering", "发现中", "探索中")
        } else {
            locale::text("Ready when needed", "按需开启", "需要時開啟")
        };

        v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(cx.theme().border.opacity(0.72))
            .bg(cx.theme().muted.opacity(0.12))
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
                            .items_start()
                            .gap_3()
                            .child(
                                div()
                                    .size(px(32.0))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.0))
                                    .bg(cx.theme().primary.opacity(0.10))
                                    .child(
                                        Icon::new(IconName::Network)
                                            .size(px(17.0))
                                            .text_color(cx.theme().primary),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        div().text_sm().font_semibold().child(locale::text(
                                            "Local network",
                                            "局域网",
                                            "區域網路",
                                        )),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(locale::text(
                                                "A private HTTPS entry for devices on the same network",
                                                "供同一网络设备使用的私有 HTTPS 入口",
                                                "供同一網路裝置使用的私人 HTTPS 入口",
                                            )),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .rounded(px(4.0))
                            .bg(status_color.opacity(0.12))
                            .px_2()
                            .py(px(2.0))
                            .text_xs()
                            .text_color(status_color)
                            .child(status),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(locale::text(
                        "This entry does not require Tailscale, Direct, or Relay.",
                        "此入口不依赖 Tailscale、Direct 或 Relay。",
                        "此入口不依賴 Tailscale、Direct 或 Relay。",
                    )),
            )
            .into_any_element()
    }

    fn render_pairing_section(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.state.selected_entry {
            RemoteAccessEntry::LocalNetwork => self.render_zero_config_pairing(cx),
            RemoteAccessEntry::TailscaleServe
            | RemoteAccessEntry::Direct
            | RemoteAccessEntry::SelfHostedRelay => self.render_offer(cx),
        }
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

    fn render_offer(&self, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.state.pending.is_some();
        let route_available = self.state.connectivity.as_ref().is_some_and(|snapshot| {
            snapshot
                .methods
                .iter()
                .any(|method| method.candidate_available)
        });
        if let Some(window) = self.state.active_lan_window.as_ref() {
            return self.render_lan_window(window, false, cx);
        }
        let Some(offer) = self.state.active_offer.as_ref() else {
            let entity = cx.weak_entity();
            return v_flex()
                .w_full()
                .gap_3()
                .child(self.render_permission_selector(false, cx))
                .child(
                    Button::new("create-pairing-offer")
                        .primary()
                        .w_full()
                        .icon(IconName::Plus)
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
                            let _ = entity.update(cx, |this, cx| {
                                this.dispatch_action(RemoteAccessAction::CreateOffer, cx)
                            });
                        }),
                )
                .when(!route_available, |column| {
                    column.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(locale::text(
                                "No validated remote entry is online",
                                "当前没有已验证的远程入口",
                                "目前沒有已驗證的遠端入口",
                            )),
                    )
                })
                .into_any_element();
        };

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
            locale::text("Claimed", "已领取", "已領取")
        } else if offer.summary.canceled {
            locale::text("Canceled", "已取消", "已取消")
        } else if expired {
            locale::text("Expired", "已过期", "已過期")
        } else {
            locale::text("Ready", "等待配对", "等待配對")
        };
        let countdown = match locale::current_locale() {
            locale::ResolvedLocale::En => format!("{remaining}s"),
            locale::ResolvedLocale::ZhCn | locale::ResolvedLocale::ZhTw => {
                format!("{remaining} 秒")
            }
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

        let offer_visual = v_flex()
            .flex_none()
            .items_center()
            .gap_2()
            .when_some(qr, |column, qr| {
                column.child(
                    div()
                        .id("pairing-qr-secret-region")
                        .size(qr_size)
                        .flex_none()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(gpui::white())
                        .child(img(qr).size(qr_size).flex_none()),
                )
            })
            .when(terminal, |column| {
                column.child(
                    div()
                        .size(qr_size)
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().muted.opacity(0.4))
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
                        ),
                )
            });

        let offer_controls = v_flex()
            .flex_1()
            .min_w_0()
            .gap_3()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(div().text_sm().font_semibold().child(status_label))
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(if expired {
                                cx.theme().danger
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(countdown),
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

        v_flex()
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .items_start()
                    .gap_4()
                    .child(offer_visual)
                    .child(offer_controls),
            )
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
                "Local network pairing is available",
                "局域网配对已开启",
                "區域網路配對已開啟",
            )
        } else {
            locale::text(
                "Nearby pairing is available",
                "附近设备可以配对",
                "附近裝置可以配對",
            )
        };
        let mut column = v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(cx.theme().border.opacity(0.72))
            .bg(cx.theme().muted.opacity(0.12))
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
                            .items_start()
                            .gap_3()
                            .child(
                                div()
                                    .size(px(30.0))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.0))
                                    .bg(cx.theme().primary.opacity(0.10))
                                    .child(
                                        Icon::new(IconName::Network)
                                            .size(px(16.0))
                                            .text_color(cx.theme().primary),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .gap_1()
                                    .child(div().text_sm().font_semibold().child(pairing_title))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(window.advertisement.display_name.clone()),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{remaining}s")),
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
                div()
                    .w_full()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .pt_3()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(locale::text(
                        "Waiting for a nearby device",
                        "正在等待附近设备",
                        "正在等待附近裝置",
                    )),
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
            let state_label = match request.state {
                RemoteLanPairingRequestState::Pending => {
                    locale::text("Confirm code", "核对代码", "核對代碼")
                }
                RemoteLanPairingRequestState::Approved => {
                    locale::text("Approved", "已允许", "已允許")
                }
                RemoteLanPairingRequestState::Rejected => {
                    locale::text("Rejected", "已拒绝", "已拒絕")
                }
                RemoteLanPairingRequestState::Expired => {
                    locale::text("Expired", "已过期", "已過期")
                }
                RemoteLanPairingRequestState::Claimed => locale::text("Paired", "已配对", "已配對"),
                RemoteLanPairingRequestState::Unknown => {
                    locale::text("Unavailable", "不可用", "不可用")
                }
            };
            column = column.child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .pt_3()
                    .child(
                        h_flex()
                            .w_full()
                            .items_start()
                            .justify_between()
                            .gap_3()
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_semibold()
                                            .child(request.display_name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "{} {}",
                                                locale::text("Fingerprint", "设备指纹", "裝置指紋"),
                                                request.device_fingerprint
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .text_lg()
                                    .font_semibold()
                                    .child(format_verification_code(&request.verification_code)),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(state_label),
                            )
                            .when(request_pending, |row| {
                                row.child(
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
                                )
                            }),
                    ),
            );
        }
        column.into_any_element()
    }

    fn render_zero_config_pairing(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(window) = self.state.active_zero_config_window.as_ref() {
            return self.render_lan_window(window, true, cx);
        }

        let can_start = self.state.can_start_zero_config_pairing();
        let entity = cx.weak_entity();

        v_flex()
            .w_full()
            .gap_3()
            .child(self.render_permission_selector(true, cx))
            .child(
                Button::new("start-zero-config-pairing")
                    .primary()
                    .w_full()
                    .icon(IconName::Network)
                    .label(locale::text(
                        "Start local pairing",
                        "开始局域网配对",
                        "開始區域網路配對",
                    ))
                    .loading(matches!(
                        self.state.pending,
                        Some(RemoteAccessMutation::StartZeroConfigPairing)
                    ))
                    .disabled(!can_start)
                    .on_click(move |_, _, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            this.dispatch_action(RemoteAccessAction::StartZeroConfigPairing, cx)
                        });
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(locale::text(
                        "The local entry is independent and does not need another remote route.",
                        "局域网入口独立工作，不需要先启用其他远程连接。",
                        "區域網路入口可獨立運作，不需要先啟用其他遠端連線。",
                    )),
            )
            .into_any_element()
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
                RemoteAccessAction::SelectMethod(method)
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
                RemoteAccessAction::SelectMethod(method)
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pending = self.state.pending.is_some();
        let selected_entry = self.state.selected_entry;
        let disable_entity = cx.weak_entity();
        let error = self.state.error_code.as_deref().map(remote_error_label);
        let notice = self.state.notice;
        let _safe_snapshot = self.state.safe_snapshot();

        v_flex()
            .id("remote-access-pairing")
            .size_full()
            .min_h_0()
            .gap_4()
            .overflow_y_scroll()
            .pr_1()
            .pb_1()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(Icon::new(IconName::Network).size(px(18.0)))
                            .child(div().text_sm().font_semibold().child(locale::text(
                                "Connect a mobile device",
                                "连接移动设备",
                                "連接行動裝置",
                            ))),
                    )
                    .child(
                        Button::new("disable-all-remote-access")
                            .small()
                            .outline()
                            .icon(IconName::Pause)
                            .label(locale::text("Disable all", "全部停用", "全部停用"))
                            .disabled(
                                pending
                                    || self
                                        .state
                                        .connectivity
                                        .as_ref()
                                        .is_none_or(|snapshot| !snapshot.desired_enabled),
                            )
                            .on_click(move |_, window, cx| {
                                let _ = disable_entity.update(cx, |this, cx| {
                                    this.present_disable_all_confirmation(window, cx)
                                });
                            }),
                    ),
            )
            .when_some(error, |column, error| {
                column.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .rounded(px(6.0))
                        .bg(cx.theme().danger.opacity(0.08))
                        .px_3()
                        .py_2()
                        .child(
                            Icon::new(IconName::TriangleAlert)
                                .size(px(14.0))
                                .text_color(cx.theme().danger),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child(error),
                        ),
                )
            })
            .when_some(notice, |column, notice| {
                column.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .rounded(px(6.0))
                        .bg(cx.theme().success.opacity(0.08))
                        .px_3()
                        .py_2()
                        .child(
                            Icon::new(IconName::CircleCheck)
                                .size(px(14.0))
                                .text_color(cx.theme().success),
                        )
                        .child(div().text_xs().text_color(cx.theme().success).child(notice)),
                )
            })
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .child(step_heading(
                        "1",
                        locale::text("Connection entry", "连接入口", "連線入口"),
                        locale::text(
                            "Choose where the mobile device will connect",
                            "选择移动设备连接到此电脑的入口",
                            "選擇行動裝置連線到此電腦的入口",
                        ),
                        cx,
                    ))
                    .child(self.render_connection_entry_selector(cx))
                    .child(self.render_connection_entry_panel(cx)),
            )
            .child(div().w_full().border_t_1().border_color(cx.theme().border))
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .child(step_heading(
                        "2",
                        locale::text("Pair the mobile device", "配对移动设备", "配對行動裝置"),
                        connection_entry_pairing_description(selected_entry),
                        cx,
                    ))
                    .child(self.render_pairing_section(cx)),
            )
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
    let dialog_height = (f32::from(viewport.height) - 32.0).clamp(360.0, DIALOG_MAX_HEIGHT);
    window.open_dialog(cx, move |dialog, _, _| {
        let close_view = close_view.clone();
        dialog
            .title(locale::text(
                "Connect a mobile device",
                "连接移动设备",
                "連接行動裝置",
            ))
            .w(px(dialog_width))
            .max_w(px(dialog_width))
            .h(px(dialog_height))
            .overlay_closable(true)
            .keyboard(true)
            .child(view.clone())
            .on_close(move |_, _, cx| {
                close_view.update(cx, |view, cx| view.dismiss(cx));
            })
    });
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

fn connection_entry_index(entry: RemoteAccessEntry) -> usize {
    RemoteAccessEntry::ALL
        .iter()
        .position(|candidate| *candidate == entry)
        .unwrap_or(0)
}

fn connection_entry_label(entry: RemoteAccessEntry) -> &'static str {
    match entry {
        RemoteAccessEntry::TailscaleServe => "Tailscale",
        RemoteAccessEntry::Direct => "Direct",
        RemoteAccessEntry::SelfHostedRelay => "Relay",
        RemoteAccessEntry::LocalNetwork => locale::text("Local network", "局域网", "區域網路"),
    }
}

fn connection_entry_description(entry: RemoteAccessEntry) -> &'static str {
    match entry {
        RemoteAccessEntry::TailscaleServe => locale::text(
            "Private access through your Tailnet",
            "通过 Tailnet 私密连接",
            "透過 Tailnet 私密連線",
        ),
        RemoteAccessEntry::Direct => locale::text(
            "Use a validated HTTPS address",
            "使用已验证的 HTTPS 地址",
            "使用已驗證的 HTTPS 位址",
        ),
        RemoteAccessEntry::SelfHostedRelay => locale::text(
            "Route through your encrypted relay",
            "通过自建加密 Relay 转发",
            "透過自建加密 Relay 轉送",
        ),
        RemoteAccessEntry::LocalNetwork => locale::text(
            "Discover this computer on the same network",
            "在同一网络中发现此电脑",
            "在同一網路中探索此電腦",
        ),
    }
}

fn connection_entry_card_copy(
    label: &'static str,
    description: &'static str,
    cx: &App,
) -> gpui::Div {
    v_flex()
        .min_w_0()
        .flex_1()
        .gap_1()
        .child(div().text_sm().font_semibold().child(label))
        .child(
            div()
                .min_w_0()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(description),
        )
}

fn connection_entry_pairing_description(entry: RemoteAccessEntry) -> &'static str {
    match entry {
        RemoteAccessEntry::LocalNetwork => locale::text(
            "Set the permission, then open a temporary discovery window",
            "设置设备权限，然后开启临时发现窗口",
            "設定裝置權限，然後開啟暫時探索視窗",
        ),
        RemoteAccessEntry::TailscaleServe
        | RemoteAccessEntry::Direct
        | RemoteAccessEntry::SelfHostedRelay => locale::text(
            "Set the permission, then generate a one-time QR code",
            "设置设备权限，然后生成一次性二维码",
            "設定裝置權限，然後產生一次性 QR Code",
        ),
    }
}

fn step_heading(
    step: &'static str,
    title: &'static str,
    description: &'static str,
    cx: &App,
) -> AnyElement {
    h_flex()
        .w_full()
        .min_w_0()
        .items_start()
        .gap_3()
        .child(
            div()
                .size(px(24.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .bg(cx.theme().primary.opacity(0.10))
                .text_xs()
                .font_semibold()
                .text_color(cx.theme().primary)
                .child(step),
        )
        .child(
            v_flex()
                .min_w_0()
                .gap_1()
                .child(div().text_sm().font_semibold().child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(description),
                ),
        )
        .into_any_element()
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

fn method_label(method: RemoteConnectivityMethod) -> &'static str {
    match method {
        RemoteConnectivityMethod::TailscaleServe => locale::text(
            "Tailscale Serve (recommended)",
            "Tailscale Serve（推荐）",
            "Tailscale Serve（建議）",
        ),
        RemoteConnectivityMethod::Direct => {
            locale::text("Direct HTTPS", "自管 Direct HTTPS", "自管 Direct HTTPS")
        }
        RemoteConnectivityMethod::SelfHostedRelay => {
            locale::text("Self-hosted Relay", "自建 Relay", "自建 Relay")
        }
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
    use std::{cell::Cell, rc::Rc};

    use super::*;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use gpui::TestAppContext;
    use gpui_component::ElementExt as _;
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

    struct ConnectionEntryCardCopyLayoutProbe {
        measured_width: Rc<Cell<f32>>,
    }

    impl Render for ConnectionEntryCardCopyLayoutProbe {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let measured_width = self.measured_width.clone();
            h_flex()
                .w(px(340.0))
                .gap_2()
                .child(div().size(px(28.0)).flex_none())
                .child(
                    connection_entry_card_copy(
                        "Tailscale",
                        "Private access through your Tailnet",
                        cx,
                    )
                    .on_prepaint(move |bounds, _, _| {
                        measured_width.set(f32::from(bounds.size.width));
                    }),
                )
        }
    }

    #[gpui::test]
    fn connection_entry_card_copy_fills_space_beside_the_icon(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let measured_width = Rc::new(Cell::new(0.0));
        let observed_width = measured_width.clone();
        let (_, cx) =
            cx.add_window_view(|_, _| ConnectionEntryCardCopyLayoutProbe { measured_width });

        for _ in 0..3 {
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            cx.run_until_parked();
        }

        assert!(
            observed_width.get() >= 300.0,
            "card copy width: {}",
            observed_width.get()
        );
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
    fn pairing_dialog_dimensions_fit_narrow_and_wide_windows() {
        let narrow = (360.0_f32 - 32.0).clamp(280.0, DIALOG_MAX_WIDTH);
        let wide = (1_440.0_f32 - 32.0).clamp(280.0, DIALOG_MAX_WIDTH);
        assert_eq!(narrow, 328.0);
        assert_eq!(wide, DIALOG_MAX_WIDTH);
    }

    #[test]
    fn connection_entries_keep_local_network_as_an_independent_fourth_entry() {
        assert_eq!(RemoteAccessEntry::ALL.len(), 4);
        assert_eq!(connection_entry_index(RemoteAccessEntry::TailscaleServe), 0);
        assert_eq!(connection_entry_index(RemoteAccessEntry::Direct), 1);
        assert_eq!(
            connection_entry_index(RemoteAccessEntry::SelfHostedRelay),
            2
        );
        assert_eq!(connection_entry_index(RemoteAccessEntry::LocalNetwork), 3);
        assert!(RemoteAccessEntry::LocalNetwork.remote_method().is_none());
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
}

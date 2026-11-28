//! The pairing service behind the pairing page.
//!
//! The page binds signals; this module owns them. It reads and writes the
//! phone's credential store, claims entries through `vibex-mobile-pairing` on
//! a background task, and reports reachability for saved runtimes.
//!
//! What still belongs to the platform host: the camera (scanning a QR code)
//! and local-network discovery. Both are seams here that say so rather than
//! pretending to work; the host integration fills them in with the scanner
//! activity and the discovery bridge.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    path::PathBuf,
    rc::Rc,
    time::Duration,
};

use futures_util::StreamExt as _;
use nsh_core::picker::PickRequest;
use nsh_core::prelude::*;
use vibex_backend::{BackendError, BackendResult};
use vibex_core::{RemoteLanPairingRequestState, RemoteServerKind, unix_timestamp_ms};
use vibex_mobile_pairing::{
    MobileCredentialBundle, MobilePairedRuntime, MobileRouteSelection, PairingStore, claim_entry,
    claim_zero_config_lan_pairing, classify_entry, entry::is_pairing_link,
};
use vibex_mobile_ui::{
    copy,
    screens::pairing::{
        HostDetails, NearbyPhase, NearbyRuntime, PairingHost, RouteOptionKind, RuntimeKind,
        RuntimeRouteOption, RuntimeStatus, SavedRuntime,
    },
};
use vibex_remote_client::{ClientDeviceIdentity, ZeroConfigLanPairingSession};

/// The name this phone pairs under until the host reports the platform's own.
const DEVICE_NAME: &str = "Vibex Mobile";

/// The file the credentials live in, inside the app's data directory.
const STORE_FILE: &str = "pairing.json";
/// The phone's own notes about saved runtimes (added and last connected).
const META_FILE: &str = "hosts.json";
/// The runtime the phone was last connected to, so a cold start can go
/// straight back to it instead of asking which one to open.
const LAST_HOST_FILE: &str = "last-runtime.txt";

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct HostMeta {
    #[serde(default)]
    added_at_ms: i64,
    #[serde(default)]
    last_connected_at_ms: i64,
    #[serde(default)]
    device_name: Option<String>,
    #[serde(default)]
    server_kind: RemoteServerKind,
    /// Phone-local custom name set via the Rename dialog (wins over server name).
    #[serde(default)]
    custom_name: Option<String>,
    /// Machine/published name reported by the runtime (`server_display_name` or mDNS name).
    #[serde(default)]
    server_display_name: Option<String>,
    /// User's active connection method choice for this runtime (defaults to `Auto`).
    #[serde(default)]
    preferred_route: MobileRouteSelection,
}

/// The runtime the page is connected to, once a claim or a probe succeeds.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectedRuntime {
    /// The label the shell shows for it.
    pub label: String,
    /// The credential the session shell's backend is built from.
    pub bundle: MobileCredentialBundle,
    /// Which route(s) the session service should connect over.
    pub route_selection: MobileRouteSelection,
}

/// The pairing page's state and actions.
#[derive(Clone)]
pub struct PairingService {
    store: Rc<PairingStore>,
    /// The app's data directory, for the identity file the old client wrote.
    data_dir: PathBuf,
    /// The install-wide identity presented when re-pairing.
    identity: Rc<RefCell<Option<ClientDeviceIdentity>>>,
    bundles: Rc<RefCell<Vec<MobileCredentialBundle>>>,
    saved: Signal<Vec<SavedRuntime>>,
    nearby: Signal<Vec<NearbyRuntime>>,
    details: Signal<Option<HostDetails>>,
    rename_draft: Signal<String>,
    meta: Rc<RefCell<std::collections::BTreeMap<String, HostMeta>>>,
    meta_path: PathBuf,
    last_host_path: PathBuf,
    switcher: Signal<bool>,
    connected_label: Signal<Option<SharedString>>,
    busy: Signal<bool>,
    error: Signal<Option<SharedString>>,
    connected: Signal<Option<ConnectedRuntime>>,
    /// The browse in flight, when "Find" is open.
    discovery: Rc<RefCell<Option<crate::discovery::Discovery>>>,
    /// What the browse found, by key (the service instance's full name).
    candidates: Rc<RefCell<BTreeMap<String, crate::discovery::Candidate>>>,
    /// What the nearby sheet shows.
    nearby_phase: Signal<NearbyPhase>,
    /// Bumped to abandon the session's poll loop.
    nearby_generation: Rc<Cell<u64>>,
    /// Bumped to abandon a discovery timeout.
    discovery_generation: Rc<Cell<u64>>,
    /// The runtime the phone is connected to, for the selected row.
    active_id: Signal<Option<SharedString>>,
    /// The runtime whose actions sheet is open.
    actions_id: Signal<Option<SharedString>>,
    /// The runtime whose rename or remove dialog is open.
    dialog_id: Signal<Option<SharedString>>,
    rename_open: Signal<bool>,
    remove_open: Signal<bool>,
}

impl PairingService {
    /// Loads the saved runtimes and starts their reachability probes.
    ///
    /// The state is created without an owner: the service outlives every page
    /// that shows it, and the pairing work started here runs on its own (see
    /// [`spawn_detached`]). A signal owned by the page that pressed "Pair" is
    /// disposed the moment that page is replaced, and writing it from a task
    /// that is still finishing the claim is what turned a successful pairing
    /// into a black screen.
    pub fn new(data_dir: PathBuf) -> Self {
        Owner::detached().run(|| Self::with_owner(data_dir))
    }

    fn with_owner(data_dir: PathBuf) -> Self {
        let store = Rc::new(PairingStore::new(data_dir.join(STORE_FILE)));
        let mut meta: BTreeMap<String, HostMeta> =
            std::fs::read_to_string(data_dir.join(META_FILE))
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
        let mut error = None;
        let mut bundles = match store.load() {
            Ok(bundles) => bundles,
            Err(load_error) => {
                error = Some(error_text(&load_error));
                Vec::new()
            }
        };
        if bundles.is_empty()
            && error.is_none()
            && let Some(imported) = crate::migration::import_hosts(&data_dir)
        {
            for host in &imported.hosts {
                let mut bundle = host.bundle.clone();
                let custom_name = host
                    .name_override
                    .as_deref()
                    .and_then(vibex_core::normalize_remote_display_name);
                if let Some(name) = custom_name.as_ref() {
                    bundle.display_name = Some(name.clone());
                }
                let id = bundle.host_id().to_owned();
                meta.insert(
                    id,
                    HostMeta {
                        added_at_ms: host.added_at_ms,
                        last_connected_at_ms: host.last_connected_at_ms.unwrap_or(0),
                        device_name: None,
                        server_kind: host.server_kind,
                        custom_name,
                        server_display_name: None,
                        preferred_route: MobileRouteSelection::Auto,
                    },
                );
                bundles.push(bundle);
            }
            if let Some(last) = imported.last_host.as_deref() {
                let _ = std::fs::write(data_dir.join(LAST_HOST_FILE), last);
            }
            if let Err(save_error) = store.save(&bundles) {
                error = Some(error_text(&save_error));
            }
            if let Ok(text) = serde_json::to_string(&meta) {
                let _ = std::fs::write(data_dir.join(META_FILE), text);
            }
        }
        // A build before the kind was persisted may have imported the hosts
        // without it; the old file still names it, so fill the gap rather than
        // showing every runtime as the generic kind.
        if let Some(imported) = crate::migration::import_hosts(&data_dir) {
            for host in imported.hosts {
                let entry = meta.entry(host.bundle.host_id().to_owned()).or_default();
                if entry.server_kind == RemoteServerKind::Unknown {
                    entry.server_kind = host.server_kind;
                }
                if entry.added_at_ms == 0 {
                    entry.added_at_ms = host.added_at_ms;
                }
            }
        }
        let identity = Rc::new(RefCell::new(crate::migration::import_identity(
            &data_dir, &bundles,
        )));
        let saved = signal(
            bundles
                .iter()
                .map(|bundle| {
                    let id = bundle.host_id().to_owned();
                    let host_meta = meta.get(&id);
                    let kind = host_meta
                        .map(|meta| runtime_kind(meta.server_kind))
                        .unwrap_or(RuntimeKind::Unknown);
                    let selected = host_meta
                        .map(|meta| &meta.preferred_route)
                        .unwrap_or(&MobileRouteSelection::Auto);
                    SavedRuntime {
                        id,
                        name: effective_host_name(bundle, host_meta),
                        status: RuntimeStatus::Unreachable,
                        kind,
                        address: bundle.record.server_url.clone(),
                        server_id: bundle.expected_server_id.clone(),
                        routes: route_options(bundle, selected),
                    }
                })
                .collect::<Vec<_>>(),
        );
        let service = Self {
            store,
            data_dir: data_dir.clone(),
            identity,
            bundles: Rc::new(RefCell::new(bundles)),
            saved,
            nearby: signal(Vec::new()),
            details: signal(None),
            rename_draft: signal(String::new()),
            meta: Rc::new(RefCell::new(meta)),
            meta_path: data_dir.join(META_FILE),
            last_host_path: data_dir.join(LAST_HOST_FILE),
            switcher: signal(false),
            connected_label: signal(None),
            busy: signal(false),
            error: signal(error),
            connected: signal(None),
            discovery: Rc::new(RefCell::new(None)),
            candidates: Rc::new(RefCell::new(BTreeMap::new())),
            nearby_phase: signal(NearbyPhase::Searching),
            nearby_generation: Rc::new(Cell::new(0)),
            discovery_generation: Rc::new(Cell::new(0)),
            active_id: signal(None),
            actions_id: signal(None),
            dialog_id: signal(None),
            rename_open: signal(false),
            remove_open: signal(false),
        };
        service.restore_last();
        service.probe_saved();
        service.watch_reachability();
        service
    }

    /// The page's bindings and callbacks.
    pub fn host(&self) -> PairingHost {
        let connect = self.clone();
        let paste = self.clone();
        let find = self.clone();
        let pick_saved = self.clone();
        let pick_nearby = self.clone();
        let scan = self.clone();
        PairingHost {
            saved: self.saved,
            nearby: self.nearby,
            nearby_phase: self.nearby_phase,
            busy: self.busy,
            error: self.error,
            on_scan: Rc::new(move || {
                if crate::host::scanner_available() {
                    // A custom host owns the camera; it polls the request
                    // queue and answers through the scan event.
                    crate::host::request_scan();
                    scan.error.set(None);
                } else {
                    scan.scan_photo();
                }
            }),
            on_connect: Rc::new(move |entry, server| connect.connect_entry(entry, server)),
            on_paste: Rc::new(move |draft| paste.paste(draft)),
            on_find: Rc::new(move || find.start_nearby()),
            on_retry_nearby: {
                let service = self.clone();
                Rc::new(move || service.start_nearby())
            },
            on_pick_saved: Rc::new(move |id| pick_saved.probe_runtime(id)),
            on_pick_route: {
                let service = self.clone();
                Rc::new(move |id: &str, route_key: &str| service.pick_route(id, route_key))
            },
            actions_id: self.actions_id,
            dialog_id: self.dialog_id,
            rename_open: self.rename_open,
            remove_open: self.remove_open,
            on_show_actions: {
                let service = self.clone();
                Rc::new(move |id: &str| service.show_actions(id))
            },
            on_close_actions: {
                let service = self.clone();
                Rc::new(move || service.close_actions())
            },
            on_open_rename: {
                let service = self.clone();
                Rc::new(move |id: &str| service.open_rename(id))
            },
            on_close_rename: {
                let service = self.clone();
                Rc::new(move || service.close_rename())
            },
            on_open_remove: {
                let service = self.clone();
                Rc::new(move |id: &str| service.open_remove(id))
            },
            on_close_remove: {
                let service = self.clone();
                Rc::new(move || service.close_remove())
            },
            details: self.details,
            on_show_details: {
                let service = self.clone();
                Rc::new(move |id: &str| service.show_details(id))
            },
            on_remove_saved: {
                let service = self.clone();
                Rc::new(move |id: &str| service.remove_host(id))
            },
            on_rename_saved: {
                let service = self.clone();
                Rc::new(move |id: &str, name: &str| service.rename_host(id, name))
            },
            rename_draft: self.rename_draft,
            switcher: self.switcher,
            on_close_switcher: {
                let service = self.clone();
                Rc::new(move || service.close_switcher())
            },
            connected_label: self.connected_label,
            active_id: self.active_id,
            on_disconnect: {
                let service = self.clone();
                Rc::new(move || service.disconnect())
            },
            on_clear_runtimes: {
                let service = self.clone();
                Rc::new(move || service.clear_runtimes())
            },
            on_close_details: {
                let service = self.clone();
                Rc::new(move || service.details.set(None))
            },
            on_pick_nearby: Rc::new(move |key| pick_nearby.start_nearby_pair(key)),
            on_open_settings: Rc::new(|| {}),
            on_open_platform_settings: Rc::new(crate::host::request_app_settings),
            on_stop_nearby: {
                let service = self.clone();
                Rc::new(move || service.stop_nearby())
            },
        }
    }

    /// Starts browsing the local network and lists what it finds.
    fn start_nearby(&self) {
        self.stop_browsing();
        self.nearby.set(Vec::new());
        self.candidates.borrow_mut().clear();
        self.error.set(None);
        self.busy.set(false);
        self.nearby_phase.set(NearbyPhase::Searching);
        // A browse that finds nothing says so after the moment a person would
        // have spent waiting, rather than looking busy forever.
        let generation = self.discovery_generation.get().wrapping_add(1);
        self.discovery_generation.set(generation);
        {
            let service = self.clone();
            spawn_detached(async move {
                nsh_core::element::timer(Duration::from_secs(3)).await;
                if service.discovery_generation.get() != generation {
                    return;
                }
                if matches!(service.nearby_phase.get_untracked(), NearbyPhase::Searching)
                    && service.nearby.get_untracked().is_empty()
                {
                    service.nearby_phase.set(NearbyPhase::Empty);
                }
            });
        }
        if crate::host::discovery_available() {
            // The custom host browses with Android's NSD; it answers through
            // the discovery events instead of the Rust mDNS daemon.
            crate::host::request_discovery(true);
            return;
        }
        let (sender, mut events) = futures_channel::mpsc::unbounded();
        match crate::discovery::start(sender) {
            Some(discovery) => *self.discovery.borrow_mut() = Some(discovery),
            None => {
                self.stop_browsing();
                self.nearby_phase.set(NearbyPhase::Failed(
                    copy::text(
                        "Local discovery could not start on this device.",
                        "无法在此设备上开始局域网发现。",
                        "無法在此裝置上開始區域網路探索。",
                    )
                    .into(),
                ));
                return;
            }
        }
        let service = self.clone();
        spawn(async move {
            while let Some(event) = events.next().await {
                if service.discovery_generation.get() != generation {
                    break;
                }
                match event {
                    crate::discovery::Event::Found(candidate) => {
                        service
                            .candidates
                            .borrow_mut()
                            .insert(candidate.key.clone(), candidate.clone());
                        let row = NearbyRuntime {
                            key: candidate.key,
                            name: candidate.name,
                        };
                        service.nearby.update(|rows| {
                            if let Some(existing) =
                                rows.iter_mut().find(|existing| existing.key == row.key)
                            {
                                existing.name = row.name;
                            } else {
                                rows.push(row);
                            }
                        });
                        if matches!(service.nearby_phase.get_untracked(), NearbyPhase::Empty) {
                            service.nearby_phase.set(NearbyPhase::Searching);
                        }
                    }
                    crate::discovery::Event::Lost(key) => {
                        service.candidates.borrow_mut().remove(&key);
                        service
                            .nearby
                            .update(|rows| rows.retain(|row| row.key != key));
                        if service.nearby.get_untracked().is_empty() {
                            service.nearby_phase.set(NearbyPhase::Empty);
                        }
                    }
                    crate::discovery::Event::Failed => {
                        service.stop_browsing();
                        service.nearby_phase.set(NearbyPhase::Failed(
                            copy::text(
                                "Local discovery stopped: the network would not advertise nearby runtimes.",
                                "局域网发现已停止：网络没有播报附近的运行时。",
                                "區域網路探索已停止：網路沒有廣播附近的執行環境。",
                            )
                            .into(),
                        ));
                        break;
                    }
                }
            }
        });
    }

    /// Stops the browse itself, leaving the phase (and any pairing session)
    /// to the caller.
    fn stop_browsing(&self) {
        if crate::host::discovery_available() {
            crate::host::request_discovery(false);
        }
        if let Some(discovery) = self.discovery.borrow_mut().take() {
            discovery.stop();
        }
        self.discovery_generation
            .set(self.discovery_generation.get().wrapping_add(1));
    }

    /// Stops browsing and abandons any nearby session.
    fn stop_nearby(&self) {
        self.stop_browsing();
        self.nearby_generation
            .set(self.nearby_generation.get().wrapping_add(1));
        self.nearby_phase.set(NearbyPhase::Searching);
        self.busy.set(false);
    }

    /// Starts a zero-config session with a discovered runtime and waits for
    /// the desktop to approve the code it shows.
    fn start_nearby_pair(&self, key: &str) {
        let phase = self.nearby_phase.get_untracked();
        log::debug!(
            "nearby pair requested: key={key} phase={phase:?} busy={} candidates={}",
            self.busy.get_untracked(),
            self.candidates.borrow().len()
        );
        if matches!(
            phase,
            NearbyPhase::Validating(_) | NearbyPhase::Waiting { .. }
        ) || self.busy.get_untracked()
        {
            return;
        }
        let Some(candidate) = self.candidates.borrow().get(key).cloned() else {
            log::warn!("nearby pair: no candidate for {key}");
            self.nearby_phase.set(NearbyPhase::Failed(
                copy::text(
                    "This computer is no longer reachable. Search again.",
                    "这台电脑现在无法连接，请重新查找。",
                    "這台電腦現在無法連線，請重新尋找。",
                )
                .into(),
            ));
            return;
        };
        log::debug!("nearby pair: origins={:?}", candidate.origins);
        self.stop_browsing();
        self.error.set(None);
        self.busy.set(true);
        self.nearby_phase
            .set(NearbyPhase::Validating(candidate.name.clone().into()));
        let generation = self.nearby_generation.get().wrapping_add(1);
        self.nearby_generation.set(generation);
        let service = self.clone();
        // Presenting the install's identity keeps a re-pair on the runtime's
        // existing device row, as the claim paths do.
        let previous_identity = self.identity.borrow().clone();
        let candidate_name = candidate.name.clone();
        let name: SharedString = candidate.name.clone().into();
        spawn_detached(async move {
            let device = crate::host::device_name().unwrap_or_else(|| DEVICE_NAME.to_owned());
            let opened = crate::runtime::handle()
                .spawn(
                    async move { start_lan_session(&candidate, &device, previous_identity).await },
                )
                .await;
            // The sheet was closed (or the pick was cancelled) while this was
            // in flight: leave the page alone and let the attempt expire.
            if service.nearby_generation.get() != generation {
                return;
            }
            let mut session = match opened {
                Ok(Ok(session)) => session,
                Ok(Err(message)) => {
                    service.busy.set(false);
                    service.nearby_phase.set(NearbyPhase::Failed(message));
                    return;
                }
                Err(error) => {
                    service.busy.set(false);
                    service.nearby_phase.set(NearbyPhase::Failed(
                        format!("the nearby pairing did not start: {error}").into(),
                    ));
                    return;
                }
            };
            service.busy.set(false);
            service.nearby_phase.set(NearbyPhase::Waiting {
                name,
                code: format_code(session.verification_code()),
                expires_at_ms: session.expires_at_ms(),
            });
            loop {
                nsh_core::element::timer(Duration::from_millis(700)).await;
                if service.nearby_generation.get() != generation {
                    return;
                }
                let polled = crate::runtime::handle()
                    .spawn(async move {
                        let result = session.poll().await;
                        (session, result)
                    })
                    .await;
                let (returned, result) = match polled {
                    Ok(pair) => pair,
                    Err(error) => {
                        service.nearby_phase.set(NearbyPhase::Failed(
                            format!("the nearby pairing did not answer: {error}").into(),
                        ));
                        return;
                    }
                };
                session = returned;
                let status = match result {
                    Ok(status) => status,
                    Err(error) => {
                        log::warn!("nearby pair: poll failed: {}", error.code);
                        service
                            .nearby_phase
                            .set(NearbyPhase::Failed(error_text(&error)));
                        return;
                    }
                };
                match status.state {
                    RemoteLanPairingRequestState::Pending => continue,
                    RemoteLanPairingRequestState::Approved => {
                        let claimed = crate::runtime::handle()
                            .spawn(async move {
                                claim_zero_config_lan_pairing(&mut session, status).await
                            })
                            .await;
                        match claimed {
                            Ok(mut claimed) => {
                                if let Ok(paired) = &mut claimed
                                    && paired.bundle.display_name.is_none()
                                {
                                    paired.bundle.display_name =
                                        vibex_core::normalize_remote_display_name(&candidate_name);
                                }
                                service.stop_nearby();
                                service.finish_claim(claimed)
                            }
                            Err(error) => service.nearby_phase.set(NearbyPhase::Failed(
                                format!("the nearby pairing did not finish: {error}").into(),
                            )),
                        }
                        return;
                    }
                    RemoteLanPairingRequestState::Rejected => {
                        service.nearby_phase.set(NearbyPhase::Rejected);
                        return;
                    }
                    RemoteLanPairingRequestState::Expired
                    | RemoteLanPairingRequestState::Claimed => {
                        service.nearby_phase.set(NearbyPhase::Expired);
                        return;
                    }
                    RemoteLanPairingRequestState::Unknown => {
                        service.nearby_phase.set(NearbyPhase::Failed(
                            copy::text(
                                "The desktop returned an unknown pairing state.",
                                "桌面端返回了未知的配对状态。",
                                "桌面版傳回了未知的配對狀態。",
                            )
                            .into(),
                        ));
                        return;
                    }
                }
            }
        });
    }

    /// Whether the pairing page is showing as the runtime switcher.
    pub fn switcher(&self) -> Signal<bool> {
        self.switcher
    }

    /// Opens the switcher over the live session.
    pub fn open_switcher(&self) {
        self.switcher.set(true);
    }

    /// Closes the switcher, back to the session it opened over.
    pub fn close_switcher(&self) {
        self.switcher.set(false);
    }

    /// The connected runtime's label, for the settings sheet.
    pub fn connected_label(&self) -> Signal<Option<SharedString>> {
        self.connected_label
    }

    /// The saved runtimes, for the settings sheet's runtime actions.
    pub fn saved(&self) -> Signal<Vec<SavedRuntime>> {
        self.saved
    }

    /// The runtime the page is connected to.
    pub fn connected(&self) -> Signal<Option<ConnectedRuntime>> {
        self.connected
    }

    /// Leaves the connected runtime; the credential stays saved.
    pub fn disconnect(&self) {
        self.switcher.set(false);
        self.connected.set(None);
        self.connected_label.set(None);
        self.active_id.set(None);
        self.error.set(None);
    }

    /// Records the kind of runtime a connected peer reported.
    pub fn set_host_kind(&self, host_id: &str, kind: RemoteServerKind) {
        if kind == RemoteServerKind::Unknown {
            return;
        }
        let mut changed = false;
        {
            let mut meta = self.meta.borrow_mut();
            let entry = meta.entry(host_id.to_owned()).or_default();
            if entry.server_kind != kind {
                entry.server_kind = kind;
                changed = true;
            }
        }
        if !changed {
            return;
        }
        self.save_meta();
        let kind = runtime_kind(kind);
        self.saved.update(|rows| {
            if let Some(row) = rows.iter_mut().find(|row| row.id == host_id) {
                row.kind = kind;
            }
        });
        if let Some(mut details) = self.details.get_untracked()
            && details.id == host_id
        {
            details.kind = kind;
            self.details.set(Some(details));
        }
    }

    /// Removes every saved credential; the settings page confirms first.
    pub fn clear_runtimes(&self) {
        let ids = self
            .bundles
            .borrow()
            .iter()
            .map(|bundle| bundle.host_id().to_owned())
            .collect::<Vec<_>>();
        let _ = std::fs::remove_file(&self.last_host_path);
        let mut remaining = self.bundles.borrow().clone();
        for id in ids {
            if let Ok(bundles) = self.store.remove(&id) {
                remaining = bundles;
            }
        }
        self.meta.borrow_mut().clear();
        self.save_meta();
        self.set_bundles(remaining);
        self.details.set(None);
        self.disconnect();
    }

    /// Claims `entry` (a link, or a code with `server`) on a background task.
    /// Claims a link the in-app scanner decoded.
    pub fn scan_link(&self, link: &str) {
        self.connect_entry(link, "");
    }

    /// Reads a QR code from a picked photo, for when the camera cannot start.
    fn scan_photo(&self) {
        self.error.set(None);
        let service = self.clone();
        spawn_detached(async move {
            let files = pick_images(PickRequest::new(false, 1)).await;
            let Some(file) = files.into_iter().next() else {
                return;
            };
            let bytes = file.data.bytes().to_vec();
            let decoded = crate::runtime::handle()
                .spawn(async move { decode_qr_photo(&bytes) })
                .await;
            match decoded {
                Ok(Some(link)) => service.scan_link(&link),
                Ok(None) => service.fail_text(copy::text(
                    "No QR code was found in that photo.",
                    "这张照片里没有识别到二维码。",
                    "這張照片裡沒有辨識到 QR Code。",
                )),
                Err(error) => {
                    service.fail_text(format!("reading the photo did not finish: {error}"))
                }
            }
        });
    }

    fn connect_entry(&self, entry: &str, server: &str) {
        if self.busy.get_untracked() {
            return;
        }
        // Parsed here as well so a missing code or address is answered at
        // once, without a task and without leaving the sheet waiting. The
        // wording names the missing piece, as the GPUI client's does.
        if entry.trim().is_empty() {
            self.fail_text(copy::text(
                "Paste the pairing code or link from the computer or the cloud runtime.",
                "请粘贴电脑或云端运行时给出的配对码或链接。",
                "請貼上電腦或雲端執行階段提供的配對碼或連結。",
            ));
            return;
        }
        if !is_pairing_link(entry) && server.trim().is_empty() {
            self.fail_text(copy::text(
                "Enter the server address as well as the pairing code.",
                "请同时填写服务器地址和配对码。",
                "請同時填寫伺服器位址和配對碼。",
            ));
            return;
        }
        if let Err(error) = classify_entry(entry, server) {
            self.fail(&error);
            return;
        }
        self.error.set(None);
        self.busy.set(true);
        let service = self.clone();
        let entry = entry.to_owned();
        let server = server.to_owned();
        spawn_detached(async move {
            let device = crate::host::device_name().unwrap_or_else(|| DEVICE_NAME.to_owned());
            // The claim is network I/O: run it on the runtime's threads, not
            // on the engine thread that renders. Presenting the install's
            // identity keeps a re-pair on the runtime's existing device row.
            let previous_identity = service.identity.borrow().clone();
            let result = crate::runtime::handle()
                .spawn(async move { claim_entry(entry, server, device, previous_identity).await })
                .await;
            let result = match result {
                Ok(result) => result,
                Err(error) => Err(BackendError::failed(
                    "pairing_task_failed",
                    format!("the pairing task did not finish: {error}"),
                )),
            };
            // A claim failure is the only signal an operator has: `fail` just
            // paints a caption, and the caption for an offline transport is the
            // same sentence no matter which hop broke. Log the code and message
            // so the reason survives the generic text.
            match &result {
                Ok(_) => log::info!("pairing claim succeeded"),
                Err(error) => {
                    log::warn!("pairing claim failed: {} ({})", error.code, error.message)
                }
            }
            service.finish_claim(result);
        });
    }

    /// Stores a claimed credential and shows it in the list.
    fn finish_claim(&self, result: BackendResult<MobilePairedRuntime>) {
        match result {
            Ok(paired) => match self.store.upsert(paired.bundle.clone()) {
                Ok(bundles) => {
                    let id = paired.bundle.host_id().to_owned();
                    let merged = bundles
                        .iter()
                        .find(|bundle| bundle.host_id() == id)
                        .cloned()
                        .unwrap_or_else(|| paired.bundle.clone());
                    {
                        let mut meta = self.meta.borrow_mut();
                        let entry = meta.entry(id).or_default();
                        if entry.added_at_ms == 0 {
                            entry.added_at_ms = unix_timestamp_ms();
                        }
                        if paired.server_kind != RemoteServerKind::Unknown {
                            entry.server_kind = paired.server_kind;
                        }
                        if let Some(published) = paired
                            .bundle
                            .display_name
                            .as_deref()
                            .and_then(vibex_core::normalize_remote_display_name)
                        {
                            entry.server_display_name = Some(published);
                        }
                        entry.preferred_route = MobileRouteSelection::Auto;
                    }
                    self.save_meta();
                    if let Ok(identity) = ClientDeviceIdentity::from_private_key_base64(
                        merged.record.auth.device_id.clone(),
                        &merged.identity_private_key,
                    ) {
                        *self.identity.borrow_mut() = Some(identity);
                        crate::migration::save_identity(&self.data_dir, &merged);
                    }
                    self.set_bundles(bundles);
                    self.busy.set(false);
                    self.connect(merged, MobileRouteSelection::Auto);
                }
                Err(error) => self.fail(&error),
            },
            Err(error) => self.fail(&error),
        }
    }

    /// Reads the clipboard into the entry field the sheet passed.
    fn paste(&self, draft: Signal<String>) {
        let service = self.clone();
        spawn_detached(async move {
            let text = clipboard_string().await;
            let text = text.trim();
            if text.is_empty() {
                service.error.set(Some(
                    copy::text(
                        "The clipboard has no text.",
                        "剪贴板里没有内容。",
                        "剪貼簿裡沒有內容。",
                    )
                    .into(),
                ));
            } else {
                draft.set(text.to_owned());
            }
        });
    }

    /// Shows one saved runtime's details.
    fn show_details(&self, host_id: &str) {
        self.actions_id.set(None);
        let Some(bundle) = self
            .bundles
            .borrow()
            .iter()
            .find(|bundle| bundle.host_id() == host_id)
            .cloned()
        else {
            return;
        };
        let meta = self.meta.borrow().get(host_id).cloned().unwrap_or_default();
        let display = effective_host_name(&bundle, Some(&meta));
        self.rename_draft.set(display.clone());
        self.details.set(Some(HostDetails {
            id: host_id.to_owned(),
            name: display,
            kind: runtime_kind(meta.server_kind),
            address: details_address(&bundle),
            server_id: bundle.expected_server_id.clone(),
            device_name: meta.device_name.clone(),
            last_connected: (meta.last_connected_at_ms > 0)
                .then(|| last_connected_label(meta.last_connected_at_ms)),
            added: (meta.added_at_ms > 0).then(|| added_label(meta.added_at_ms)),
        }));
    }

    /// Seeds the rename field with a saved runtime's current name.
    fn seed_rename(&self, host_id: &str) {
        if let Some(bundle) = self
            .bundles
            .borrow()
            .iter()
            .find(|bundle| bundle.host_id() == host_id)
        {
            let meta = self.meta.borrow();
            self.rename_draft
                .set(effective_host_name(bundle, meta.get(host_id)));
        }
    }

    fn show_actions(&self, host_id: &str) {
        self.details.set(None);
        self.actions_id.set(Some(host_id.to_owned().into()));
    }

    fn close_actions(&self) {
        self.actions_id.set(None);
    }

    fn open_rename(&self, host_id: &str) {
        self.seed_rename(host_id);
        self.actions_id.set(None);
        self.dialog_id.set(Some(host_id.to_owned().into()));
        self.rename_open.set(true);
    }

    fn close_rename(&self) {
        self.rename_open.set(false);
        self.dialog_id.set(None);
    }

    fn open_remove(&self, host_id: &str) {
        self.actions_id.set(None);
        self.dialog_id.set(Some(host_id.to_owned().into()));
        self.remove_open.set(true);
    }

    fn close_remove(&self) {
        self.remove_open.set(false);
        self.dialog_id.set(None);
    }

    /// Whether one of the runtime surfaces is over the page.
    pub fn overlay_open(&self) -> bool {
        self.remove_open.get_untracked()
            || self.rename_open.get_untracked()
            || self.details.get_untracked().is_some()
            || self.actions_id.get_untracked().is_some()
    }

    /// Closes the topmost runtime surface, one step per press.
    pub fn close_overlay(&self) {
        if self.remove_open.get_untracked() {
            self.close_remove();
        } else if self.rename_open.get_untracked() {
            self.close_rename();
        } else if self.details.get_untracked().is_some() {
            self.details.set(None);
        } else {
            self.close_actions();
        }
    }

    /// Renames a saved runtime locally on this phone.
    ///
    /// Clearing the name removes the local override and falls back to the
    /// machine/published name reported by the runtime (never reverting to a
    /// bare IP address when a runtime name is already known).
    fn rename_host(&self, host_id: &str, name: &str) {
        let custom = vibex_core::normalize_remote_display_name(name);
        let fallback_name = {
            let mut meta = self.meta.borrow_mut();
            let entry = meta.entry(host_id.to_owned()).or_default();
            entry.custom_name = custom.clone();
            custom.clone().or_else(|| entry.server_display_name.clone())
        };
        self.save_meta();
        let updated = self
            .bundles
            .borrow()
            .iter()
            .find(|bundle| bundle.host_id() == host_id)
            .cloned()
            .map(|mut bundle| {
                bundle.display_name = fallback_name;
                bundle
            });
        let Some(bundle) = updated else {
            return;
        };
        match self.store.update_in_place(bundle) {
            Ok(bundles) => {
                let saved_name = {
                    let meta = self.meta.borrow();
                    bundles
                        .iter()
                        .find(|bundle| bundle.host_id() == host_id)
                        .map(|bundle| effective_host_name(bundle, meta.get(host_id)))
                        .unwrap_or_default()
                };
                self.set_bundles(bundles);
                if self
                    .active_id
                    .get_untracked()
                    .as_deref()
                    .is_some_and(|active| active == host_id)
                {
                    self.connected_label.set(Some(saved_name.clone().into()));
                }
                self.rename_draft.set(saved_name.clone());
                if let Some(mut details) = self.details.get_untracked()
                    && details.id == host_id
                {
                    details.name = saved_name;
                    self.details.set(Some(details));
                }
            }
            Err(error) => self.fail(&error),
        }
    }

    /// Removes a saved runtime; the page asks for confirmation first.
    fn remove_host(&self, host_id: &str) {
        let result = self.store.remove(host_id);
        if self
            .last_host_id()
            .as_deref()
            .is_some_and(|last| last == host_id)
        {
            let _ = std::fs::remove_file(&self.last_host_path);
        }
        match result {
            Ok(bundles) => {
                self.meta.borrow_mut().remove(host_id);
                self.save_meta();
                let was_connected = self
                    .connected
                    .get_untracked()
                    .is_some_and(|connected| connected.bundle.host_id() == host_id);
                let empty = bundles.is_empty();
                self.set_bundles(bundles);
                self.details.set(None);
                self.dialog_id.set(None);
                self.actions_id.set(None);
                if was_connected || empty {
                    self.disconnect();
                }
            }
            Err(error) => self.fail(&error),
        }
    }

    /// Probes every saved runtime once, at startup.
    fn probe_saved(&self) {
        // Probe every saved runtime, including the one this phone is on. The
        // connected row used to be skipped, so stopping its desktop left the
        // row green until the phone forgot the runtime.
        let bundles = self.bundles.borrow().clone();
        for bundle in bundles {
            self.spawn_probe(bundle, None);
        }
    }

    /// Keeps the saved runtimes' reachability fresh while the page is visible.
    fn watch_reachability(&self) {
        let service = self.clone();
        spawn_detached(async move {
            loop {
                nsh_core::element::timer(Duration::from_secs(20)).await;
                if service.connected.get_untracked().is_none() || service.switcher.get_untracked() {
                    service.probe_saved();
                }
            }
        });
    }

    /// Connects to the saved runtime with `host_id`.
    ///
    /// The connection method the reader last chose for this runtime is kept:
    /// tapping the row means "open it", not "forget the route I picked". A
    /// runtime that has no stored choice (or whose stored route disappeared
    /// when it was repaired) falls back to `Auto`, which tries the direct
    /// routes concurrently and reaches for the relay only if none answers.
    fn probe_runtime(&self, host_id: &str) {
        let bundle = self
            .bundles
            .borrow()
            .iter()
            .find(|bundle| bundle.host_id() == host_id)
            .cloned();
        let Some(bundle) = bundle else {
            return;
        };
        let preferred = self
            .meta
            .borrow()
            .get(host_id)
            .map(|meta| meta.preferred_route.clone())
            .filter(|selection| bundle.has_route(selection))
            .unwrap_or(MobileRouteSelection::Auto);
        self.set_preferred_route(host_id, &bundle, preferred.clone());
        self.set_status(host_id, RuntimeStatus::Probing);
        self.error.set(None);
        self.spawn_probe(bundle, Some(preferred));
    }

    /// Actively switches a saved runtime to a specific connection route chosen
    /// from its "More" actions sheet and reconnects over that route.
    pub fn pick_route(&self, host_id: &str, route_key: &str) {
        let bundle = self
            .bundles
            .borrow()
            .iter()
            .find(|bundle| bundle.host_id() == host_id)
            .cloned();
        let Some(bundle) = bundle else {
            return;
        };
        let requested = parse_route_key(route_key);
        let selection = if bundle.has_route(&requested) {
            requested
        } else {
            MobileRouteSelection::Auto
        };
        self.set_preferred_route(host_id, &bundle, selection.clone());
        self.set_status(host_id, RuntimeStatus::Probing);
        self.error.set(None);
        self.spawn_probe(bundle, Some(selection));
    }

    fn set_preferred_route(
        &self,
        host_id: &str,
        bundle: &MobileCredentialBundle,
        selection: MobileRouteSelection,
    ) {
        let changed = {
            let mut meta = self.meta.borrow_mut();
            let entry = meta.entry(host_id.to_owned()).or_default();
            let changed = entry.preferred_route != selection;
            if changed {
                entry.preferred_route = selection.clone();
            }
            changed
        };
        if changed {
            self.save_meta();
        }
        let updated_routes = route_options(bundle, &selection);
        self.saved.update(|rows| {
            if let Some(row) = rows.iter_mut().find(|row| row.id == host_id) {
                row.routes = updated_routes;
            }
        });
    }

    fn spawn_probe(
        &self,
        bundle: MobileCredentialBundle,
        connect_selection: Option<MobileRouteSelection>,
    ) {
        let service = self.clone();
        let id = bundle.host_id().to_owned();
        let probing = match &connect_selection {
            Some(selection) => bundle.for_route_selection(selection),
            None => bundle.clone(),
        };
        spawn_detached(async move {
            // A panicking probe is an unreachable runtime, not a crash of the
            // page: the row simply stays red.
            let probe = crate::runtime::handle()
                .spawn(async move { probing.probe_status().await })
                .await
                .ok();
            let online = probe.as_ref().is_some_and(|result| result.online);
            service.set_status(
                &id,
                if online {
                    RuntimeStatus::Reachable
                } else {
                    RuntimeStatus::Unreachable
                },
            );
            if let Some(probe) = probe {
                if probe.server_kind != RemoteServerKind::Unknown {
                    service.set_host_kind(&id, probe.server_kind);
                }
                if let Some(name) = probe.server_display_name.as_deref() {
                    service.note_runtime_name(&id, name);
                }
            }
            if let Some(selection) = connect_selection {
                let latest = service
                    .bundles
                    .borrow()
                    .iter()
                    .find(|entry| entry.host_id() == id)
                    .cloned()
                    .unwrap_or(bundle);
                // A saved runtime is entered even when the probe failed: the
                // session service shows the connecting page and its own retry,
                // which is where an unavailable runtime belongs. Refusing here
                // left the reader on the list with a line of red text.
                service.connect(latest, selection);
            }
        });
    }

    fn set_status(&self, host_id: &str, status: RuntimeStatus) {
        self.saved.update(|saved| {
            if let Some(runtime) = saved.iter_mut().find(|runtime| runtime.id == host_id) {
                runtime.status = status;
            }
        });
    }

    fn set_bundles(&self, bundles: Vec<MobileCredentialBundle>) {
        let previous: Vec<(String, RuntimeStatus)> = self
            .saved
            .get_untracked()
            .into_iter()
            .map(|runtime| (runtime.id, runtime.status))
            .collect();
        let meta = self.meta.borrow();
        let rows = bundles
            .iter()
            .map(|bundle| {
                let id = bundle.host_id().to_owned();
                let status = previous
                    .iter()
                    .find(|(previous_id, _)| *previous_id == id)
                    .map_or(RuntimeStatus::Unreachable, |(_, status)| *status);
                let host_meta = meta.get(&id);
                let kind = host_meta
                    .map(|meta| runtime_kind(meta.server_kind))
                    .unwrap_or(RuntimeKind::Unknown);
                let selected = host_meta
                    .map(|meta| &meta.preferred_route)
                    .unwrap_or(&MobileRouteSelection::Auto);
                SavedRuntime {
                    id,
                    name: effective_host_name(bundle, host_meta),
                    status,
                    kind,
                    address: bundle.record.server_url.clone(),
                    server_id: bundle.expected_server_id.clone(),
                    routes: route_options(bundle, selected),
                }
            })
            .collect::<Vec<_>>();
        drop(meta);
        *self.bundles.borrow_mut() = bundles;
        self.saved.set(rows);
    }

    fn connect(&self, bundle: MobileCredentialBundle, route_selection: MobileRouteSelection) {
        {
            let id = bundle.host_id().to_owned();
            let mut meta = self.meta.borrow_mut();
            let entry = meta.entry(id).or_default();
            entry.last_connected_at_ms = unix_timestamp_ms();
            entry.device_name = crate::host::device_name();
            entry.preferred_route = route_selection.clone();
            drop(meta);
            self.save_meta();
        }
        self.show_connected(bundle, route_selection, true);
    }

    /// Shows `bundle` as the connected runtime without treating it as a new
    /// connection, so a cold start does not move the "last connected" clock.
    fn restore(&self, bundle: MobileCredentialBundle, route_selection: MobileRouteSelection) {
        self.show_connected(bundle, route_selection, false);
    }

    fn show_connected(
        &self,
        bundle: MobileCredentialBundle,
        route_selection: MobileRouteSelection,
        remember: bool,
    ) {
        if remember {
            let _ = std::fs::write(&self.last_host_path, bundle.host_id());
        }
        self.switcher.set(false);
        let id = bundle.host_id().to_owned();
        let label = {
            let meta = self.meta.borrow();
            effective_host_name(&bundle, meta.get(&id))
        };
        let routes = route_options(&bundle, &route_selection);
        self.saved.update(|saved| {
            if let Some(row) = saved.iter_mut().find(|row| row.id == id) {
                row.status = RuntimeStatus::Reachable;
                row.name = label.clone();
                row.routes = routes;
            }
        });
        self.active_id.set(Some(id.into()));
        self.connected_label.set(Some(label.clone().into()));
        self.connected.set(Some(ConnectedRuntime {
            label,
            bundle,
            route_selection,
        }));
    }

    /// Reopens the runtime this phone was on when it last ran.
    fn restore_last(&self) {
        if self.connected.get_untracked().is_some() {
            return;
        }
        let Ok(id) = std::fs::read_to_string(&self.last_host_path) else {
            return;
        };
        let id = id.trim();
        if id.is_empty() {
            return;
        }
        let bundle = self
            .bundles
            .borrow()
            .iter()
            .find(|bundle| bundle.host_id() == id)
            .cloned();
        if let Some(bundle) = bundle {
            let selection = self
                .meta
                .borrow()
                .get(id)
                .map(|meta| meta.preferred_route.clone())
                .filter(|selection| bundle.has_route(selection))
                .unwrap_or(MobileRouteSelection::Auto);
            self.restore(bundle, selection);
        }
    }

    /// Records the runtime's published machine/display name and updates the
    /// saved row, details sheet, and active label without rebuilding the
    /// session shell (so the pager stays on its current pane).
    ///
    /// Returns the effective label after applying any phone-local custom name
    /// override.
    pub fn note_runtime_name(&self, host_id: &str, display_name: &str) -> Option<String> {
        let name = vibex_core::normalize_remote_display_name(display_name)?;
        let (effective, meta_changed) = {
            let mut meta = self.meta.borrow_mut();
            let entry = meta.entry(host_id.to_owned()).or_default();
            let changed = entry.server_display_name.as_deref() != Some(name.as_str());
            if changed {
                entry.server_display_name = Some(name.clone());
            }
            let effective = entry
                .custom_name
                .as_deref()
                .and_then(vibex_core::normalize_remote_display_name)
                .unwrap_or_else(|| name.clone());
            (effective, changed)
        };
        if meta_changed {
            self.save_meta();
        }
        let mut updated_bundle = None;
        {
            let mut bundles = self.bundles.borrow_mut();
            if let Some(bundle) = bundles
                .iter_mut()
                .find(|bundle| bundle.host_id() == host_id)
                && bundle.display_name.as_deref() != Some(effective.as_str())
            {
                bundle.display_name = Some(effective.clone());
                updated_bundle = Some(bundle.clone());
            }
        }
        if let Some(bundle) = updated_bundle {
            let _ = self.store.update_in_place(bundle);
        }
        self.saved.update(|rows| {
            if let Some(row) = rows.iter_mut().find(|row| row.id == host_id) {
                row.name = effective.clone();
            }
        });
        if self
            .active_id
            .get_untracked()
            .as_deref()
            .is_some_and(|active| active == host_id)
        {
            self.connected_label.set(Some(effective.clone().into()));
        }
        if let Some(mut details) = self.details.get_untracked()
            && details.id == host_id
        {
            details.name = effective.clone();
            self.details.set(Some(details));
        }
        Some(effective)
    }

    /// Applies the name the runtime gave this phone to the saved row.
    pub fn rename_device(&self, display_name: &str) {
        let Some(name) = vibex_core::normalize_remote_display_name(display_name) else {
            return;
        };
        let Some(connected) = self.connected.get_untracked() else {
            return;
        };
        let id = connected.bundle.host_id().to_owned();
        {
            let mut meta = self.meta.borrow_mut();
            meta.entry(id.clone()).or_default().device_name = Some(name.clone());
        }
        self.save_meta();
        if let Some(mut details) = self.details.get_untracked()
            && details.id == id
        {
            details.device_name = Some(name);
            self.details.set(Some(details));
        }
    }

    fn last_host_id(&self) -> Option<String> {
        std::fs::read_to_string(&self.last_host_path)
            .ok()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty())
    }

    fn save_meta(&self) {
        if let Ok(text) = serde_json::to_string(&*self.meta.borrow()) {
            let _ = std::fs::write(&self.meta_path, text);
        }
    }

    /// Applies something a platform host observed (a scan, discovery).
    pub fn apply_host_event(&self, event: crate::host::HostEvent) {
        use crate::host::HostEvent;
        match event {
            HostEvent::ScanResult(entry) => self.connect_entry(&entry, ""),
            HostEvent::DiscoveryCandidate {
                key,
                name,
                origins,
                server_id,
                server_key,
            } => {
                // Android's NSD can resolve a service without its instance
                // name, which would make every runtime share the empty key.
                // The first origin is stable for as long as the record is.
                let key = if key.trim().is_empty() {
                    origins.first().cloned().unwrap_or_default()
                } else {
                    key
                };
                if let (Some(server_id), Some(server_key)) = (server_id, server_key)
                    && !origins.is_empty()
                {
                    self.candidates.borrow_mut().insert(
                        key.clone(),
                        crate::discovery::Candidate {
                            key: key.clone(),
                            name: name.clone(),
                            origins,
                            server_id,
                            server_key,
                        },
                    );
                    // A resolve that carried no address cannot be paired with:
                    // listing it would only offer a row whose "Pair" does
                    // nothing. Android's NSD re-resolves the record, and that
                    // event brings the address.
                    self.nearby.update(|nearby| {
                        if let Some(candidate) = nearby.iter_mut().find(|nearby| nearby.key == key)
                        {
                            candidate.name = name;
                        } else {
                            nearby.push(NearbyRuntime { key, name });
                        }
                    });
                    if matches!(self.nearby_phase.get_untracked(), NearbyPhase::Empty) {
                        self.nearby_phase.set(NearbyPhase::Searching);
                    }
                } else {
                    log::warn!("discovery candidate without a usable address: key={key:?}");
                }
            }
            HostEvent::DiscoveryRemoved { key } => {
                self.candidates.borrow_mut().remove(&key);
                self.nearby
                    .update(|nearby| nearby.retain(|nearby| nearby.key != key));
                if self.nearby.get_untracked().is_empty() {
                    self.nearby_phase.set(NearbyPhase::Empty);
                }
            }
            HostEvent::DiscoveryPermissionDenied => {
                self.stop_browsing();
                self.busy.set(false);
                self.nearby_phase.set(NearbyPhase::PermissionDenied);
            }
            HostEvent::DiscoveryFailed(message) => {
                self.stop_browsing();
                self.busy.set(false);
                self.nearby_phase.set(NearbyPhase::Failed(message.into()));
            }
        }
    }

    fn fail(&self, error: &BackendError) {
        self.busy.set(false);
        self.error.set(Some(error_text(error)));
    }

    fn fail_text(&self, message: impl AsRef<str>) {
        self.busy.set(false);
        self.error.set(Some(message.as_ref().to_owned().into()));
    }
}

/// Spawns engine work that must outlive the element that started it.
///
/// nsh runs a spawned task under the owner of the element whose handler called
/// it, so a task started from a list row dies when the list is replaced — the
/// pairing sheet does exactly that on press. These tasks report through the
/// service's signals, and the service's generation counter is what abandons
/// them, so they are spawned without an owner.
fn spawn_detached(task: impl std::future::Future<Output = ()> + 'static) {
    Owner::detached().run(|| {
        spawn(task);
    });
}

/// How long one advertised address gets before the next one is tried.
const LAN_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(4);

/// How long the whole set of addresses gets before the page says it could not
/// reach this computer.
const LAN_TOTAL_TIMEOUT: Duration = Duration::from_secs(12);

/// Opens a zero-config session, trying every address the runtime advertised.
///
/// A runtime publishes each address of each interface it has — the LAN one, a
/// VPN, a container bridge — and only the LAN one answers from the phone.
/// Dialling a single arbitrary address is how "Find" sits on "Pairing..."
/// while the desktop never sees a request, so each is tried in turn, the one
/// on this phone's own subnet first.
async fn start_lan_session(
    candidate: &crate::discovery::Candidate,
    device: &str,
    identity: Option<ClientDeviceIdentity>,
) -> Result<ZeroConfigLanPairingSession, SharedString> {
    let mut last = None;
    // Addresses that black-hole would otherwise add up: the whole search is
    // bounded too, so a phone that cannot reach any of them says so promptly.
    let deadline = tokio::time::Instant::now() + LAN_TOTAL_TIMEOUT;
    for origin in candidate.origins.clone() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let attempted = origin.clone();
        let started = tokio::time::timeout(
            remaining.min(LAN_ATTEMPT_TIMEOUT),
            ZeroConfigLanPairingSession::start_with_identity(
                origin,
                &candidate.server_id,
                &candidate.server_key,
                device,
                identity.clone(),
            ),
        )
        .await;
        match started {
            Ok(Ok(session)) => {
                log::info!("nearby pair: reached {attempted}");
                return Ok(session);
            }
            Ok(Err(error)) => {
                log::warn!("nearby pair: {attempted} failed: {}", error.code);
                last = Some(error_text(&error));
            }
            Err(_) => {
                log::warn!("nearby pair: {attempted} timed out");
                last = Some(
                    copy::text(
                        "The computer did not answer on its local address.",
                        "这台电脑的局域网地址没有响应。",
                        "這台電腦的區域網路位址沒有回應。",
                    )
                    .into(),
                )
            }
        }
    }
    Err(last.unwrap_or_else(|| {
        copy::text(
            "This computer did not advertise an address this phone can reach.",
            "这台电脑没有播报手机能访问的地址。",
            "這台電腦沒有廣播手機能存取的位址。",
        )
        .into()
    }))
}

/// The page's wording for a pairing failure.
///
/// The transport's messages describe the protocol; the page says what the
/// person can do next. An unmapped code falls back to the message, bounded.
/// Reads the first QR code in an image, for the photo fallback.
fn decode_qr_photo(bytes: &[u8]) -> Option<String> {
    let picture = image::load_from_memory(bytes).ok()?.to_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(picture);
    for grid in prepared.detect_grids() {
        if let Ok((_, content)) = grid.decode() {
            return Some(content);
        }
    }
    None
}

/// Resolves the display label for a saved runtime:
/// phone-local custom name -> runtime's published machine name -> credential label.
fn effective_host_name(bundle: &MobileCredentialBundle, meta: Option<&HostMeta>) -> String {
    if let Some(custom) = meta
        .and_then(|meta| meta.custom_name.as_deref())
        .and_then(vibex_core::normalize_remote_display_name)
    {
        return custom;
    }
    if let Some(published) = meta
        .and_then(|meta| meta.server_display_name.as_deref())
        .and_then(vibex_core::normalize_remote_display_name)
    {
        return published;
    }
    bundle.host_label()
}

fn url_authority(url: &str) -> Option<String> {
    let rest = url
        .trim()
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url.trim());
    let authority = rest.split(['/', '?', '#']).next()?.trim();
    (!authority.is_empty()).then(|| authority.to_owned())
}

fn url_host(url: &str) -> Option<String> {
    let authority = url_authority(url)?;
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next()?
    } else {
        authority.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_owned())
}

fn is_tailscale_host(host: &str) -> bool {
    if host.to_ascii_lowercase().ends_with(".ts.net") {
        return true;
    }
    host.parse::<std::net::Ipv4Addr>().ok().is_some_and(|ip| {
        let [a, b, ..] = ip.octets();
        a == 100 && (64..=127).contains(&b)
    })
}

fn parse_route_key(key: &str) -> MobileRouteSelection {
    match key.trim() {
        "" | "auto" => MobileRouteSelection::Auto,
        "lan" => MobileRouteSelection::LocalNetwork,
        "relay" => MobileRouteSelection::Relay,
        other => other
            .strip_prefix("direct:")
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(|url| MobileRouteSelection::Direct {
                url: url.to_owned(),
            })
            .unwrap_or(MobileRouteSelection::Auto),
    }
}

fn route_options(
    bundle: &MobileCredentialBundle,
    selected: &MobileRouteSelection,
) -> Vec<RuntimeRouteOption> {
    let effective = if bundle.has_route(selected) {
        selected
    } else {
        &MobileRouteSelection::Auto
    };
    let mut options = vec![RuntimeRouteOption {
        key: "auto".to_owned(),
        kind: RouteOptionKind::Auto,
        detail: None,
        active: matches!(effective, MobileRouteSelection::Auto),
    }];
    let Some(route) = bundle.route.as_ref() else {
        return options;
    };
    if let Some(lan) = route.local_network.as_ref() {
        options.push(RuntimeRouteOption {
            key: "lan".to_owned(),
            kind: RouteOptionKind::LocalNetwork,
            detail: url_authority(&lan.url),
            active: matches!(effective, MobileRouteSelection::LocalNetwork),
        });
    }
    for url in &route.direct_candidates {
        let kind = if url_host(url).as_deref().is_some_and(is_tailscale_host) {
            RouteOptionKind::Tailscale
        } else {
            RouteOptionKind::Direct
        };
        let active = matches!(
            effective,
            MobileRouteSelection::Direct { url: selected_url }
                if selected_url
                    .trim()
                    .trim_end_matches('/')
                    .eq_ignore_ascii_case(url.trim().trim_end_matches('/'))
        );
        options.push(RuntimeRouteOption {
            key: format!("direct:{url}"),
            kind,
            detail: url_authority(url),
            active,
        });
    }
    if let Some(relay) = route.relay.as_ref() {
        let official = vibex_core::is_official_relay_origin(&relay.url);
        options.push(RuntimeRouteOption {
            key: "relay".to_owned(),
            kind: if official {
                RouteOptionKind::OfficialRelay
            } else {
                RouteOptionKind::SelfHostedRelay
            },
            detail: (!official).then(|| url_authority(&relay.url)).flatten(),
            active: matches!(effective, MobileRouteSelection::Relay),
        });
    }
    options
}

fn details_address(bundle: &MobileCredentialBundle) -> String {
    let concrete: Vec<String> = route_options(bundle, &MobileRouteSelection::Auto)
        .into_iter()
        .skip(1)
        .map(|option| match option.detail {
            Some(detail) => format!("{} · {detail}", option.kind.label()),
            None => option.kind.label().to_owned(),
        })
        .collect();
    if concrete.len() > 1 {
        concrete.join("\n")
    } else if vibex_core::is_official_relay_origin(&bundle.record.server_url) {
        copy::text("Official relay", "官方中继服务", "官方中繼服務").to_owned()
    } else {
        bundle.record.server_url.clone()
    }
}

/// Maps the wire's runtime kind to the page's.
fn runtime_kind(kind: RemoteServerKind) -> RuntimeKind {
    match kind {
        RemoteServerKind::Desktop => RuntimeKind::Desktop,
        RemoteServerKind::Headless => RuntimeKind::Headless,
        RemoteServerKind::Unknown => RuntimeKind::Unknown,
    }
}

/// The age of `at_ms` in the units the details page uses.
fn relative_age(at_ms: i64) -> (u64, u8) {
    let seconds = (unix_timestamp_ms() - at_ms).max(0) / 1000;
    match seconds {
        0..=59 => (0, 0),
        60..=3599 => (seconds as u64 / 60, 1),
        3600..=86399 => (seconds as u64 / 3600, 2),
        _ => (seconds as u64 / 86400, 3),
    }
}

/// The "Added …" line for a saved runtime.
fn added_label(at_ms: i64) -> String {
    let (value, unit) = relative_age(at_ms);
    match copy::language() {
        copy::Language::English => match unit {
            0 => "Added just now".to_owned(),
            1 => format!("Added {value}m ago"),
            2 => format!("Added {value}h ago"),
            _ => format!("Added {value}d ago"),
        },
        copy::Language::SimplifiedChinese => match unit {
            0 => "添加时间：刚刚".to_owned(),
            1 => format!("添加时间：{value} 分钟前"),
            2 => format!("添加时间：{value} 小时前"),
            _ => format!("添加时间：{value} 天前"),
        },
        copy::Language::TraditionalChinese => match unit {
            0 => "新增時間：剛剛".to_owned(),
            1 => format!("新增時間：{value} 分鐘前"),
            2 => format!("新增時間：{value} 小時前"),
            _ => format!("新增時間：{value} 天前"),
        },
        _ => match unit {
            0 => "Added just now".to_owned(),
            1 => format!("Added {value}m ago"),
            2 => format!("Added {value}h ago"),
            _ => format!("Added {value}d ago"),
        },
    }
}

/// The "Last connected …" line for a saved runtime.
fn last_connected_label(at_ms: i64) -> String {
    let (value, unit) = relative_age(at_ms);
    match copy::language() {
        copy::Language::English => match unit {
            0 => "Last connected just now".to_owned(),
            1 => format!("Last connected {value}m ago"),
            2 => format!("Last connected {value}h ago"),
            _ => format!("Last connected {value}d ago"),
        },
        copy::Language::SimplifiedChinese => match unit {
            0 => "上次连接：刚刚".to_owned(),
            1 => format!("上次连接：{value} 分钟前"),
            2 => format!("上次连接：{value} 小时前"),
            _ => format!("上次连接：{value} 天前"),
        },
        copy::Language::TraditionalChinese => match unit {
            0 => "上次連線：剛剛".to_owned(),
            1 => format!("上次連線：{value} 分鐘前"),
            2 => format!("上次連線：{value} 小時前"),
            _ => format!("上次連線：{value} 天前"),
        },
        _ => match unit {
            0 => "Last connected just now".to_owned(),
            1 => format!("Last connected {value}m ago"),
            2 => format!("Last connected {value}h ago"),
            _ => format!("Last connected {value}d ago"),
        },
    }
}

/// Groups a six-digit verification code for reading aloud.
fn format_code(code: &str) -> SharedString {
    let code = code.trim();
    if code.len() == 6 && code.chars().all(|character| character.is_ascii_digit()) {
        format!("{} {}", &code[..3], &code[3..]).into()
    } else {
        code.to_owned().into()
    }
}

pub(crate) fn error_text(error: &BackendError) -> SharedString {
    let text = match error.code.as_str() {
        "remote_pairing_code_invalid" | "remote_pairing_code_expired" => copy::text(
            "The pairing code is invalid or has expired.",
            "配对码无效或已过期。",
            "配對碼無效或已過期。",
        ),
        "remote_pairing_fragment_invalid" => copy::text(
            "This is not a valid Vibex pairing link.",
            "这不是有效的 Vibex 配对链接。",
            "這不是有效的 Vibex 配對連結。",
        ),
        "remote_pairing_offer_already_claimed" => copy::text(
            "This pairing link has already been used.",
            "这个配对链接已被使用。",
            "這個配對連結已被使用。",
        ),
        "remote_pairing_code_request_invalid" => copy::text(
            "Enter the pairing code and the server address.",
            "请填写配对码和服务器地址。",
            "請填寫配對碼和伺服器位址。",
        ),
        "mobile_pairing_store_invalid" | "mobile_pairing_store_unreadable" => copy::text(
            "The saved runtimes could not be read.",
            "无法读取已保存的运行时。",
            "無法讀取已儲存的執行環境。",
        ),
        "mobile_pairing_store_incompatible" => copy::text(
            "The saved runtimes were written by a newer Vibex.",
            "已保存的运行时由更新版本的 Vibex 写入。",
            "已儲存的執行環境由更新版本的 Vibex 寫入。",
        ),
        "mobile_pairing_store_write_failed" => copy::text(
            "The runtime could not be saved.",
            "无法保存运行时。",
            "無法儲存執行環境。",
        ),
        "remote_client_identity_mismatch" => copy::text(
            "The saved credential does not match this phone.",
            "保存的凭据与这台手机不匹配。",
            "儲存的憑證與這台手機不相符。",
        ),
        code if code == vibex_core::OFFICIAL_RELAY_TRAFFIC_LIMIT_EXCEEDED_CODE => copy::text(
            "Official relay unavailable: traffic limit reached",
            "官方中继不可用：已达最大限额",
            "官方中繼不可用：已達最大限額",
        ),
        code if code == vibex_core::RelayErrorCode::TrafficLimitExceeded.as_str() => copy::text(
            "Relay unavailable: traffic limit reached",
            "中继服务不可用：已达最大限额",
            "中繼服務不可用：已達最大限額",
        ),
        _ if error.kind == vibex_backend::BackendErrorKind::Offline => copy::text(
            "The runtime could not be reached.",
            "无法连接到运行时。",
            "無法連線到執行環境。",
        ),
        _ => return bounded(&error.message),
    };
    text.into()
}

/// A transport message cut to what a caption can show.
fn bounded(message: &str) -> SharedString {
    let message = message.trim();
    if message.is_empty() {
        "Pairing failed.".into()
    } else {
        message.chars().take(160).collect::<String>().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(origins: Vec<&str>) -> crate::discovery::Candidate {
        crate::discovery::Candidate {
            key: "desktop-test".to_owned(),
            name: "Studio".to_owned(),
            origins: origins.into_iter().map(str::to_owned).collect(),
            server_id: "desktop-test".to_owned(),
            server_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        }
    }

    #[test]
    fn every_advertised_address_is_tried_before_the_pairing_fails() {
        // Two addresses that refuse immediately: the loop visits both and
        // reports an error instead of waiting on the first one forever.
        let candidate = candidate(vec!["http://127.0.0.1:1", "http://127.0.0.1:2"]);
        let started = crate::runtime::block_on(start_lan_session(&candidate, "Test Phone", None));
        assert!(started.is_err());
    }

    fn saved_bundle(
        host_id: &str,
        display_name: Option<&str>,
        route: vibex_mobile_pairing::MobileRemoteRouteBundle,
    ) -> MobileCredentialBundle {
        let identity =
            ClientDeviceIdentity::generate(vibex_core::DeviceId::new()).expect("identity");
        MobileCredentialBundle {
            schema_version: vibex_mobile_pairing::MOBILE_CREDENTIAL_SCHEMA_VERSION.to_owned(),
            record: vibex_remote_client::RemoteCredentialRecord {
                server_url: "https://studio.tailnet.ts.net".to_owned(),
                auth: vibex_core::RemoteAuthProof {
                    device_id: identity.device_id().clone(),
                    auth_token: "grant-token".to_owned(),
                },
                device_identity_public_key: identity.public_key_base64(),
                server_identity_public_key: Some("server-public-key".to_owned()),
            },
            identity_private_key: identity.private_key_base64(),
            expected_server_id: host_id.to_owned(),
            client_type: vibex_core::RemoteClientType::Mobile,
            allow_insecure_local_dev: false,
            display_name: display_name.map(str::to_owned),
            route: Some(route),
        }
    }

    #[component]
    fn NoteRuntimeNameProbe() -> impl IntoElement {
        let dir = data_dir().expect("test data_dir");
        let store = PairingStore::new(dir.join(STORE_FILE));
        let service = PairingService::new(dir);
        assert_eq!(
            service.saved().get_untracked()[0].name,
            "studio.tailnet.ts.net"
        );
        let before_connected = service.connected().get_untracked().unwrap();

        // Observing the runtime's machine name updates the row and persists it
        // without rewriting `connected` (which would rebuild the session pager).
        let label = service.note_runtime_name("machine-1", "Studio Mac");
        assert_eq!(label.as_deref(), Some("Studio Mac"));
        assert_eq!(service.saved().get_untracked()[0].name, "Studio Mac");
        assert_eq!(
            service.connected_label().get_untracked().as_deref(),
            Some("Studio Mac")
        );
        assert_eq!(
            service.connected().get_untracked().unwrap(),
            before_connected
        );
        assert_eq!(
            store.load().unwrap()[0].display_name.as_deref(),
            Some("Studio Mac")
        );

        // Local custom rename wins over subsequent server display names.
        service.rename_host("machine-1", "Workstation");
        assert_eq!(service.saved().get_untracked()[0].name, "Workstation");
        assert_eq!(
            service.connected_label().get_untracked().as_deref(),
            Some("Workstation")
        );
        let label = service.note_runtime_name("machine-1", "Studio Mac");
        assert_eq!(label.as_deref(), Some("Workstation"));
        assert_eq!(service.saved().get_untracked()[0].name, "Workstation");

        // Clearing the local custom rename restores the machine name (not the Tailscale host).
        service.rename_host("machine-1", "");
        assert_eq!(service.saved().get_untracked()[0].name, "Studio Mac");
        assert_eq!(
            service.connected_label().get_untracked().as_deref(),
            Some("Studio Mac")
        );
        text("note-runtime-name-ok")
    }

    #[component]
    fn RoutePreferenceProbe() -> impl IntoElement {
        let dir = data_dir().expect("test data_dir");
        let service = PairingService::new(dir.clone());
        let bundle = service.bundles.borrow()[0].clone();
        assert!(
            bundle.has_route(&MobileRouteSelection::Relay),
            "the seeded runtime advertises a relay"
        );

        // Choosing a method marks it on the row and stores it for next time.
        service.set_preferred_route("machine-1", &bundle, MobileRouteSelection::Relay);
        let active_kind = |service: &PairingService| {
            service.saved().get_untracked()[0]
                .routes
                .iter()
                .find(|route| route.active)
                .map(|route| route.kind)
        };
        assert_eq!(
            active_kind(&service),
            Some(RouteOptionKind::SelfHostedRelay)
        );

        // A cold start restores the choice instead of falling back to Auto.
        let reopened = PairingService::new(dir);
        assert_eq!(
            active_kind(&reopened),
            Some(RouteOptionKind::SelfHostedRelay),
            "the chosen connection method must survive a restart"
        );
        text("route-preference-ok")
    }

    #[test]
    fn the_chosen_connection_method_survives_a_restart() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = PairingStore::new(dir.path().join(STORE_FILE));
        let bundle = saved_bundle(
            "machine-1",
            Some("Studio Mac"),
            vibex_mobile_pairing::MobileRemoteRouteBundle {
                local_network: Some(vibex_mobile_pairing::MobileLocalNetworkCandidate {
                    url: "https://192.168.1.20:8443".to_owned(),
                    tls_certificate_der: "cert".to_owned(),
                }),
                direct_candidates: vec!["https://studio.tailnet.ts.net".to_owned()],
                relay: Some(vibex_mobile_pairing::MobileRelayCandidate {
                    url: "https://relay.example.com".to_owned(),
                    room_id: vibex_core::RelayRoomId::new(),
                    local_peer_id: vibex_core::RelayPeerId::new(),
                    pc_peer_id: vibex_core::RelayPeerId::new(),
                    pc_public_key: "relay-pub".to_owned(),
                }),
            },
        );
        store.upsert(bundle).unwrap();
        std::fs::write(dir.path().join(LAST_HOST_FILE), "machine-1").unwrap();

        let mut config =
            nsh_engine::EngineConfig::new(nsh_engine::Root::of::<RoutePreferenceProbe>());
        config.data_dir = Some(dir.path().to_path_buf());
        let mut app = nsh_testing::Headless::start(config).unwrap();
        app.wait_for("route-preference-ok").unwrap();
    }

    #[test]
    fn note_runtime_name_updates_saved_row_and_respects_custom_name() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = PairingStore::new(dir.path().join(STORE_FILE));
        let bundle = saved_bundle(
            "machine-1",
            None,
            vibex_mobile_pairing::MobileRemoteRouteBundle {
                local_network: None,
                direct_candidates: vec!["https://studio.tailnet.ts.net".to_owned()],
                relay: None,
            },
        );
        store.upsert(bundle).unwrap();
        std::fs::write(dir.path().join(LAST_HOST_FILE), "machine-1").unwrap();

        let mut config =
            nsh_engine::EngineConfig::new(nsh_engine::Root::of::<NoteRuntimeNameProbe>());
        config.data_dir = Some(dir.path().to_path_buf());
        let mut app = nsh_testing::Headless::start(config).unwrap();
        app.wait_for("note-runtime-name-ok").unwrap();
    }

    #[component]
    fn RemoveLastDeviceProbe() -> impl IntoElement {
        let dir = data_dir().expect("test data_dir");
        let service = PairingService::new(dir);
        assert!(service.connected().get_untracked().is_some());
        service.open_switcher();
        assert!(service.switcher().get_untracked());

        service.remove_host("machine-1");
        assert!(service.saved().get_untracked().is_empty());
        assert!(service.connected().get_untracked().is_none());
        assert!(!service.switcher().get_untracked());
        text("remove-last-device-ok")
    }

    #[test]
    fn removing_last_device_clears_switcher_and_connection() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = PairingStore::new(dir.path().join(STORE_FILE));
        let bundle = saved_bundle(
            "machine-1",
            Some("Studio Mac"),
            vibex_mobile_pairing::MobileRemoteRouteBundle {
                local_network: None,
                direct_candidates: vec!["https://studio.tailnet.ts.net".to_owned()],
                relay: None,
            },
        );
        store.upsert(bundle).unwrap();
        std::fs::write(dir.path().join(LAST_HOST_FILE), "machine-1").unwrap();

        let mut config =
            nsh_engine::EngineConfig::new(nsh_engine::Root::of::<RemoveLastDeviceProbe>());
        config.data_dir = Some(dir.path().to_path_buf());
        let mut app = nsh_testing::Headless::start(config).unwrap();
        app.wait_for("remove-last-device-ok").unwrap();
    }

    #[test]
    fn route_options_classify_lan_tailscale_direct_and_relay() {
        let bundle = saved_bundle(
            "machine-1",
            Some("Studio Mac"),
            vibex_mobile_pairing::MobileRemoteRouteBundle {
                local_network: Some(vibex_mobile_pairing::MobileLocalNetworkCandidate {
                    url: "https://192.168.1.20:8443".to_owned(),
                    tls_certificate_der: "cert".to_owned(),
                }),
                direct_candidates: vec![
                    "https://studio.tailnet.ts.net".to_owned(),
                    "https://100.88.12.34:8443".to_owned(),
                    "https://dev.example.com".to_owned(),
                ],
                relay: Some(vibex_mobile_pairing::MobileRelayCandidate {
                    url: "https://relay.example.com".to_owned(),
                    room_id: vibex_core::RelayRoomId::new(),
                    local_peer_id: vibex_core::RelayPeerId::new(),
                    pc_peer_id: vibex_core::RelayPeerId::new(),
                    pc_public_key: "relay-pub".to_owned(),
                }),
            },
        );

        let options = route_options(&bundle, &MobileRouteSelection::Relay);
        let summary: Vec<(RouteOptionKind, Option<&str>, bool)> = options
            .iter()
            .map(|opt| (opt.kind, opt.detail.as_deref(), opt.active))
            .collect();
        assert_eq!(
            summary,
            vec![
                (RouteOptionKind::Auto, None, false),
                (
                    RouteOptionKind::LocalNetwork,
                    Some("192.168.1.20:8443"),
                    false
                ),
                (
                    RouteOptionKind::Tailscale,
                    Some("studio.tailnet.ts.net"),
                    false
                ),
                (RouteOptionKind::Tailscale, Some("100.88.12.34:8443"), false),
                (RouteOptionKind::Direct, Some("dev.example.com"), false),
                (
                    RouteOptionKind::SelfHostedRelay,
                    Some("relay.example.com"),
                    true
                ),
            ]
        );
        for option in &options {
            let parsed = parse_route_key(&option.key);
            assert!(
                bundle.has_route(&parsed),
                "key {} should match bundle",
                option.key
            );
        }
    }

    #[component]
    fn RelayTrafficLimitErrorTextProbe() -> impl IntoElement {
        let official = BackendError::offline(
            vibex_core::OFFICIAL_RELAY_TRAFFIC_LIMIT_EXCEEDED_CODE,
            "relay traffic limit reached",
        );
        let official_text = error_text(&official);
        assert!(
            matches!(
                official_text.as_ref(),
                "Official relay unavailable: traffic limit reached"
                    | "官方中继不可用：已达最大限额"
                    | "官方中繼不可用：已達最大限額"
            ),
            "unexpected official relay error text: {official_text}"
        );

        let self_hosted = BackendError::offline(
            vibex_core::RelayErrorCode::TrafficLimitExceeded.as_str(),
            "relay traffic limit reached",
        );
        let self_hosted_text = error_text(&self_hosted);
        assert!(
            matches!(
                self_hosted_text.as_ref(),
                "Relay unavailable: traffic limit reached"
                    | "中继服务不可用：已达最大限额"
                    | "中繼服務不可用：已達最大限額"
            ),
            "unexpected self-hosted relay error text: {self_hosted_text}"
        );
        text("relay-traffic-limit-error-text-ok")
    }

    #[test]
    fn error_text_formats_relay_traffic_limit_codes() {
        let config = nsh_engine::EngineConfig::new(nsh_engine::Root::of::<
            RelayTrafficLimitErrorTextProbe,
        >());
        let mut app = nsh_testing::Headless::start(config).unwrap();
        app.wait_for("relay-traffic-limit-error-text-ok").unwrap();
    }
}

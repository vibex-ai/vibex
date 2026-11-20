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
use vibex_core::{RemoteLanPairingRequestState, unix_timestamp_ms};
use vibex_mobile_pairing::{
    MobileCredentialBundle, MobilePairedRuntime, PairingStore, claim_entry,
    claim_zero_config_lan_pairing, classify_entry,
};
use vibex_nsh_ui::{
    copy,
    screens::pairing::{NearbyRuntime, PairingHost, RuntimeStatus, SavedRuntime},
};
use vibex_remote_client::ZeroConfigLanPairingSession;

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
}

/// The runtime the page is connected to, once a claim or a probe succeeds.
#[derive(Clone, PartialEq)]
pub struct ConnectedRuntime {
    /// The label the shell shows for it.
    pub label: String,
    /// The credential the session shell's backend is built from.
    pub bundle: MobileCredentialBundle,
}

/// The pairing page's state and actions.
#[derive(Clone)]
pub struct PairingService {
    store: Rc<PairingStore>,
    bundles: Rc<RefCell<Vec<MobileCredentialBundle>>>,
    saved: Signal<Vec<SavedRuntime>>,
    nearby: Signal<Vec<NearbyRuntime>>,
    details: Signal<Option<vibex_nsh_ui::screens::pairing::HostDetails>>,
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
    /// A nearby pairing session waits for the desktop's approval.
    nearby_waiting: Signal<bool>,
    /// The code the desktop shows, once the session started.
    nearby_code: Signal<Option<SharedString>>,
    /// Bumped to abandon the session's poll loop.
    nearby_generation: Rc<Cell<u64>>,
}

impl PairingService {
    /// Loads the saved runtimes and starts their reachability probes.
    pub fn new(data_dir: PathBuf) -> Self {
        let store = Rc::new(PairingStore::new(data_dir.join(STORE_FILE)));
        let (bundles, error) = match store.load() {
            Ok(bundles) => (bundles, None),
            Err(error) => (Vec::new(), Some(error_text(&error))),
        };
        let saved = signal(
            bundles
                .iter()
                .map(|bundle| SavedRuntime {
                    id: bundle.host_id().to_owned(),
                    name: bundle.host_label(),
                    status: RuntimeStatus::Unreachable,
                    address: bundle.record.server_url.clone(),
                    server_id: bundle.expected_server_id.clone(),
                })
                .collect::<Vec<_>>(),
        );
        let service = Self {
            store,
            bundles: Rc::new(RefCell::new(bundles)),
            saved,
            nearby: signal(Vec::new()),
            details: signal(None),
            rename_draft: signal(String::new()),
            meta: Rc::new(RefCell::new(
                std::fs::read_to_string(data_dir.join(META_FILE))
                    .ok()
                    .and_then(|text| serde_json::from_str(&text).ok())
                    .unwrap_or_default(),
            )),
            meta_path: data_dir.join(META_FILE),
            last_host_path: data_dir.join(LAST_HOST_FILE),
            switcher: signal(false),
            connected_label: signal(None),
            busy: signal(false),
            error: signal(error),
            connected: signal(None),
            discovery: Rc::new(RefCell::new(None)),
            candidates: Rc::new(RefCell::new(BTreeMap::new())),
            nearby_waiting: signal(false),
            nearby_code: signal(None),
            nearby_generation: Rc::new(Cell::new(0)),
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
            on_pick_saved: Rc::new(move |id| pick_saved.probe_runtime(id)),
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
            nearby_waiting: self.nearby_waiting,
            nearby_code: self.nearby_code,
            on_cancel_nearby: {
                let service = self.clone();
                Rc::new(move || service.cancel_nearby())
            },
            on_stop_nearby: {
                let service = self.clone();
                Rc::new(move || service.stop_nearby())
            },
        }
    }

    /// Starts browsing the local network and lists what it finds.
    fn start_nearby(&self) {
        self.stop_nearby();
        self.nearby.set(Vec::new());
        self.candidates.borrow_mut().clear();
        self.error.set(None);
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
                self.fail_message(copy::text(
                    "Local discovery could not start on this device.",
                    "无法在此设备上开始局域网发现。",
                    "無法在此裝置上開始區域網路探索。",
                ));
                return;
            }
        }
        let service = self.clone();
        spawn(async move {
            while let Some(event) = events.next().await {
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
                    }
                    crate::discovery::Event::Lost(key) => {
                        service.candidates.borrow_mut().remove(&key);
                        service
                            .nearby
                            .update(|rows| rows.retain(|row| row.key != key));
                    }
                    crate::discovery::Event::Failed => {
                        service.fail_message(copy::text(
                            "Local discovery stopped: the network would not advertise nearby runtimes.",
                            "局域网发现已停止：网络没有播报附近的运行时。",
                            "區域網路探索已停止：網路沒有廣播附近的執行環境。",
                        ));
                        service.stop_nearby();
                        break;
                    }
                }
            }
        });
    }

    /// Stops browsing and abandons any nearby session.
    fn stop_nearby(&self) {
        if crate::host::discovery_available() {
            crate::host::request_discovery(false);
        }
        if let Some(discovery) = self.discovery.borrow_mut().take() {
            discovery.stop();
        }
        self.nearby_generation
            .set(self.nearby_generation.get().wrapping_add(1));
        self.nearby_waiting.set(false);
        self.nearby_code.set(None);
    }

    /// Starts a zero-config session with a discovered runtime and waits for
    /// the desktop to approve the code it shows.
    fn start_nearby_pair(&self, key: &str) {
        if self.nearby_waiting.get_untracked() || self.busy.get_untracked() {
            return;
        }
        let Some(candidate) = self.candidates.borrow().get(key).cloned() else {
            return;
        };
        self.error.set(None);
        self.busy.set(true);
        self.nearby_waiting.set(true);
        self.nearby_code.set(None);
        let generation = self.nearby_generation.get().wrapping_add(1);
        self.nearby_generation.set(generation);
        let service = self.clone();
        spawn(async move {
            let device = crate::host::device_name().unwrap_or_else(|| DEVICE_NAME.to_owned());
            let opened = background(async move {
                ZeroConfigLanPairingSession::start(
                    candidate.origin,
                    &candidate.server_id,
                    &candidate.server_key,
                    &device,
                )
                .await
            })
            .await;
            let mut session = match opened {
                Ok(Ok(session)) => session,
                Ok(Err(error)) => {
                    service.busy.set(false);
                    service.nearby_waiting.set(false);
                    service.fail(&error);
                    return;
                }
                Err(error) => {
                    service.busy.set(false);
                    service.nearby_waiting.set(false);
                    service.fail_text(format!("the nearby pairing did not start: {error}"));
                    return;
                }
            };
            service.busy.set(false);
            service
                .nearby_code
                .set(Some(format_code(session.verification_code())));
            loop {
                nsh_core::element::timer(Duration::from_millis(700)).await;
                if service.nearby_generation.get() != generation {
                    return;
                }
                let polled = background(async move {
                    let result = session.poll().await;
                    (session, result)
                })
                .await;
                let (returned, result) = match polled {
                    Ok(pair) => pair,
                    Err(error) => {
                        service.nearby_waiting.set(false);
                        service.nearby_code.set(None);
                        service.fail_text(format!("the nearby pairing did not answer: {error}"));
                        return;
                    }
                };
                session = returned;
                let status = match result {
                    Ok(status) => status,
                    Err(error) => {
                        service.nearby_waiting.set(false);
                        service.nearby_code.set(None);
                        service.fail(&error);
                        return;
                    }
                };
                match status.state {
                    RemoteLanPairingRequestState::Pending => continue,
                    RemoteLanPairingRequestState::Approved => {
                        let claimed = background(async move {
                            claim_zero_config_lan_pairing(&mut session, status).await
                        })
                        .await;
                        service.nearby_waiting.set(false);
                        service.nearby_code.set(None);
                        match claimed {
                            Ok(claimed) => {
                                service.stop_nearby();
                                service.finish_claim(claimed)
                            }
                            Err(error) => service
                                .fail_text(format!("the nearby pairing did not finish: {error}")),
                        }
                        return;
                    }
                    RemoteLanPairingRequestState::Rejected => {
                        service.nearby_waiting.set(false);
                        service.nearby_code.set(None);
                        service.fail_message(copy::text(
                            "The desktop declined this pairing.",
                            "桌面端拒绝了这个配对请求。",
                            "桌面版拒絕了這個配對請求。",
                        ));
                        return;
                    }
                    RemoteLanPairingRequestState::Expired
                    | RemoteLanPairingRequestState::Claimed => {
                        service.nearby_waiting.set(false);
                        service.nearby_code.set(None);
                        service.fail_message(copy::text(
                            "This pairing expired. Find the computer again.",
                            "这个配对请求已过期，请重新查找电脑。",
                            "這個配對請求已過期，請重新尋找電腦。",
                        ));
                        return;
                    }
                    RemoteLanPairingRequestState::Unknown => {
                        service.nearby_waiting.set(false);
                        service.nearby_code.set(None);
                        service.fail_message(copy::text(
                            "The desktop returned an unknown pairing state.",
                            "桌面端返回了未知的配对状态。",
                            "桌面版傳回了未知的配對狀態。",
                        ));
                        return;
                    }
                }
            }
        });
    }

    /// Abandons the nearby session; its poll loop stops on the next tick.
    fn cancel_nearby(&self) {
        self.nearby_generation
            .set(self.nearby_generation.get().wrapping_add(1));
        self.nearby_waiting.set(false);
        self.nearby_code.set(None);
        self.busy.set(false);
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

    /// The runtime the page is connected to.
    pub fn connected(&self) -> Signal<Option<ConnectedRuntime>> {
        self.connected
    }

    /// Leaves the connected runtime; the credential stays saved.
    pub fn disconnect(&self) {
        self.connected.set(None);
        self.connected_label.set(None);
        self.error.set(None);
    }

    /// Removes every saved credential; the settings sheet asks first.
    pub fn clear_runtimes(&self) {
        let service = self.clone();
        spawn(async move {
            let answer = nsh_base::alert_dialog()
                .title(copy::text(
                    "Remove all saved runtimes?",
                    "移除所有已保存的运行时？",
                    "移除所有已儲存的執行環境？",
                ))
                .description(copy::text(
                    "Every credential leaves this phone; pair again to reconnect.",
                    "所有凭据都会从这台手机上删除，需要重新配对才能连接。",
                    "所有憑證都會從這台手機上刪除，需要重新配對才能連線。",
                ))
                .cancel("cancel", copy::text("Cancel", "取消", "取消"))
                .destructive("remove", copy::text("Remove", "移除", "移除"))
                .show()
                .await;
            if !matches!(&answer, Ok(answer) if answer.action.as_deref() == Some("remove")) {
                return;
            }
            let ids = service
                .bundles
                .borrow()
                .iter()
                .map(|bundle| bundle.host_id().to_owned())
                .collect::<Vec<_>>();
            let _ = std::fs::remove_file(&service.last_host_path);
            let mut remaining = service.bundles.borrow().clone();
            for id in ids {
                if let Ok(bundles) = service.store.remove(&id) {
                    remaining = bundles;
                }
            }
            service.set_bundles(remaining);
            service.details.set(None);
            service.disconnect();
        });
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
        spawn(async move {
            let files = pick_images(PickRequest::new(false, 1)).await;
            let Some(file) = files.into_iter().next() else {
                return;
            };
            let bytes = file.data.bytes().to_vec();
            let decoded = background(async move { decode_qr_photo(&bytes) }).await;
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
        // once, without a task and without leaving the sheet waiting.
        if let Err(error) = classify_entry(entry, server) {
            self.fail(&error);
            return;
        }
        self.error.set(None);
        self.busy.set(true);
        let service = self.clone();
        let entry = entry.to_owned();
        let server = server.to_owned();
        spawn(async move {
            let device = crate::host::device_name().unwrap_or_else(|| DEVICE_NAME.to_owned());
            // The claim is network I/O: run it on the runtime's threads, not
            // on the engine thread that renders.
            let result =
                background(async move { claim_entry(entry, server, device, None).await }).await;
            let result = match result {
                Ok(result) => result,
                Err(error) => Err(BackendError::failed(
                    "pairing_task_failed",
                    format!("the pairing task did not finish: {error}"),
                )),
            };
            service.finish_claim(result);
        });
    }

    /// Stores a claimed credential and shows it in the list.
    fn finish_claim(&self, result: BackendResult<MobilePairedRuntime>) {
        match result {
            Ok(paired) => match self.store.upsert(paired.bundle.clone()) {
                Ok(bundles) => {
                    let id = paired.bundle.host_id().to_owned();
                    self.meta.borrow_mut().entry(id).or_default().added_at_ms = unix_timestamp_ms();
                    self.save_meta();
                    self.set_bundles(bundles);
                    self.busy.set(false);
                    self.connect(paired.bundle);
                }
                Err(error) => self.fail(&error),
            },
            Err(error) => self.fail(&error),
        }
    }

    /// Reads the clipboard into the entry field the sheet passed.
    fn paste(&self, draft: Signal<String>) {
        let service = self.clone();
        spawn(async move {
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
        self.rename_draft.set(bundle.host_label());
        self.details
            .set(Some(vibex_nsh_ui::screens::pairing::HostDetails {
                id: host_id.to_owned(),
                name: bundle.host_label(),
                address: bundle.record.server_url.clone(),
                server_id: bundle.expected_server_id.clone(),
                device_name: meta.device_name.clone().or_else(crate::host::device_name),
                last_connected: (meta.last_connected_at_ms > 0)
                    .then(|| format_when(meta.last_connected_at_ms)),
                added: (meta.added_at_ms > 0).then(|| format_when(meta.added_at_ms)),
            }));
    }

    /// Renames a saved runtime locally (the credential keeps the authority's
    /// name until the runtime is reached again).
    fn rename_host(&self, host_id: &str, name: &str) {
        let name = name.trim();
        let mut bundle = self
            .bundles
            .borrow()
            .iter()
            .find(|bundle| bundle.host_id() == host_id)
            .cloned()
            .map(|bundle| {
                let mut bundle = bundle;
                bundle.display_name = if name.is_empty() {
                    None
                } else {
                    Some(name.to_owned())
                };
                bundle
            });
        let Some(bundle) = bundle.take() else {
            return;
        };
        match self.store.upsert(bundle) {
            Ok(bundles) => {
                let saved_bundle = bundles
                    .iter()
                    .find(|bundle| bundle.host_id() == host_id)
                    .cloned();
                let saved_name = saved_bundle
                    .as_ref()
                    .map(|bundle| bundle.host_label())
                    .unwrap_or_default();
                self.set_bundles(bundles);
                if let Some(bundle) = saved_bundle
                    && self
                        .connected
                        .get_untracked()
                        .is_some_and(|connected| connected.bundle.host_id() == host_id)
                {
                    self.connected_label.set(Some(saved_name.clone().into()));
                    self.connected.set(Some(ConnectedRuntime {
                        label: saved_name.clone(),
                        bundle,
                    }));
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

    /// Removes a saved runtime after the native confirmation.
    fn remove_host(&self, host_id: &str) {
        let host_id = host_id.to_owned();
        let service = self.clone();
        spawn(async move {
            let answer = nsh_base::alert_dialog()
                .title(copy::text(
                    "Remove this runtime?",
                    "移除这个运行时？",
                    "移除這個執行環境？",
                ))
                .description(copy::text(
                    "Its credential leaves this phone; pair again to reconnect.",
                    "它的凭据会从这台手机上删除，需要重新配对才能连接。",
                    "它的憑證會從這台手機上刪除，需要重新配對才能連線。",
                ))
                .cancel("cancel", copy::text("Cancel", "取消", "取消"))
                .destructive("remove", copy::text("Remove", "移除", "移除"))
                .show()
                .await;
            if !matches!(&answer, Ok(answer) if answer.action.as_deref() == Some("remove")) {
                return;
            }
            let result = service.store.remove(&host_id);
            if service
                .last_host_id()
                .as_deref()
                .is_some_and(|last| last == host_id)
            {
                let _ = std::fs::remove_file(&service.last_host_path);
            }
            match result {
                Ok(bundles) => {
                    let was_connected = service
                        .connected
                        .get_untracked()
                        .is_some_and(|connected| connected.bundle.host_id() == host_id);
                    service.set_bundles(bundles);
                    service.details.set(None);
                    if was_connected {
                        service.connected.set(None);
                    }
                }
                Err(error) => service.fail(&error),
            }
        });
    }

    /// Probes every saved runtime once, at startup.
    fn probe_saved(&self) {
        let active = self
            .connected
            .get_untracked()
            .map(|connected| connected.bundle.host_id().to_owned());
        let bundles = self.bundles.borrow().clone();
        for bundle in bundles {
            if active.as_deref() == Some(bundle.host_id()) {
                continue;
            }
            self.spawn_probe(bundle, false);
        }
    }

    /// Keeps the saved runtimes' reachability fresh while the page is visible.
    fn watch_reachability(&self) {
        let service = self.clone();
        spawn(async move {
            loop {
                nsh_core::element::timer(Duration::from_secs(20)).await;
                if service.connected.get_untracked().is_none() || service.switcher.get_untracked() {
                    service.probe_saved();
                }
            }
        });
    }

    /// Probes the saved runtime with `host_id`.
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
        self.set_status(host_id, RuntimeStatus::Probing);
        self.error.set(None);
        self.spawn_probe(bundle, true);
    }

    fn spawn_probe(&self, bundle: MobileCredentialBundle, connect_on_success: bool) {
        let service = self.clone();
        let id = bundle.host_id().to_owned();
        let probing = bundle.clone();
        spawn(async move {
            // A panicking probe is an unreachable runtime, not a crash of the
            // page: the row simply stays red.
            let online = background(async move { probing.probe_online().await })
                .await
                .unwrap_or(false);
            service.set_status(
                &id,
                if online {
                    RuntimeStatus::Reachable
                } else {
                    RuntimeStatus::Unreachable
                },
            );
            if connect_on_success {
                // A saved runtime is entered even when the probe failed: the
                // session service shows the connecting page and its own retry,
                // which is where an unavailable runtime belongs. Refusing here
                // left the reader on the list with a line of red text.
                service.connect(bundle);
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
        let rows = bundles
            .iter()
            .map(|bundle| {
                let id = bundle.host_id().to_owned();
                let status = previous
                    .iter()
                    .find(|(previous_id, _)| *previous_id == id)
                    .map_or(RuntimeStatus::Unreachable, |(_, status)| *status);
                SavedRuntime {
                    id,
                    name: bundle.host_label(),
                    status,
                    address: bundle.record.server_url.clone(),
                    server_id: bundle.expected_server_id.clone(),
                }
            })
            .collect::<Vec<_>>();
        *self.bundles.borrow_mut() = bundles;
        self.saved.set(rows);
    }

    fn connect(&self, bundle: MobileCredentialBundle) {
        {
            let id = bundle.host_id().to_owned();
            let mut meta = self.meta.borrow_mut();
            let entry = meta.entry(id).or_default();
            entry.last_connected_at_ms = unix_timestamp_ms();
            entry.device_name = crate::host::device_name();
            drop(meta);
            self.save_meta();
        }
        self.show_connected(bundle, true);
    }

    /// Shows `bundle` as the connected runtime without treating it as a new
    /// connection, so a cold start does not move the "last connected" clock.
    fn restore(&self, bundle: MobileCredentialBundle) {
        self.show_connected(bundle, false);
    }

    fn show_connected(&self, bundle: MobileCredentialBundle, remember: bool) {
        if remember {
            let _ = std::fs::write(&self.last_host_path, bundle.host_id());
        }
        self.switcher.set(false);
        let id = bundle.host_id().to_owned();
        self.saved.update(|saved| {
            if let Some(row) = saved.iter_mut().find(|row| row.id == id) {
                row.status = RuntimeStatus::Reachable;
            }
        });
        self.connected_label.set(Some(bundle.host_label().into()));
        self.connected.set(Some(ConnectedRuntime {
            label: bundle.host_label(),
            bundle,
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
            self.restore(bundle);
        }
    }

    /// Applies the runtime's new name to the credential and the page.
    pub fn rename_connected(&self, display_name: &str) {
        let Some(name) = vibex_core::normalize_remote_display_name(display_name) else {
            return;
        };
        let connected = self.connected.get_untracked();
        let mut updated = None;
        {
            let Some(connected) = connected else {
                return;
            };
            let mut bundles = self.bundles.borrow_mut();
            for bundle in bundles.iter_mut() {
                if connected.bundle.host_id() == bundle.host_id() {
                    if bundle.display_name.as_deref() == Some(name.as_str()) {
                        break;
                    }
                    bundle.display_name = Some(name.clone());
                    updated = Some(bundle.clone());
                    break;
                }
            }
        }
        let Some(bundle) = updated else {
            return;
        };
        if let Ok(bundles) = self.store.upsert(bundle.clone()) {
            let label = bundle.host_label();
            self.set_bundles(bundles);
            self.connected_label.set(Some(label.clone().into()));
            self.connected.set(Some(ConnectedRuntime { label, bundle }));
        }
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
                origin,
                server_id,
                server_key,
            } => {
                if let (Some(origin), Some(server_id), Some(server_key)) =
                    (origin, server_id, server_key)
                {
                    self.candidates.borrow_mut().insert(
                        key.clone(),
                        crate::discovery::Candidate {
                            key: key.clone(),
                            name: name.clone(),
                            origin,
                            server_id,
                            server_key,
                        },
                    );
                }
                self.nearby.update(|nearby| {
                    if let Some(candidate) = nearby.iter_mut().find(|nearby| nearby.key == key) {
                        candidate.name = name;
                    } else {
                        nearby.push(NearbyRuntime { key, name });
                    }
                });
            }
            HostEvent::DiscoveryRemoved { key } => {
                self.nearby
                    .update(|nearby| nearby.retain(|nearby| nearby.key != key));
            }
            HostEvent::DiscoveryPermissionDenied => self.fail_text(copy::text(
                "Local network access is needed to find nearby computers.",
                "查找附近的电脑需要局域网权限。",
                "尋找附近的電腦需要區域網路權限。",
            )),
            HostEvent::DiscoveryFailed(message) => self.fail_text(message),
        }
    }

    fn fail(&self, error: &BackendError) {
        self.busy.set(false);
        self.error.set(Some(error_text(error)));
    }

    fn fail_message(&self, message: &'static str) {
        self.fail_text(message);
    }

    fn fail_text(&self, message: impl AsRef<str>) {
        self.busy.set(false);
        self.error.set(Some(message.as_ref().to_owned().into()));
    }
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

/// A short, local reading of a millisecond timestamp.
fn format_when(at_ms: i64) -> String {
    let seconds = (unix_timestamp_ms() - at_ms).max(0) / 1000;
    match seconds {
        0..=59 => copy::text("Just now", "刚刚", "剛剛").to_owned(),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
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

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
use vibex_core::RemoteLanPairingRequestState;
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
                })
                .collect::<Vec<_>>(),
        );
        let service = Self {
            store,
            bundles: Rc::new(RefCell::new(bundles)),
            saved,
            nearby: signal(Vec::new()),
            busy: signal(false),
            error: signal(error),
            connected: signal(None),
            discovery: Rc::new(RefCell::new(None)),
            candidates: Rc::new(RefCell::new(BTreeMap::new())),
            nearby_waiting: signal(false),
            nearby_code: signal(None),
            nearby_generation: Rc::new(Cell::new(0)),
        };
        service.probe_saved();
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
            on_pick_nearby: Rc::new(move |key| pick_nearby.start_nearby_pair(key)),
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

    /// The runtime the page is connected to.
    pub fn connected(&self) -> Signal<Option<ConnectedRuntime>> {
        self.connected
    }

    /// Leaves the connected runtime; the credential stays saved.
    pub fn disconnect(&self) {
        self.connected.set(None);
        self.error.set(None);
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

    /// Probes every saved runtime once, at startup.
    fn probe_saved(&self) {
        let bundles = self.bundles.borrow().clone();
        for bundle in bundles {
            self.spawn_probe(bundle, false);
        }
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
                if online {
                    service.connect(bundle);
                } else {
                    service.fail_message(copy::text(
                        "The runtime could not be reached.",
                        "无法连接到运行时。",
                        "無法連線到執行環境。",
                    ));
                }
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
                }
            })
            .collect::<Vec<_>>();
        *self.bundles.borrow_mut() = bundles;
        self.saved.set(rows);
    }

    fn connect(&self, bundle: MobileCredentialBundle) {
        self.connected.set(Some(ConnectedRuntime {
            label: bundle.host_label(),
            bundle,
        }));
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

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

use std::{cell::RefCell, path::PathBuf, rc::Rc};

use nsh_core::prelude::*;
use vibex_backend::{BackendError, BackendResult};
use vibex_mobile_pairing::{
    MobileCredentialBundle, MobilePairedRuntime, PairingStore, claim_entry, classify_entry,
};
use vibex_nsh_ui::{
    copy,
    screens::pairing::{NearbyRuntime, PairingHost, RuntimeStatus, SavedRuntime},
};

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
                // The shell polls for this and opens the camera; the scan
                // comes back through the host event channel.
                crate::host::request_scan();
                scan.error.set(None);
            }),
            on_connect: Rc::new(move |entry, server| connect.connect_entry(entry, server)),
            on_paste: Rc::new(move |draft| paste.paste(draft)),
            on_find: Rc::new(move || {
                crate::host::request_discovery(true);
                find.nearby.set(Vec::new());
                find.error.set(None);
            }),
            on_pick_saved: Rc::new(move |id| pick_saved.probe_runtime(id)),
            on_pick_nearby: Rc::new(move |_| {
                pick_nearby.fail_message(copy::text(
                    "Nearby pairing needs the local-network host, which is not wired yet.",
                    "附近配对需要局域网宿主，尚未接入。",
                    "附近配對需要區域網路宿主，尚未接入。",
                ));
            }),
        }
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
            HostEvent::DiscoveryCandidate { key, name } => {
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

    fn fail_text(&self, message: impl Into<SharedString>) {
        self.busy.set(false);
        self.error.set(Some(message.into()));
    }
}

/// The page's wording for a pairing failure.
///
/// The transport's messages describe the protocol; the page says what the
/// person can do next. An unmapped code falls back to the message, bounded.
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

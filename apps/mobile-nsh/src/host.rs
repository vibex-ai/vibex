//! Events the platform hosts send into the running app.
//!
//! The Kotlin and Swift shells own the camera and the local-network browser,
//! so their results arrive here: a scanned pairing link, a discovered runtime
//! or a permission answer. The channel is process-wide because one phone runs
//! one surface; the app starts the reader once, on the engine thread, so the
//! pairing service's signals are written where they are owned.
//!
//! Discovery payloads are a small JSON contract until the host bridge is
//! aligned with the desktop's advertisements:
//!
//! - `{"kind":"candidate","key":"…","name":"…"}`
//! - `{"kind":"removed","key":"…"}`
//! - `{"kind":"permission_denied"}`
//! - `{"kind":"failed","message":"…"}`

use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};

use futures_channel::mpsc::{UnboundedSender, unbounded};
use futures_util::StreamExt as _;
use nsh_core::prelude::*;
use serde::Deserialize;

use crate::pairing::PairingService;

/// Something the host observed and the app should act on.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(any(target_os = "android", target_os = "ios")),
    allow(dead_code, reason = "built by the platform entry points only")
)]
pub enum HostEvent {
    /// The camera read a pairing link or a server connection string.
    ScanResult(String),
    /// Discovery found a runtime. A zero-config runtime also carries the
    /// origin and identity a pairing session needs; the simplified marker
    /// (old hosts, tests) leaves them empty.
    DiscoveryCandidate {
        key: String,
        name: String,
        origin: Option<String>,
        server_id: Option<String>,
        server_key: Option<String>,
    },
    /// A runtime left the network.
    DiscoveryRemoved { key: String },
    /// The user refused local-network access.
    DiscoveryPermissionDenied,
    /// Discovery failed.
    DiscoveryFailed(String),
}

static SENDER: OnceLock<Mutex<Option<UnboundedSender<HostEvent>>>> = OnceLock::new();

/// Whether the host carries a live camera scanner (set by a custom host).
static SCANNER_AVAILABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the host browses the local network itself (a custom host).
static DISCOVERY_AVAILABLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Records that the host can scan a QR code with its camera. Called by the
/// host before it mounts the app.
#[cfg_attr(
    not(target_os = "android"),
    allow(dead_code, reason = "the Android host sets this")
)]
pub fn set_scanner_available(available: bool) {
    SCANNER_AVAILABLE.store(available, std::sync::atomic::Ordering::Relaxed);
}

/// Whether Scan should ask the host for the camera instead of the photo
/// picker.
pub fn scanner_available() -> bool {
    SCANNER_AVAILABLE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Records that the host runs discovery (NSD) itself.
#[cfg_attr(
    not(target_os = "android"),
    allow(dead_code, reason = "the Android host sets this")
)]
pub fn set_discovery_available(available: bool) {
    DISCOVERY_AVAILABLE.store(available, std::sync::atomic::Ordering::Relaxed);
}

/// Whether Find should ask the host instead of browsing mDNS in Rust.
pub fn discovery_available() -> bool {
    DISCOVERY_AVAILABLE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether the host can open a camera for attachments.
static CAMERA_AVAILABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The photo the camera host wrote, waiting for the engine to read it.
static CAMERA_PHOTO: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// A launcher shortcut asked for a new session.
static NEW_SESSION_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Text another app shared into Vibex, waiting for the engine to take it.
static SHARED_TEXT: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// The name the platform reports for this phone, once the host sets it.
static DEVICE_NAME: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// Whether the app has a surface the system back key should close before it
/// leaves the app. The Android host reads it synchronously from its UI thread.
static BACK_INTERCEPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A back press the host consumed and the engine has not handled yet.
static BACK_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The screen-awake state the app wants, and the change the host has not
/// taken yet: -1 none, 0 off, 1 on.
static KEEP_SCREEN_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static KEEP_SCREEN_ON_REQUEST: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// The connection-service change the host has not taken yet: -1 none, 0 stop,
/// 1 start. The service keeps the process runnable while the app is away.
static CONNECTION_SERVICE_REQUEST: std::sync::atomic::AtomicI32 =
    std::sync::atomic::AtomicI32::new(-1);

/// Records whether the app has a sub-surface open.
pub fn set_back_intercept(intercept: bool) {
    BACK_INTERCEPT.store(intercept, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the host should consume the next back press.
#[cfg_attr(
    not(target_os = "android"),
    allow(dead_code, reason = "the Android host is the one with a back key")
)]
pub fn back_intercept() -> bool {
    BACK_INTERCEPT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Records a consumed back press for the engine to handle.
#[cfg_attr(
    not(target_os = "android"),
    allow(dead_code, reason = "the Android host is the one with a back key")
)]
pub fn request_back() {
    BACK_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Takes the pending back press, if there is one.
pub fn take_back() -> bool {
    BACK_REQUESTED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Asks the host to hold the display awake (a running turn, an open terminal).
pub fn set_keep_screen_on(on: bool) {
    KEEP_SCREEN_ON.store(on, std::sync::atomic::Ordering::Relaxed);
    KEEP_SCREEN_ON_REQUEST.store(if on { 1 } else { 0 }, std::sync::atomic::Ordering::Relaxed);
}

/// The pending screen-awake change: -1 none, 0 off, 1 on.
#[cfg_attr(
    not(target_os = "android"),
    allow(dead_code, reason = "the Android host is the one with a window")
)]
pub fn take_keep_screen_on() -> i32 {
    KEEP_SCREEN_ON_REQUEST.swap(-1, std::sync::atomic::Ordering::Relaxed)
}

/// Asks the host to keep the process runnable while the app is in the
/// background.
pub fn set_connection_service(on: bool) {
    CONNECTION_SERVICE_REQUEST.store(if on { 1 } else { 0 }, std::sync::atomic::Ordering::Relaxed);
}

/// The pending connection-service change: -1 none, 0 stop, 1 start.
#[cfg_attr(
    not(target_os = "android"),
    allow(dead_code, reason = "the Android host owns the foreground service")
)]
pub fn take_connection_service() -> i32 {
    CONNECTION_SERVICE_REQUEST.swap(-1, std::sync::atomic::Ordering::Relaxed)
}

/// Names the phone the way the platform does ("Pixel 8").
///
/// The runtime keeps this name in its paired-device list; the phone's own
/// credential identity is unchanged by it. A name the runtime would reject is
/// ignored rather than truncated.
#[cfg_attr(
    not(any(target_os = "android", target_os = "ios")),
    allow(dead_code, reason = "called by the platform entry points only")
)]
pub fn set_device_name(name: &str) {
    let Some(name) = vibex_core::normalize_remote_display_name(name) else {
        return;
    };
    *DEVICE_NAME
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(name);
}

/// Records text a system share sent into the app.
#[cfg_attr(
    not(target_os = "android"),
    allow(dead_code, reason = "the Android host receives system shares")
)]
pub fn share_text(text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    *SHARED_TEXT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(text.to_owned());
}

#[cfg_attr(
    not(target_os = "android"),
    allow(dead_code, reason = "the Android host sets this")
)]
pub fn set_camera_available(available: bool) {
    CAMERA_AVAILABLE.store(available, std::sync::atomic::Ordering::Relaxed);
}

pub fn camera_available() -> bool {
    CAMERA_AVAILABLE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Asks the host to open its camera.
pub fn request_camera() -> bool {
    if !camera_available() {
        return false;
    }
    requests()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push_back(HostRequest::Camera);
    true
}

/// Records the path of the photo the camera host wrote.
#[cfg_attr(
    not(target_os = "android"),
    allow(dead_code, reason = "the Android host sets this")
)]
pub fn set_camera_photo(path: &str) {
    if path.is_empty() {
        return;
    }
    *CAMERA_PHOTO
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(path.to_owned());
}

/// Takes the camera photo path, if one is waiting.
pub fn take_camera_photo() -> Option<String> {
    CAMERA_PHOTO
        .get()
        .and_then(|slot| slot.lock().ok().and_then(|mut slot| slot.take()))
}

/// Records a launcher shortcut asking for a new session.
#[cfg_attr(
    not(target_os = "android"),
    allow(dead_code, reason = "the Android host owns the shortcuts")
)]
pub fn request_new_session() {
    NEW_SESSION_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Takes the shortcut's request, if one is waiting.
pub fn take_new_session_request() -> bool {
    NEW_SESSION_REQUESTED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Takes the shared text, if one is waiting.
pub fn take_shared_text() -> Option<String> {
    SHARED_TEXT
        .get()
        .and_then(|slot| slot.lock().ok().and_then(|mut slot| slot.take()))
}

/// The platform's name for this phone, if the host reported one.
pub fn device_name() -> Option<String> {
    DEVICE_NAME
        .get()
        .and_then(|slot| slot.lock().ok().and_then(|slot| slot.clone()))
}

/// Something the app asks the platform host to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "an embedding host may still poll this queue; the standalone app scans and discovers in Rust"
)]
pub enum HostRequest {
    /// Open the camera and scan a pairing link.
    Scan,
    /// Start looking for runtimes on the local network.
    DiscoveryStart,
    /// Stop looking.
    DiscoveryStop,
    /// Open the camera for an attachment.
    Camera,
}

impl HostRequest {
    /// The number the platform entry points hand out.
    fn code(self) -> i32 {
        match self {
            HostRequest::Scan => 1,
            HostRequest::DiscoveryStart => 2,
            HostRequest::DiscoveryStop => 3,
            HostRequest::Camera => 4,
        }
    }
}

/// Requests the host has not taken yet. The shells poll them: a request is a
/// button press, not a stream, so polling on their existing timer is enough
/// and needs no callback object across JNI.
static REQUESTS: OnceLock<Mutex<VecDeque<HostRequest>>> = OnceLock::new();

fn requests() -> &'static Mutex<VecDeque<HostRequest>> {
    REQUESTS.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Asks the shell for the camera.
#[allow(
    dead_code,
    reason = "an embedding host may still poll this queue; the standalone app scans in Rust"
)]
pub fn request_scan() {
    requests()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push_back(HostRequest::Scan);
}

/// Asks the shell to start or stop local-network discovery.
#[allow(
    dead_code,
    reason = "an embedding host may still poll this queue; the standalone app discovers in Rust"
)]
pub fn request_discovery(start: bool) {
    requests()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push_back(if start {
            HostRequest::DiscoveryStart
        } else {
            HostRequest::DiscoveryStop
        });
}

/// The next request, if any.
#[cfg_attr(
    not(any(target_os = "android", target_os = "ios")),
    allow(dead_code, reason = "called by the platform entry points only")
)]
pub fn take_request() -> Option<HostRequest> {
    requests()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .pop_front()
}

/// The next request as the platform entry points' number (0: none).
#[cfg_attr(
    not(any(target_os = "android", target_os = "ios")),
    allow(dead_code, reason = "called by the platform entry points only")
)]
pub fn take_request_code() -> i32 {
    take_request().map_or(0, HostRequest::code)
}

/// Starts the host-event reader for `service`.
///
/// Called once from the root component. A second call is a no-op: the phone
/// runs one surface, and a second reader would split the events.
pub fn start(service: PairingService) {
    let (sender, mut receiver) = unbounded();
    let slot = SENDER.get_or_init(|| Mutex::new(None));
    let mut slot = slot.lock().unwrap_or_else(|error| error.into_inner());
    if slot.is_some() {
        return;
    }
    *slot = Some(sender);
    drop(slot);
    spawn(async move {
        while let Some(event) = receiver.next().await {
            service.apply_host_event(event);
        }
    });
}

/// Sends an event to the running app; false when it has not started.
pub fn send(event: HostEvent) -> bool {
    let Some(slot) = SENDER.get() else {
        return false;
    };
    let sender = slot
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    sender.is_some_and(|sender| sender.unbounded_send(event).is_ok())
}

/// A discovery message in the host's JSON contract.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
enum DiscoveryMessage {
    Candidate {
        #[serde(default)]
        key: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        service_instance: String,
        #[serde(default)]
        host: String,
        #[serde(default)]
        port: u16,
        #[serde(default)]
        txt: std::collections::BTreeMap<String, String>,
    },
    Removed {
        #[serde(default)]
        key: String,
    },
    PermissionDenied,
    Failed {
        message: String,
    },
}

/// Parses a discovery message from the host.
///
/// A message that does not parse becomes a failure rather than being dropped:
/// a bridge sending the wrong shape should be visible on the page.
pub fn parse_discovery(value: &str) -> HostEvent {
    match serde_json::from_str::<DiscoveryMessage>(value) {
        Ok(DiscoveryMessage::Candidate {
            key,
            name,
            service_instance,
            host,
            port,
            txt,
        }) => {
            let key = if key.is_empty() {
                service_instance.clone()
            } else {
                key
            };
            let name = if name.is_empty() {
                txt.get("display_name")
                    .cloned()
                    .unwrap_or_else(|| service_instance.clone())
            } else {
                name
            };
            let zero_config = txt.get("mode").map(String::as_str) == Some("zero_config")
                && txt.get("version").map(String::as_str) == Some("1")
                && txt.get("pairing").map(String::as_str) == Some("available")
                && txt.get("protocol_min").map(String::as_str) == Some("2")
                && txt.get("protocol_max").map(String::as_str) == Some("2");
            let origin = zero_config
                .then(|| format!("http://{host}:{port}"))
                .filter(|_| !host.is_empty() && port != 0);
            let server_id = zero_config.then(|| txt.get("server_id").cloned()).flatten();
            let server_key = zero_config
                .then(|| txt.get("server_identity_public_key").cloned())
                .flatten();
            HostEvent::DiscoveryCandidate {
                key,
                name,
                origin,
                server_id,
                server_key,
            }
        }
        Ok(DiscoveryMessage::Removed { key }) => HostEvent::DiscoveryRemoved { key },
        Ok(DiscoveryMessage::PermissionDenied) => HostEvent::DiscoveryPermissionDenied,
        Ok(DiscoveryMessage::Failed { message }) => HostEvent::DiscoveryFailed(message),
        Err(error) => HostEvent::DiscoveryFailed(format!(
            "the discovery bridge sent an invalid message: {error}"
        )),
    }
}

/// Parses a discovery message and sends it to the running app.
///
/// Only the platform modules call this; on the host target it has no caller.
#[cfg_attr(
    not(any(target_os = "android", target_os = "ios")),
    allow(dead_code, reason = "called by the platform entry points only")
)]
pub fn send_discovery_json(value: &str) -> bool {
    send(parse_discovery(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_candidate_carries_its_key_and_name() {
        assert_eq!(
            parse_discovery(r#"{"kind":"candidate","key":"desktop-1","name":"Studio"}"#),
            HostEvent::DiscoveryCandidate {
                key: "desktop-1".to_owned(),
                name: "Studio".to_owned(),
                origin: None,
                server_id: None,
                server_key: None,
            }
        );
    }

    #[test]
    fn a_removed_runtime_is_reported() {
        assert_eq!(
            parse_discovery(r#"{"kind":"removed","key":"desktop-1"}"#),
            HostEvent::DiscoveryRemoved {
                key: "desktop-1".to_owned()
            }
        );
    }

    #[test]
    fn a_platform_device_name_is_kept_when_usable() {
        set_device_name("  Pixel 8  ");
        assert_eq!(device_name().as_deref(), Some("Pixel 8"));
        set_device_name("   ");
        // A blank name does not erase the one already reported.
        assert_eq!(device_name().as_deref(), Some("Pixel 8"));
    }

    #[test]
    fn host_requests_are_taken_once_in_order() {
        while take_request_code() != 0 {}
        request_scan();
        request_discovery(true);
        request_discovery(false);
        assert_eq!(take_request_code(), 1);
        assert_eq!(take_request_code(), 2);
        assert_eq!(take_request_code(), 3);
        assert_eq!(take_request_code(), 0);
    }

    #[test]
    fn an_unknown_message_is_a_failure() {
        match parse_discovery("not json") {
            HostEvent::DiscoveryFailed(message) => assert!(message.contains("invalid message")),
            other => panic!("expected a failure, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod shared_text_tests {
    use super::*;

    #[test]
    fn a_shortcut_request_is_taken_once() {
        request_new_session();
        assert!(take_new_session_request());
        assert!(!take_new_session_request());
    }

    #[test]
    fn shared_text_is_trimmed_and_taken_once() {
        share_text("  hello  ");
        assert_eq!(take_shared_text().as_deref(), Some("hello"));
        assert_eq!(take_shared_text(), None);
    }
}

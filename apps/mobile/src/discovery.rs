use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

use futures_channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use serde::Deserialize;
use vibex_backend::{BackendError, BackendResult};
use vibex_remote_client::normalize_lan_https_origin;

const MAX_SERVICE_INSTANCE_BYTES: usize = 63;
const MAX_DISPLAY_NAME_BYTES: usize = 192;
const MAX_ADVERTISEMENT_ID_BYTES: usize = 128;
const MAX_TXT_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanDiscoveryCandidate {
    pub advertisement_id: String,
    pub service_instance: String,
    pub display_name: String,
    pub origin: String,
    pub interface_scope: String,
}

impl LanDiscoveryCandidate {
    pub fn key(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.advertisement_id, self.service_instance, self.origin, self.interface_scope
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanDiscoveryEvent {
    Candidate(LanDiscoveryCandidate),
    Removed { service_instance: String },
    PermissionDenied,
    Failed(BackendError),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NativeDiscoveryEvent {
    kind: String,
    #[serde(default)]
    service_instance: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    port: u16,
    #[serde(default)]
    interface_scope: String,
    #[serde(default)]
    txt: BTreeMap<String, String>,
}

fn event_sender() -> &'static Mutex<Option<UnboundedSender<LanDiscoveryEvent>>> {
    static SENDER: OnceLock<Mutex<Option<UnboundedSender<LanDiscoveryEvent>>>> = OnceLock::new();
    SENDER.get_or_init(|| Mutex::new(None))
}

fn enqueue(event: LanDiscoveryEvent) {
    if let Ok(sender) = event_sender().lock()
        && let Some(sender) = sender.as_ref()
    {
        let _ = sender.unbounded_send(event);
    }
}

fn enqueue_native_json(value: &str) {
    let event = serde_json::from_str::<NativeDiscoveryEvent>(value)
        .map_err(|_| invalid_discovery())
        .and_then(parse_native_event)
        .unwrap_or_else(LanDiscoveryEvent::Failed);
    enqueue(event);
}

pub fn subscribe() -> UnboundedReceiver<LanDiscoveryEvent> {
    let (sender, receiver) = mpsc::unbounded();
    if let Ok(mut current) = event_sender().lock() {
        *current = Some(sender);
    }
    receiver
}

fn parse_native_event(event: NativeDiscoveryEvent) -> BackendResult<LanDiscoveryEvent> {
    match event.kind.as_str() {
        "candidate" => parse_candidate(event).map(LanDiscoveryEvent::Candidate),
        "removed" => {
            validate_bounded_text(&event.service_instance, 1, MAX_SERVICE_INSTANCE_BYTES)?;
            Ok(LanDiscoveryEvent::Removed {
                service_instance: event.service_instance,
            })
        }
        "permission_denied" => Ok(LanDiscoveryEvent::PermissionDenied),
        "failed" => Ok(LanDiscoveryEvent::Failed(BackendError::offline(
            "mobile_lan_discovery_failed",
            "nearby Vibex devices could not be discovered",
        ))),
        _ => Err(invalid_discovery()),
    }
}

fn parse_candidate(event: NativeDiscoveryEvent) -> BackendResult<LanDiscoveryCandidate> {
    validate_bounded_text(&event.service_instance, 1, MAX_SERVICE_INSTANCE_BYTES)?;
    if event.port == 0 || event.host.is_empty() || event.host.len() > 253 {
        return Err(invalid_discovery());
    }
    let expected_keys = BTreeSet::from([
        "advertisement_id",
        "display_name",
        "pairing",
        "protocol_max",
        "protocol_min",
        "version",
    ]);
    if event
        .txt
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != expected_keys
    {
        return Err(invalid_discovery());
    }
    let txt_bytes = event
        .txt
        .iter()
        .map(|(key, value)| key.len() + value.len() + 2)
        .sum::<usize>();
    if txt_bytes > MAX_TXT_BYTES
        || event.txt.get("version").map(String::as_str) != Some("1")
        || event.txt.get("pairing").map(String::as_str) != Some("available")
        || event.txt.get("protocol_min").map(String::as_str) != Some("2")
        || event.txt.get("protocol_max").map(String::as_str) != Some("2")
    {
        return Err(invalid_discovery());
    }
    let advertisement_id = event.txt["advertisement_id"].clone();
    let display_name = event.txt["display_name"].clone();
    validate_bounded_text(&advertisement_id, 16, MAX_ADVERTISEMENT_ID_BYTES)?;
    validate_bounded_text(&display_name, 1, MAX_DISPLAY_NAME_BYTES)?;
    if event.interface_scope.len() > 128 || event.interface_scope.chars().any(char::is_control) {
        return Err(invalid_discovery());
    }
    let host = event.host.trim_end_matches('.');
    let authority = if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]:{}", event.port)
    } else {
        format!("{host}:{}", event.port)
    };
    let origin = normalize_lan_https_origin(&format!("https://{authority}"))?;
    Ok(LanDiscoveryCandidate {
        advertisement_id,
        service_instance: event.service_instance,
        display_name,
        origin,
        interface_scope: event.interface_scope,
    })
}

fn validate_bounded_text(value: &str, min: usize, max: usize) -> BackendResult<()> {
    if value.len() < min || value.len() > max || value.chars().any(char::is_control) {
        return Err(invalid_discovery());
    }
    Ok(())
}

fn invalid_discovery() -> BackendError {
    BackendError::failed(
        "remote_lan_discovery_invalid",
        "nearby device advertisement is invalid or exceeds its bounds",
    )
}

#[cfg(target_os = "android")]
mod android {
    use std::sync::{Mutex, OnceLock};

    use gpui_android::AndroidApp;
    use jni::{
        EnvUnowned, JavaVM,
        objects::{JClass, JObject, JString},
        refs::Global,
    };
    use vibex_backend::{BackendError, BackendResult};

    use super::enqueue_native_json;

    fn android_app() -> &'static Mutex<Option<AndroidApp>> {
        static APP: OnceLock<Mutex<Option<AndroidApp>>> = OnceLock::new();
        APP.get_or_init(|| Mutex::new(None))
    }

    pub fn initialize(app: &AndroidApp) {
        if let Ok(mut current) = android_app().lock() {
            *current = Some(app.clone());
        }
    }

    fn call_activity(method: &'static str) -> BackendResult<()> {
        let app = android_app()
            .lock()
            .ok()
            .and_then(|app| app.clone())
            .ok_or_else(unavailable)?;
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
        vm.attach_current_thread(|env| -> jni::errors::Result<()> {
            let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
            let activity = unsafe { env.as_cast_raw::<Global<JObject>>(&raw_activity)? };
            let result = env.call_method(activity, method, jni::jni_sig!(() -> ()), &[]);
            if result.is_err() {
                let _ = env.exception_clear();
            }
            result.map(|_| ())
        })
        .map_err(|_| unavailable())
    }

    pub fn start() -> BackendResult<()> {
        call_activity("startLanPairingDiscovery")
    }

    pub fn stop() {
        let _ = call_activity("stopLanPairingDiscovery");
    }

    fn unavailable() -> BackendError {
        BackendError::unsupported(
            "mobile_lan_discovery_unavailable",
            "nearby device discovery is unavailable on this device",
        )
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_ai_vibex_mobile_GpuiNativeActivity_nativeOnLanDiscoveryEvent<
        'caller,
    >(
        mut unowned_env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        value: JString<'caller>,
    ) {
        unowned_env
            .with_env(|_| -> jni::errors::Result<()> {
                enqueue_native_json(&value.to_string());
                Ok(())
            })
            .resolve::<jni::errors::LogErrorAndDefault>()
    }
}

#[cfg(target_os = "android")]
pub use android::{initialize as initialize_android, start, stop};

#[cfg(target_os = "ios")]
unsafe extern "C" {
    fn vibex_ios_start_lan_discovery();
    fn vibex_ios_stop_lan_discovery();
}

#[cfg(target_os = "ios")]
pub fn start() -> BackendResult<()> {
    unsafe { vibex_ios_start_lan_discovery() };
    Ok(())
}

#[cfg(target_os = "ios")]
pub fn stop() {
    unsafe { vibex_ios_stop_lan_discovery() };
}

#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_lan_discovery_event(value: *const std::ffi::c_char) {
    if value.is_null() {
        return;
    }
    let value = unsafe { std::ffi::CStr::from_ptr(value) };
    if let Ok(value) = value.to_str() {
        enqueue_native_json(value);
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn start() -> BackendResult<()> {
    Err(BackendError::unsupported(
        "mobile_lan_discovery_unavailable",
        "nearby device discovery is unavailable on this platform",
    ))
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn stop() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate_json(extra: &str) -> String {
        format!(
            r#"{{"kind":"candidate","serviceInstance":"Vibex-1234","host":"desktop.example","port":443,"interfaceScope":"wifi","txt":{{"version":"1","advertisement_id":"adv-0123456789012345","display_name":"Desktop","protocol_min":"2","protocol_max":"2","pairing":"available"{extra}}}}}"#
        )
    }

    #[test]
    fn valid_candidate_preserves_advertisement_identity() {
        let native: NativeDiscoveryEvent = serde_json::from_str(&candidate_json("")).unwrap();
        let LanDiscoveryEvent::Candidate(candidate) = parse_native_event(native).unwrap() else {
            panic!("expected candidate");
        };
        assert_eq!(candidate.origin, "https://desktop.example");
        assert!(candidate.key().contains("adv-0123456789012345"));
    }

    #[test]
    fn malicious_or_unknown_txt_is_rejected() {
        let mut unknown: NativeDiscoveryEvent = serde_json::from_str(&candidate_json("")).unwrap();
        unknown.txt.insert("offer_id".into(), "secret".into());
        assert_eq!(
            parse_native_event(unknown).unwrap_err().code,
            "remote_lan_discovery_invalid"
        );

        let mut oversized: NativeDiscoveryEvent =
            serde_json::from_str(&candidate_json("")).unwrap();
        oversized.txt.insert("display_name".into(), "x".repeat(500));
        assert_eq!(
            parse_native_event(oversized).unwrap_err().code,
            "remote_lan_discovery_invalid"
        );
    }

    #[test]
    fn candidates_are_not_merged_by_display_name() {
        let mut first =
            match parse_native_event(serde_json::from_str(&candidate_json("")).unwrap()).unwrap() {
                LanDiscoveryEvent::Candidate(candidate) => candidate,
                _ => panic!("expected candidate"),
            };
        let second = LanDiscoveryCandidate {
            advertisement_id: "adv-9999999999999999".into(),
            ..first.clone()
        };
        assert_ne!(first.key(), second.key());
        first.interface_scope = "cellular".into();
        assert_ne!(first.key(), second.key());
    }
}

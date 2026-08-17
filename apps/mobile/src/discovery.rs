use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

use base64::Engine as _;
use futures_channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use serde::Deserialize;
use vibex_backend::{BackendError, BackendResult};
use vibex_remote_client::{normalize_lan_https_origin, normalize_zero_config_lan_origin};

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
    pub mode: LanDiscoveryMode,
    pub server_id: Option<String>,
    pub server_identity_public_key: Option<String>,
    pub interface_scope: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanDiscoveryMode {
    DirectHttps,
    ZeroConfig,
}

impl LanDiscoveryCandidate {
    pub fn key(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{:?}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.advertisement_id,
            self.service_instance,
            self.origin,
            self.mode,
            self.server_id.as_deref().unwrap_or_default(),
            self.server_identity_public_key
                .as_deref()
                .unwrap_or_default(),
            self.interface_scope
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
    if let Some(event) = decode_native_json(value) {
        enqueue(event);
    }
}

fn decode_native_json(value: &str) -> Option<LanDiscoveryEvent> {
    let native = match serde_json::from_str::<NativeDiscoveryEvent>(value) {
        Ok(native) => native,
        Err(_) => return Some(LanDiscoveryEvent::Failed(invalid_discovery())),
    };
    let candidate_local = matches!(native.kind.as_str(), "candidate" | "removed");
    match parse_native_event(native) {
        Ok(event) => Some(event),
        Err(error) if candidate_local && error.code == "remote_lan_discovery_invalid" => None,
        Err(error) => Some(LanDiscoveryEvent::Failed(error)),
    }
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
    let txt = normalize_txt(event.txt)?;
    let base_keys = BTreeSet::from([
        "advertisement_id",
        "display_name",
        "pairing",
        "protocol_max",
        "protocol_min",
        "version",
    ]);
    let actual_keys = txt.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if !actual_keys.is_superset(&base_keys) {
        return Err(invalid_discovery());
    }
    if let Some(txtvers) = txt.get("txtvers")
        && txtvers != "1"
    {
        return Err(invalid_discovery());
    }
    let txt_bytes = txt
        .iter()
        .map(|(key, value)| key.len() + value.len() + 2)
        .sum::<usize>();
    if txt_bytes > MAX_TXT_BYTES
        || txt.get("version").map(String::as_str) != Some("1")
        || txt.get("pairing").map(String::as_str) != Some("available")
        || txt.get("protocol_min").map(String::as_str) != Some("2")
        || txt.get("protocol_max").map(String::as_str) != Some("2")
    {
        return Err(invalid_discovery());
    }
    let mode = match txt.get("mode").map(String::as_str) {
        Some("direct") | None
            if !txt.contains_key("server_id")
                && !txt.contains_key("server_identity_public_key") =>
        {
            LanDiscoveryMode::DirectHttps
        }
        Some("zero_config") | None => LanDiscoveryMode::ZeroConfig,
        Some(_) => return Err(invalid_discovery()),
    };
    let mut expected_keys = match mode {
        LanDiscoveryMode::DirectHttps => base_keys.clone(),
        LanDiscoveryMode::ZeroConfig => BTreeSet::from([
            "advertisement_id",
            "display_name",
            "mode",
            "pairing",
            "protocol_max",
            "protocol_min",
            "server_id",
            "server_identity_public_key",
            "version",
        ]),
    };
    if mode == LanDiscoveryMode::ZeroConfig && !txt.contains_key("mode") {
        expected_keys.remove("mode");
    }
    let comparable_keys = actual_keys
        .iter()
        .copied()
        .filter(|key| *key != "txtvers")
        .collect::<BTreeSet<_>>();
    if comparable_keys != expected_keys {
        return Err(invalid_discovery());
    }
    let advertisement_id = txt["advertisement_id"].clone();
    let display_name = txt["display_name"].clone();
    validate_bounded_text(&advertisement_id, 16, MAX_ADVERTISEMENT_ID_BYTES)?;
    validate_bounded_text(&display_name, 1, MAX_DISPLAY_NAME_BYTES)?;
    if event.interface_scope.len() > 128 || event.interface_scope.chars().any(char::is_control) {
        return Err(invalid_discovery());
    }
    let origin = match mode {
        LanDiscoveryMode::DirectHttps => {
            let authority = host_authority(&event.host, event.port)?;
            normalize_lan_https_origin(&format!("https://{authority}"))?
        }
        LanDiscoveryMode::ZeroConfig => {
            let authority = ipv4_host_authority(&event.host, event.port)?;
            normalize_zero_config_lan_origin(&format!("http://{authority}"))
                .map_err(|_| invalid_discovery())?
        }
    };
    let (server_id, server_identity_public_key) = match mode {
        LanDiscoveryMode::DirectHttps => (None, None),
        LanDiscoveryMode::ZeroConfig => {
            let server_id = txt["server_id"].clone();
            validate_bounded_text(&server_id, 1, 128)?;
            let key = txt["server_identity_public_key"].clone();
            let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&key)
                .map_err(|_| invalid_discovery())?;
            if decoded.len() != 32 || decoded.iter().all(|byte| *byte == 0) {
                return Err(invalid_discovery());
            }
            (Some(server_id), Some(key))
        }
    };
    Ok(LanDiscoveryCandidate {
        advertisement_id,
        service_instance: event.service_instance,
        display_name,
        origin,
        mode,
        server_id,
        server_identity_public_key,
        interface_scope: event.interface_scope,
    })
}

fn normalize_txt(raw: BTreeMap<String, String>) -> BackendResult<BTreeMap<String, String>> {
    let mut normalized = BTreeMap::new();
    for (key, value) in raw {
        if key.is_empty()
            || !key.is_ascii()
            || key
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(invalid_discovery());
        }
        let key = key.to_ascii_lowercase();
        if normalized.insert(key, value).is_some() {
            return Err(invalid_discovery());
        }
    }
    Ok(normalized)
}

fn host_authority(host: &str, port: u16) -> BackendResult<String> {
    let host = host.trim_end_matches('.');
    if host.is_empty() || host.chars().any(char::is_control) {
        return Err(invalid_discovery());
    }
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(address) = host.parse::<std::net::Ipv6Addr>() {
        return Ok(format!("[{address}]:{port}"));
    }
    if let Some((address, zone)) = host.split_once('%')
        && address.parse::<std::net::Ipv6Addr>().is_ok()
        && !zone.is_empty()
        && zone
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
    {
        // `url::Url` intentionally rejects scoped IPv6 literals. Android can
        // return one when the resolver exposes an address instead of the SRV
        // hostname, so keep the validated address and omit only the interface
        // label from the origin representation.
        return Ok(format!("[{address}]:{port}"));
    }
    if host.contains(':') || host.contains('/') || host.contains('[') || host.contains(']') {
        return Err(invalid_discovery());
    }
    Ok(format!("{host}:{port}"))
}

fn ipv4_host_authority(host: &str, port: u16) -> BackendResult<String> {
    let address = host
        .trim_end_matches('.')
        .parse::<std::net::Ipv4Addr>()
        .map_err(|_| invalid_discovery())?;
    if !(address.is_loopback() || address.is_private() || address.is_link_local()) {
        return Err(invalid_discovery());
    }
    Ok(format!("{address}:{port}"))
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
            let result = env.call_method(
                activity,
                jni::strings::JNIString::new(method),
                jni::jni_sig!(() -> ()),
                &[],
            );
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
        assert_eq!(candidate.mode, LanDiscoveryMode::DirectHttps);
        assert!(candidate.key().contains("adv-0123456789012345"));
    }

    #[test]
    fn native_txt_metadata_is_case_insensitive_and_txtvers_is_optional() {
        let mut native: NativeDiscoveryEvent = serde_json::from_str(&candidate_json("")).unwrap();
        native.txt = native
            .txt
            .into_iter()
            .map(|(key, value)| (key.to_ascii_uppercase(), value))
            .collect();
        native.txt.insert("txtvers".into(), "1".into());

        let LanDiscoveryEvent::Candidate(candidate) = parse_native_event(native).unwrap() else {
            panic!("expected candidate");
        };
        assert_eq!(candidate.mode, LanDiscoveryMode::DirectHttps);
    }

    #[test]
    fn zero_config_candidate_requires_and_preserves_desktop_identity() {
        let public_key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7u8; 32]);
        let mut native: NativeDiscoveryEvent = serde_json::from_str(&candidate_json(&format!(
            r#","mode":"zero_config","server_id":"desktop-test","server_identity_public_key":"{public_key}""#
        )))
        .unwrap();
        native.host = "192.168.1.10".into();
        let LanDiscoveryEvent::Candidate(candidate) = parse_native_event(native).unwrap() else {
            panic!("expected candidate");
        };

        assert_eq!(candidate.mode, LanDiscoveryMode::ZeroConfig);
        assert_eq!(candidate.origin, "http://192.168.1.10:443");
        assert_eq!(candidate.server_id.as_deref(), Some("desktop-test"));
        assert_eq!(
            candidate.server_identity_public_key.as_deref(),
            Some(public_key.as_str())
        );
    }

    #[test]
    fn zero_config_candidate_accepts_a_native_event_without_mode() {
        let public_key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7u8; 32]);
        let mut native: NativeDiscoveryEvent = serde_json::from_str(&candidate_json(&format!(
            r#","mode":"zero_config","server_id":"desktop-test","server_identity_public_key":"{public_key}""#
        )))
        .unwrap();
        native.txt.remove("mode");
        native.host = "192.168.1.10".into();

        let LanDiscoveryEvent::Candidate(candidate) = parse_native_event(native).unwrap() else {
            panic!("expected candidate");
        };
        assert_eq!(candidate.mode, LanDiscoveryMode::ZeroConfig);
        assert_eq!(candidate.server_id.as_deref(), Some("desktop-test"));
    }

    #[test]
    fn scoped_ipv6_host_is_encoded_as_a_valid_origin() {
        let mut native: NativeDiscoveryEvent = serde_json::from_str(&candidate_json("")).unwrap();
        native.host = "fe80::1%wlan0".into();
        let LanDiscoveryEvent::Candidate(candidate) = parse_native_event(native).unwrap() else {
            panic!("expected candidate");
        };
        assert_eq!(candidate.origin, "https://[fe80::1]");
    }

    #[test]
    fn zero_config_ipv6_candidate_is_discarded_before_user_selection() {
        let public_key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7u8; 32]);
        let mut encoded: serde_json::Value = serde_json::from_str(&candidate_json(&format!(
            r#","mode":"zero_config","server_id":"desktop-test","server_identity_public_key":"{public_key}""#
        )))
        .unwrap();
        encoded["host"] = serde_json::Value::String("fe80::1%wlan0".into());

        assert!(decode_native_json(&serde_json::to_string(&encoded).unwrap()).is_none());
    }

    #[test]
    fn zero_config_candidate_rejects_invalid_identity_key() {
        let mut native: NativeDiscoveryEvent = serde_json::from_str(&candidate_json(
            r#","mode":"zero_config","server_id":"desktop-test","server_identity_public_key":"invalid""#,
        ))
        .unwrap();
        native.host = "192.168.1.10".into();

        assert_eq!(
            parse_native_event(native).unwrap_err().code,
            "remote_lan_discovery_invalid"
        );
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
    fn invalid_candidates_do_not_terminate_the_discovery_browser() {
        let invalid = candidate_json(r#","offer_id":"secret""#);
        assert!(decode_native_json(&invalid).is_none());

        let malformed = decode_native_json("not-json").unwrap();
        let LanDiscoveryEvent::Failed(error) = malformed else {
            panic!("expected bridge failure");
        };
        assert_eq!(error.code, "remote_lan_discovery_invalid");
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

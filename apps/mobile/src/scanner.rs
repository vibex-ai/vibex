use std::sync::{Mutex, OnceLock};

use futures_channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use vibex_backend::{BackendError, BackendResult};

fn result_sender() -> &'static Mutex<Option<UnboundedSender<String>>> {
    static SENDER: OnceLock<Mutex<Option<UnboundedSender<String>>>> = OnceLock::new();
    SENDER.get_or_init(|| Mutex::new(None))
}

fn enqueue_pairing_link(link: String) {
    if let Ok(sender) = result_sender().lock()
        && let Some(sender) = sender.as_ref()
    {
        let _ = sender.unbounded_send(link);
    }
}

pub fn subscribe() -> UnboundedReceiver<String> {
    let (sender, receiver) = mpsc::unbounded();
    if let Ok(mut current) = result_sender().lock() {
        *current = Some(sender);
    }
    receiver
}

fn unavailable() -> BackendError {
    BackendError::unsupported(
        "mobile_pairing_scanner_unavailable",
        "QR scanning is unavailable on this device",
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

    use super::{BackendError, BackendResult, enqueue_pairing_link, unavailable};

    fn android_app() -> &'static Mutex<Option<AndroidApp>> {
        static APP: OnceLock<Mutex<Option<AndroidApp>>> = OnceLock::new();
        APP.get_or_init(|| Mutex::new(None))
    }

    pub fn initialize(app: &AndroidApp) {
        if let Ok(mut current) = android_app().lock() {
            *current = Some(app.clone());
        }
    }

    pub fn launch() -> BackendResult<()> {
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
                jni::jni_str!("launchPairingQrScanner"),
                jni::jni_sig!(() -> ()),
                &[],
            );
            if result.is_err() {
                let _ = env.exception_clear();
            }
            result.map(|_| ())
        })
        .map_err(|_| {
            BackendError::failed(
                "mobile_pairing_scanner_launch_failed",
                "The QR scanner could not be opened",
            )
        })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_ai_vibex_mobile_PairingQrScannerActivity_nativeOnPairingQrScanned<
        'caller,
    >(
        mut unowned_env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        value: JString<'caller>,
    ) {
        unowned_env
            .with_env(|_| -> jni::errors::Result<()> {
                enqueue_pairing_link(value.to_string());
                Ok(())
            })
            .resolve::<jni::errors::LogErrorAndDefault>()
    }
}

#[cfg(target_os = "android")]
pub use android::initialize as initialize_android;

#[cfg(target_os = "android")]
pub use android::launch;

#[cfg(target_os = "ios")]
unsafe extern "C" {
    fn vibex_ios_present_pairing_scanner();
}

#[cfg(target_os = "ios")]
pub fn launch() -> BackendResult<()> {
    unsafe { vibex_ios_present_pairing_scanner() };
    Ok(())
}

#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_pairing_qr_scanned(value: *const std::ffi::c_char) {
    if value.is_null() {
        return;
    }
    let value = unsafe { std::ffi::CStr::from_ptr(value) };
    if let Ok(value) = value.to_str() {
        enqueue_pairing_link(value.to_string());
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn launch() -> BackendResult<()> {
    Err(unavailable())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_scan_result_is_consumed_once() {
        let mut receiver = subscribe();
        enqueue_pairing_link("vibex://open/direct#/pair/test".to_string());

        assert_eq!(
            receiver.try_recv().unwrap(),
            "vibex://open/direct#/pair/test"
        );
        assert!(receiver.try_recv().is_err());
    }
}

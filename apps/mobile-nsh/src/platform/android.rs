//! The Kotlin host's entry point.
//!
//! The host adds a `com.longbridge.nsh.NshView` (a `FrameLayout`) to its own
//! layout and passes that view's native handle here; the Rust root mounts into
//! it. The handle is the package-private `NshView.handle` field the nsh AAR
//! documents for custom Rust hosts, read by the Kotlin shell and handed over
//! as a `jlong`.
//!
//! The export keeps the JNI ABI with plain C types (a `JNIEnv*`, a `jclass`
//! and a `jlong`) so this crate does not have to match the Kotlin side's `jni`
//! crate version.

use std::ffi::c_void;

use nsh_android::run_root;

/// Mounts the app into the `NshView` whose native handle is `handle`.
///
/// # Safety
///
/// `handle` must be the live handle of an `NshView` on the main thread, and
/// the view must stay alive until the app stops.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_nsh_NshHost_nativeRunRoot(
    _env: *mut c_void,
    _class: *mut c_void,
    handle: i64,
) {
    run_root(handle, crate::android_root());
}

/// Records that the host runs local-network discovery itself.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_nsh_NshHost_nativeSetDiscoveryAvailable(
    _env: *mut c_void,
    _class: *mut c_void,
    available: jni::sys::jboolean,
) {
    crate::host::set_discovery_available(available);
}

/// Records that the host can scan QR codes with its camera.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_nsh_NshHost_nativeSetScannerAvailable(
    _env: *mut c_void,
    _class: *mut c_void,
    available: jni::sys::jboolean,
) {
    crate::host::set_scanner_available(available);
}

use jni::{
    EnvUnowned,
    objects::{JClass, JString},
};

use crate::host::{self, HostEvent};

/// Reports the pairing link the host's camera read.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_nsh_NshHost_nativeScanResult(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    value: JString<'_>,
) {
    env.with_env(|_| -> jni::errors::Result<()> {
        host::send(HostEvent::ScanResult(value.to_string()));
        Ok(())
    })
    .resolve::<jni::errors::LogErrorAndDefault>()
}

/// Records that the host can open a camera for attachments.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_nsh_NshHost_nativeSetCameraAvailable(
    _env: *mut c_void,
    _class: *mut c_void,
    available: jni::sys::jboolean,
) {
    crate::host::set_camera_available(available);
}

/// Reports the path of a photo the host's camera wrote.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_nsh_NshHost_nativeCameraPhoto(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    value: JString<'_>,
) {
    env.with_env(|_| -> jni::errors::Result<()> {
        crate::host::set_camera_photo(&value.to_string());
        Ok(())
    })
    .resolve::<jni::errors::LogErrorAndDefault>()
}

/// The pending update APK URL, or null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_nsh_NshHost_nativeTakeUpdateUrl(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
) -> jni::sys::jstring {
    let Some(url) = crate::host::take_update_url() else {
        return std::ptr::null_mut();
    };
    let mut out = std::ptr::null_mut();
    env.with_env(|env| -> jni::errors::Result<()> {
        out = env.new_string(url)?.into_raw();
        Ok(())
    })
    .resolve::<jni::errors::LogErrorAndDefault>();
    out
}

/// Records a launcher shortcut asking for a new session.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_nsh_NshHost_nativeNewSession(
    _env: *mut c_void,
    _class: *mut c_void,
) {
    host::request_new_session();
}

/// Reports text another app shared into Vibex.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_nsh_NshHost_nativeShareText(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    value: JString<'_>,
) {
    env.with_env(|_| -> jni::errors::Result<()> {
        host::share_text(&value.to_string());
        Ok(())
    })
    .resolve::<jni::errors::LogErrorAndDefault>()
}

/// Reports one local-network discovery event as the bridge's JSON contract.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_nsh_NshHost_nativeDiscoveryEvent(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    value: JString<'_>,
) {
    env.with_env(|_| -> jni::errors::Result<()> {
        host::send_discovery_json(&value.to_string());
        Ok(())
    })
    .resolve::<jni::errors::LogErrorAndDefault>()
}

/// The next thing the app asks the host to do: 0 none, 1 scan, 2 start
/// discovery, 3 stop discovery. Poll it from the host's UI loop.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_nsh_NshHost_nativeTakeHostRequest(
    _env: *mut c_void,
    _class: *mut c_void,
) -> i32 {
    crate::host::take_request_code()
}

/// Names the phone the way Android reports it (`android.os.Build.MODEL`).
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_nsh_NshHost_nativeSetDeviceName(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    value: JString<'_>,
) {
    env.with_env(|_| -> jni::errors::Result<()> {
        crate::host::set_device_name(&value.to_string());
        Ok(())
    })
    .resolve::<jni::errors::LogErrorAndDefault>()
}

/// The Android back key: consumes it when a surface of the app can close.
///
/// The host calls this before `NshView.handleBack`, so a drawer, a file
/// preview or the terminal closes before the activity does. The engine
/// handles the press on its own thread through the app's poller.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_nsh_NshHost_nativeBack(
    _env: *mut c_void,
    _class: *mut c_void,
) -> jni::sys::jboolean {
    if crate::host::back_intercept() {
        crate::host::request_back();
        true
    } else {
        false
    }
}

/// The pending screen-awake change: -1 none, 0 off, 1 on. The host applies it
/// to its window flag on its own UI loop.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_nsh_NshHost_nativeTakeKeepScreenOn(
    _env: *mut c_void,
    _class: *mut c_void,
) -> i32 {
    crate::host::take_keep_screen_on()
}

/// The pending connection-service change: -1 none, 0 stop, 1 start.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_nsh_NshHost_nativeTakeConnectionService(
    _env: *mut c_void,
    _class: *mut c_void,
) -> i32 {
    crate::host::take_connection_service()
}

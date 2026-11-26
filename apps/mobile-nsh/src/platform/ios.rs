//! The Swift host's entry point.
//!
//! The Swift shell (`ios/`) creates a container `UIView` in its own view
//! hierarchy and hands it here; the app mounts into it, sized to the
//! container, and the returned pointer stops it when the shell goes away.
//!
//! The rest of the surface mirrors the Android host's (`android/`): the shell
//! owns the camera and Bonjour, so it reports what it saw into
//! [`crate::host`] and polls a small request queue for what the app asks it to
//! do. The C ABI is declared for Swift in `ios/Headers/VibexMobile.h`, which
//! is the app's only contract with the shell.

use std::ffi::{CStr, c_char, c_void};

use nsh_platform_ios::{AppView, SurfaceOptions};
use objc2::MainThreadMarker;
use objc2_ui_kit::UIView;

use crate::host::{self, HostEvent};

/// Mounts the app into `host`, returning the view that owns it.
///
/// The pointer must be passed back to [`vibex_mobile_nsh_detach`] once, when
/// the shell is done with it.
///
/// # Safety
///
/// `host` must be a live `UIView` on the main thread that stays alive while
/// the app runs.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_attach(host: *mut c_void) -> *mut c_void {
    if host.is_null() {
        return std::ptr::null_mut();
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return std::ptr::null_mut();
    };
    // `log` lines from Rust and nsh go to the unified log, where Console and
    // `log stream` show them under the host app's bundle identifier.
    let mut logging = nsh_platform_apple::unified_log::Options::default();
    // Lines on a terminal's stderr share it with the host app's own.
    logging.stderr_prefix = "[vibex] ";
    nsh_platform_apple::unified_log::install(logging);
    // SAFETY: the caller passes a live UIView on the main thread.
    let container = unsafe { &*(host as *const UIView) };
    let app = AppView::new(mtm, container.bounds(), SurfaceOptions::default());
    // A standalone app runs without a sandbox's VM limits, as the Android host
    // asks NshView to do. Set before `run`, which loads the app.
    app.set_limits(nsh_core::js::Limits::standalone());
    container.addSubview(app.view());
    app.run(crate::phone_root());
    Box::into_raw(Box::new(app)) as *mut c_void
}

/// Stops the app mounted by [`vibex_mobile_nsh_attach`] and drops its view.
///
/// # Safety
///
/// `app` must be a pointer returned by [`vibex_mobile_nsh_attach`], passed once.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_detach(app: *mut c_void) {
    if app.is_null() {
        return;
    }
    // SAFETY: `app` came from `Box::into_raw` in `vibex_mobile_nsh_attach`.
    let app = unsafe { Box::from_raw(app as *mut AppView) };
    app.view().removeFromSuperview();
    drop(app);
}

/// Makes NSH's notification center the app's delegate.
///
/// The shell calls this before the app finishes launching, so a notification
/// press that started the process is kept until the app listens for it. Left
/// to `AppView::run`, a press from a cold start would be dropped.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_install_notifications() {
    nsh_platform_apple::notifications::install();
}

/// Names the phone the way iOS reports it ("Ada's iPhone"), so the runtime's
/// paired-device list is readable.
///
/// # Safety
///
/// `value` must be a valid NUL-terminated C string, or null.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_set_device_name(value: *const c_char) {
    let Some(value) = c_string(value) else {
        return;
    };
    host::set_device_name(&value);
}

/// Tells the app that Scan can use the shell's camera.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_set_scanner_available(available: i32) {
    host::set_scanner_available(available != 0);
}

/// Tells the app that Find can use the shell's Bonjour browser.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_set_discovery_available(available: i32) {
    host::set_discovery_available(available != 0);
}

/// Tells the app that the composer can open the shell's camera.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_set_camera_available(available: i32) {
    host::set_camera_available(available != 0);
}

/// Reports the pairing link the shell's camera read.
///
/// # Safety
///
/// `value` must be a valid NUL-terminated C string, or null.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_scan_result(value: *const c_char) {
    let Some(value) = c_string(value) else {
        return;
    };
    host::send(HostEvent::ScanResult(value));
}

/// Reports one local-network discovery event as the bridge's JSON contract.
///
/// # Safety
///
/// `value` must be a valid NUL-terminated C string, or null.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_discovery_event(value: *const c_char) {
    let Some(value) = c_string(value) else {
        return;
    };
    host::send_discovery_json(&value);
}

/// Reports the path of a photo the shell's camera wrote.
///
/// # Safety
///
/// `value` must be a valid NUL-terminated C string, or null.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_camera_photo(value: *const c_char) {
    let Some(value) = c_string(value) else {
        return;
    };
    host::set_camera_photo(&value);
}

/// Reports that the shell could not take a photo, and why.
///
/// # Safety
///
/// `value` must be a valid NUL-terminated C string, or null.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_camera_failed(value: *const c_char) {
    let Some(value) = c_string(value) else {
        return;
    };
    host::set_camera_failure(&value);
}

/// Reports text another app shared into Vibex (the share extension or a URL
/// scheme open).
///
/// # Safety
///
/// `value` must be a valid NUL-terminated C string, or null.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_share_text(value: *const c_char) {
    let Some(value) = c_string(value) else {
        return;
    };
    host::share_text(&value);
}

/// Reports the Home Screen's "New session" quick action.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_new_session() {
    host::request_new_session();
}

/// The next thing the app asks the shell to do: 0 none, 1 scan, 2 start
/// discovery, 3 stop discovery, 4 camera, 5 app settings. Poll it from the
/// shell's own timer.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_take_host_request() -> i32 {
    host::take_request_code()
}

/// The pending screen-awake change: -1 none, 0 off, 1 on.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_take_keep_screen_on() -> i32 {
    host::take_keep_screen_on()
}

/// The pending "stay connected in the background" change: -1 none, 0 stop,
/// 1 start. iOS has no foreground service, so the shell answers it with a
/// background task (and keeps the connection while the system allows).
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_take_connection_service() -> i32 {
    host::take_connection_service()
}

/// Reads a NUL-terminated UTF-8 string, if the pointer is valid.
fn c_string(value: *const c_char) -> Option<String> {
    if value.is_null() {
        return None;
    }
    // SAFETY: the caller passes a NUL-terminated string.
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

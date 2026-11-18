//! The Swift host's entry point.
//!
//! The Swift shell creates a container `UIView` in its own view hierarchy and
//! hands it here; the app mounts into it, sized to the container, and the
//! returned pointer stops it when the shell goes away. The Swift side is
//! documented in the app's README.

use std::ffi::c_void;

use nsh_platform_ios::{AppView, Root, SurfaceOptions};
use objc2::MainThreadMarker;
use objc2_ui_kit::UIView;

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
    // SAFETY: the caller passes a live UIView on the main thread.
    let container = unsafe { &*(host as *const UIView) };
    let app = AppView::new(mtm, container.bounds(), SurfaceOptions::default());
    container.addSubview(app.view());
    app.run(Root::of::<crate::App>());
    Box::into_raw(Box::new(app)) as *mut c_void
}

/// Stops the app mounted by [`vibex_mobile_nsh_attach`] and drops its view.
///
/// # Safety
///
/// `app` must be a pointer returned by `vibex_mobile_nsh_attach`, passed once.
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_nsh_detach(app: *mut c_void) {
    if app.is_null() {
        return;
    }
    // SAFETY: `app` came from `Box::into_raw` in `vibex_mobile_nsh_attach`.
    drop(unsafe { Box::from_raw(app as *mut AppView) });
}

use std::ffi::{CStr, c_char};

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
    crate::host::send(crate::host::HostEvent::ScanResult(value));
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
    crate::host::send_discovery_json(&value);
}

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

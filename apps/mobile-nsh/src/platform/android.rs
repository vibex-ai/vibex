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

use nsh_platform_android::{Root, run_root};

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
    run_root(handle, Root::of::<crate::App>());
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

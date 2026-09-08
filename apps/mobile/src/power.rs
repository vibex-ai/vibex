//! Battery-optimization allowlist awareness for reliable background delivery.
//!
//! Android's Doze and App Standby modes can suspend network access to a
//! backgrounded app even while its foreground service is running, so the app
//! asks the user to exempt it from battery optimizations. Whether the user
//! granted that exemption is only knowable on Android; on every other platform
//! the query reports `true` so the settings row stays hidden.

#[cfg(target_os = "android")]
mod platform {
    use std::sync::{Mutex, OnceLock};

    use gpui_android::AndroidApp;
    use jni::{JavaVM, objects::JObject, refs::Global};

    fn android_app() -> &'static Mutex<Option<AndroidApp>> {
        static APP: OnceLock<Mutex<Option<AndroidApp>>> = OnceLock::new();
        APP.get_or_init(|| Mutex::new(None))
    }

    pub fn initialize(app: &AndroidApp) {
        if let Ok(mut current) = android_app().lock() {
            *current = Some(app.clone());
        }
    }

    fn with_activity<T>(
        call: impl FnOnce(&mut jni::Env<'_>, &JObject<'_>) -> jni::errors::Result<T>,
    ) -> Option<T> {
        let app = android_app().lock().ok()?.clone()?;
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
        vm.attach_current_thread(|env| -> jni::errors::Result<Option<T>> {
            let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
            let activity = unsafe { env.as_cast_raw::<Global<JObject>>(&raw_activity)? };
            call(env, &activity).map(Some)
        })
        .ok()?
    }

    pub fn is_ignoring_battery_optimizations() -> bool {
        with_activity(|env, activity| {
            env.call_method(
                activity,
                jni::jni_str!("isIgnoringBatteryOptimizations"),
                jni::jni_sig!(() -> bool),
                &[],
            )?
            .z()
        })
        .unwrap_or(true)
    }

    pub fn request_ignore_battery_optimizations() {
        with_activity(|env, activity| {
            env.call_method(
                activity,
                jni::jni_str!("requestIgnoreBatteryOptimizations"),
                jni::jni_sig!(() -> ()),
                &[],
            )?;
            Ok(())
        });
    }
}

#[cfg(target_os = "android")]
pub use platform::initialize as initialize_android;

#[cfg(target_os = "android")]
pub use platform::{is_ignoring_battery_optimizations, request_ignore_battery_optimizations};

#[cfg(not(target_os = "android"))]
pub fn is_ignoring_battery_optimizations() -> bool {
    true
}

#[cfg(not(target_os = "android"))]
pub fn request_ignore_battery_optimizations() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_android_platforms_report_the_allowlist_as_granted() {
        // Non-Android platforms must not surface the allowlist prompt; the
        // query returning `true` is what keeps the settings row hidden.
        assert!(is_ignoring_battery_optimizations());
    }
}

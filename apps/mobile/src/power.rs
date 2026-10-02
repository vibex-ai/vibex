//! Battery-optimization allowlist awareness for reliable background delivery.
//!
//! Android's Doze and App Standby modes can suspend network access to a
//! backgrounded app even while its foreground service is running, so the app
//! asks the user to exempt it from battery optimizations. Whether the user
//! granted that exemption is only knowable on Android; on every other platform
//! the query reports `true` so the settings row stays hidden.

#[cfg(target_os = "android")]
mod platform {
    pub fn is_ignoring_battery_optimizations() -> bool {
        crate::android_bridge::with_activity(|env, activity| {
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
        let _ = crate::android_bridge::with_activity(|env, activity| {
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

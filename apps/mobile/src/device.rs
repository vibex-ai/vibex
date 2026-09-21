//! This phone's own device name.
//!
//! The name is the identity the phone pairs under, so the runtime's
//! paired-device list shows "Pixel 8" instead of a generic product name. The
//! runtime stays the authority for it: an operator rename in that list
//! overrides whatever the phone reports here, and the phone reads the new name
//! back from its next handshake.

/// The product name used when the platform cannot report a device name.
pub const FALLBACK_DEVICE_NAME: &str = "Vibex Mobile";

/// The phone's device name, bounded and normalized, or the fallback.
pub fn device_display_name() -> String {
    platform_device_name()
        .as_deref()
        .and_then(vibex_core::normalize_remote_display_name)
        .unwrap_or_else(|| FALLBACK_DEVICE_NAME.to_string())
}

/// Android reports the hardware model through `android.os.Build.MODEL`.
///
/// `android.os.Build` is a framework class, so the system classloader can find
/// it from any attached thread; no application classloader is involved.
#[cfg(target_os = "android")]
fn platform_device_name() -> Option<String> {
    use jni::{jni_sig, jni_str, objects::JString};

    gpui_mobile::android::jni::with_env(|env| -> Result<String, String> {
        let model = env
            .get_static_field(
                jni_str!("android/os/Build"),
                jni_str!("MODEL"),
                jni_sig!("Ljava/lang/String;"),
            )
            .map_err(|error| error.to_string())?
            .l()
            .map_err(|error| error.to_string())?;
        if model.is_null() {
            return Ok(String::new());
        }
        // Safety: `MODEL` is declared `java.lang.String`, and the local
        // reference is alive for as long as the attached frame.
        let model: JString<'_> = unsafe { JString::from_raw(env, model.as_raw()) };
        Ok(model.to_string())
    })
    .ok()
    .filter(|name| !name.trim().is_empty())
}

/// iOS reports the user-assigned device name through UIKit, which this client
/// does not bridge yet. The fallback keeps the runtime list readable.
#[cfg(not(target_os = "android"))]
fn platform_device_name() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fallback_name_is_usable_as_a_runtime_label() {
        assert!(vibex_core::normalize_remote_display_name(FALLBACK_DEVICE_NAME).is_some());
        assert!(!device_display_name().trim().is_empty());
    }
}

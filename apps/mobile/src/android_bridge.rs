//! JNI calls into the running Android Activity.
//!
//! The Android host entry point deliberately does not hand the app an
//! `AndroidApp` handle (there is no `android-activity` on this path), so every
//! module that calls a Java method on the Activity goes through here instead.
//! The JavaVM and the current Activity are the ones `gpui-pre-mobile` records
//! from [`crate::android_host`]'s `nativeStart` — which runs again on every
//! `Activity.onCreate`, so a recreated Activity is picked up automatically.

#[cfg(target_os = "android")]
mod imp {
    use jni::JavaVM;
    use jni::objects::{Global, JObject};

    /// Runs `call` with the current Activity, on a thread attached to the JVM.
    ///
    /// Returns `None` when the JVM or the Activity is not known yet, when the
    /// thread cannot be attached, or when the Java call itself failed (a pending
    /// Java exception is cleared so the next call starts clean). Callers decide
    /// their own fallback, exactly as they did on the `android-activity` path.
    pub fn with_activity<T>(
        call: impl FnOnce(&mut jni::Env<'_>, &JObject<'_>) -> jni::errors::Result<T>,
    ) -> Option<T> {
        let vm_ptr = gpui_mobile::android::jni::java_vm();
        let activity_ptr = gpui_mobile::android::jni::activity_as_ptr();
        if vm_ptr.is_null() || activity_ptr.is_null() {
            return None;
        }
        // SAFETY: both pointers come from the platform layer, which stores them
        // on every Activity creation and keeps its own global reference to the
        // Activity, so neither dangles while the call runs.
        let vm = unsafe { JavaVM::from_raw(vm_ptr.cast()) };
        vm.attach_current_thread(|env| -> jni::errors::Result<Option<T>> {
            let raw_activity = activity_ptr as jni::sys::jobject;
            let activity = unsafe { env.as_cast_raw::<Global<JObject>>(&raw_activity)? };
            match call(env, &activity) {
                Ok(value) => Ok(Some(value)),
                Err(error) => {
                    env.exception_clear();
                    Err(error)
                }
            }
        })
        .ok()
        .flatten()
    }
}

#[cfg(target_os = "android")]
pub use imp::with_activity;

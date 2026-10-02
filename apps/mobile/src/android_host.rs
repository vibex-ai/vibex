//! The Android host-driven platform entry.
//!
//! `gpui-pre-mobile` offers two Android entry points. The `android-activity`
//! one (`android_main`) builds the process-global `AndroidPlatform` exactly
//! once: a second `android_main` in the same process cannot replace it, so
//! GPUI's `App::new_app` trips its "must construct App on main thread" assert,
//! `android-activity` catches the panic and finishes the Activity. That is
//! reachable whenever the system reclaims the backgrounded Activity while the
//! foreground connection service keeps the process alive — the app flashed
//! closed on every tap until the process was killed.
//!
//! The host entry owns a **process-lived** render thread instead. This module
//! is the whole Rust half of that contract: [`GpuiHostActivity`] hands over the
//! Activity, its surface, input, window insets and lifecycle, and a recreated
//! Activity simply re-attaches its surface to the same GPUI window.
//!
//! [`GpuiHostActivity`]: ../../../android/app/src/main/java/ai/vibex/mobile/GpuiHostActivity.java

use std::path::PathBuf;
use std::sync::Mutex;

use gpui::AppLifecyclePhase;
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jboolean, jfloat, jint, jlong};
use jni::{Env, EnvUnowned};

use crate::assets::MobileAssets;
use crate::platform;

/// The content rect the Activity reported last, in physical pixels.
///
/// The window-inset listener can fire before the render thread has opened the
/// window, so the value is cached and applied as soon as there is a window.
static CONTENT_RECT: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);

/// Applies a reported content rect once the platform has a window to size.
///
/// Called when the Activity reports insets, and again from
/// [`crate::open_root_window`] because the first report usually arrives before
/// the window exists — dropping it would leave GPUI with no safe area at all.
pub fn apply_pending_insets() {
    let rect = CONTENT_RECT.lock().ok().and_then(|rect| *rect);
    let Some((left, top, right, bottom)) = rect else {
        return;
    };
    if let Some(platform) = gpui_mobile::android::jni::platform()
        && let Some(window) = platform.primary_window()
    {
        window.update_safe_area_from_content_rect(left, top, right, bottom);
    }
}

/// Records the Activity and starts the process-lived render thread.
///
/// Called from `GpuiHostActivity.onCreate` — every time, recreation included.
/// `gpui-pre-mobile` takes its own global reference to the Activity, keeps the
/// JVM, and makes `start_with_assets` a no-op after the first Activity, so a
/// recreated one only replaces the Activity the Java bridges talk to.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeStart<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    activity: JObject<'caller>,
    data_dir: JString<'caller>,
) {
    let mut launch_dir = None;
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            platform::install_diagnostics();
            if let Err(error) = gpui_mobile::android::jni::set_host_activity(env, &activity) {
                log::error!("nativeStart: recording the host Activity failed: {error}");
            }
            initialize_android_tls(env, &activity)?;
            launch_dir = Some(PathBuf::from(data_dir.to_string()));
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();

    let Some(data_dir) = launch_dir else {
        return;
    };
    gpui_mobile::android::host::start_with_assets(MobileAssets, move |cx| {
        crate::open_root_window(data_dir, cx);
    });
}

/// Points the platform TLS verifier at this Activity's application context.
///
/// The Android trust store is only reachable through JNI, so the verifier has
/// to be initialized before the first connection is attempted.
fn initialize_android_tls(env: &mut Env<'_>, activity: &JObject<'_>) -> jni::errors::Result<()> {
    use jni::signature::RuntimeMethodSignature;

    let signature = RuntimeMethodSignature::from_str("()Landroid/content/Context;")?;
    let context = env
        .call_method(
            activity,
            jni::jni_str!("getApplicationContext"),
            signature.method_signature(),
            &[],
        )?
        .l()?;
    rustls_platform_verifier::android::init_with_env(env, context)
}

/// Hands a new `Surface` to the render thread.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeSurfaceCreated<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    surface: JObject<'caller>,
    scale: jfloat,
) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            // SAFETY: `surface` is the `android.view.Surface` the callback was
            // handed, and the NDK conversion takes its own reference to the
            // matching `ANativeWindow`.
            let window = unsafe {
                ndk::native_window::NativeWindow::from_surface(
                    env.get_raw().cast(),
                    surface.as_raw().cast(),
                )
            };
            match window {
                Some(window) => gpui_mobile::android::host::surface_created(window, scale),
                None => log::warn!("nativeSurfaceCreated: the Surface has no native window"),
            }
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

/// Takes the surface back once Android tears it down.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeSurfaceDestroyed<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            gpui_mobile::android::host::surface_destroyed();
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

/// Forwards `onResume` / `onPause`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeOnAppLifecycle<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    foreground: jboolean,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            if foreground {
                gpui_mobile::android::host::resumed();
                platform::notify_lifecycle(AppLifecyclePhase::Active);
            } else {
                platform::notify_lifecycle(AppLifecyclePhase::Background);
                gpui_mobile::android::host::paused();
            }
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

/// Reports the window area the system bars leave free, in physical pixels.
///
/// GPUI draws edge to edge, so the app pads its own root view by these values.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeInsets<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    content_left: jint,
    content_top: jint,
    content_right: jint,
    content_bottom: jint,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            if let Ok(mut rect) = CONTENT_RECT.lock() {
                *rect = Some((content_left, content_top, content_right, content_bottom));
            }
            apply_pending_insets();
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

/// Reports the software keyboard's visibility and its height in logical pixels.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeKeyboardState<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    visible: jboolean,
    height: jfloat,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            platform::set_keyboard_visible(visible);
            gpui_mobile::set_keyboard_height(if visible { height.max(0.0) } else { 0.0 });
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

/// Forwards one InputConnection update from the IME proxy.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeIme<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    session: jlong,
    kind: jint,
    text: JString<'caller>,
    start: jint,
    end: jint,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            gpui_mobile::android::host::ime_event(
                session as u64,
                kind,
                text.to_string(),
                start.max(0) as usize,
                end.max(0) as usize,
            );
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

/// Forwards one pointer of a `MotionEvent`.
///
/// The Java host already collapsed `ACTION_POINTER_DOWN`/`ACTION_POINTER_UP`
/// onto the affected pointer, so a one-pointer slice is exactly the fan-out
/// [`gpui_mobile::android::host::motion_event`] documents.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeTouch<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    action: jint,
    pointer_id: jint,
    x: jfloat,
    y: jfloat,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            // GPUI wants physical pixels relative to the surface, which is
            // what `MotionEvent` reports for a full-window view.
            gpui_mobile::android::host::motion_event(
                action as u32,
                0,
                &[gpui_mobile::android::host::Pointer {
                    id: pointer_id,
                    x,
                    y,
                }],
            );
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

/// Forwards a hardware key. `action` is 0 for down and 1 for up.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeKey<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    key_code: jint,
    action: jint,
    meta_state: jint,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            gpui_mobile::android::host::key(key_code, action, meta_state);
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

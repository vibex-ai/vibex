//! The mobile platform layer.
//!
//! Both native clients run on `gpui-pre-mobile`, whose platform types are
//! supplied per target (`gpui_mobile::android` / `::ios`) rather than by
//! `gpui_platform`. This module is the single place that knows how to build the
//! [`gpui::Platform`] for the host, so the rest of the app can keep calling
//! window-level APIs.
//!
//! Three things differ from the desktop platform surface and are normalized
//! here instead of at every call site:
//!
//! - **Safe-area and IME insets.** `gpui-pre` has `WindowInsets`, but the
//!   mobile platform never feeds it, so `Window::fully_visible_bounds()` would
//!   report the whole window. The crate exposes the real values per platform
//!   (`safe_area_insets` / `AndroidWindow::safe_area_insets_logical` /
//!   `keyboard_height`); [`insets`] composes them into gpui's own type.
//! - **Software keyboard.** The crate drives the keyboard off focus changes and
//!   does not implement `PlatformWindow::show_soft_keyboard`, so
//!   `Window::request_virtual_keyboard()` is a no-op. [`show_keyboard`] and
//!   [`hide_keyboard`] call the crate's free functions instead.
//! - **Application lifecycle.** Nothing implements `Platform::on_app_lifecycle`
//!   on this platform; the host forwards iOS phases over FFI and Android phases
//!   over JNI, both landing in [`notify_lifecycle`].

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{Edges, Pixels, Platform, Window, px};

/// The insets obscuring a window: system UI plus the software keyboard.
///
/// Mirrors the shape of `gpui::WindowInsets` for the call sites that lay the
/// root view out.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct WindowInsets {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl WindowInsets {
    /// The inset content should avoid on each edge: the system safe area and
    /// the keyboard overlap instead of stacking.
    pub fn effective(&self) -> Edges<Pixels> {
        Edges {
            top: px(self.top),
            right: px(self.right),
            bottom: px(self.bottom),
            left: px(self.left),
        }
    }
}

/// The platform for the current mobile target.
///
/// On Android the platform is a process-wide singleton created by
/// `jni::init_platform`, which `android_main` must have called already; on iOS
/// each call builds a fresh `IosPlatform`, matching the crate's own example.
pub fn current_platform(_headless: bool) -> Rc<dyn Platform> {
    #[cfg(target_os = "android")]
    {
        gpui_mobile::android::jni::shared_platform()
            .expect("the Android platform must be initialized before the window opens")
            .into_rc()
    }
    #[cfg(target_os = "ios")]
    {
        gpui_mobile::current_platform(false)
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        unreachable!("the mobile client only runs on Android and iOS")
    }
}

/// The window's obscuring insets, in logical pixels.
///
/// The keyboard's height is reported on its own rather than through the safe
/// area, so it is folded into the bottom edge — which is where it always sits.
pub fn insets(window: &Window) -> WindowInsets {
    #[cfg(target_os = "ios")]
    {
        let _ = window;
        let (top, bottom, left, right) = gpui_mobile::safe_area_insets();
        WindowInsets {
            top,
            right,
            bottom: bottom.max(gpui_mobile::keyboard_height()),
            left,
        }
    }
    #[cfg(target_os = "android")]
    {
        let _ = window;
        let Some(platform) = gpui_mobile::android::jni::platform() else {
            return WindowInsets::default();
        };
        let Some(active) = platform.primary_window() else {
            return WindowInsets::default();
        };
        let safe_area = active.safe_area_insets_logical();
        WindowInsets {
            top: safe_area.top,
            right: safe_area.right,
            bottom: safe_area.bottom.max(gpui_mobile::keyboard_height()),
            left: safe_area.left,
        }
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        // Host builds never open a window; the visible bounds are the closest
        // stand-in so tests that lay out a root view still get sane numbers.
        let visible = window.fully_visible_bounds();
        let viewport = window.viewport_size();
        WindowInsets {
            top: f32::from(visible.top()),
            right: f32::from(viewport.width - visible.right()),
            bottom: f32::from(viewport.height - visible.bottom()),
            left: f32::from(visible.left()),
        }
    }
}

/// Requests the software keyboard for the focused input.
pub fn show_keyboard() {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        gpui_mobile::show_keyboard();
    }
}

/// Dismisses the software keyboard without changing GPUI focus.
pub fn hide_keyboard() {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        gpui_mobile::hide_keyboard();
    }
}

/// The keyboard's visibility as last reported by the platform.
///
/// Android's host activity reports it from the layout change the IME causes;
/// iOS derives it from the keyboard height `gpui-pre-mobile` tracks itself.
static KEYBOARD_VISIBLE: AtomicBool = AtomicBool::new(false);

/// Records the software keyboard's visibility (Android host callback).
pub fn set_keyboard_visible(visible: bool) {
    KEYBOARD_VISIBLE.store(visible, Ordering::Release);
}

/// Whether the software keyboard is on screen.
pub fn keyboard_visible() -> bool {
    #[cfg(target_os = "ios")]
    {
        gpui_mobile::keyboard_height() > 0.0
    }
    #[cfg(not(target_os = "ios"))]
    {
        KEYBOARD_VISIBLE.load(Ordering::Acquire)
    }
}

/// Re-requests the software keyboard for a tap on a text field.
///
/// The platform shows and hides the IME from focus changes alone, so an input
/// that keeps GPUI focus while the user dismisses the keyboard never asks for
/// it again — the tap has to, or the field stays focused with no way back to
/// typing but tapping elsewhere first.
pub fn resume_keyboard() {
    if !keyboard_visible() {
        show_keyboard();
    }
}

/// The window-inset API the rest of the app uses.
///
/// Implemented for `Window` so call sites read like the desktop ones
/// (`window.insets().effective()`), keeping the platform difference in one
/// place.
pub trait WindowInsetsExt {
    fn insets(&self) -> WindowInsets;
}

impl WindowInsetsExt for Window {
    fn insets(&self) -> WindowInsets {
        insets(self)
    }
}

/// Forward an application lifecycle phase to the subscribers.
///
/// `gpui-pre-mobile` never calls `Platform::on_app_lifecycle`, so the hosts
/// report phases directly: iOS from its app-delegate FFI shims, Android from
/// the Activity's `onResume`/`onPause`.
pub fn notify_lifecycle(phase: gpui::AppLifecyclePhase) {
    crate::lifecycle::notify(phase);
}

/// Installs the platform's logging and panic handling.
///
/// Routing `log` to logcat has to happen before the first frame, or everything
/// the platform layer reports is dropped.
pub fn install_diagnostics() {
    #[cfg(target_os = "android")]
    {
        gpui_mobile::android::init_logger();
        gpui_mobile::android::jni::install_panic_hook();
    }
}

/// Opens this app's page in the OS settings.
///
/// A denied local-network permission can only be re-granted there, and the
/// nearby-pairing panel that reports the denial has no other way to send the
/// user to it. Returns `false` when the platform refused to open it.
pub fn open_app_settings() -> bool {
    #[cfg(target_os = "android")]
    {
        android::open_app_settings()
    }
    #[cfg(target_os = "ios")]
    {
        unsafe { vibex_ios_open_app_settings() };
        true
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        false
    }
}

#[cfg(target_os = "ios")]
unsafe extern "C" {
    fn vibex_ios_open_app_settings();
}

/// The Android half of [`open_app_settings`].
///
/// Mirrors `scanner.rs`: the host activity is the only object that can start an
/// intent, so the app handle is captured once at startup and the call is made
/// through JNI by method name.
#[cfg(target_os = "android")]
mod android {
    use std::sync::{Mutex, OnceLock};

    use android_activity::AndroidApp;
    use jni::{EnvUnowned, JavaVM, objects::JObject, refs::Global};

    fn android_app() -> &'static Mutex<Option<AndroidApp>> {
        static APP: OnceLock<Mutex<Option<AndroidApp>>> = OnceLock::new();
        APP.get_or_init(|| Mutex::new(None))
    }

    pub fn initialize(app: &AndroidApp) {
        if let Ok(mut current) = android_app().lock() {
            *current = Some(app.clone());
        }
    }

    pub fn open_app_settings() -> bool {
        let Some(app) = android_app().lock().ok().and_then(|app| app.clone()) else {
            return false;
        };
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
        vm.attach_current_thread(|env| -> jni::errors::Result<bool> {
            let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
            let activity = unsafe { env.as_cast_raw::<Global<JObject>>(&raw_activity)? };
            let result = env.call_method(
                activity,
                jni::jni_str!("openAppSettings"),
                jni::jni_sig!(() -> ()),
                &[],
            );
            if result.is_err() {
                let _ = env.exception_clear();
            }
            Ok(result.is_ok())
        })
        .unwrap_or(false)
    }
}

#[cfg(target_os = "android")]
pub use android::initialize as initialize_android;

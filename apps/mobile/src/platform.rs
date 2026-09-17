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

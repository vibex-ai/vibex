//! Native GPUI mobile client for iOS and Android.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]
#![cfg_attr(not(any(target_os = "android", target_os = "ios")), allow(dead_code))]

#[cfg(target_os = "android")]
mod android_bridge;
#[cfg(target_os = "android")]
mod android_host;
#[cfg(any(target_os = "android", test))]
mod android_insets;
mod app;
mod assets;
mod background_connection;
mod device;
mod discovery;
mod lifecycle;
mod locale;
mod markdown;
mod notifications;
mod pairing;
mod platform;
mod power;
mod scanner;
mod scroll_capture;
mod selection_menu;
mod sidebar;
mod storage;
mod theme;
mod workbench;

use std::path::PathBuf;

use gpui::{App, AppContext as _, WindowBackgroundAppearance, WindowOptions};

pub use pairing::{MobileCredentialBundle, MobileRemoteRouteBundle};

/// Builds the shared state and opens the root window.
///
/// Runs inside the application callback on both targets: on Android on the
/// host-driven render thread, once the first surface has arrived; on iOS once
/// UIKit has finished launching. Everything before the window — Tokio, the kit,
/// the bundled fonts — has to happen here because the GPUI context only exists
/// at this point.
pub(crate) fn open_root_window(data_dir: PathBuf, cx: &mut App) {
    let tokio_handle = background_connection::tokio_handle();
    gpui_tokio::init_from_handle(cx, tokio_handle.clone());
    // gpui-kit registers the global theme and the overlay state every
    // component reads, so it has to be initialized before the first
    // window opens.
    gpui_component::init(cx);
    app::bind_keys(cx);
    // Resolve the native platform's preferred language before the
    // first window is created so the initial pairing screen is never
    // rendered with a stale English fallback.
    let _ = locale::current();
    assets::load_fonts(cx).expect("failed to load bundled mobile fonts");
    // Point the kit theme at the shared vibex tokens before the first
    // paint, so no component is ever drawn from the framework palette.
    theme::apply_component_theme(None, cx);

    let _window = cx
        .open_window(
            WindowOptions {
                // Mobile windows are fullscreen; the platform owns their geometry.
                window_bounds: None,
                window_background: WindowBackgroundAppearance::Opaque,
                focus: true,
                show: true,
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| app::MobileApp::new(data_dir, window, cx));
                // `Root` owns the overlay layers (sheets, dialogs,
                // notifications, menus) and restores focus after one
                // closes. A fullscreen phone window is not client
                // decorated, so the kit's window frame draws nothing.
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            },
        )
        .expect("failed to open Vibex mobile window");

    // Replay early Android inset reports and observe later changes on the GPUI
    // thread, where surface dimensions and root layout are owned.
    #[cfg(target_os = "android")]
    android_insets::observe(_window.into(), cx);
}

/// Keeps `android-activity`'s glue linkable. **Not** an entry point.
///
/// `gpui-pre-mobile` enables `android-activity`'s `native-activity` feature, so
/// its glue — and the `ANativeActivity_onCreate` export that references
/// `android_main` — is linked into this library even though Vibex enters
/// through [`android_host`]. Android resolves every dynamic symbol when it
/// loads a library, so dropping the symbol made `dlopen` fail with
/// `cannot locate symbol "android_main"` and the app could not start at all.
///
/// Nothing can call this: the manifest declares no NativeActivity and no
/// `android.app.lib_name`, so the system never invokes `ANativeActivity_onCreate`.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(_app: android_activity::AndroidApp) {
    log::error!("android_main was called; Vibex starts through GpuiHostActivity");
}

/// Registers the iOS root-view callback.
///
/// The UIKit host calls this from `application:didFinishLaunchingWithOptions:`
/// before `gpui_ios_run_demo()`, which invokes the callback once GPUI's run
/// loop starts.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_register_app() {
    platform::install_diagnostics();
    let data_dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/Application Support/Vibex");
    gpui_mobile::ios::ffi::set_app_callback(Box::new(move |cx: &mut App| {
        open_root_window(data_dir, cx);
    }));
}

/// Reports an application lifecycle transition from the UIKit host.
///
/// `phase` is `1` when the app becomes foreground and `0` when it enters the
/// background. `gpui-pre-mobile` has no `Platform::on_app_lifecycle`
/// implementation, so the host bridge is the only source for these events.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_set_lifecycle(phase: i32) {
    let phase = if phase == 0 {
        gpui::AppLifecyclePhase::Background
    } else {
        gpui::AppLifecyclePhase::Active
    };
    platform::notify_lifecycle(phase);
}

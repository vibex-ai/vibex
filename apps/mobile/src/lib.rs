//! Native GPUI mobile client for iOS and Android.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]
#![cfg_attr(not(any(target_os = "android", target_os = "ios")), allow(dead_code))]

mod app;
mod assets;
mod input;
mod markdown;
mod pairing;
mod scanner;
mod storage;
mod theme;

use std::path::PathBuf;

use gpui::{App, AppContext as _, Bounds, WindowBackgroundAppearance, WindowBounds, WindowOptions};

pub use pairing::{MobileCredentialBundle, MobileRemoteRouteBundle};

fn run(data_dir: PathBuf) {
    gpui_platform::application()
        .with_assets(assets::MobileAssets)
        .run(move |cx: &mut App| {
            gpui_tokio::init(cx);
            app::bind_keys(cx);
            assets::load_fonts(cx).expect("failed to load bundled mobile fonts");

            let bounds = Bounds::centered(None, gpui::size(gpui::px(390.0), gpui::px(844.0)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_background: WindowBackgroundAppearance::Opaque,
                    focus: true,
                    show: true,
                    ..Default::default()
                },
                move |window, cx| cx.new(|cx| app::MobileApp::new(data_dir, window, cx)),
            )
            .expect("failed to open Vibex mobile window");
        });
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(android_app: gpui_android::AndroidApp) {
    let data_dir = android_app
        .internal_data_path()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    scanner::initialize_android(&android_app);
    gpui_platform::android_init(android_app);
    run(data_dir);
}

/// Called by the tiny Objective-C host. `gpui_ios` enters UIApplicationMain.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_main() {
    let data_dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/Application Support/Vibex");
    run(data_dir);
}

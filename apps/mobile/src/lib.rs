//! Native GPUI mobile client for iOS and Android.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]
#![cfg_attr(not(any(target_os = "android", target_os = "ios")), allow(dead_code))]

mod app;
mod assets;
mod background_connection;
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
/// Runs inside the application callback on both targets: on Android once the
/// native surface exists, on iOS once UIKit has finished launching. Everything
/// before the window — Tokio, the kit, the bundled fonts — has to happen here
/// because the GPUI context only exists at this point.
fn open_root_window(data_dir: PathBuf, cx: &mut App) {
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

    cx.open_window(
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
            // closes. Phone windows are fullscreen, so no border.
            cx.new(|cx| gpui_component::Root::new(view, window, cx).bordered(false))
        },
    )
    .expect("failed to open Vibex mobile window");
}

#[cfg(target_os = "android")]
fn initialize_android_tls(android_app: &android_activity::AndroidApp) {
    use jni::{JavaVM, objects::JObject, refs::Global, signature::RuntimeMethodSignature};

    let vm = unsafe { JavaVM::from_raw(android_app.vm_as_ptr().cast()) };
    vm.attach_current_thread_for_scope(|env| -> jni::errors::Result<()> {
        let raw_activity = android_app.activity_as_ptr() as jni::sys::jobject;
        let activity = unsafe { env.as_cast_raw::<Global<JObject>>(&raw_activity)? };
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
    })
    .expect("failed to initialize Android TLS certificate verifier");
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(android_app: android_activity::AndroidApp) {
    let data_dir = android_app
        .internal_data_path()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    // Logging and the panic hook come first so every later failure is visible
    // in logcat instead of vanishing with the native thread.
    platform::install_diagnostics();
    gpui_mobile::android::jni::init_platform(&android_app);
    initialize_android_tls(&android_app);
    background_connection::initialize_android(&android_app);
    discovery::initialize_android(&android_app);
    notifications::initialize_android(&android_app);
    power::initialize_android(&android_app);
    scanner::initialize_android(&android_app);

    // `Application::run` blocks by driving the Android event loop, and defers
    // the callback until the activity has a native surface.
    let platform = platform::current_platform(false);
    gpui::Application::with_platform(platform)
        .with_assets(assets::MobileAssets)
        .run(move |cx: &mut App| open_root_window(data_dir, cx));
}

/// Reports an application lifecycle transition from the Android host.
///
/// Called from `GpuiNativeActivity.onResume` / `onPause`, which are the only
/// places that see Android's process lifecycle. `gpui-pre-mobile` implements no
/// `Platform::on_app_lifecycle`, so this bridge is what feeds
/// [`background_connection`]'s suspend/resume handling.
///
/// # Safety
/// Must only be called from the JVM on a valid JNI thread.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_ai_vibex_mobile_GpuiNativeActivity_nativeOnAppLifecycle(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
    foreground: jni::sys::jboolean,
) {
    let phase = if foreground {
        gpui::AppLifecyclePhase::Active
    } else {
        gpui::AppLifecyclePhase::Background
    };
    platform::notify_lifecycle(phase);
}

/// Reports the software keyboard's visibility from the Android host.
///
/// `gpui-pre-mobile` drives the IME from focus changes alone, so nothing on the
/// Rust side learns that the user dismissed the keyboard while an input kept
/// GPUI focus. The vendored activity reports the real state so a tap on that
/// input can ask for the keyboard again.
///
/// # Safety
/// Must only be called from the JVM on a valid JNI thread.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeKeyboardVisible<'caller>(
    mut unowned_env: jni::EnvUnowned<'caller>,
    _class: jni::objects::JClass<'caller>,
    visible: jni::sys::jboolean,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            platform::set_keyboard_visible(visible);
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>()
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

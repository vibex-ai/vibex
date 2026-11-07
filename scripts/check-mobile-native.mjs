import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SELF_TEST = process.argv.includes("--self-test");

function source(path) {
  return readFileSync(join(ROOT, path), "utf8");
}

function assert(condition, code) {
  if (!condition) throw new Error(code);
}

function validateContract(read = source, exists = (path) => existsSync(join(ROOT, path))) {
  assert(!exists("apps/mobile-wasm"), "legacy_mobile_wasm_tree_present");
  for (const path of [
    "apps/mobile/Cargo.toml",
    "apps/mobile/src/lib.rs",
    "apps/mobile/src/app.rs",
    "apps/mobile/src/question.rs",
    "apps/mobile/src/pairing.rs",
    "apps/mobile/src/scanner.rs",
    "apps/mobile/src/storage.rs",
    "crates/vibex-remote-client/Cargo.toml",
    "crates/vibex-remote-client/src/transport.rs",
    "apps/mobile/assets/fonts/wqy-microhei/wqy-microhei.ttc",
    // gpui-mobile reads this exact APK asset path when the Android system
    // emoji font is COLR v1, which swash cannot rasterize. Dropping it leaves
    // every emoji on Android 13+ falling back to .notdef, so the checker pins
    // both the font and the license text that must travel beside it.
    "apps/mobile/android/app/src/main/assets/fonts/NotoColorEmoji.ttf",
    "apps/mobile/android/app/src/main/assets/fonts/OFL.txt",
    "apps/mobile/android/app/src/main/AndroidManifest.xml",
    "apps/mobile/android/settings.gradle",
    "apps/mobile/android/app/build.gradle",
    "apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiHostActivity.java",
    "apps/mobile/android/app/src/main/java/ai/vibex/mobile/PairingQrScannerActivity.java",
    "apps/mobile/android/app/src/main/java/dev/gpui/mobile/GpuiClipboard.java",
    "apps/mobile/android/app/src/main/res/values/styles.xml",
    "apps/mobile/android/app/src/main/res/xml/file_paths.xml",
    "apps/mobile/ios/project.yml",
    "apps/mobile/ios/Vibex/main.m",
    "apps/mobile/ios/Vibex/QRScanner.swift",
    "apps/mobile/ios/Headers/vibex_mobile.h",
    "apps/mobile/ios/Headers/module.modulemap",
    "apps/mobile/src/android_bridge.rs",
    "apps/mobile/src/android_host.rs",
    "apps/mobile/src/android_insets.rs",
    "apps/mobile/src/platform.rs",
    "apps/mobile/src/scroll_capture.rs",
    "apps/mobile/src/update.rs"
  ]) {
    assert(exists(path), `native_mobile_file_missing:${path}`);
  }

  const workspace = read("Cargo.toml");
  const manifest = read("apps/mobile/Cargo.toml");
  const entry = read("apps/mobile/src/lib.rs");
  const app = read("apps/mobile/src/app.rs");
  const question = read("apps/mobile/src/question.rs");
  const pairing = read("apps/mobile/src/pairing.rs");
  const scanner = read("apps/mobile/src/scanner.rs");
  const storage = read("apps/mobile/src/storage.rs");
  const remoteManifest = read("crates/vibex-remote-client/Cargo.toml");
  const remoteTransport = read("crates/vibex-remote-client/src/transport.rs");
  const platform = read("apps/mobile/src/platform.rs");
  const android = read("apps/mobile/android/app/src/main/AndroidManifest.xml");
  const androidSettings = read("apps/mobile/android/settings.gradle");
  const androidBuild = read("apps/mobile/android/app/build.gradle");
  const androidActivity = read("apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiHostActivity.java");
  const androidScanner = read("apps/mobile/android/app/src/main/java/ai/vibex/mobile/PairingQrScannerActivity.java");
  const androidClipboard = read("apps/mobile/android/app/src/main/java/dev/gpui/mobile/GpuiClipboard.java");
  const androidStyles = read("apps/mobile/android/app/src/main/res/values/styles.xml");
  const androidUpdatePaths = read("apps/mobile/android/app/src/main/res/xml/file_paths.xml");
  const androidBridge = read("apps/mobile/src/android_bridge.rs");
  const androidHost = read("apps/mobile/src/android_host.rs");
  const androidInsets = read("apps/mobile/src/android_insets.rs");
  const iosMain = read("apps/mobile/ios/Vibex/main.m");
  const iosProject = read("apps/mobile/ios/project.yml");
  const iosScanner = read("apps/mobile/ios/Vibex/QRScanner.swift");
  const iosHeader = read("apps/mobile/ios/Headers/vibex_mobile.h");
  const iosModuleMap = read("apps/mobile/ios/Headers/module.modulemap");
  const updater = read("apps/mobile/src/update.rs");
  const appUpdateInstall = read("crates/app-update/src/install.rs");
  const appUpdateSource = read("crates/app-update/src/source.rs");

  assert(workspace.includes('"apps/mobile"'), "native_mobile_workspace_member_missing");
  assert(!workspace.includes('"apps/mobile-wasm"'), "legacy_mobile_workspace_member_present");
  assert(manifest.includes('crate-type = ["cdylib", "staticlib", "rlib"]'), "native_mobile_crate_types_invalid");
  assert(manifest.includes('gpui-mobile = { package = "gpui-pre-mobile"'), "mobile_gpui_pre_mobile_dependency_missing");
  // Upstream still pins gpui-pre 0.3.10, so the pin names the reviewed fork
  // (longbridge/gpui-mobile#46) that accepts the 0.3.11 gpui-kit 0.9.0 pins.
  assert(manifest.includes("PeatBoy/gpui-mobile"), "mobile_gpui_pre_mobile_source_missing");
  assert(manifest.includes('rev = "f6e8b3769933bed5f838b9ec3d45ec84fc2a5a8e"'), "mobile_gpui_pre_mobile_revision_missing");
  assert(!manifest.includes("vendor/zed"), "mobile_vendor_zed_dependency_present");
  assert(manifest.includes('rustls-platform-verifier = "0.7"'), "android_tls_platform_verifier_dependency_missing");
  assert(workspace.includes('webpki-root-certs = "1"'), "android_webpki_root_workspace_dependency_missing");
  assert(remoteManifest.includes("webpki-root-certs.workspace = true"), "android_webpki_root_dependency_missing");
  assert(remoteManifest.includes("rustls.workspace = true"), "android_rustls_dependency_missing");
  assert(remoteTransport.includes('cfg(target_os = "android")'), "android_remote_tls_target_guard_missing");
  assert(remoteTransport.includes(".tls_certs_only(roots)"), "android_remote_http_webpki_roots_missing");
  assert(remoteTransport.includes("android_websocket_connector"), "android_remote_websocket_connector_missing");
  assert(
    remoteTransport.includes("connect_async_tls_with_config"),
    "android_remote_websocket_webpki_roots_missing"
  );
  assert(
    remoteTransport.includes("webpki_root_certs::TLS_SERVER_ROOT_CERTS"),
    "android_remote_tls_root_set_missing"
  );
  assert(
    (remoteTransport.match(/reqwest::Client::new\(\)/g) ?? []).length === 1,
    "android_remote_http_client_bypass_present"
  );
  assert(
    !remoteTransport.includes("danger_accept_invalid_certs"),
    "android_remote_tls_verification_disabled"
  );
  // Android enters through gpui-pre-mobile's host-driven entry. The
  // `android-activity` entry point (`android_main`) can only build its
  // process-global platform once: a recreated Activity — which the foreground
  // connection service makes routine, because the process outlives the
  // Activity — tripped GPUI's main-thread assert and finished itself, so the
  // app flashed closed until the process was killed. The host entry owns a
  // process-lived render thread instead and must stay the only Android entry.
  assert(androidHost.includes("gpui_mobile::android::host::start_with_assets"), "android_host_entry_missing");
  // The platform builds a host's window through `on_open_window`, and only when
  // that callback is registered before the first surface arrives. Without it the
  // surface opens a native window that GPUI never draws into.
  assert(androidHost.includes("gpui_mobile::android::host::on_open_window"), "android_host_open_window_missing");
  assert(androidHost.includes("gpui_mobile::android::jni::set_host_activity"), "android_host_activity_bridge_missing");
  assert(
    androidHost.includes("Java_ai_vibex_mobile_GpuiHostActivity_nativeStart"),
    "android_host_start_export_missing"
  );
  assert(androidHost.includes("nativeSurfaceCreated"), "android_host_surface_export_missing");
  assert(androidHost.includes("nativeSurfaceDestroyed"), "android_host_surface_release_export_missing");
  assert(
    androidBridge.includes("pub fn with_activity"),
    "android_activity_jni_bridge_missing"
  );
  assert(
    read("apps/mobile/src/background_connection.rs").includes("crate::android_bridge::with_activity"),
    "android_connection_service_bridge_missing"
  );
  assert(
    read("apps/mobile/src/discovery.rs").includes("crate::android_bridge::with_activity"),
    "android_lan_discovery_bridge_missing"
  );
  // `gpui-pre-mobile` links android-activity's NativeActivity glue, and that
  // glue references `android_main`: the symbol has to exist for Android to
  // `dlopen` the library at all, but it must stay an inert stub, because the
  // real entry is the host one.
  assert(entry.includes("pub fn android_main("), "android_glue_entry_symbol_missing");
  assert(!entry.includes("init_platform"), "android_process_platform_init_present");
  assert(!entry.includes("Application::with_platform"), "android_application_built_by_app_present");
  assert(!manifest.includes("android.app.lib_name"), "android_native_activity_library_metadata_present");
  assert(platform.includes("gpui_mobile::android::init_logger()"), "android_gpui_logger_missing");
  assert(androidHost.includes("rustls_platform_verifier::android::init_with_env"), "android_tls_platform_verifier_init_missing");
  assert(androidHost.includes('"getApplicationContext"'), "android_tls_application_context_missing");
  assert(
    androidHost.indexOf("initialize_android_tls(env, &activity)")
      < androidHost.indexOf("host::start_with_assets"),
    "android_tls_platform_verifier_init_order_invalid"
  );
  assert(entry.includes('target_os = "ios"'), "ios_rust_entry_missing");
  assert(entry.includes("assets::load_fonts(cx)"), "native_mobile_font_loading_missing");
  // Text entry used to be a hand-rolled field that focused itself and asked for
  // the soft keyboard directly. Both duties moved to the kit: the composer is a
  // kit textarea, and the kit input element claims the window input handler that
  // the platform turns into a soft keyboard. Assert each half so the guarantee
  // is not quietly lost along with the field that used to provide it.
  assert(app.includes("Textarea::new(&self.composer_input)"), "native_composer_kit_input_missing");
  assert(entry.includes("gpui_component::init(cx)"), "native_component_init_missing");
  assert(platform.includes("gpui_mobile::show_keyboard()"), "native_text_input_keyboard_missing");
  assert(platform.includes("gpui_mobile::hide_keyboard()"), "native_text_input_keyboard_hide_missing");
  // A held Backspace repeats only while the keyboard's editor reports the caret
  // moving, and the host's editor is a cleared scratch widget between commits.
  // The app therefore publishes the focused field's text and caret to the Java
  // host, which answers the keyboard's reads from that report. Both halves are
  // a JNI contract: losing either turns a held delete back into a single one.
  assert(app.includes("crate::platform::sync_editor_document"), "native_ime_document_sync_missing");
  assert(platform.includes("pub fn sync_editor_document"), "native_ime_document_publish_missing");
  assert(platform.includes("gpuiSetDocument"), "native_ime_document_bridge_missing");
  assert(androidActivity.includes("public void gpuiSetDocument"), "android_ime_document_entry_missing");
  assert(androidActivity.includes("updateSelection"), "android_ime_selection_report_missing");
  assert(androidActivity.includes("info.initialSelStart"), "android_ime_initial_selection_missing");

  assert(android.includes('android:name=".GpuiHostActivity"'), "android_gpui_activity_missing");
  assert(android.includes('android:windowSoftInputMode="adjustResize"'), "android_keyboard_resize_missing");
  // The system's back callback is the only place the app is told which edge a
  // back gesture was swiped from, so losing it costs the side pages their swipe
  // along with the app's own back handling.
  assert(
    android.includes('android:enableOnBackInvokedCallback="true"'),
    "android_back_navigation_callback_missing"
  );
  assert(androidActivity.includes("registerOnBackInvokedCallback"), "android_back_gesture_callback_missing");
  assert(androidActivity.includes("OnBackAnimationCallback"), "android_back_gesture_progress_missing");
  assert(androidActivity.includes("getSwipeEdge()"), "android_back_gesture_edge_missing");
  assert(
    androidActivity.includes("nativeBackGesture"),
    "android_back_gesture_bridge_missing"
  );
  assert(!android.includes('android:hasCode="false"'), "android_java_host_disabled");
  assert(!android.includes("WebView"), "android_webview_host_present");
  assert(androidActivity.includes("extends Activity"), "android_gpui_host_activity_base_missing");
  assert(
    !androidActivity.includes("extends android.app.NativeActivity"),
    "android_gpui_host_activity_native_activity_base_present"
  );
  assert(
    androidActivity.includes('System.loadLibrary("vibex_mobile")'),
    "android_gpui_native_library_classloader_load_missing"
  );
  // The host contract: a SurfaceView the app hands to GPUI, input forwarded by
  // hand, window insets reported to native code, and the IME bridge kept
  // working without a NativeActivity.
  assert(androidActivity.includes("SurfaceHolder.Callback"), "android_host_surface_callback_missing");
  assert(androidActivity.includes("surfaceCreated"), "android_host_surface_created_missing");
  assert(androidActivity.includes("nativeSurfaceCreated"), "android_host_surface_bridge_missing");
  assert(androidActivity.includes("nativeSurfaceDestroyed"), "android_host_surface_release_bridge_missing");
  assert(androidActivity.includes("dispatchTouchEvent"), "android_host_touch_bridge_missing");
  assert(androidActivity.includes("dispatchKeyEvent"), "android_host_key_bridge_missing");
  assert(androidActivity.includes("setDecorFitsSystemWindows(getWindow(), false)"), "android_host_edge_to_edge_missing");
  assert(androidActivity.includes("setOnApplyWindowInsetsListener"), "android_host_insets_bridge_missing");
  assert(androidActivity.includes("nativeInsets"), "android_host_insets_export_missing");
  assert(
    androidActivity.includes("nativeInsets(bars.left, bars.top, bars.right, bars.bottom)"),
    "android_host_insets_must_be_edge_distances"
  );
  assert(
    androidHost.includes("android_insets::report(left, top, right, bottom)"),
    "android_host_insets_queue_missing"
  );
  assert(entry.includes("android_insets::observe("), "android_host_insets_observer_missing");
  assert(androidInsets.includes("window.refresh()"), "android_host_insets_refresh_missing");
  assert(androidActivity.includes("nativeKeyboardState"), "android_host_keyboard_state_missing");
  assert(androidActivity.includes("extends EditText"), "android_ime_editor_missing");
  assert(androidActivity.includes("public void gpuiShowKeyboard"), "android_ime_show_bridge_missing");
  assert(androidActivity.includes("public void gpuiHideKeyboard"), "android_ime_hide_bridge_missing");
  assert(androidActivity.includes("gpuiResetComposition"), "android_ime_reset_bridge_missing");
  assert(androidActivity.includes("InputConnectionWrapper"), "android_ime_connection_bridge_missing");
  assert(androidActivity.includes("private static native void nativeIme"), "android_ime_jni_callback_missing");
  assert(androidActivity.includes("nativeOnAppLifecycle"), "android_lifecycle_bridge_missing");
  assert(androidActivity.includes("protected void onResume()"), "android_lifecycle_resume_missing");
  assert(androidActivity.includes("protected void onPause()"), "android_lifecycle_pause_missing");
  assert(androidActivity.includes("launchPairingQrScanner"), "android_qr_scanner_launch_bridge_missing");
  assert(android.includes("android.permission.CAMERA"), "android_camera_permission_missing");
  assert(android.includes("PairingQrScannerActivity"), "android_qr_scanner_activity_missing");
  assert(
    /<activity\b(?=[^>]*android:name="\.PairingQrScannerActivity")(?=[^>]*android:exported="false")[^>]*>/.test(
      android
    ),
    "android_qr_scanner_activity_exported"
  );
  assert(androidScanner.includes('System.loadLibrary("vibex_mobile")'), "android_qr_scanner_native_library_classloader_load_missing");
  assert(androidScanner.includes("nativeOnPairingQrScanned"), "android_qr_scanner_result_bridge_missing");
  assert(androidScanner.includes("CameraSelector.DEFAULT_BACK_CAMERA"), "android_qr_scanner_camera_missing");
  assert(androidStyles.includes("ScannerTheme"), "android_qr_scanner_theme_missing");
  assert(!androidScanner.includes("debugPairingQr"), "android_qr_scanner_debug_injection_present");
  // The platform layer reaches the Android clipboard by loading this class from
  // the host Activity's loader, so its package, name and three static entry
  // points are a JNI contract: without them every copy stays inside the app.
  assert(androidClipboard.includes("package dev.gpui.mobile;"), "android_clipboard_package_missing");
  assert(androidClipboard.includes("class GpuiClipboard"), "android_clipboard_class_missing");
  for (const entry of ["setText(Activity activity, String text)", "getText(Activity activity)", "hasText(Activity activity)"]) {
    assert(androidClipboard.includes(entry), `android_clipboard_entry_missing:${entry}`);
  }
  assert(androidClipboard.includes("ClipboardManager"), "android_clipboard_manager_missing");
  assert(
    androidClipboard.includes("setPrimaryClip"),
    "android_clipboard_never_reaches_the_system_clipboard"
  );
  assert(
    androidSettings.includes("it.name == 'rustls-platform-verifier-android'"),
    "android_tls_verifier_cargo_metadata_lookup_missing"
  );
  assert(androidSettings.includes("--filter-platform', 'aarch64-linux-android"), "android_tls_verifier_cargo_target_missing");
  assert(androidSettings.includes("metadataSources.artifact()"), "android_tls_verifier_maven_artifact_source_missing");
  assert(
    androidBuild.includes("implementation 'rustls:rustls-platform-verifier:latest.release'"),
    "android_tls_verifier_aar_dependency_missing"
  );
  assert(
    androidHost.includes("Java_ai_vibex_mobile_GpuiHostActivity_nativeOnAppLifecycle"),
    "android_lifecycle_jni_export_missing"
  );

  assert(iosMain.includes("vibex_mobile_register_app();"), "ios_rust_entry_call_missing");
  assert(iosMain.includes("    gpui_ios_run_demo();"), "ios_gpui_run_loop_missing");
  assert(iosMain.includes("gpui_ios_get_window()"), "ios_gpui_window_lookup_missing");
  assert(iosMain.includes("gpui_ios_request_frame"), "ios_gpui_frame_driver_missing");
  assert(iosMain.includes("gpui_ios_set_frame_waker"), "ios_gpui_frame_waker_missing");
  assert(iosMain.includes("vibex_mobile_set_lifecycle"), "ios_lifecycle_bridge_missing");
  assert(
    iosMain.includes("return UIApplicationMain(argc, argv"),
    "ios_host_must_enter_ui_application"
  );
  assert(entry.includes("vibex_mobile_register_app"), "ios_register_app_export_missing");
  assert(entry.includes("vibex_mobile_set_lifecycle"), "ios_lifecycle_export_missing");
  assert(iosProject.includes("VibexFFI.xcframework"), "ios_xcframework_missing");
  assert(!iosProject.includes("HEADER_SEARCH_PATHS"), "ios_header_search_root_exposes_module_map");
  for (const iosSource of [iosMain, read("apps/mobile/ios/Vibex/LanDiscovery.m"), read("apps/mobile/ios/Vibex/Notifications.m")]) {
    assert(iosSource.includes('#import "vibex_mobile.h"'), "ios_host_header_import_missing");
    assert(!iosSource.includes('#import "Headers/vibex_mobile.h"'), "ios_source_header_path_reintroduces_module_map");
  }
  assert(!iosProject.includes("SWIFT_INCLUDE_PATHS"), "ios_swift_module_include_path_redundant");
  assert(iosProject.includes("ARCHS"), "ios_arm64_arch_pin_missing");
  assert(iosModuleMap.includes("module VibexFFI"), "ios_swift_module_declaration_missing");
  assert(iosModuleMap.includes('header "vibex_mobile.h"'), "ios_swift_module_header_missing");
  assert(iosProject.includes("INFOPLIST_KEY_NSCameraUsageDescription"), "ios_camera_permission_missing");
  assert(iosScanner.includes("AVCaptureMetadataOutput"), "ios_qr_scanner_camera_missing");
  assert(iosScanner.includes("vibex_mobile_pairing_qr_scanned"), "ios_qr_scanner_result_bridge_missing");
  assert(iosHeader.includes("vibex_mobile_pairing_qr_scanned"), "ios_qr_scanner_header_missing");
  assert(scanner.includes("mpsc::unbounded()"), "mobile_qr_scanner_event_queue_missing");
  assert(scanner.includes("nativeOnPairingQrScanned"), "android_qr_scanner_rust_callback_missing");
  assert(scanner.includes("vibex_mobile_pairing_qr_scanned"), "ios_qr_scanner_rust_callback_missing");
  assert(app.includes("start_scanner_result_stream"), "mobile_qr_scanner_gpui_stream_missing");
  assert(!app.includes("pairing_input"), "mobile_pairing_link_input_still_present");

  for (const marker of [
    "AgentWorkflowController",
    "conversation_turns()",
    "markdown::render",
    "composer_input",
    "drawer_open",
    "begin_send_message",
    "begin_interrupt",
    "begin_continue_turn",
    "window.safe_insets().effective()",
    // The phone draws in the platform's own faces; naming the shared constant
    // keeps every surface on the one family the theme declares.
    "font_family(theme::UI_FONT_FAMILY)",
    "create_session(request)",
    "elicitation_surfaces(ShellKind::Compact)",
    "apply_elicitation_mutation",
    "with_animation(",
    "DRAWER_VERTICAL_CANCEL_RATIO",
    // Touch pans reach the app as scroll events, so the drawer swipe must ride the
    // platform touch stream; mouse-move listeners only ever describe a tap.
    "crate::scroll_capture::capture_scroll_wheel(",
    "cx.listener(Self::drawer_pan)",
    "TouchPhase::Started"
  ]) {
    assert(app.includes(marker), `native_agent_gui_contract_missing:${marker}`);
  }
  // The approval surfaces left app.rs when the phone client was rebuilt around
  // the desktop's model and its questions moved into their own module, so the
  // contract follows them to the file that draws them.
  assert(
    question.includes("approval_surfaces(ShellKind::Compact)"),
    "native_agent_gui_contract_missing:approval_surfaces(ShellKind::Compact)"
  );
  assert(pairing.includes("has_identity_private_key"), "mobile_credential_debug_redaction_missing");
  assert(pairing.includes('url.scheme() != "vibex"'), "mobile_pairing_entry_validation_missing");
  assert(pairing.includes('"/self_hosted_relay"'), "mobile_pairing_transport_selection_missing");
  assert(storage.includes("reject_invalid"), "invalid_mobile_credential_cleanup_missing");
  assert(platform.includes("pub fn insets(window: &Window) -> WindowInsets"), "gpui_mobile_insets_api_missing");
  assert(platform.includes("safe_area_insets"), "gpui_mobile_safe_area_missing");
  assert(platform.includes("keyboard_height"), "gpui_mobile_keyboard_height_missing");
  assert(platform.includes("impl WindowInsetsExt for Window"), "gpui_mobile_insets_trait_missing");
  for (const marker of [
    "claim_pairing_offer",
    "claim_pairing_offer_via_relay",
    "AutoRemoteTransport",
    "RemoteClientType::Mobile"
  ]) {
    assert(pairing.includes(marker), `native_pairing_contract_missing:${marker}`);
  }

  // Android updates itself from the signed GitHub Release: the same Ed25519
  // manifest the desktop reads, the APK verified against its pinned digest,
  // and the system package installer as the only replacement path. Only the
  // stable channel is offered, so a prerelease build never follows rc or
  // preview tags.
  assert(manifest.includes("vibex-app-update"), "android_updater_dependency_missing");
  assert(updater.includes("UpdateChannel::Stable"), "android_update_channel_not_stable");
  assert(
    !updater.includes("UpdateChannel::Preview") && !updater.includes("UpdateChannel::Rc"),
    "android_prerelease_update_channel_present"
  );
  assert(updater.includes('cfg!(target_os = "android")'), "android_update_platform_gate_missing");
  assert(
    updater.includes("Java_ai_vibex_mobile_GpuiHostActivity_nativeOnUpdateNotificationActivated"),
    "android_update_tap_export_missing"
  );
  for (const bridge of [
    "appVersionName",
    "openExternalUrl",
    "installUpdatePackage",
    "showUpdateNotification"
  ]) {
    assert(updater.includes(`jni_str!("${bridge}")`), `android_update_jni_call_missing:${bridge}`);
    assert(androidActivity.includes(bridge), `android_update_host_method_missing:${bridge}`);
  }
  assert(
    androidActivity.includes("nativeOnUpdateNotificationActivated"),
    "android_update_tap_bridge_missing"
  );
  assert(android.includes("REQUEST_INSTALL_PACKAGES"), "android_update_install_permission_missing");
  assert(android.includes("androidx.core.content.FileProvider"), "android_update_file_provider_missing");
  assert(
    android.includes('android:authorities="${applicationId}.updates"'),
    "android_update_file_authority_missing"
  );
  assert(androidUpdatePaths.includes('path="updates/"'), "android_update_shared_path_missing");
  assert(appUpdateInstall.includes("AndroidApk"), "android_update_install_kind_missing");
  assert(appUpdateSource.includes(".tls_certs_only(roots)"), "android_update_tls_roots_missing");

  const scopedSource = [manifest, entry, app, pairing, scanner, android, androidActivity, androidScanner, androidStyles, iosMain, iosProject, iosScanner, iosHeader].join("\n");  for (const forbidden of ["Capacitor", "capacitor", "wasm-bindgen", "mobile-wasm"]) {
    assert(!scopedSource.includes(forbidden), `legacy_mobile_technology_present:${forbidden}`);
  }
}

function runSelfTest() {
  const files = new Map([
    ["Cargo.toml", source("Cargo.toml")],
    ["apps/mobile/Cargo.toml", source("apps/mobile/Cargo.toml")],
    ["apps/mobile/src/lib.rs", source("apps/mobile/src/lib.rs")],
    ["apps/mobile/src/app.rs", source("apps/mobile/src/app.rs")],
    ["apps/mobile/src/question.rs", source("apps/mobile/src/question.rs")],
    ["apps/mobile/src/pairing.rs", source("apps/mobile/src/pairing.rs")],
    ["apps/mobile/src/scanner.rs", source("apps/mobile/src/scanner.rs")],
    ["apps/mobile/src/storage.rs", source("apps/mobile/src/storage.rs")],
    ["apps/mobile/src/background_connection.rs", source("apps/mobile/src/background_connection.rs")],
    ["apps/mobile/src/discovery.rs", source("apps/mobile/src/discovery.rs")],
    ["apps/mobile/src/android_bridge.rs", source("apps/mobile/src/android_bridge.rs")],
    ["apps/mobile/src/android_host.rs", source("apps/mobile/src/android_host.rs")],
    ["apps/mobile/src/android_insets.rs", source("apps/mobile/src/android_insets.rs")],
    ["crates/vibex-remote-client/Cargo.toml", source("crates/vibex-remote-client/Cargo.toml")],
    ["crates/vibex-remote-client/src/transport.rs", source("crates/vibex-remote-client/src/transport.rs")],
    ["apps/mobile/src/platform.rs", source("apps/mobile/src/platform.rs")],
    ["apps/mobile/src/scroll_capture.rs", source("apps/mobile/src/scroll_capture.rs")],
    ["apps/mobile/src/update.rs", source("apps/mobile/src/update.rs")],
    ["crates/app-update/src/install.rs", source("crates/app-update/src/install.rs")],
    ["crates/app-update/src/source.rs", source("crates/app-update/src/source.rs")],
    ["apps/mobile/android/app/src/main/AndroidManifest.xml", source("apps/mobile/android/app/src/main/AndroidManifest.xml")],
    // The contract only needs the font's presence; its bytes are pinned by the
    // license tree digest, so the self-test stands in an empty payload.
    ["apps/mobile/android/app/src/main/assets/fonts/NotoColorEmoji.ttf", ""],
    ["apps/mobile/android/app/src/main/assets/fonts/OFL.txt", source("apps/mobile/android/app/src/main/assets/fonts/OFL.txt")],
    ["apps/mobile/android/settings.gradle", source("apps/mobile/android/settings.gradle")],
    ["apps/mobile/android/app/build.gradle", source("apps/mobile/android/app/build.gradle")],
    ["apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiHostActivity.java", source("apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiHostActivity.java")],
    ["apps/mobile/android/app/src/main/java/ai/vibex/mobile/PairingQrScannerActivity.java", source("apps/mobile/android/app/src/main/java/ai/vibex/mobile/PairingQrScannerActivity.java")],
    ["apps/mobile/android/app/src/main/java/dev/gpui/mobile/GpuiClipboard.java", source("apps/mobile/android/app/src/main/java/dev/gpui/mobile/GpuiClipboard.java")],
    ["apps/mobile/android/app/src/main/res/values/styles.xml", source("apps/mobile/android/app/src/main/res/values/styles.xml")],
    ["apps/mobile/android/app/src/main/res/xml/file_paths.xml", source("apps/mobile/android/app/src/main/res/xml/file_paths.xml")],
    ["apps/mobile/ios/project.yml", source("apps/mobile/ios/project.yml")],
    ["apps/mobile/ios/Vibex/main.m", source("apps/mobile/ios/Vibex/main.m")],
    ["apps/mobile/ios/Vibex/LanDiscovery.m", source("apps/mobile/ios/Vibex/LanDiscovery.m")],
    ["apps/mobile/ios/Vibex/Notifications.m", source("apps/mobile/ios/Vibex/Notifications.m")],
    ["apps/mobile/ios/Vibex/QRScanner.swift", source("apps/mobile/ios/Vibex/QRScanner.swift")],
    ["apps/mobile/ios/Headers/vibex_mobile.h", source("apps/mobile/ios/Headers/vibex_mobile.h")],
    ["apps/mobile/ios/Headers/module.modulemap", source("apps/mobile/ios/Headers/module.modulemap")]
  ]);

  function expectRejected(path, from, to, code) {
    expectRejectedWith(path, (original) => original.replace(from, to), code);
  }

  // A marker with more than one call site needs every occurrence gone before
  // the checker can be shown to reject it.
  function expectRejectedAll(path, from, to, code) {
    expectRejectedWith(path, (original) => original.split(from).join(to), code);
  }

  function expectRejectedWith(path, mutate, code) {
    const original = files.get(path);
    files.set(path, mutate(original));
    let rejected = false;
    try {
      validateContract((candidate) => files.get(candidate), (candidate) => candidate !== "apps/mobile-wasm" && files.has(candidate));
    } catch {
      rejected = true;
    } finally {
      files.set(path, original);
    }
    assert(rejected, code);
  }

  expectRejected(
    "apps/mobile/android/app/src/main/AndroidManifest.xml",
    'android:name=".GpuiHostActivity"',
    'android:name="android.webkit.WebView"',
    "native_mobile_checker_self_test_accepted_webview_host"
  );
  expectRejected(
    "apps/mobile/android/app/src/main/AndroidManifest.xml",
    'android:exported="false"',
    'android:exported="true"',
    "native_mobile_checker_self_test_accepted_exported_scanner"
  );
  expectRejected(
    "apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiHostActivity.java",
    "extends Activity",
    "extends android.app.NativeActivity",
    "native_mobile_checker_self_test_accepted_native_activity_host"
  );
  expectRejected(
    "apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiHostActivity.java",
    "private static native void nativeIme",
    "private static native void nativeMissing",
    "native_mobile_checker_self_test_accepted_missing_ime_bridge"
  );
  expectRejected(
    "apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiHostActivity.java",
    "nativeInsets(bars.left, bars.top, bars.right, bars.bottom)",
    "nativeInsets(bars.left, bars.top, width - bars.right, height - bars.bottom)",
    "native_mobile_checker_self_test_accepted_host_inset_rect"
  );
  expectRejected(
    "apps/mobile/src/lib.rs",
    "android_insets::observe(",
    "android_insets::missing_observer(",
    "native_mobile_checker_self_test_accepted_missing_insets_observer"
  );
  expectRejected(
    "apps/mobile/src/android_insets.rs",
    "window.refresh()",
    "window.viewport_size()",
    "native_mobile_checker_self_test_accepted_missing_insets_refresh"
  );
  expectRejected(
    "apps/mobile/Cargo.toml",
    'gpui-mobile = { package = "gpui-pre-mobile"',
    'gpui-mobile = { package = "gpui-pre-missing"',
    "native_mobile_checker_self_test_accepted_missing_gpui_mobile_dependency"
  );
  expectRejected(
    "apps/mobile/src/android_host.rs",
    "Java_ai_vibex_mobile_GpuiHostActivity_nativeOnAppLifecycle",
    "Java_ai_vibex_mobile_GpuiHostActivity_nativeMissingLifecycle",
    "native_mobile_checker_self_test_accepted_missing_android_lifecycle_bridge"
  );
  expectRejected(
    "apps/mobile/src/lib.rs",
    "pub(crate) fn open_root_window",
    'fn android_main() { gpui_mobile::android::jni::init_platform(); }\npub(crate) fn open_root_window',
    "native_mobile_checker_self_test_accepted_android_process_platform_init"
  );
  expectRejected(
    "apps/mobile/src/lib.rs",
    "pub fn android_main(_app: android_activity::AndroidApp) {",
    "pub fn android_missing(_app: android_activity::AndroidApp) {",
    "native_mobile_checker_self_test_accepted_missing_android_glue_symbol"
  );
  expectRejected(
    "apps/mobile/src/android_host.rs",
    "gpui_mobile::android::host::start_with_assets",
    "gpui_mobile::android::jni::init_platform",
    "native_mobile_checker_self_test_accepted_android_host_process_platform_init"
  );
  expectRejected(
    "apps/mobile/src/android_host.rs",
    "gpui_mobile::android::host::on_open_window",
    "gpui_mobile::android::host::on_missing_window",
    "native_mobile_checker_self_test_accepted_missing_android_host_open_window"
  );
  expectRejected(
    "apps/mobile/ios/Vibex/main.m",
    "    vibex_mobile_register_app();\n    gpui_ios_run_demo();",
    "    vibex_mobile_register_app();\n    gpui_ios_missing_demo();",
    "native_mobile_checker_self_test_accepted_missing_ios_run_loop"
  );
  expectRejected(
    "apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiHostActivity.java",
    'System.loadLibrary("vibex_mobile")',
    'System.loadLibrary("missing")',
    "native_mobile_checker_self_test_accepted_unloaded_gpui_jni"
  );
  expectRejected(
    "apps/mobile/android/app/src/main/java/ai/vibex/mobile/PairingQrScannerActivity.java",
    'System.loadLibrary("vibex_mobile")',
    'System.loadLibrary("missing")',
    "native_mobile_checker_self_test_accepted_unloaded_scanner_jni"
  );
  expectRejected(
    "apps/mobile/Cargo.toml",
    'rustls-platform-verifier = "0.7"',
    'rustls-platform-verifier = "missing"',
    "native_mobile_checker_self_test_accepted_missing_tls_verifier_dependency"
  );
  expectRejected(
    "apps/mobile/src/android_host.rs",
    "rustls_platform_verifier::android::init_with_env",
    "rustls_platform_verifier::android::missing_init",
    "native_mobile_checker_self_test_accepted_missing_tls_verifier_init"
  );
  expectRejected(
    "apps/mobile/src/android_host.rs",
    '"getApplicationContext"',
    '"getBaseContext"',
    "native_mobile_checker_self_test_accepted_activity_tls_context"
  );
  expectRejected(
    "apps/mobile/android/settings.gradle",
    "it.name == 'rustls-platform-verifier-android'",
    "it.name == 'missing-platform-verifier-android'",
    "native_mobile_checker_self_test_accepted_missing_tls_verifier_repository"
  );
  expectRejected(
    "apps/mobile/android/app/build.gradle",
    "implementation 'rustls:rustls-platform-verifier:latest.release'",
    "implementation 'rustls:missing:latest.release'",
    "native_mobile_checker_self_test_accepted_missing_tls_verifier_aar"
  );
  expectRejected(
    "crates/vibex-remote-client/Cargo.toml",
    "webpki-root-certs.workspace = true",
    "webpki-root-certs.workspace = false",
    "native_mobile_checker_self_test_accepted_missing_webpki_roots"
  );
  expectRejected(
    "crates/vibex-remote-client/src/transport.rs",
    ".tls_certs_only(roots)",
    ".tls_certs_merge(roots)",
    "native_mobile_checker_self_test_accepted_platform_http_verifier"
  );
  expectRejected(
    "crates/vibex-remote-client/src/transport.rs",
    "connect_async_tls_with_config",
    "connect_async_with_config",
    "native_mobile_checker_self_test_accepted_platform_websocket_verifier"
  );
  expectRejected(
    "crates/vibex-remote-client/src/transport.rs",
    ".tls_certs_only(roots)",
    ".danger_accept_invalid_certs(true).tls_certs_only(roots)",
    "native_mobile_checker_self_test_accepted_disabled_tls_verification"
  );
  expectRejected(
    "crates/vibex-remote-client/src/transport.rs",
    "remote_http_client_for_url(&url)?",
    "reqwest::Client::new().post(endpoint)",
    "native_mobile_checker_self_test_accepted_http_client_bypass"
  );
  expectRejected(
    "apps/mobile/ios/Headers/module.modulemap",
    "module VibexFFI",
    "module MissingFFI",
    "native_mobile_checker_self_test_accepted_missing_swift_module_declaration"
  );
  expectRejected(
    "apps/mobile/ios/project.yml",
    "CLANG_ENABLE_OBJC_ARC: YES",
    'CLANG_ENABLE_OBJC_ARC: YES\n        HEADER_SEARCH_PATHS: ["$(inherited)", "$(SRCROOT)"]',
    "native_mobile_checker_self_test_accepted_duplicate_module_map_path"
  );
  expectRejected(
    "apps/mobile/ios/Vibex/main.m",
    '#import "vibex_mobile.h"',
    '#import "Headers/vibex_mobile.h"',
    "native_mobile_checker_self_test_accepted_source_module_map_path"
  );
  expectRejected(
    "apps/mobile/ios/Headers/vibex_mobile.h",
    "vibex_mobile_pairing_qr_scanned",
    "vibex_mobile_missing_qr_scanned",
    "native_mobile_checker_self_test_accepted_missing_ios_qr_bridge_header"
  );
  expectRejected(
    "apps/mobile/src/update.rs",
    "UpdateChannel::Stable",
    "UpdateChannel::Preview",
    "native_mobile_checker_self_test_accepted_prerelease_update_channel"
  );
  // Dropping the document publish or the Java side of it is exactly how the
  // held-Backspace repeat regressed, so each half has to fail the check alone.
  expectRejected(
    "apps/mobile/src/app.rs",
    "crate::platform::sync_editor_document",
    "crate::platform::hidden_editor_document",
    "native_mobile_checker_self_test_accepted_missing_ime_document_sync"
  );
  expectRejected(
    "apps/mobile/src/platform.rs",
    "gpuiSetDocument",
    "gpuiSetHiddenDocument",
    "native_mobile_checker_self_test_accepted_missing_ime_document_bridge"
  );
  expectRejected(
    "apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiHostActivity.java",
    "info.initialSelStart",
    "info.hiddenSelStart",
    "native_mobile_checker_self_test_accepted_missing_ime_initial_selection"
  );
  expectRejected(
    "apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiHostActivity.java",
    "public void installUpdatePackage",
    "public void installMissingPackage",
    "native_mobile_checker_self_test_accepted_missing_update_install_bridge"
  );
  expectRejected(
    "apps/mobile/android/app/src/main/java/dev/gpui/mobile/GpuiClipboard.java",
    "public static void setText(Activity activity, String text)",
    "public static void putText(Activity activity, String text)",
    "native_mobile_checker_self_test_accepted_renamed_clipboard_entry_point"
  );
  expectRejectedAll(
    "apps/mobile/src/question.rs",
    "approval_surfaces(ShellKind::Compact)",
    "approval_surfaces(ShellKind::Roomy)",
    "native_mobile_checker_self_test_accepted_missing_approval_surfaces"
  );
}

try {
  if (SELF_TEST) runSelfTest();
  else validateContract();
  console.log(SELF_TEST ? "Native mobile checker self-test passed" : "Native GPUI mobile contract verified");
} catch (error) {
  console.error(error instanceof Error ? error.message : "native_mobile_check_failed");
  process.exitCode = 1;
}

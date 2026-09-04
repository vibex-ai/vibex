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
    "apps/mobile/src/input.rs",
    "apps/mobile/src/pairing.rs",
    "apps/mobile/src/scanner.rs",
    "apps/mobile/src/storage.rs",
    "crates/vibex-remote-client/Cargo.toml",
    "crates/vibex-remote-client/src/transport.rs",
    "apps/mobile/assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf",
    "apps/mobile/assets/fonts/wqy-microhei/wqy-microhei.ttc",
    "apps/mobile/android/app/src/main/AndroidManifest.xml",
    "apps/mobile/android/settings.gradle",
    "apps/mobile/android/app/build.gradle",
    "apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiNativeActivity.java",
    "apps/mobile/android/app/src/main/java/ai/vibex/mobile/PairingQrScannerActivity.java",
    "apps/mobile/android/app/src/main/res/values/styles.xml",
    "apps/mobile/ios/project.yml",
    "apps/mobile/ios/Vibex/main.m",
    "apps/mobile/ios/Vibex/QRScanner.swift",
    "apps/mobile/ios/Headers/vibex_mobile.h",
    "apps/mobile/ios/Headers/module.modulemap",
    "vendor/zed/crates/gpui_android/src/ime.rs",
    "vendor/zed/crates/gpui_android/src/platform.rs",
    "vendor/zed/crates/gpui_android/src/window.rs"
  ]) {
    assert(exists(path), `native_mobile_file_missing:${path}`);
  }

  const workspace = read("Cargo.toml");
  const manifest = read("apps/mobile/Cargo.toml");
  const entry = read("apps/mobile/src/lib.rs");
  const app = read("apps/mobile/src/app.rs");
  const input = read("apps/mobile/src/input.rs");
  const pairing = read("apps/mobile/src/pairing.rs");
  const scanner = read("apps/mobile/src/scanner.rs");
  const storage = read("apps/mobile/src/storage.rs");
  const remoteManifest = read("crates/vibex-remote-client/Cargo.toml");
  const remoteTransport = read("crates/vibex-remote-client/src/transport.rs");
  const gpuiWindow = read("vendor/zed/crates/gpui/src/window.rs");
  const android = read("apps/mobile/android/app/src/main/AndroidManifest.xml");
  const androidSettings = read("apps/mobile/android/settings.gradle");
  const androidBuild = read("apps/mobile/android/app/build.gradle");
  const androidActivity = read("apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiNativeActivity.java");
  const androidScanner = read("apps/mobile/android/app/src/main/java/ai/vibex/mobile/PairingQrScannerActivity.java");
  const androidStyles = read("apps/mobile/android/app/src/main/res/values/styles.xml");
  const androidIme = read("vendor/zed/crates/gpui_android/src/ime.rs");
  const androidPlatform = read("vendor/zed/crates/gpui_android/src/platform.rs");
  const androidWindow = read("vendor/zed/crates/gpui_android/src/window.rs");
  const iosMain = read("apps/mobile/ios/Vibex/main.m");
  const iosProject = read("apps/mobile/ios/project.yml");
  const iosScanner = read("apps/mobile/ios/Vibex/QRScanner.swift");
  const iosHeader = read("apps/mobile/ios/Headers/vibex_mobile.h");
  const iosModuleMap = read("apps/mobile/ios/Headers/module.modulemap");

  assert(workspace.includes('"apps/mobile"'), "native_mobile_workspace_member_missing");
  assert(!workspace.includes('"apps/mobile-wasm"'), "legacy_mobile_workspace_member_present");
  assert(manifest.includes('crate-type = ["cdylib", "staticlib", "rlib"]'), "native_mobile_crate_types_invalid");
  assert(manifest.includes('../../vendor/zed/crates/gpui_android'), "vibex_zed_android_backend_missing");
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
  assert(entry.includes("gpui_platform::android_init(android_app)"), "android_gpui_init_missing");
  assert(entry.includes("rustls_platform_verifier::android::init_with_env"), "android_tls_platform_verifier_init_missing");
  assert(entry.includes('"getApplicationContext"'), "android_tls_application_context_missing");
  assert(
    entry.indexOf("initialize_android_tls(&android_app)") < entry.indexOf("gpui_platform::android_init(android_app)"),
    "android_tls_platform_verifier_init_order_invalid"
  );
  assert(entry.includes("scanner::initialize_android(&android_app)"), "android_qr_scanner_init_missing");
  assert(
    entry.includes("gpui_platform::application()") ||
      entry.includes("gpui::Application::with_platform(platform)"),
    "native_gpui_application_missing"
  );
  assert(entry.includes('target_os = "ios"'), "ios_rust_entry_missing");
  assert(entry.includes("assets::load_fonts(cx)"), "native_mobile_font_loading_missing");
  assert(input.includes("self.focus_handle.focus(window, cx)"), "native_text_input_focus_missing");
  assert(input.includes("window.show_soft_keyboard()"), "native_text_input_keyboard_missing");

  assert(android.includes('android:name=".GpuiNativeActivity"'), "android_gpui_activity_missing");
  assert(android.includes('android:value="vibex_mobile"'), "android_native_library_name_invalid");
  assert(android.includes('android:windowSoftInputMode="adjustResize"'), "android_keyboard_resize_missing");
  assert(!android.includes('android:hasCode="false"'), "android_java_host_disabled");
  assert(!android.includes("WebView"), "android_webview_host_present");
  assert(androidActivity.includes("extends NativeActivity"), "android_native_activity_base_missing");
  assert(androidActivity.includes('System.loadLibrary("vibex_mobile")'), "android_gpui_native_library_classloader_load_missing");
  assert(androidActivity.includes("extends EditText"), "android_ime_editor_missing");
  assert(androidActivity.includes("showGpuiKeyboard"), "android_ime_show_bridge_missing");
  assert(androidActivity.includes("nativeReplaceText"), "android_ime_replace_callback_missing");
  assert(androidActivity.includes("nativeSetSelection"), "android_ime_selection_callback_missing");
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
  assert(androidIme.includes("VecDeque<ImeEvent>"), "android_ime_event_queue_missing");
  assert(androidIme.includes("nativeReplaceText"), "android_ime_jni_replace_missing");
  assert(androidIme.includes("nativeSetSelection"), "android_ime_jni_selection_missing");
  assert(androidWindow.includes("update_java_editor"), "android_ime_editor_sync_missing");
  assert(androidWindow.includes("apply_pending_ime_events"), "android_ime_event_drain_missing");
  assert(androidPlatform.includes("Mutex<Option<AndroidApp>>"), "android_reentrant_activity_handle_missing");
  assert(androidPlatform.includes("clear_events()"), "android_stale_ime_event_cleanup_missing");

  assert(iosMain.includes("vibex_mobile_main();"), "ios_rust_entry_call_missing");
  assert(!iosMain.includes("UIApplicationMain"), "ios_host_double_enters_ui_application");
  assert(iosProject.includes("VibexFFI.xcframework"), "ios_xcframework_missing");
  assert(
    iosProject.includes('HEADER_SEARCH_PATHS: ["$(inherited)", "$(SRCROOT)"]'),
    "ios_header_search_root_exposes_module_map"
  );
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
    "approval_surfaces(ShellKind::Compact)",
    "composer_input",
    "drawer_open",
    "begin_send_message",
    "begin_interrupt",
    "begin_continue_turn",
    "window.insets().effective()",
    'font_family("IBM Plex Sans")',
    "create_session(request)",
    "elicitation_surfaces(ShellKind::Compact)",
    "apply_elicitation_mutation",
    "with_animation(",
    "DRAWER_VERTICAL_CANCEL_RATIO",
    // Touch pans reach the app as scroll events, so the drawer swipe must ride the
    // platform touch stream; mouse-move listeners only ever describe a tap.
    "capture_scroll_wheel(cx.listener(Self::drawer_pan))",
    "TouchPhase::Started"
  ]) {
    assert(app.includes(marker), `native_agent_gui_contract_missing:${marker}`);
  }
  assert(pairing.includes("has_identity_private_key"), "mobile_credential_debug_redaction_missing");
  assert(pairing.includes('url.scheme() != "vibex"'), "mobile_pairing_entry_validation_missing");
  assert(pairing.includes('"/self_hosted_relay"'), "mobile_pairing_transport_selection_missing");
  assert(storage.includes("reject_invalid"), "invalid_mobile_credential_cleanup_missing");
  assert(gpuiWindow.includes("pub fn insets(&self) -> WindowInsets"), "gpui_mobile_insets_api_missing");
  assert(gpuiWindow.includes("on_insets_changed"), "gpui_mobile_insets_refresh_missing");
  for (const marker of [
    "claim_pairing_offer",
    "claim_pairing_offer_via_relay",
    "AutoRemoteTransport",
    "RemoteClientType::Mobile"
  ]) {
    assert(pairing.includes(marker), `native_pairing_contract_missing:${marker}`);
  }

  const scopedSource = [manifest, entry, app, input, pairing, scanner, android, androidActivity, androidScanner, androidStyles, iosMain, iosProject, iosScanner, iosHeader].join("\n");
  for (const forbidden of ["Capacitor", "capacitor", "wasm-bindgen", "mobile-wasm"]) {
    assert(!scopedSource.includes(forbidden), `legacy_mobile_technology_present:${forbidden}`);
  }
}

function runSelfTest() {
  const files = new Map([
    ["Cargo.toml", source("Cargo.toml")],
    ["apps/mobile/Cargo.toml", source("apps/mobile/Cargo.toml")],
    ["apps/mobile/src/lib.rs", source("apps/mobile/src/lib.rs")],
    ["apps/mobile/src/app.rs", source("apps/mobile/src/app.rs")],
    ["apps/mobile/src/input.rs", source("apps/mobile/src/input.rs")],
    ["apps/mobile/src/pairing.rs", source("apps/mobile/src/pairing.rs")],
    ["apps/mobile/src/scanner.rs", source("apps/mobile/src/scanner.rs")],
    ["apps/mobile/src/storage.rs", source("apps/mobile/src/storage.rs")],
    ["crates/vibex-remote-client/Cargo.toml", source("crates/vibex-remote-client/Cargo.toml")],
    ["crates/vibex-remote-client/src/transport.rs", source("crates/vibex-remote-client/src/transport.rs")],
    ["vendor/zed/crates/gpui/src/window.rs", source("vendor/zed/crates/gpui/src/window.rs")],
    ["apps/mobile/assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf", "font"],
    ["apps/mobile/assets/fonts/wqy-microhei/wqy-microhei.ttc", "font"],
    ["apps/mobile/android/app/src/main/AndroidManifest.xml", source("apps/mobile/android/app/src/main/AndroidManifest.xml")],
    ["apps/mobile/android/settings.gradle", source("apps/mobile/android/settings.gradle")],
    ["apps/mobile/android/app/build.gradle", source("apps/mobile/android/app/build.gradle")],
    ["apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiNativeActivity.java", source("apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiNativeActivity.java")],
    ["apps/mobile/android/app/src/main/java/ai/vibex/mobile/PairingQrScannerActivity.java", source("apps/mobile/android/app/src/main/java/ai/vibex/mobile/PairingQrScannerActivity.java")],
    ["apps/mobile/android/app/src/main/res/values/styles.xml", source("apps/mobile/android/app/src/main/res/values/styles.xml")],
    ["apps/mobile/ios/project.yml", source("apps/mobile/ios/project.yml")],
    ["apps/mobile/ios/Vibex/main.m", source("apps/mobile/ios/Vibex/main.m")],
    ["apps/mobile/ios/Vibex/QRScanner.swift", source("apps/mobile/ios/Vibex/QRScanner.swift")],
    ["apps/mobile/ios/Headers/vibex_mobile.h", source("apps/mobile/ios/Headers/vibex_mobile.h")],
    ["apps/mobile/ios/Headers/module.modulemap", source("apps/mobile/ios/Headers/module.modulemap")],
    ["vendor/zed/crates/gpui_android/src/ime.rs", source("vendor/zed/crates/gpui_android/src/ime.rs")],
    ["vendor/zed/crates/gpui_android/src/platform.rs", source("vendor/zed/crates/gpui_android/src/platform.rs")],
    ["vendor/zed/crates/gpui_android/src/window.rs", source("vendor/zed/crates/gpui_android/src/window.rs")]
  ]);

  function expectRejected(path, from, to, code) {
    const original = files.get(path);
    files.set(path, original.replace(from, to));
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
    'android:name=".GpuiNativeActivity"',
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
    "apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiNativeActivity.java",
    "extends NativeActivity",
    "extends Activity",
    "native_mobile_checker_self_test_accepted_non_native_activity_host"
  );
  expectRejected(
    "apps/mobile/android/app/src/main/java/ai/vibex/mobile/GpuiNativeActivity.java",
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
    "apps/mobile/src/lib.rs",
    "rustls_platform_verifier::android::init_with_env",
    "rustls_platform_verifier::android::missing_init",
    "native_mobile_checker_self_test_accepted_missing_tls_verifier_init"
  );
  expectRejected(
    "apps/mobile/src/lib.rs",
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
    "apps/mobile/ios/Headers/vibex_mobile.h",
    "vibex_mobile_pairing_qr_scanned",
    "vibex_mobile_missing_qr_scanned",
    "native_mobile_checker_self_test_accepted_missing_ios_qr_bridge_header"
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

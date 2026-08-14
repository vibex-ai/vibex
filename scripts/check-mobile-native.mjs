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
    "apps/mobile/src/storage.rs",
    "apps/mobile/assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf",
    "apps/mobile/assets/fonts/wqy-microhei/wqy-microhei.ttc",
    "apps/mobile/android/app/src/main/AndroidManifest.xml",
    "apps/mobile/ios/project.yml",
    "apps/mobile/ios/Vibex/main.m"
  ]) {
    assert(exists(path), `native_mobile_file_missing:${path}`);
  }

  const workspace = read("Cargo.toml");
  const manifest = read("apps/mobile/Cargo.toml");
  const entry = read("apps/mobile/src/lib.rs");
  const app = read("apps/mobile/src/app.rs");
  const input = read("apps/mobile/src/input.rs");
  const pairing = read("apps/mobile/src/pairing.rs");
  const storage = read("apps/mobile/src/storage.rs");
  const gpuiWindow = read("vendor/zed/crates/gpui/src/window.rs");
  const android = read("apps/mobile/android/app/src/main/AndroidManifest.xml");
  const iosMain = read("apps/mobile/ios/Vibex/main.m");
  const iosProject = read("apps/mobile/ios/project.yml");

  assert(workspace.includes('"apps/mobile"'), "native_mobile_workspace_member_missing");
  assert(!workspace.includes('"apps/mobile-wasm"'), "legacy_mobile_workspace_member_present");
  assert(manifest.includes('crate-type = ["cdylib", "staticlib", "rlib"]'), "native_mobile_crate_types_invalid");
  assert(manifest.includes('../../vendor/zed/crates/gpui_android'), "vibex_zed_android_backend_missing");
  assert(entry.includes("gpui_platform::android_init(android_app)"), "android_gpui_init_missing");
  assert(entry.includes("gpui_platform::application()"), "native_gpui_application_missing");
  assert(entry.includes('target_os = "ios"'), "ios_rust_entry_missing");
  assert(entry.includes("assets::load_fonts(cx)"), "native_mobile_font_loading_missing");
  assert(input.includes("self.focus_handle.focus(window, cx)"), "native_text_input_focus_missing");
  assert(input.includes("window.show_soft_keyboard()"), "native_text_input_keyboard_missing");

  assert(android.includes("android.app.NativeActivity"), "android_native_activity_missing");
  assert(android.includes('android:value="vibex_mobile"'), "android_native_library_name_invalid");
  assert(android.includes('android:windowSoftInputMode="adjustResize"'), "android_keyboard_resize_missing");
  assert(!android.includes("WebView"), "android_webview_host_present");

  assert(iosMain.includes("vibex_mobile_main();"), "ios_rust_entry_call_missing");
  assert(!iosMain.includes("UIApplicationMain"), "ios_host_double_enters_ui_application");
  assert(iosProject.includes("VibexFFI.xcframework"), "ios_xcframework_missing");

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

  const scopedSource = [manifest, entry, app, input, pairing, android, iosMain, iosProject].join("\n");
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
    ["apps/mobile/src/storage.rs", source("apps/mobile/src/storage.rs")],
    ["vendor/zed/crates/gpui/src/window.rs", source("vendor/zed/crates/gpui/src/window.rs")],
    ["apps/mobile/assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf", "font"],
    ["apps/mobile/assets/fonts/wqy-microhei/wqy-microhei.ttc", "font"],
    ["apps/mobile/android/app/src/main/AndroidManifest.xml", source("apps/mobile/android/app/src/main/AndroidManifest.xml")],
    ["apps/mobile/ios/project.yml", source("apps/mobile/ios/project.yml")],
    ["apps/mobile/ios/Vibex/main.m", source("apps/mobile/ios/Vibex/main.m")]
  ]);
  files.set(
    "apps/mobile/android/app/src/main/AndroidManifest.xml",
    files.get("apps/mobile/android/app/src/main/AndroidManifest.xml").replace(
      "android.app.NativeActivity",
      "android.webkit.WebView"
    )
  );
  let rejected = false;
  try {
    validateContract((path) => files.get(path), (path) => path !== "apps/mobile-wasm" && files.has(path));
  } catch {
    rejected = true;
  }
  assert(rejected, "native_mobile_checker_self_test_accepted_webview_host");
}

try {
  if (SELF_TEST) runSelfTest();
  else validateContract();
  console.log(SELF_TEST ? "Native mobile checker self-test passed" : "Native GPUI mobile contract verified");
} catch (error) {
  console.error(error instanceof Error ? error.message : "native_mobile_check_failed");
  process.exitCode = 1;
}

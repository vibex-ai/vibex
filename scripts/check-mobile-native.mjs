// The phone client's contract, checked without an Android device or an Xcode.
//
// `apps/mobile-nsh` is the only mobile client in this tree. Its Kotlin/Swift
// hosts own the platform surface the `nsh` runtime cannot reach — the camera,
// the scanner, notifications, the connection service, LAN discovery and the
// share intake — and they meet the Rust library at a JNI/FFI boundary that no
// compiler on this machine checks end to end. A typo there is an
// `UnsatisfiedLinkError` at runtime or a link error on a Mac, both invisible
// until someone builds for a phone.
//
// So the contract is pinned here: the host files exist, each native method the
// Java side declares has a Rust export, the manifest keeps the permissions and
// components those features need, and the iOS project keeps the same keys its
// plists and Swift code expect. The upgrade path (applicationId, versionCode,
// bundle ids) is pinned next door in `apps/mobile-nsh/scripts/check-upgrade.mjs`.
//
// Run directly or through `pnpm check:mobile-native`.

import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SELF_TEST = process.argv.includes("--self-test");

const ANDROID = "apps/mobile-nsh/android/app/src/main/AndroidManifest.xml";
const ANDROID_BUILD = "apps/mobile-nsh/android/app/build.gradle";
const ANDROID_ACTIVITY =
  "apps/mobile-nsh/android/app/src/main/java/com/longbridge/nsh/MainActivity.java";
const ANDROID_HOST_JAVA = "apps/mobile-nsh/android/app/src/main/java/ai/vibex/mobile/nsh/NshHost.java";
const ANDROID_SCANNER =
  "apps/mobile-nsh/android/app/src/main/java/com/longbridge/nsh/ScannerActivity.java";
const ANDROID_SERVICE =
  "apps/mobile-nsh/android/app/src/main/java/com/longbridge/nsh/ConnectionService.java";
const ANDROID_DISCOVERY =
  "apps/mobile-nsh/android/app/src/main/java/com/longbridge/nsh/LanDiscovery.java";
const ANDROID_PATHS = "apps/mobile-nsh/android/app/src/main/res/xml/file_paths.xml";
const ANDROID_SHORTCUTS = "apps/mobile-nsh/android/app/src/main/res/xml/shortcuts.xml";
const ANDROID_PLATFORM = "apps/mobile-nsh/src/platform/android.rs";

const IOS_PROJECT = "apps/mobile-nsh/ios/project.yml";
const IOS_PLIST = "apps/mobile-nsh/ios/Resources/Info.plist";
const IOS_DELEGATE = "apps/mobile-nsh/ios/Vibex/AppDelegate.swift";
const IOS_BRIDGE = "apps/mobile-nsh/ios/Vibex/HostBridge.swift";
const IOS_DISCOVERY = "apps/mobile-nsh/ios/Vibex/LanDiscovery.swift";
const IOS_SHARE_APP = "apps/mobile-nsh/ios/Vibex/ShareIntake.swift";
const IOS_SHARE_EXTENSION = "apps/mobile-nsh/ios/VibexShare/ShareViewController.swift";

/** Every host file the client is made of. */
const HOST_FILES = [
  ANDROID,
  ANDROID_BUILD,
  ANDROID_ACTIVITY,
  ANDROID_HOST_JAVA,
  ANDROID_SCANNER,
  ANDROID_SERVICE,
  ANDROID_DISCOVERY,
  ANDROID_PATHS,
  ANDROID_SHORTCUTS,
  ANDROID_PLATFORM,
  IOS_PROJECT,
  IOS_PLIST,
  IOS_DELEGATE,
  IOS_BRIDGE,
  IOS_DISCOVERY,
  IOS_SHARE_APP,
  IOS_SHARE_EXTENSION
];

function source(path) {
  return readFileSync(join(ROOT, path), "utf8");
}

function assert(condition, code) {
  if (!condition) throw new Error(code);
}

/** `static native` names a Java host declares, in source order. */
function javaNativeMethods(java) {
  return [...java.matchAll(/static native\s+[A-Za-z<>[\].]+\s+([A-Za-z0-9_]+)\s*\(/gu)].map(
    (match) => match[1]
  );
}

/** The `Java_…` symbols the Rust Android platform module exports. */
function rustJniExports(rust) {
  return [...rust.matchAll(/fn (Java_[A-Za-z0-9_]+)\s*\(/gu)].map((match) => match[1]);
}

/** A JNI symbol name for a host class and native method, as `javac -h` writes it. */
function jniSymbol(packageName, className, method) {
  const path = `${packageName}.${className}`.replaceAll("_", "_1").replaceAll(".", "_");
  return `Java_${path}_${method.replaceAll("_", "_1")}`;
}

function validateContract(read = source, exists = (path) => existsSync(join(ROOT, path))) {
  assert(!exists("apps/mobile-wasm"), "legacy_mobile_wasm_tree_present");
  // The GPUI client was removed; a copy coming back would ship two clients that
  // both claim the same application id and the same store.
  assert(!exists("apps/mobile"), "removed_gpui_mobile_tree_present");
  assert(!exists("apps/mobile/nsh.json"), "removed_gpui_mobile_app_present");

  for (const path of HOST_FILES) {
    assert(exists(path), `mobile_host_file_missing:${path}`);
  }

  const android = read(ANDROID);
  const androidBuild = read(ANDROID_BUILD);
  const activity = read(ANDROID_ACTIVITY);
  const hostJava = read(ANDROID_HOST_JAVA);
  const scanner = read(ANDROID_SCANNER);
  const service = read(ANDROID_SERVICE);
  const discovery = read(ANDROID_DISCOVERY);
  const paths = read(ANDROID_PATHS);
  const shortcuts = read(ANDROID_SHORTCUTS);
  const platform = read(ANDROID_PLATFORM);
  const iosProject = read(IOS_PROJECT);
  const plist = read(IOS_PLIST);
  const delegate = read(IOS_DELEGATE);
  const bridge = read(IOS_BRIDGE);
  const iosDiscovery = read(IOS_DISCOVERY);
  const shareApp = read(IOS_SHARE_APP);
  const shareExtension = read(IOS_SHARE_EXTENSION);

  // The Java host declares what it needs from the library; the library has to
  // export it. A method declared and not exported is an UnsatisfiedLinkError
  // the first time that path runs, which is exactly the kind of failure no
  // unit test on a desktop reaches.
  const exports = new Set(rustJniExports(platform));
  for (const method of javaNativeMethods(hostJava)) {
    const symbol = jniSymbol("ai.vibex.mobile.nsh", "NshHost", method);
    assert(exports.has(symbol), `android_jni_export_missing:${method}`);
  }
  // The view comes from the nsh AAR and calls back into the same class.
  assert(
    exports.has("Java_ai_vibex_mobile_nsh_NshHost_nativeRunRoot"),
    "android_jni_root_export_missing"
  );
  for (const required of [
    'System.loadLibrary("nsh")',
    "public final class NshHost"
  ]) {
    assert(hostJava.includes(required), `android_host_library_contract_missing:${required}`);
  }

  // The app may not be built out of a javac-invisible package: the Rust JNI
  // names, the Java package and the namespace all have to agree.
  assert(
    androidBuild.includes("namespace 'ai.vibex.mobile.nsh'"),
    "android_namespace_missing: the JNI symbol names depend on the Java package"
  );

  // Android: the platform surface the client needs, and nothing it must not
  // add back. A WebView host would be a different product; the native activity
  // glue was removed on purpose.
  for (const permission of [
    "android.permission.CAMERA",
    "android.permission.INTERNET",
    "android.permission.POST_NOTIFICATIONS",
    "android.permission.FOREGROUND_SERVICE",
    "android.permission.FOREGROUND_SERVICE_CONNECTED_DEVICE",
    "android.permission.NEARBY_WIFI_DEVICES",
    "android.permission.CHANGE_WIFI_MULTICAST_STATE",
    "android.permission.REQUEST_INSTALL_PACKAGES"
  ]) {
    assert(android.includes(permission), `android_permission_missing:${permission}`);
  }
  assert(
    /<service\b(?=[^>]*android:foregroundServiceType="connectedDevice")(?=[^>]*android:stopWithTask="false")[^>]*>/u.test(
      android
    ),
    "android_connection_service_metadata_missing"
  );
  assert(
    android.includes('android:name="com.longbridge.nsh.ConnectionService"'),
    "android_connection_service_missing"
  );
  assert(
    /<activity\b(?=[^>]*android:name="com\.longbridge\.nsh\.ScannerActivity")(?=[^>]*android:exported="false")[^>]*>/u.test(
      android
    ),
    "android_scanner_activity_exported"
  );
  assert(
    /<provider\b(?=[^>]*android:authorities="\$\{applicationId\}\.files")[^>]*>/u.test(android),
    "android_file_provider_missing"
  );
  assert(
    android.includes('android:name="androidx.core.content.FileProvider"'),
    "android_file_provider_class_missing"
  );
  assert(paths.includes('path="camera/"'), "android_camera_share_path_missing");
  assert(paths.includes('path="Download/updates/"'), "android_update_share_path_missing");
  assert(shortcuts.includes('android:action="ai.vibex.mobile.NEW_SESSION"'), "android_shortcut_action_missing");
  assert(
    shortcuts.includes('android:targetPackage="ai.vibex.mobile"'),
    "android_shortcut_target_missing"
  );
  assert(
    shortcuts.includes('android:targetClass="com.longbridge.nsh.MainActivity"'),
    "android_shortcut_class_missing"
  );
  assert(
    android.includes('android:windowSoftInputMode="adjustResize"'),
    "android_keyboard_resize_missing"
  );
  assert(
    android.includes('android:launchMode="singleTask"'),
    "android_launch_mode_missing: a shared link would stack a second task"
  );
  assert(!android.includes("WebView"), "android_webview_host_present");
  assert(!android.includes("NativeActivity"), "android_native_activity_host_present");
  assert(!android.includes('android:hasCode="false"'), "android_java_host_disabled");

  // The hidden platform surface: each host class exists for one job, and the
  // Activity is the only place that drives it.
  assert(activity.includes("extends Activity"), "android_activity_base_missing");
  assert(activity.includes("onNewIntent"), "android_activity_reentry_missing");
  assert(activity.includes("onActivityResult"), "android_activity_result_missing");
  assert(activity.includes("registerReceiver"), "android_activity_receiver_missing");
  assert(activity.includes("FileProvider.getUriForFile"), "android_activity_file_provider_missing");
  assert(
    activity.includes("ConnectionService.start(") && activity.includes("ConnectionService.stop("),
    "android_activity_service_bridge_missing"
  );
  // `startForegroundService` alone only schedules the service: Android kills a
  // service that never calls `startForeground` within a few seconds, which is
  // the connection dropping the moment the app leaves the foreground.
  assert(
    service.includes("startForegroundService"),
    "android_service_start_missing: the connection must survive backgrounding"
  );
  assert(
    /startForeground\(\s*NOTIFICATION_ID\s*,/u.test(service),
    "android_service_foreground_promotion_missing"
  );
  assert(service.includes("NotificationChannel"), "android_service_channel_missing");
  assert(scanner.includes("class ScannerActivity"), "android_scanner_class_missing");
  assert(scanner.includes("ImageReader") || scanner.includes("Camera"), "android_scanner_camera_missing");
  assert(scanner.includes("MultiFormatReader"), "android_scanner_decoder_missing");
  assert(service.includes("startForeground"), "android_service_foreground_missing");
  assert(
    discovery.includes("NsdManager") && discovery.includes("discoverServices"),
    "android_discovery_browser_missing"
  );
  assert(
    /_vibex\._tcp/u.test(discovery),
    "android_discovery_service_type_missing"
  );

  // iOS: the project has to describe the bundle the plists and Swift files
  // expect, and keep the keys the camera, the scanner and the share intake use.
  assert(
    iosProject.includes("ASSETCATALOG_COMPILER_APPICON_NAME: AppIcon"),
    "ios_app_icon_missing"
  );
  assert(
    iosProject.includes("TARGETED_DEVICE_FAMILY: \"1\""),
    "ios_device_family_missing"
  );
  assert(
    iosProject.includes("iOS: \"17.0\""),
    "ios_deployment_target_missing"
  );
  assert(
    iosProject.includes("SWIFT_OBJC_BRIDGING_HEADER: Vibex/Vibex-Bridging-Header.h"),
    "ios_bridging_header_missing"
  );
  // The flag has to be a real linker argument, not just mentioned: the comment
  // beside it names it too, which is how it nearly passed while removed.
  assert(
    /^\s+- "-force_load"$/mu.test(iosProject) &&
      iosProject.includes("$(SRCROOT)/Frameworks/libvibex_mobile_nsh.a"),
    "ios_static_library_link_missing: nsh's UI set is only reachable with -force_load"
  );
  assert(
    iosProject.includes("- path: Resources"),
    "ios_resources_missing: the app icon and Info.plist live there"
  );
  for (const key of [
    "NSCameraUsageDescription",
    "NSLocalNetworkUsageDescription",
    "NSBonjourServices",
    "NSPhotoLibraryUsageDescription",
    "CFBundleURLTypes",
    "UILaunchScreen"
  ]) {
    assert(plist.includes(key), `ios_plist_key_missing:${key}`);
  }
  assert(plist.includes("<string>_vibex._tcp</string>"), "ios_bonjour_service_missing");
  assert(plist.includes("<string>vibex</string>"), "ios_url_scheme_missing");
  // The share handoff is a URL, not an App Group: a free Apple ID — the one
  // capability the unsigned install path has — cannot sign a managed
  // entitlement, and asking for one would break the install and the upgrade.
  assert(!iosProject.includes("CODE_SIGN_ENTITLEMENTS"), "ios_entitlement_requested");
  for (const [side, file] of [
    ["app", shareApp],
    ["extension", shareExtension]
  ]) {
    for (const value of ['"vibex"', '"share"', '"text"', '"pasteboard"']) {
      assert(
        file.includes(value),
        `ios_share_handoff_mismatch:${side}:${value}`
      );
    }
  }
  assert(delegate.includes('"share"'), "ios_share_route_missing");
  assert(delegate.includes("ShareIntake.take"), "ios_share_intake_missing");
  assert(delegate.includes("UNUserNotificationCenter") === false, "ios_notifications_moved_check_stale");
  // iOS has no foreground service: this assertion is what keeps the connection
  // alive for the seconds the system grants after the app leaves the screen.
  assert(
    bridge.includes("beginBackgroundTask(withName:") && bridge.includes("endBackgroundTask("),
    "ios_background_task_missing"
  );
  assert(iosDiscovery.includes("NSNetServiceBrowser"), "ios_discovery_browser_missing");
  assert(
    iosDiscovery.includes('searchForServices(ofType: "_vibex._tcp."'),
    "ios_discovery_service_type_missing"
  );

  // Nothing here may reintroduce the technology the mobile client was rebuilt
  // away from.
  const scoped = [
    android,
    androidBuild,
    activity,
    hostJava,
    scanner,
    service,
    discovery,
    iosProject,
    plist,
    delegate,
    bridge,
    iosDiscovery,
    shareApp,
    shareExtension
  ].join("\n");
  for (const forbidden of ["Capacitor", "capacitor", "wasm-bindgen", "mobile-wasm"]) {
    assert(!scoped.includes(forbidden), `legacy_mobile_technology_present:${forbidden}`);
  }
}

function runSelfTest() {
  // The unmutated tree must pass first, or a checker that rejects everything
  // would still pass every negative case below.
  validateContract();

  const files = new Map(HOST_FILES.map((path) => [path, source(path)]));
  const read = (path) => files.get(path);
  const exists = (path) => files.has(path) || existsSync(join(ROOT, path));

  function expectRejected(path, from, to, code, { all = false } = {}) {
    const original = files.get(path);
    assert(original.includes(from), `self-test marker missing in ${path}: ${from}`);
    files.set(path, all ? original.replaceAll(from, to) : original.replace(from, to));
    let rejected = false;
    try {
      validateContract(read, exists);
    } catch {
      rejected = true;
    } finally {
      files.set(path, original);
    }
    assert(rejected, code);
  }

  expectRejected(
    ANDROID_HOST_JAVA,
    "public static native String nativeTakeUpdateUrl();",
    "public static native String nativeTakeMissingUrl();",
    "self_test_accepted_a_missing_jni_export"
  );
  expectRejected(
    ANDROID_HOST_JAVA,
    'System.loadLibrary("nsh")',
    'System.loadLibrary("missing")',
    "self_test_accepted_an_unloaded_library"
  );
  expectRejected(
    ANDROID_BUILD,
    "namespace 'ai.vibex.mobile.nsh'",
    "namespace 'ai.vibex.mobile.other'",
    "self_test_accepted_a_mismatched_jni_namespace"
  );
  expectRejected(
    ANDROID,
    "android.permission.POST_NOTIFICATIONS",
    "android.permission.MISSING",
    "self_test_accepted_a_missing_permission"
  );
  expectRejected(
    ANDROID,
    'android:name="com.longbridge.nsh.ScannerActivity"\n            android:exported="false"',
    'android:name="com.longbridge.nsh.ScannerActivity"\n            android:exported="true"',
    "self_test_accepted_an_exported_scanner"
  );
  expectRejected(
    ANDROID,
    'android:foregroundServiceType="connectedDevice"',
    'android:foregroundServiceType="dataSync"',
    "self_test_accepted_a_service_without_the_connection_type"
  );
  expectRejected(
    ANDROID_PATHS,
    'path="Download/updates/"',
    'path="Download/"',
    "self_test_accepted_a_broadened_update_path"
  );
  expectRejected(
    ANDROID_SHORTCUTS,
    'android:action="ai.vibex.mobile.NEW_SESSION"',
    'android:action="ai.vibex.mobile.OLD_SESSION"',
    "self_test_accepted_a_stale_shortcut_action"
  );
  // Both call sites (the update APK and the camera photo) have to disappear
  // before the checker may accept the tree, and `replace` only takes the first.
  expectRejected(
    ANDROID_ACTIVITY,
    "FileProvider.getUriForFile",
    "Uri.fromFile",
    "self_test_accepted_a_raw_file_uri",
    { all: true }
  );
  expectRejected(
    ANDROID_SERVICE,
    "startForeground(",
    "startService(",
    "self_test_accepted_a_non_foreground_service"
  );
  expectRejected(
    IOS_PROJECT,
    "ASSETCATALOG_COMPILER_APPICON_NAME: AppIcon",
    "# the icon went missing",
    "self_test_accepted_a_missing_app_icon"
  );
  expectRejected(
    IOS_PROJECT,
    '          - "-force_load"',
    '          - "-forceLoad"',
    "self_test_accepted_a_dropped_force_load"
  );
  expectRejected(
    IOS_PLIST,
    "<string>_vibex._tcp</string>",
    "<string>_other._tcp</string>",
    "self_test_accepted_a_changed_bonjour_service"
  );
  expectRejected(
    IOS_PROJECT,
    "        TARGETED_DEVICE_FAMILY: \"1\"",
    "        TARGETED_DEVICE_FAMILY: \"1,2\"\n        CODE_SIGN_ENTITLEMENTS: Vibex.entitlements",
    "self_test_accepted_an_entitlement"
  );
  expectRejected(
    IOS_SHARE_EXTENSION,
    '"pasteboard"',
    '"clipboard"',
    "self_test_accepted_a_changed_share_key"
  );
  expectRejected(
    IOS_BRIDGE,
    "beginBackgroundTask(withName:",
    "beginTask(withName:",
    "self_test_accepted_a_missing_background_task"
  );
  expectRejected(
    IOS_DISCOVERY,
    'searchForServices(ofType: "_vibex._tcp."',
    'searchForServices(ofType: "_other._tcp."',
    "self_test_accepted_a_changed_ios_service_type"
  );
}

try {
  if (SELF_TEST) runSelfTest();
  else validateContract();
  console.log(
    SELF_TEST
      ? "Mobile host checker self-test passed"
      : "Mobile host contract verified (JNI exports, manifest, resources, iOS project and plists)"
  );
} catch (error) {
  console.error(error instanceof Error ? error.message : "mobile_host_check_failed");
  process.exitCode = 1;
}

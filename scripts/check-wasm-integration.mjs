import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { MOBILE_RUNTIME_SOURCE_INPUTS } from "./wasm-source-tree.mjs";
import {
  createHostServices,
  extractOpaqueDeepLink,
  extractPairingFragment,
  validateCredentialBundle
} from "../apps/mobile-wasm/host/host-services.js";

const ROOT = resolve(fileURLToPath(new URL("..", import.meta.url)));
const MOBILE_RUNTIME = join(ROOT, "apps/mobile-wasm");
const MOBILE = join(ROOT, "apps/mobile");
const selfTest = process.argv.includes("--self-test");

function fail(message) {
  throw new Error(message);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function text(path) {
  return readFileSync(join(ROOT, path), "utf8");
}

function checkForbiddenUi() {
  const files = [
    "apps/mobile-wasm/src/lib.rs",
    "apps/mobile-wasm/host/index.html",
    "apps/mobile-wasm/host/host.js",
    "apps/mobile-wasm/host/host-services.js",
    "apps/mobile-wasm/host/platform-compat.js",
    "apps/mobile-wasm/host/styles.css"
  ];
  const forbidden = [
    /from\s+["'](?:react|react-dom|@?tanstack|tailwind|@radix-ui)/i,
    /(?:^|[\s"'])(?:React|Tailwind|shadcn)\b/i,
    /apps\/web\/dist|apps\\web\\dist/,
    /legacy[-_ ]transport|old[-_ ]transport/i
  ];
  for (const path of files) {
    const source = text(path);
    for (const pattern of forbidden) assert(!pattern.test(source), `${path} contains forbidden legacy UI/transport text`);
  }
}

function checkPackaging() {
  const config = text("apps/mobile/capacitor.config.ts");
  assert(/webDir:\s*["']\.\.\/mobile-wasm\/dist["']/.test(config), "Capacitor webDir must be ../mobile-wasm/dist");
  const packageJson = JSON.parse(text("apps/mobile/package.json"));
  for (const script of ["validate", "runtime:build", "android:debug", "android:release", "ios:debug", "ios:release"]) {
    assert(typeof packageJson.scripts?.[script] === "string", `mobile package script ${script} is missing`);
  }
  const contract = JSON.parse(text("apps/mobile/native-shell-contract.json"));
  assert(contract.deepLink?.fragmentRoute === "#/pair/", "native pairing fragment route is not fixed");
  assert(contract.applicationId === "dev.vibex.remote", "native shell does not own the stable mobile application id");
  assert(contract.applicationName === "Vibex Remote", "native shell does not own the stable mobile product name");
  assert(JSON.stringify(contract.deepLink?.customSchemes) === JSON.stringify(["vibex", "dev.vibex.remote"]), "native pairing schemes are not fixed");
  assert(text("apps/mobile/scripts/configure-native-links.mjs").includes("VIBEX_PAIRING_LINKS_V1"), "native deep-link configuration script is missing");
  const packages = new Set(contract.plugins.map((plugin) => plugin.package));
  assert(contract.platform?.android?.minSdk === 26, "native scanner Android minSdk is not fixed");
  assert(contract.platform?.android?.permissions?.includes("android.permission.CAMERA"), "native scanner camera permission is missing");
  assert(typeof contract.platform?.ios?.usageDescriptions?.NSCameraUsageDescription === "string", "native scanner iOS camera usage is missing");
  for (const required of ["@capacitor/app", "@capacitor/barcode-scanner", "@capacitor/browser", "@capacitor/camera", "@capacitor/network", "@capacitor/share", "capacitor-secure-storage-plugin"]) {
    assert(packages.has(required), `native shell contract is missing ${required}`);
  }
  const nativeScript = text("apps/mobile/scripts/configure-native-links.mjs");
  assert(nativeScript.includes("nativeShellContract.platform?.android?.minSdk") && nativeScript.includes("minSdkVersion = ${androidMinSdk}"), "native configuration does not enforce the scanner minSdk contract");
  assert(nativeScript.includes("androidPermissions") && nativeScript.includes("iosUsageDescriptions"), "native configuration does not consume the platform permission contract");
  assert(nativeScript.includes("configureAndroidApplicationId") && nativeScript.includes('nativeShellContract.applicationId'), "native configuration does not migrate the generated Android package to the stable application id");
}

function checkHostContracts() {
  const services = text("apps/mobile-wasm/host/host-services.js");
  const host = text("apps/mobile-wasm/host/host.js");
  assert(services.includes("history.replaceState"), "pairing deep links are not scrubbed from history");
  assert(services.indexOf("clearPairingFragment(windowLike") < services.indexOf("onPairingFragment?.({"), "pairing payload is dispatched before URL cleanup");
  assert(services.indexOf("clearOpaqueDeepLink(windowLike") < services.indexOf("onOpaqueDeepLink?.(item)"), "opaque deep link is dispatched before URL cleanup");
  assert(host.includes("pendingDeepLink") && host.includes("wasmRuntime.resolve_deep_link"), "opaque deep links do not call the authoritative PC resolver");
  assert(host.includes("authoritativeFetch: \"remote_deep_link\"") && !host.includes("authoritativeFetch: \"remote_state\""), "opaque deep links can still claim success from a connection snapshot");
  assert(services.includes("SecureStoragePlugin"), "secure storage plugin boundary is missing");
  assert(services.includes("local_storage"), "browser scoped storage fallback is missing");
  assert(services.includes("camera") && services.includes("filePicker") && services.includes("scanQr"), "camera/file/QR host capabilities are missing");
  assert(services.includes('plugin(capacitor, "CapacitorBarcodeScanner")') && services.includes("scanBarcode"), "maintained Capacitor barcode scanner is not wired");
  assert(!services.includes("value.slice(0, MAX_PAIRING_FRAGMENT_BYTES)"), "oversized QR results are truncated instead of rejected");
  assert(services.includes("function safeArea()") && services.includes("function viewport()"), "safe-area/viewport host capabilities are missing");
  assert(services.includes("share") && services.includes("openSystemUrl"), "share/system URL host capabilities are missing");
  assert(host.includes("pairing_preview") && host.includes("claim_pairing_fragment"), "Rust pairing bridge is not connected");
  assert(host.includes('code.includes("browser_network_policy")') && host.includes('state: "access_error"'), "browser pairing policy failures still masquerade as Desktop offline");
  assert(host.indexOf("writeCredentialBundle(credentials)") < host.indexOf("configureRemote(credentials)"), "pairing credentials are not persisted before configure");
  assert(host.indexOf("const verified = await hostServices.readCredentialBundle()") < host.indexOf("configureRemote(credentials)"), "pairing credentials are not read back before configure");
  assert(host.indexOf("configureRemote(credentials)") < host.indexOf("await connectConfigured()", host.indexOf("configureRemote(credentials)")), "pairing runtime connects before configuration");
  assert(host.includes("function exposeHostServices()"), "host capability methods do not have a stable exposure boundary");
  assert(/await hostServices\.initialize\(\);\s*exposeHostServices\(\);/.test(host), "host capability methods are lost after initialization refresh");
  assert(!host.includes("runtime.events.push({ pairing") && !host.includes("runtime.events.push({ fragment"), "pairing payload is written to runtime events");
  assert(!host.includes("runtime.connectRemote") && !host.includes("runtime.claimPairing"), "production credential injection API is still exposed");
  assert(!host.includes("readCredentials:") && !host.includes("writeCredentials:"), "credential storage is exposed through the public Gate host");
  assert(!host.includes("runtime.wasm =") && host.includes("let wasmRuntime = null"), "credential-bearing WASM exports remain public");
  assert(host.includes("pairingFocusableElements") && host.includes('event.key !== "Tab"') && host.includes('event.key === "Escape"'), "pairing sheet focus containment is missing");
  assert(host.includes('runtime.remote.pairing.state === "unpaired"'), "unpaired recovery can be dismissed without another pairing entry");
  assert(
    host.includes('!["idle", "unpaired"].includes(runtime.remote.pairing.state)'),
    "initial pairing recovery can be overwritten by the no-credential state"
  );
  assert(text("apps/mobile-wasm/host/index.html").includes('id="pairing-confirm"'), "pairing confirmation sheet is missing");
  assert(host.includes("remote_lifecycle") && host.includes("network_lost"), "lifecycle/network bridge is incomplete");
  assert(host.includes("REMOTE_STATE_STEADY_POLL_MS") && host.includes("snapshot?.configured"), "configured mobile runtime does not keep observing authoritative reconnect state");
  assert(host.includes('const diagnosticsGate = parameters.get("diagnostics") === "gate"'), "Gate fixture does not require explicit diagnostics mode");
  assert(/if \(!stored\) \{[\s\S]{0,160}if \(!diagnosticsGate\) setPairingState\("unpaired"/.test(host), "default product entry does not require pairing while the diagnostics Gate stays unobstructed");
  assert(host.includes("wasm.start(diagnosticsGate)"), "diagnostics mode is not shared with the GPUI start boundary");
  assert(!/runtime\.gpuiBooted\s*&&\s*runtime\.remote\.page[\s\S]{0,240}setState\(/.test(host), "remote failures can still cover the product workbench with the Gate status layer");
  assert(host.indexOf("wasmRuntime.configure_remote") < host.indexOf("scheduleRemoteStateSync();", host.indexOf("function configureRemote")), "remote state synchronization does not start with configuration");
  const rust = text("apps/mobile-wasm/src/lib.rs");
  for (const symbol of ["configure_remote", "connect_remote", "forget_remote", "resolve_deep_link", "pairing_preview", "claim_pairing_fragment", "navigation_action", "closed_compact_navigation"]) {
    assert(rust.includes(symbol), `Rust mobile runtime contract is missing ${symbol}`);
  }
  for (const symbol of ["AutoRemoteTransport", "RelayClientConfig", "claim_pairing_offer_via_relay"]) {
    assert(rust.includes(symbol), `Rust mobile Relay fallback contract is missing ${symbol}`);
  }
  assert(rust.includes("select_pairing_claim_route") && rust.includes("PairingClaimRoute"), "entry-bound single-route claim selection is missing");
  assert(rust.includes('"activeRoute": runtime.active_route()'), "safe remote state omits the active route");
  assert(rust.includes("DisconnectedBackend::facade()"), "default product entry does not construct the real workflow shell while unpaired");
  assert(rust.includes("MobileRootMode::Workbench"), "mobile root has no explicit product workbench mode");
  assert(!/configure_remote[\s\S]{0,1200}show_gate\(\)/.test(rust), "remote configuration still falls back to the Gate fixture");
}

function checkMobileRuntimeBuildContract() {
  const host = text("apps/mobile-wasm/host/index.html");
  const build = text("apps/mobile-wasm/scripts/build.mjs");
  assert(!host.includes("manifest.webmanifest"), "mobile runtime must not expose a PWA manifest");
  assert(!text("apps/mobile-wasm/host/host.js").includes("serviceWorker"), "mobile host must not register a service worker");
  assert(!build.includes("service-worker"), "mobile runtime must not build a service worker");
  assert(build.includes('runtimeRole: "capacitor_mobile_runtime"'), "mobile runtime role is missing");
  assert(build.includes('schemaVersion: "vibex-mobile-wasm-build.v1"'), "mobile build schema is missing");
}

function checkSourceIdentityCoverage() {
  const tree = execFileSync(
    "cargo",
    [
      "tree",
      "-p",
      "vibex-mobile-wasm",
      "--target",
      "wasm32-unknown-unknown",
      "--edges",
      "normal",
      "--prefix",
      "none",
      "--format",
      "{p}"
    ],
    { cwd: ROOT, encoding: "utf8" }
  );
  const localCrates = new Set();
  for (const line of tree.split(/\r?\n/)) {
    const match = line.match(/\(([^)]+)\)/);
    if (!match) continue;
    const prefix = `${ROOT}/`;
    if (!match[1].startsWith(prefix)) continue;
    localCrates.add(match[1].slice(prefix.length).replaceAll("\\", "/"));
  }
  assert(localCrates.size > 0, "Cargo did not report local mobile WASM crates");
  for (const root of localCrates) {
    assert(
      MOBILE_RUNTIME_SOURCE_INPUTS.includes(`${root}/Cargo.toml`),
      `Mobile runtime source identity omits ${root}/Cargo.toml`
    );
    assert(
      MOBILE_RUNTIME_SOURCE_INPUTS.includes(`${root}/src`),
      `Mobile runtime source identity omits ${root}/src`
    );
  }
}

function checkNegativePaths() {
  let rejected = false;
  try {
    extractPairingFragment(`https://vibex.invalid/#/pair/${"a".repeat(32 * 1024)}`);
  } catch {
    rejected = true;
  }
  assert(rejected, "invalid pairing fragment was accepted by the host parser");
  rejected = false;
  try {
    extractPairingFragment("javascript:alert(1)#/pair/abc");
  } catch {
    rejected = true;
  }
  assert(rejected, "an executable URL scheme was accepted for pairing");
  const receipt = extractPairingFragment("https://desktop.example/#/pair/abc");
  assert(
    receipt.accepted === true && receipt.entryType === "development_host",
    "development-host pairing receipt is invalid"
  );
  assert(!("fragment" in receipt) && !("entryHint" in receipt) && !("origin" in receipt), "safe pairing receipt exposes intake secrets");
  const mobileReceipt = extractPairingFragment("vibex://open/direct#/pair/abc");
  assert(
    mobileReceipt.accepted === true && mobileReceipt.entryType === "mobile_app",
    "mobile app pairing receipt is invalid"
  );
  rejected = false;
  try {
    extractPairingFragment("vibex://open/unknown#/pair/abc");
  } catch {
    rejected = true;
  }
  assert(rejected, "an unknown mobile pairing transport was accepted");
  const deepLink = extractOpaqueDeepLink("https://vibex.invalid/#/notify/notification-a/opaque-ref");
  assert(deepLink.notificationId === "notification-a" && deepLink.opaqueLocator === "opaque-ref", "opaque deep link did not parse");
  rejected = false;
  try {
    extractOpaqueDeepLink("https://vibex.invalid/#/notify/notification-a/prompt%20leak");
  } catch {
    rejected = true;
  }
  assert(rejected, "opaque deep link accepted a non-opaque locator");
  rejected = false;
  try {
    extractOpaqueDeepLink("https://vibex.invalid/#/notify/notification-a/opaque-ref/extra");
  } catch {
    rejected = true;
  }
  assert(rejected, "opaque deep link ignored an extra path segment");
  rejected = false;
  try {
    validateCredentialBundle({ schemaVersion: "vibex-remote-client-credentials.v1" });
  } catch {
    rejected = true;
  }
  assert(rejected, "incomplete credential bundle was accepted");
}

async function checkHostServiceRoundTrip() {
  const values = new Map();
  const events = [];
  const deepLinks = [];
  const documentLike = {
    title: "Vibex",
    visibilityState: "visible",
    addEventListener() {},
    removeEventListener() {},
    querySelector() { return null; },
    documentElement: null
  };
  const windowLike = {
    location: { href: "https://vibex.invalid/open?token=drop#/pair/abc" },
    history: { replaceState(_state, _title, value) { this.value = value; } },
    document: documentLike,
    localStorage: {
      getItem(key) { return values.get(key) ?? null; },
      setItem(key, value) { values.set(key, value); },
      removeItem(key) { values.delete(key); }
    },
    navigator: { onLine: true },
    isSecureContext: true,
    addEventListener() {},
    removeEventListener() {},
    getComputedStyle() {
      return { paddingTop: "0px", paddingRight: "0px", paddingBottom: "0px", paddingLeft: "0px" };
    }
  };
  const services = createHostServices({
    windowLike,
    documentLike,
    onPairingFragment: (item) => events.push(item),
    onOpaqueDeepLink: (item) => deepLinks.push(item)
  });
  assert(windowLike.history.value === "/open", "startup pairing cleanup was not synchronous");
  assert(events.length === 0, "startup pairing callback ran before host initialization");
  await services.initialize();
  assert(windowLike.history.value === "/open", "pairing cleanup retained query state");
  assert(events.length === 1 && events[0].fragment === "#/pair/abc", "private pairing callback contract is unstable");
  assert(events[0].entryHint?.origin === "https://vibex.invalid", "pairing entry origin was not normalized");
  const mobilePairing = await services.dispatchPairing(
    "vibex://open/self_hosted_relay#/pair/def",
    "app_link"
  );
  assert(mobilePairing.accepted === true, "mobile app pairing intake was rejected");
  assert(
    events.at(-1)?.entryHint?.kind === "mobile_app" &&
      events.at(-1)?.entryHint?.transport === "self_hosted_relay" &&
      !("origin" in events.at(-1).entryHint),
    "mobile app pairing intake did not preserve the restricted transport hint"
  );
  const universalLink = await services.dispatchPairing(
    "https://mobile.vibex.example/open/tailnet#/pair/ghi",
    "app_link"
  );
  assert(universalLink.accepted === true, "Universal Link pairing intake was rejected");
  assert(
    events.at(-1)?.entryHint?.kind === "mobile_app" &&
      events.at(-1)?.entryHint?.transport === "tailnet" &&
      !("origin" in events.at(-1).entryHint),
    "Universal Link intake was treated as a development-host origin"
  );
  const rejectedPairing = await services.dispatchPairing(
    "javascript:alert(1)#/pair/abc",
    "app_link"
  );
  assert(
    rejectedPairing.accepted === false && rejectedPairing.errorCode === "pairing_link_invalid",
    "runtime pairing rejection receipt is not typed"
  );
  assert(
    events.at(-1)?.errorCode === "pairing_link_invalid" && events.at(-1)?.entryType === "mobile_app",
    "runtime pairing rejection did not reach the recovery controller"
  );
  await services.dispatchOpaqueDeepLink("https://vibex.invalid/dashboard?secret=drop#/notify/notification-a/opaque-ref", "development_host");
  assert(windowLike.history.value === "/dashboard", "opaque deep-link cleanup retained query state");
  assert(deepLinks.length === 1 && deepLinks[0].opaqueLocator === "opaque-ref", "opaque deep-link callback contract is unstable");
  const bundle = {
    schemaVersion: "vibex-remote-client-credentials.v1",
    record: {
      serverUrl: "https://desktop.example",
      auth: { deviceId: "device_test", authToken: "grant-1234567890123456" },
      deviceIdentityPublicKey: "a".repeat(32),
      serverIdentityPublicKey: "b".repeat(32)
    },
    identityPrivateKey: "c".repeat(43),
    expectedServerId: "desktop",
    clientType: "mobile"
  };
  await services.writeCredentialBundle(bundle);
  assert((await services.readCredentialBundle()).expectedServerId === "desktop", "browser storage round-trip failed");

  const nativeStorageWith = (get) => createHostServices({
    windowLike: { ...windowLike, location: { href: "https://localhost/" } },
    documentLike,
    capacitor: {
      isNativePlatform: () => true,
      Plugins: {
        SecureStoragePlugin: {
          get,
          async set() {},
          async remove() {}
        }
      }
    }
  });
  const emptyNativeStorage = nativeStorageWith(async () => {
    throw new Error("Item with given key does not exist");
  });
  assert(await emptyNativeStorage.readCredentialBundle() === null, "Android missing secure-storage key is not an unpaired state");
  let secureStorageFailure = null;
  try {
    await nativeStorageWith(async () => {
      throw new Error("keystore decryption failed");
    }).readCredentialBundle();
  } catch (error) {
    secureStorageFailure = error?.code ?? null;
  }
  assert(secureStorageFailure === "secure_storage_read_failed", "secure-storage corruption did not fail closed");

  const scannerWith = (scanBarcode) => createHostServices({
    windowLike: { ...windowLike, location: { href: "https://vibex.invalid/" } },
    documentLike,
    capacitor: {
      isNativePlatform: () => true,
      Plugins: { CapacitorBarcodeScanner: { scanBarcode } }
    }
  });
  const scanner = scannerWith(async (options) => {
    assert(options.hint === 0 && options.cameraDirection === 1, "QR-only scanner options changed");
    return { ScanResult: "vibex://open/direct#/pair/abc", format: 0 };
  });
  const scan = await scanner.scanQr();
  assert(scan.status === "scanned" && scan.format === "qr_code", "native QR result was not normalized");

  const canceled = await scannerWith(async () => ({ ScanResult: "", format: 0 })).scanQr();
  assert(canceled.status === "canceled", "native QR cancellation was not normalized");
  for (const [scanBarcode, expectedCode] of [
    [async () => { throw new Error("Camera permission denied"); }, "qr_camera_permission_denied"],
    [async () => { throw new Error("User cancelled scan"); }, "qr_scan_canceled"],
    [async () => ({ ScanResult: "a".repeat(32 * 1024 + 1), format: 0 }), "qr_scan_result_too_large"]
  ]) {
    let code = null;
    try {
      await scannerWith(scanBarcode).scanQr();
    } catch (error) {
      code = error?.code ?? null;
    }
    assert(code === expectedCode, `native QR failure did not map to ${expectedCode}`);
  }
  let unavailableCode = null;
  try {
    await services.scanQr();
  } catch (error) {
    unavailableCode = error?.code ?? null;
  }
  assert(unavailableCode === "qr_scanner_unsupported", "missing QR plugin was not fail-closed");
}

async function main() {
  checkForbiddenUi();
  checkPackaging();
  checkHostContracts();
  checkMobileRuntimeBuildContract();
  checkSourceIdentityCoverage();
  if (selfTest) {
    checkNegativePaths();
    await checkHostServiceRoundTrip();
  }
  console.log(JSON.stringify({
    schemaVersion: "vibex-wasm-integration-check.v1",
    status: "passed",
    selfTest,
    mobileRuntimeSource: MOBILE_RUNTIME,
    mobileSource: MOBILE,
    sameBusinessBundle: true,
    legacyReactUi: false,
    relayFallback: "auto_direct_to_self_hosted_relay"
  }, null, 2));
}

await main();

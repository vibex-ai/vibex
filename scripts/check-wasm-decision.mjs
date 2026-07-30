import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { resolveGpuiSourceIdentities } from "./source-identities.mjs";
import {
  MOBILE_SOURCE_INPUTS,
  WEB_SOURCE_INPUTS,
  sourceTreeSha256
} from "./wasm-source-tree.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const BROWSER_EVIDENCE = "docs/platform/evidence/wasm-browser-gate.json";
const ANDROID_EVIDENCE = "docs/platform/evidence/wasm-android-build.json";
const MOBILE_EVIDENCE = "docs/platform/evidence/wasm-mobile-physical.json";
const NATIVE_SHELL_CONTRACT = "apps/mobile/native-shell-contract.json";
const DECISION = "docs/platform/wasm-browser-gate.md";

function fail(message) {
  throw new Error(message);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function read(path) {
  return readFileSync(join(ROOT, path));
}

function json(path) {
  assert(existsSync(join(ROOT, path)), `${path} is missing`);
  return JSON.parse(read(path));
}

const identities = resolveGpuiSourceIdentities(ROOT);
const browser = json(BROWSER_EVIDENCE);
const android = json(ANDROID_EVIDENCE);
const mobile = json(MOBILE_EVIDENCE);
const nativeShellContract = json(NATIVE_SHELL_CONTRACT);
const decision = read(DECISION).toString("utf8");

assert(browser.schemaVersion === "vibex-wasm-browser-evidence.v1", "browser evidence schema is invalid");
assert(browser.source.zedRevision === identities.zedRevision, "browser Zed revision is stale");
assert(browser.source.gpuiComponentRevision === identities.gpuiComponentRevision, "browser component revision is stale");
const currentWebSourceHash = sourceTreeSha256(ROOT, WEB_SOURCE_INPUTS);
assert(browser.source.sourceTreeSha256 === currentWebSourceHash, "browser Web source identity is stale");
assert(browser.targets.chromium.status === "passed", "Chromium Web gate did not pass");
assert(browser.targets.firefox.status === "passed", "Firefox Web gate did not pass");
assert(browser.targets.chromium.runtime.contract.runtime.dispatcher === "single_threaded_web", "Web runtime is not single-threaded");
assert(browser.targets.chromium.components.dialogRendered === true, "shared Dialog evidence is missing");
assert(browser.targets.chromium.components.sheetRendered === true, "shared Sheet evidence is missing");
assert(browser.targets.chromium.appearance.darkModeTransition === true, "dark-mode transition evidence is missing");
assert(browser.targets.chromium.fullscreen.entered === true, "fullscreen entry evidence is missing");
assert(browser.targets.chromium.fullscreen.exited === true, "fullscreen exit evidence is missing");
assert(browser.targets.chromium.accessibility.gpuiSemanticTreeExposed === false, "a11y evidence contradicts the release blocker");
assert(browser.decision.technicalSpike === "go", "technical Web spike is not GO");
assert(browser.decision.productionRelease === "no_go", "browser evidence must retain its pre-approval NO_GO fact");

assert(android.schemaVersion === "vibex-android-build.v1", "Android build evidence schema is invalid");
assert(android.source.zedRevision === identities.zedRevision, "Android build Zed revision is stale");
assert(android.source.gpuiComponentRevision === identities.gpuiComponentRevision, "Android build component revision is stale");
assert(android.source.webSourceTreeSha256 === currentWebSourceHash, "Android Web source evidence is stale");
assert(
  android.source.mobileShellTreeSha256 === sourceTreeSha256(ROOT, MOBILE_SOURCE_INPUTS),
  "Android mobile shell evidence is stale"
);
assert(android.source.cargoLockfileSha256 === sha256(read("Cargo.lock")), "Android Cargo lock identity is stale");
assert(android.source.pnpmLockfileSha256 === sha256(read("pnpm-lock.yaml")), "Android pnpm lock identity is stale");
assert(android.status === "packaged_not_device_validated", "Android build evidence overclaims physical validation");
assert(
  android.artifact.minSdk === nativeShellContract.platform.android.minSdk &&
    android.artifact.targetSdk === 36,
  "Android SDK contract changed"
);
assert(android.packageContract.webDir === "../web/dist", "Android shell references the wrong Web dist");
assert(
  android.packageContract.containsPlatformCompatibilityAdapter === true,
  "Android APK is missing the platform compatibility adapter"
);
assert(android.packageContract.containsGpuiWasm === true, "Android APK is missing GPUI WASM");
assert(android.packageContract.containsLegacyReactDist === false, "Android APK contains the legacy React dist");
assert(
  JSON.stringify(android.packageContract.plugins) ===
    JSON.stringify(
      nativeShellContract.plugins.map(({ package: packageName, version }) =>
        `${packageName}@${version}`
      )
    ),
  "Android APK plugin contract is stale"
);
const localApk = join(ROOT, android.artifact.path);
if (existsSync(localApk)) {
  assert(sha256(readFileSync(localApk)) === android.artifact.sha256, "local Android APK hash is stale");
}

assert(mobile.schemaVersion === "vibex-mobile-physical-evidence.v1", "mobile evidence schema is invalid");
assert(
  mobile.targets.android_physical.status === "pending",
  "Android physical evidence must remain pending until every confirmation passes"
);
const ANDROID_INTERACTION_BASELINE = [
  "ime_commit",
  "touch_page_and_timeline",
  "keyboard_focus_and_inset",
  "compact_dialog_and_sheet",
  "android_back",
  "rotation",
  "lifecycle"
];
const ANDROID_UNCLAIMED_CAPABILITIES = [
  "network",
  "secure_storage",
  "webgpu_pixels",
  "physical_clipboard",
  "non_us_hardware_keyboard"
];
const currentAndroidBuild = mobile.targets.android_physical.currentBuild;
// A rebuilt APK is forced back to not_tested until a new physical capture runs.
assert(
  ["partial_physical", "not_tested"].includes(currentAndroidBuild?.status),
  "the current Android APK must be partial physical or an untested rebuild"
);
assert(
  currentAndroidBuild?.apk?.sha256 === android.artifact.sha256,
  "Android physical status is not bound to the current APK"
);
assert(
  currentAndroidBuild?.status !== "not_tested" ||
    (!mobile.targets.android_physical.lastValidation &&
      !mobile.targets.android_physical.history),
  "an untested Android build retained prior validation history"
);
if (currentAndroidBuild.status === "partial_physical") {
  assert(
    currentAndroidBuild.scenarios
      .filter((scenario) => ANDROID_INTERACTION_BASELINE.includes(scenario.id))
      .every((scenario) => scenario.status === "passed"),
    "the Android interaction baseline is incomplete"
  );
  assert(
    currentAndroidBuild.scenarios
      .filter((scenario) => ANDROID_UNCLAIMED_CAPABILITIES.includes(scenario.id))
      .every((scenario) => scenario.status === "not_tested"),
    "the current Android APK overclaims an untested physical capability"
  );
} else {
  assert(
    currentAndroidBuild.scenarios.every((scenario) => scenario.status === "not_tested"),
    "a rebuilt Android APK claims a scenario result without a physical capture"
  );
}
assert(mobile.targets.ios_physical.status === "pending", "iOS physical evidence was not expected in this iteration");
assert(
  mobile.targets.ios_physical.disposition === "accepted_deviation" &&
    typeof mobile.targets.ios_physical.followUp === "string",
  "iOS physical deferral must be explicit and actionable"
);
assert(mobile.releaseGateSatisfied === false, "physical mobile release gate cannot be satisfied yet");

for (const phrase of [
  "Web validation:",
  "Production release: blocked",
  "Accessibility",
  "High-DPI",
  "Every rebuilt APK",
  "Omitted scenarios remain `not_tested`",
  "iOS physical validation: pending"
]) {
  assert(decision.includes(phrase), `${DECISION} is missing: ${phrase}`);
}

console.log(
  `GPUI-WASM decision verified: Chromium ${browser.targets.chromium.version}, ` +
    `Firefox ${browser.targets.firefox.version}, Android APK ${android.artifact.sha256.slice(0, 12)}, ` +
    "Web validation passed, production release blocked"
);

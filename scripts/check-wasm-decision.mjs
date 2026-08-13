import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { resolveGpuiSourceIdentities } from "./source-identities.mjs";
import {
  MOBILE_RUNTIME_SOURCE_INPUTS,
  MOBILE_SOURCE_INPUTS,
  sourceTreeSha256
} from "./wasm-source-tree.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const HOST_EVIDENCE = "docs/platform/evidence/mobile-wasm-host-gate.json";
const ANDROID_EVIDENCE = "docs/platform/evidence/wasm-android-build.json";
const MOBILE_EVIDENCE = "docs/platform/evidence/wasm-mobile-physical.json";
const NATIVE_SHELL_CONTRACT = "apps/mobile/native-shell-contract.json";
const RUNTIME_BUILD = "apps/mobile-wasm/dist/build.json";
const DECISION = "docs/platform/mobile-wasm-runtime-gate.md";

function fail(message) {
  throw new Error(message);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function read(path) {
  return readFileSync(join(ROOT, path));
}

function json(path) {
  assert(existsSync(join(ROOT, path)), `${path} is missing`);
  return JSON.parse(read(path));
}

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function buildIdentity(build) {
  return Object.fromEntries(
    ["buildId", "profile", "wasmBytes", "wasmSha256", "glueSha256", "staticSha256"].map(
      (field) => [field, build[field]]
    )
  );
}

const identities = resolveGpuiSourceIdentities(ROOT);
const host = json(HOST_EVIDENCE);
const android = json(ANDROID_EVIDENCE);
const mobile = json(MOBILE_EVIDENCE);
const nativeShell = json(NATIVE_SHELL_CONTRACT);
const runtimeBuild = json(RUNTIME_BUILD);
const decision = read(DECISION).toString("utf8");
const runtimeSourceHash = sourceTreeSha256(ROOT, MOBILE_RUNTIME_SOURCE_INPUTS);

assert(host.schemaVersion === "vibex-mobile-wasm-host-evidence.v1", "development host evidence schema is invalid");
assert(host.purpose === "development_and_automation_only", "development host evidence overclaims scope");
assert(host.releaseClaim === "none", "development host evidence makes a release claim");
assert(host.source.zedRevision === identities.zedRevision, "development host Zed revision is stale");
assert(host.source.gpuiComponentRevision === identities.gpuiComponentRevision, "development host component revision is stale");
assert(host.source.sourceTreeSha256 === runtimeSourceHash, "development host source identity is stale");
assert(host.targets.chromium.status === "passed", "Chromium development host did not pass");
assert(host.targets.chromium.maximumShell === "medium", "development host can still select Wide");
assert(host.targets.chromium.viewports.medium.shell === "medium", "Medium host evidence is missing");
assert(host.targets.chromium.viewports.compact.shell === "compact", "Compact host evidence is missing");
assert(host.targets.chromium.hostBridge.keyboard === "passed", "keyboard host evidence is missing");
assert(host.targets.chromium.hostBridge.touch.status === "passed", "touch host evidence is missing");
assert(!JSON.stringify(host).includes("screenshot"), "development host evidence contains screenshots");
assert(
  JSON.stringify(buildIdentity(host.runtimeBuild)) === JSON.stringify(buildIdentity(runtimeBuild)),
  "development host evidence is bound to a different runtime build"
);

assert(runtimeBuild.schemaVersion === "vibex-mobile-wasm-build.v1", "mobile runtime build schema is invalid");
assert(runtimeBuild.runtimeRole === "capacitor_mobile_runtime", "mobile runtime build role drifted");
assert(runtimeBuild.browserHost === "development_and_test_only", "mobile runtime browser host role drifted");

assert(android.schemaVersion === "vibex-android-build.v1", "Android build evidence schema is invalid");
assert(android.source.zedRevision === identities.zedRevision, "Android build Zed revision is stale");
assert(android.source.gpuiComponentRevision === identities.gpuiComponentRevision, "Android build component revision is stale");
assert(android.source.mobileRuntimeSourceTreeSha256 === runtimeSourceHash, "Android runtime source evidence is stale");
assert(
  android.source.mobileShellTreeSha256 === sourceTreeSha256(ROOT, MOBILE_SOURCE_INPUTS),
  "Android mobile shell evidence is stale"
);
assert(android.source.cargoLockfileSha256 === sha256(read("Cargo.lock")), "Android Cargo lock identity is stale");
assert(android.source.pnpmLockfileSha256 === sha256(read("pnpm-lock.yaml")), "Android pnpm lock identity is stale");
assert(android.status === "packaged_not_device_validated", "Android build evidence overclaims physical validation");
assert(android.artifact.minSdk === nativeShell.platform.android.minSdk, "Android minSdk contract changed");
assert(android.artifact.targetSdk === 36, "Android targetSdk contract changed");
assert(android.packageContract.webDir === "../mobile-wasm/dist", "Android shell references the wrong runtime dist");
assert(android.packageContract.containsGpuiWasm === true, "Android APK is missing GPUI WASM");
assert(android.packageContract.containsLegacyReactDist === false, "Android APK contains the legacy React dist");
assert(
  JSON.stringify(buildIdentity(android.runtimeBuild)) === JSON.stringify(buildIdentity(runtimeBuild)),
  "Android APK packages a different mobile runtime build"
);
assert(
  JSON.stringify(android.packageContract.plugins) ===
    JSON.stringify(nativeShell.plugins.map(({ package: name, version }) => `${name}@${version}`)),
  "Android APK plugin contract is stale"
);
const localApk = join(ROOT, android.artifact.path);
if (existsSync(localApk)) {
  assert(sha256(readFileSync(localApk)) === android.artifact.sha256, "local Android APK hash is stale");
}

assert(mobile.schemaVersion === "vibex-mobile-physical-evidence.v1", "mobile physical evidence schema is invalid");
assert(mobile.releaseGateSatisfied === false, "physical mobile release gate cannot be satisfied yet");
assert(
  mobile.targets.android_physical.currentBuild?.apk?.sha256 === android.artifact.sha256,
  "Android physical status is not bound to the current APK"
);
assert(mobile.targets.ios_physical.status === "pending", "iOS physical evidence unexpectedly changed");

for (const phrase of [
  "Development host:",
  "Browser product: retired",
  "Maximum shell: Medium",
  "Android physical validation:",
  "iOS physical validation: pending"
]) {
  assert(decision.includes(phrase), `${DECISION} is missing: ${phrase}`);
}

console.log(
  `Mobile GPUI-WASM decision verified: Chromium ${host.targets.chromium.version}, ` +
    `Android APK ${android.artifact.sha256.slice(0, 12)}, browser product retired`
);

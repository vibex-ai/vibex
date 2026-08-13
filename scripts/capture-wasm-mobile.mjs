import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { chromium } from "@playwright/test";
import {
  MOBILE_SOURCE_INPUTS,
  MOBILE_RUNTIME_SOURCE_INPUTS,
  sourceTreeSha256
} from "./wasm-source-tree.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const EVIDENCE_PATH = "docs/platform/evidence/wasm-mobile-physical.json";
const ANDROID_BUILD_EVIDENCE = "docs/platform/evidence/wasm-android-build.json";
const ANDROID_APK = "apps/mobile/artifacts/vibex-gate-debug.apk";
const ANDROID_SCREENSHOT = "docs/parity/screenshots/current/spikes/android-physical.png";
const NATIVE_SHELL_CONTRACT = "apps/mobile/native-shell-contract.json";
const SCENARIO_DEFINITIONS = [
  { id: "ime_commit", flag: "ime-pass" },
  { id: "touch_page_and_timeline", flag: "touch-pass" },
  { id: "keyboard_focus_and_inset", flag: "keyboard-focus-pass" },
  { id: "compact_dialog_and_sheet", flag: "compact-overlays-pass" },
  { id: "android_back", flag: "android-back-pass" },
  { id: "rotation", flag: "rotation-pass" },
  { id: "lifecycle", flag: "lifecycle-pass" },
  { id: "network", flag: "network-pass" },
  { id: "secure_storage", flag: "secure-storage-pass" },
  { id: "webgpu_pixels", flag: "pixels-pass" },
  { id: "physical_clipboard", flag: "clipboard-pass" },
  { id: "non_us_hardware_keyboard", flag: "non-us-keyboard-pass" }
];
const REQUIRED_CONFIRMATIONS = SCENARIO_DEFINITIONS.map(({ flag }) => flag);
const SCENARIO_STATUSES = new Set(["passed", "failed", "not_tested"]);

function fail(message) {
  throw new Error(message);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function argument(name) {
  const index = process.argv.indexOf(`--${name}`);
  return index >= 0 ? process.argv[index + 1] : null;
}

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function readJson(path) {
  return JSON.parse(readFileSync(join(ROOT, path), "utf8"));
}

function artifactMatches(left, right) {
  return (
    left?.path === right?.path &&
    left?.bytes === right?.bytes &&
    left?.sha256 === right?.sha256 &&
    left?.applicationId === right?.applicationId
  );
}

function verifyScenarioRecords(records, label, requirePassed = false) {
  assert(Array.isArray(records), `${label} scenarios are missing`);
  const expectedIds = SCENARIO_DEFINITIONS.map(({ id }) => id);
  assert(
    JSON.stringify(records.map(({ id }) => id)) === JSON.stringify(expectedIds),
    `${label} scenario set is invalid`
  );
  for (const record of records) {
    assert(SCENARIO_STATUSES.has(record.status), `${label}/${record.id} has an invalid status`);
    assert(typeof record.note === "string" && record.note.length > 0, `${label}/${record.id} has no note`);
    if (requirePassed) assert(record.status === "passed", `${label}/${record.id} did not pass`);
  }
}

function scenarioRecordsFromConfirmations() {
  return SCENARIO_DEFINITIONS.map(({ id, flag }) => {
    if (process.argv.includes(`--${flag}`)) {
      return {
        id,
        status: "passed",
        note: `Physical operator confirmation: --${flag}`
      };
    }
    return {
      id,
      status: "not_tested",
      note: `No physical operator confirmation supplied; omit --${flag} until this scenario is tested.`
    };
  });
}

function notTestedScenarioRecords() {
  return SCENARIO_DEFINITIONS.map(({ id, flag }) => ({
    id,
    status: "not_tested",
    note: `No physical operator confirmation is recorded for this APK; test it before supplying --${flag}.`
  }));
}

function command(commandName, args, options = {}) {
  const result = spawnSync(commandName, args, {
    cwd: ROOT,
    encoding: options.binary ? null : "utf8",
    maxBuffer: 32 * 1024 * 1024
  });
  if (result.error || result.status !== 0) {
    fail(`${commandName} ${args.join(" ")} failed:\n${result.stderr ?? result.error?.message}`);
  }
  return options.binary ? result.stdout : result.stdout.trim();
}

function verifyBuildEvidence() {
  assert(existsSync(join(ROOT, ANDROID_BUILD_EVIDENCE)), `${ANDROID_BUILD_EVIDENCE} is missing`);
  const build = readJson(ANDROID_BUILD_EVIDENCE);
  assert(build.schemaVersion === "vibex-android-build.v1", "Android build evidence schema is invalid");
  assert(build.status === "packaged_not_device_validated", "Android build evidence overclaims physical validation");
  assert(
    build.source.mobileRuntimeSourceTreeSha256 === sourceTreeSha256(ROOT, MOBILE_RUNTIME_SOURCE_INPUTS),
    "Android build evidence does not match the current mobile runtime source"
  );
  assert(
    build.source.mobileShellTreeSha256 === sourceTreeSha256(ROOT, MOBILE_SOURCE_INPUTS),
    "Android build evidence does not match the current mobile shell source"
  );
  assert(build.packageContract.webDir === "../mobile-wasm/dist", "Android package references the wrong mobile runtime dist");
  assert(
    build.packageContract.containsPlatformCompatibilityAdapter === true,
    "Android package does not contain the platform compatibility adapter"
  );
  assert(build.packageContract.containsGpuiWasm === true, "Android package does not contain GPUI WASM");
  assert(build.packageContract.containsLegacyReactDist === false, "Android package contains the legacy React dist");
  const nativeContract = readJson(NATIVE_SHELL_CONTRACT);
  const expectedPlugins = nativeContract.plugins.map(({ package: packageName, version }) =>
    `${packageName}@${version}`
  );
  assert(
    JSON.stringify(build.packageContract.plugins) === JSON.stringify(expectedPlugins),
    "Android package plugin contract is stale"
  );
  assert(
    build.nativeShell?.contractSchemaVersion === nativeContract.schemaVersion,
    "Android native shell contract is stale"
  );
  assert(
    JSON.stringify(build.nativeShell.plugins) ===
      JSON.stringify(
        nativeContract.plugins.map(({ package: packageName, version, classpath }) => ({
          package: packageName,
          version,
          classpath
        }))
      ),
    "Android native plugin identity is stale"
  );
  const localApk = join(ROOT, build.artifact.path);
  if (existsSync(localApk)) {
    const apk = readFileSync(localApk);
    assert(apk.length === build.artifact.bytes, "local Android APK size is stale");
    assert(sha256(apk) === build.artifact.sha256, "local Android APK hash is stale");
  }
  return build;
}

function verifyCurrentAndroidBuild(target, build) {
  const current = target.currentBuild;
  assert(current, "Android evidence is missing the current build record");
  assert(artifactMatches(current.apk, build.artifact), "Android current build identity is stale");
  assert(
    ["not_tested", "partial_physical", "captured_physical"].includes(current.status),
    "Android current build has an invalid status"
  );
  verifyScenarioRecords(current.scenarios, "Android current build", current.status === "captured_physical");
  if (current.status === "not_tested") {
    assert(
      current.scenarios.every(({ status }) => status === "not_tested"),
      "an untested Android build cannot contain physical results"
    );
  }
  if (target.status === "captured_physical") {
    assert(current.status === "captured_physical", "captured Android target is not bound to the current APK");
  } else {
    assert(current.status !== "captured_physical", "pending Android target contradicts current build evidence");
  }
}

function syncCurrentAndroidBuild(evidence, build) {
  const target = evidence.targets?.android_physical;
  assert(target, "mobile evidence is missing android_physical");
  const sameArtifact = artifactMatches(target.currentBuild?.apk, build.artifact);
  const staleValidation =
    target.currentBuild?.status === "not_tested" &&
    (target.lastValidation !== undefined || target.history !== undefined);
  if (sameArtifact && !staleValidation) return false;

  const nextTarget = {
    ...target,
    status: "pending",
    reason: "The current source-bound APK has not been physically tested.",
    currentBuild: {
      status: "not_tested",
      apk: { ...build.artifact },
      scenarios: notTestedScenarioRecords()
    },
    confirmations: Object.fromEntries(REQUIRED_CONFIRMATIONS.map((flag) => [flag, false]))
  };
  for (const field of [
    "capturedAt",
    "operator",
    "device",
    "apk",
    "screenshot",
    "lastValidation",
    "history"
  ]) {
    delete nextTarget[field];
  }
  evidence.targets.android_physical = nextTarget;
  evidence.updatedAt = new Date().toISOString();
  evidence.releaseGateSatisfied = false;
  return true;
}

function runSelfTest() {
  const oldArtifact = {
    path: ANDROID_APK,
    bytes: 100,
    sha256: "a".repeat(64),
    applicationId: "dev.vibex.remote"
  };
  const build = {
    artifact: {
      path: ANDROID_APK,
      bytes: 200,
      sha256: "b".repeat(64),
      applicationId: "dev.vibex.remote",
      minSdk: 26,
      targetSdk: 36
    }
  };
  const lastValidation = {
    status: "captured_physical",
    capturedAt: "2026-07-25T00:00:00.000Z",
    apk: oldArtifact,
    scenarios: SCENARIO_DEFINITIONS.map(({ id }) => ({ id, status: "passed", note: "passed" }))
  };
  const evidence = {
    releaseGateSatisfied: true,
    targets: {
      android_physical: {
        status: "captured_physical",
        synthetic: false,
        capturedAt: lastValidation.capturedAt,
        operator: "operator",
        device: { model: "device" },
        apk: oldArtifact,
        screenshot: { path: "old.png", sha256: "c".repeat(64) },
        currentBuild: { status: "captured_physical", apk: oldArtifact, scenarios: lastValidation.scenarios },
        lastValidation,
        history: [],
        confirmations: Object.fromEntries(REQUIRED_CONFIRMATIONS.map((flag) => [flag, true]))
      }
    }
  };

  assert(syncCurrentAndroidBuild(evidence, build), "a new APK did not invalidate physical evidence");
  const target = evidence.targets.android_physical;
  assert(target.status === "pending", "a rebuilt APK retained a captured target status");
  assert(target.currentBuild.status === "not_tested", "a rebuilt APK was not marked not_tested");
  assert(artifactMatches(target.currentBuild.apk, build.artifact), "the rebuilt APK identity was not recorded");
  assert(
    target.currentBuild.scenarios.every(({ status }) => status === "not_tested"),
    "a rebuilt APK retained physical scenario results"
  );
  assert(!("history" in target), "a rebuilt APK retained validation history");
  assert(!("lastValidation" in target), "a rebuilt APK retained a previous validation");
  assert(
    REQUIRED_CONFIRMATIONS.every((flag) => target.confirmations[flag] === false),
    "a rebuilt APK retained physical confirmations"
  );
  assert(!("device" in target) && !("screenshot" in target), "current-device metadata survived rebuild invalidation");
  assert(evidence.releaseGateSatisfied === false, "a rebuilt APK retained a satisfied release gate");

  const synchronized = JSON.stringify(evidence);
  assert(!syncCurrentAndroidBuild(evidence, build), "matching APK evidence was invalidated twice");
  assert(JSON.stringify(evidence) === synchronized, "matching APK synchronization changed evidence");

  verifyCurrentAndroidScreenshot(lastValidation, build);
  const missingScreenshot = "docs/parity/screenshots/current/spikes/android-self-test-missing.png";
  assert(!existsSync(join(ROOT, missingScreenshot)), "mobile self-test screenshot fixture unexpectedly exists");
  let missingCurrentScreenshotRejected = false;
  try {
    verifyCurrentAndroidScreenshot(
      {
        ...lastValidation,
        apk: build.artifact,
        screenshot: { path: missingScreenshot, sha256: "c".repeat(64) }
      },
      build
    );
  } catch (error) {
    missingCurrentScreenshotRejected =
      error instanceof Error && error.message === `${missingScreenshot} is missing`;
  }
  assert(missingCurrentScreenshotRejected, "current Android validation accepted a missing screenshot");
  console.log("GPUI-WASM mobile evidence self-test passed");
}

function verifyPhysicalEvidence(evidence, build) {
  assert(evidence.schemaVersion === "vibex-mobile-physical-evidence.v1", "mobile evidence schema is invalid");
  assert(
    JSON.stringify(evidence.requiredTargets) === JSON.stringify(["android_physical", "ios_physical"]),
    "mobile evidence target set is invalid"
  );
  assert(
    JSON.stringify(evidence.scenarioProtocol?.statuses) ===
      JSON.stringify(["passed", "failed", "not_tested"]),
    "mobile evidence scenario statuses are invalid"
  );
  assert(
    JSON.stringify(evidence.scenarioProtocol?.android) ===
      JSON.stringify(SCENARIO_DEFINITIONS.map(({ id }) => id)),
    "mobile evidence Android scenario set is invalid"
  );
  for (const id of evidence.requiredTargets) {
    const target = evidence.targets[id];
    assert(target, `mobile evidence is missing ${id}`);
    assert(["pending", "captured_physical"].includes(target.status), `${id} has an invalid status`);
    assert(target.synthetic === false, `${id} cannot use synthetic evidence`);
    if (id === "ios_physical") {
      assert(
        target.status === "pending" && target.disposition === "accepted_deviation",
        "iOS physical evidence must remain pending with the approved deferred disposition"
      );
      assert(
        typeof target.followUp === "string" && target.followUp.length > 0,
        "iOS physical evidence is missing its follow-up"
      );
    }
    if (target.status === "captured_physical") {
      for (const confirmation of REQUIRED_CONFIRMATIONS) {
        assert(target.confirmations[confirmation] === true, `${id} is missing ${confirmation}`);
      }
    }
  }
  verifyCurrentAndroidBuild(evidence.targets.android_physical, build);
  const allCaptured = evidence.requiredTargets.every(
    (id) => evidence.targets[id].status === "captured_physical"
  );
  assert(evidence.releaseGateSatisfied === allCaptured, "mobile release aggregate contradicts target evidence");
}

function verifyCurrentAndroidScreenshot(validation, build) {
  if (validation.apk?.sha256 !== build.artifact.sha256) return;
  assert(validation.screenshot?.path, "current Android screenshot is missing");
  const screenshot = join(ROOT, validation.screenshot.path);
  assert(existsSync(screenshot), `${validation.screenshot.path} is missing`);
  assert(
    sha256(readFileSync(screenshot)) === validation.screenshot.sha256,
    "current Android screenshot hash is stale"
  );
}

async function waitForAndroidRuntime(device) {
  const deadline = Date.now() + 30_000;
  let pid = "";
  while (!/^\d+$/.test(pid) && Date.now() < deadline) {
    pid = command("adb", ["-s", device, "shell", "pidof", "dev.vibex.remote"]);
    if (!pid) await new Promise((resolvePromise) => setTimeout(resolvePromise, 250));
  }
  assert(/^\d+$/.test(pid), "Android GPUI process did not start");

  const port = command("adb", [
    "-s",
    device,
    "forward",
    "tcp:0",
    `localabstract:webview_devtools_remote_${pid}`
  ]);
  assert(/^\d+$/.test(port), "Android WebView CDP forwarding failed");
  let browser = null;
  try {
    while (Date.now() < deadline) {
      try {
        const response = await fetch(`http://127.0.0.1:${port}/json/version`);
        if (response.ok) break;
      } catch {
        // The debug WebView appears after the Activity is reported as visible.
      }
      await new Promise((resolvePromise) => setTimeout(resolvePromise, 250));
    }
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
    const page = browser.contexts()[0]?.pages()[0];
    assert(page, "Android WebView page is unavailable");
    await page.waitForFunction(
      () => {
        const gate = window.__VIBEX_GATE__;
        return gate?.gpuiBooted === true && gate.state === "ready";
      },
      null,
      { timeout: 30_000 }
    );
    await page.waitForFunction(
      () => {
        const layer = document.querySelector("#gate-status-layer");
        if (!layer) return false;
        const style = getComputedStyle(layer);
        return style.visibility === "hidden" && Number(style.opacity) === 0;
      },
      null,
      { timeout: 5_000 }
    );
    return await page.evaluate(() => {
      const gate = window.__VIBEX_GATE__;
      return {
        state: gate.state,
        errorCount: gate.errors?.length ?? 0,
        pixelMetrics: gate.pixelMetrics ?? null,
        secureStorageStatus: gate.probes?.secureStorage?.status ?? "missing"
      };
    });
  } finally {
    await browser?.close();
    command("adb", ["-s", device, "forward", "--remove", `tcp:${port}`]);
  }
}

async function captureAndroid(evidence) {
  const device = argument("device");
  const operator = argument("operator");
  assert(device, "Android capture requires --device <adb serial>");
  assert(operator, "Android capture requires --operator <name>");
  assert(
    REQUIRED_CONFIRMATIONS.some((confirmation) => process.argv.includes(`--${confirmation}`)),
    "Android capture requires at least one physical scenario confirmation"
  );
  assert(existsSync(join(ROOT, ANDROID_APK)), `${ANDROID_APK} is missing; build the debug APK first`);
  const connected = command("adb", ["devices"]).split("\n").slice(1);
  assert(
    connected.some((line) => line.startsWith(`${device}\tdevice`)),
    `ADB device ${device} is not connected and authorized`
  );
  command("adb", ["-s", device, "install", "-r", join(ROOT, ANDROID_APK)]);
  command("adb", [
    "-s",
    device,
    "shell",
    "am",
    "start",
    "-W",
    "-n",
    "dev.vibex.remote/.MainActivity"
  ]);
  const runtime = await waitForAndroidRuntime(device);
  assert(runtime.errorCount === 0, "Android GPUI runtime contains errors");
  if (process.argv.includes("--pixels-pass")) {
    assert(
      runtime.pixelMetrics?.uniqueColors >= 8 && runtime.pixelMetrics?.standardDeviation >= 2,
      "Android WebGPU pixel confirmation does not match the live canvas"
    );
  }
  if (process.argv.includes("--secure-storage-pass")) {
    assert(
      runtime.secureStorageStatus === "passed",
      "Android secure-storage confirmation does not match the live probe"
    );
  }
  const screenshot = command("adb", ["-s", device, "exec-out", "screencap", "-p"], {
    binary: true
  });
  assert(screenshot.length > 10_000, "Android screenshot is not credible");
  mkdirSync(dirname(join(ROOT, ANDROID_SCREENSHOT)), { recursive: true });
  writeFileSync(join(ROOT, ANDROID_SCREENSHOT), screenshot);

  const property = (name) => command("adb", ["-s", device, "shell", "getprop", name]);
  const build = verifyBuildEvidence();
  const scenarios = scenarioRecordsFromConfirmations();
  const allScenariosPassed = scenarios.every(({ status }) => status === "passed");
  const validation = {
    status: allScenariosPassed ? "captured_physical" : "partial_physical",
    observationMode: "physical_operator_confirmations",
    capturedAt: new Date().toISOString(),
    operator,
    device: {
      serialSha256: sha256(device),
      manufacturer: property("ro.product.manufacturer"),
      model: property("ro.product.model"),
      osVersion: property("ro.build.version.release"),
      sdk: Number(property("ro.build.version.sdk")),
      webViewPackage: property("ro.webview.chromium.package_name") || "com.google.android.webview"
    },
    apk: build.artifact,
    screenshot: {
      path: ANDROID_SCREENSHOT,
      sha256: sha256(screenshot)
    },
    scenarios
  };
  evidence.targets.android_physical = {
    status: allScenariosPassed ? "captured_physical" : "pending",
    synthetic: false,
    reason: allScenariosPassed
      ? "All physical Android scenarios were explicitly confirmed for the current APK."
      : "The current APK has partial physical confirmation; unconfirmed scenarios remain not_tested.",
    capturedAt: validation.capturedAt,
    operator,
    device: validation.device,
    apk: build.artifact,
    screenshot: validation.screenshot,
    currentBuild: {
      status: allScenariosPassed ? "captured_physical" : "partial_physical",
      apk: build.artifact,
      scenarios
    },
    lastValidation: validation,
    confirmations: Object.fromEntries(
      SCENARIO_DEFINITIONS.map(({ flag }) => [flag, process.argv.includes(`--${flag}`)])
    )
  };
  evidence.updatedAt = validation.capturedAt;
  evidence.releaseGateSatisfied =
    evidence.targets.android_physical.status === "captured_physical" &&
    evidence.targets.ios_physical.status === "captured_physical";
  return evidence;
}

if (process.argv.includes("--self-test")) {
  runSelfTest();
} else {
  const build = verifyBuildEvidence();
  assert(existsSync(join(ROOT, EVIDENCE_PATH)), `${EVIDENCE_PATH} is missing`);
  let evidence = readJson(EVIDENCE_PATH);
  const platform = argument("platform");
  const syncCurrentBuild = process.argv.includes("--sync-current-build");
  assert(!(platform && syncCurrentBuild), "physical capture and build synchronization are mutually exclusive");

  if (syncCurrentBuild) {
    assert(process.argv.includes("--write"), "build synchronization requires --write");
    if (syncCurrentAndroidBuild(evidence, build)) {
      writeFileSync(join(ROOT, EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
    }
  }
  verifyPhysicalEvidence(evidence, build);
  if (evidence.targets.android_physical.lastValidation) {
    verifyCurrentAndroidScreenshot(evidence.targets.android_physical.lastValidation, build);
  }

  if (platform) {
    assert(process.argv.includes("--write"), "physical capture requires --write");
    if (platform === "android") {
      evidence = await captureAndroid(evidence);
    } else if (platform === "ios") {
      assert(process.platform === "darwin", "iOS physical capture requires a macOS/Xcode host");
      fail("iOS capture requires signed-device automation and is intentionally still pending");
    } else {
      fail(`unsupported physical platform: ${platform}`);
    }
    writeFileSync(join(ROOT, EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
    verifyPhysicalEvidence(evidence, build);
    verifyCurrentAndroidScreenshot(evidence.targets.android_physical.lastValidation, build);
  }

  const androidStatus =
    `${evidence.targets.android_physical.status}/` +
    `${evidence.targets.android_physical.currentBuild.status}`;
  console.log(
    `GPUI-WASM mobile evidence verified: Android=${androidStatus}, ` +
      `iOS=${evidence.targets.ios_physical.status}, APK=${build.artifact.sha256.slice(0, 12)}`
  );
}

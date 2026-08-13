import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { chromium } from "@playwright/test";
import {
  activeDeviceDelta,
  cleanupFixture,
  configureMethods,
  controlJson,
  createOffer,
  recoveryDriver,
  revokeExact,
  setupFixture,
  trustSummary,
  waitDesktop
} from "./e2e-desktop-pairing.mjs";
import {
  PRODUCT_PAIRING_EVIDENCE_PATH,
  mergeProductPairingPhysical,
  mergeProductPairingPhysicalUnavailable,
  resolveProductPairingCandidate
} from "./product-pairing-evidence.mjs";
import {
  assert,
  fail,
  runProductMatrix,
  waitForConnectedRuntime,
  waitForRuntime
} from "./e2e-workflows.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const APP_ID = "dev.vibex.remote";
const ANDROID_BUILD = join(
  ROOT,
  "apps/mobile/artifacts/android-debug-build.json"
);
const MAX_COMMAND_OUTPUT = 64 * 1024 * 1024;

function option(name, fallback = null) {
  const inline = process.argv.find((argument) => argument.startsWith(`--${name}=`));
  if (inline) return inline.slice(name.length + 3);
  const index = process.argv.indexOf(`--${name}`);
  return index >= 0 ? process.argv[index + 1] ?? fallback : fallback;
}

function run(command, args, code, encoding = "utf8") {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding,
    env: process.env,
    maxBuffer: MAX_COMMAND_OUTPUT,
    stdio: ["ignore", "pipe", "pipe"]
  });
  if (result.error || result.status !== 0) fail(code);
  return encoding === null ? result.stdout : (result.stdout ?? "").trim();
}

function adb(serial, args, code = "product_pairing_adb_command_failed") {
  return run("adb", ["-s", serial, ...args], code);
}

function adbMaybe(serial, args) {
  const result = spawnSync("adb", ["-s", serial, ...args], {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 4 * 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"]
  });
  return result.status === 0 ? (result.stdout ?? "").trim() : "";
}

function adbBytes(serial, args, code) {
  return run("adb", ["-s", serial, ...args], code, null);
}

function parseAuthorizedDevices(output) {
  return output
    .split("\n")
    .slice(1)
    .map((line) => line.trim().split(/\s+/))
    .filter((parts) => parts.length >= 2 && parts[1] === "device")
    .map(([serial]) => serial);
}

function authorizedDevice(requested) {
  const output = run("adb", ["devices"], "product_pairing_adb_devices_failed");
  const devices = parseAuthorizedDevices(output);
  if (requested) assert(devices.includes(requested), "product_pairing_android_device_unavailable");
  assert(requested || devices.length === 1, "product_pairing_android_device_selection_required");
  return requested ?? devices[0];
}

function androidDeviceProperties(serial, command = adb) {
  return {
    fingerprint: command(serial, ["shell", "getprop", "ro.build.fingerprint"]),
    model: command(serial, ["shell", "getprop", "ro.product.model"]),
    kernelQemu: command(serial, ["shell", "getprop", "ro.kernel.qemu"]),
    bootQemu: command(serial, ["shell", "getprop", "ro.boot.qemu"]),
    hardware: command(serial, ["shell", "getprop", "ro.hardware"])
  };
}

function isPhysicalAndroidDevice(properties) {
  return Boolean(
    properties.fingerprint &&
      properties.model &&
      properties.kernelQemu !== "1" &&
      properties.bootQemu !== "1" &&
      !/(?:generic|sdk_gphone|emulator|ranchu|goldfish|vbox|nox)/i.test(
        `${properties.fingerprint} ${properties.model} ${properties.hardware}`
      )
  );
}

function physicalDevice(serial) {
  const properties = androidDeviceProperties(serial);
  assert(properties.fingerprint && properties.model, "product_pairing_android_identity_missing");
  assert(isPhysicalAndroidDevice(properties), "product_pairing_android_not_physical");
  return {
    fingerprint: properties.fingerprint,
    deviceIdentitySha256: createHash("sha256")
      .update(`${serial}:${properties.fingerprint}`)
      .digest("hex")
  };
}

function physicalUnavailabilityReason() {
  const result = spawnSync("adb", ["devices"], {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 4 * 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"]
  });
  if (result.error || result.status !== 0) {
    return "product_pairing_android_runner_unavailable";
  }
  const devices = parseAuthorizedDevices(result.stdout ?? "");
  const physicalAvailable = devices.some((device) =>
    isPhysicalAndroidDevice(androidDeviceProperties(device, adbMaybe))
  );
  assert(!physicalAvailable, "product_pairing_android_physical_device_available");
  return "product_pairing_android_device_unavailable";
}

function recordPhysicalUnavailability() {
  assert(process.argv.includes("--write"), "product_pairing_android_unavailable_write_required");
  const reasonCode = physicalUnavailabilityReason();
  const candidate = resolveProductPairingCandidate(ROOT);
  const path = join(ROOT, PRODUCT_PAIRING_EVIDENCE_PATH);
  const existing = existsSync(path) ? JSON.parse(readFileSync(path, "utf8")) : null;
  const evidence = mergeProductPairingPhysicalUnavailable(existing, candidate, reasonCode);
  writeFileSync(path, `${JSON.stringify(evidence, null, 2)}\n`);
  console.log(`GPUI product pairing Android release risk recorded: ${reasonCode}`);
}

function buildAndroidArtifact() {
  run(
    process.execPath,
    ["apps/mobile/scripts/build-android-debug.mjs"],
    "product_pairing_android_build_failed"
  );
  assert(existsSync(ANDROID_BUILD), "product_pairing_android_build_evidence_missing");
  let build;
  try {
    build = JSON.parse(readFileSync(ANDROID_BUILD, "utf8"));
  } catch {
    fail("product_pairing_android_build_evidence_invalid");
  }
  const artifact = build.artifact;
  const apk = artifact?.path ? join(ROOT, artifact.path) : null;
  assert(
    build.schemaVersion === "vibex-android-build.v1" &&
      artifact?.applicationId === APP_ID &&
      /^[0-9a-f]{64}$/.test(artifact.sha256 ?? "") &&
      apk &&
      existsSync(apk),
    "product_pairing_android_artifact_invalid"
  );
  const bytes = readFileSync(apk);
  assert(
    bytes.length === artifact.bytes &&
      createHash("sha256").update(bytes).digest("hex") === artifact.sha256,
    "product_pairing_android_artifact_stale"
  );
  return { apk, sha256: artifact.sha256, bytes: artifact.bytes };
}

function verifyInstalledApk(serial, artifact) {
  const paths = adb(
    serial,
    ["shell", "pm", "path", APP_ID],
    "product_pairing_android_package_missing"
  )
    .split("\n")
    .filter((line) => line.startsWith("package:") && line.endsWith("/base.apk"))
    .map((line) => line.slice("package:".length));
  assert(paths.length === 1, "product_pairing_android_package_ambiguous");
  const installed = adbBytes(
    serial,
    ["exec-out", "cat", paths[0]],
    "product_pairing_android_package_read_failed"
  );
  assert(
    installed.length === artifact.bytes &&
      createHash("sha256").update(installed).digest("hex") === artifact.sha256,
    "product_pairing_android_package_stale"
  );
}

async function waitForAndroidCdp(serial, port) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json/version`, {
        signal: AbortSignal.timeout(1_000)
      });
      if (response.ok) return;
    } catch {
      // The physical WebView starts asynchronously.
    }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 250));
  }
  fail("product_pairing_android_webview_debugging_unavailable");
}

async function waitPairingState(page, states, code, timeout = 90_000) {
  const expected = new Set(Array.isArray(states) ? states : [states]);
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const state = await page.evaluate(
      () => window.__VIBEX_GATE__?.remote?.pairing?.state ?? null
    );
    if (expected.has(state)) return state;
    await page.waitForTimeout(100);
  }
  fail(code);
}

async function verifyMobileViewport(page) {
  const projection = await page.evaluate(() => {
    const canvases = [...document.querySelectorAll("canvas")];
    return {
      gate: document.body.dataset.gateState,
      width: window.innerWidth,
      height: window.innerHeight,
      canvasVisible: canvases.some((canvas) => {
        const rect = canvas.getBoundingClientRect();
        return rect.width > 0 && rect.height > 0;
      })
    };
  });
  assert(
    projection.gate === "ready" &&
      projection.width >= 240 &&
      projection.width <= 1_024 &&
      projection.height >= 400 &&
      projection.canvasVisible,
    "product_pairing_android_viewport_invalid"
  );
}

async function launchWebView(serial, artifact, reuseInstalled) {
  if (reuseInstalled) {
    verifyInstalledApk(serial, artifact);
  } else {
    adb(serial, ["install", "-r", artifact.apk], "product_pairing_android_install_failed");
  }
  adb(serial, ["shell", "pm", "clear", APP_ID], "product_pairing_android_state_reset_failed");
  adb(serial, ["shell", "am", "start", "-n", `${APP_ID}/.MainActivity`], "product_pairing_android_launch_failed");
  let pid = "";
  const deadline = Date.now() + 20_000;
  while (!pid && Date.now() < deadline) {
    pid = adbMaybe(serial, ["shell", "pidof", APP_ID]);
    if (!pid) await new Promise((resolvePromise) => setTimeout(resolvePromise, 250));
  }
  assert(/^\d+$/.test(pid), "product_pairing_android_process_missing");
  const port = adb(serial, [
    "forward",
    "tcp:0",
    `localabstract:webview_devtools_remote_${pid}`
  ]);
  assert(/^\d+$/.test(port), "product_pairing_android_cdp_forward_failed");
  await waitForAndroidCdp(serial, port);
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  const page = browser.contexts()[0]?.pages()[0];
  assert(page, "product_pairing_android_webview_page_missing");
  await waitForRuntime(page);
  await waitPairingState(page, ["unpaired", "idle"], "product_pairing_android_not_fresh");
  return { browser, page, port };
}

function productDeepLink(link) {
  let url;
  try {
    url = new URL(link);
  } catch {
    fail("product_pairing_android_link_invalid");
  }
  assert(url.hash.startsWith("#/pair/"), "product_pairing_android_link_invalid");
  assert(
    url.protocol === "vibex:" &&
      url.hostname === "open" &&
      ["/tailnet", "/self_hosted_relay"].includes(url.pathname),
    "product_pairing_android_transport_invalid"
  );
  return url.toString();
}

async function clearRevokedDevice(page) {
  await waitPairingState(page, "revoked", "product_pairing_android_revoke_missing");
  await page.locator("#pairing-clear").click();
  await waitPairingState(page, "unpaired", "product_pairing_android_clear_failed");
}

let androidArtifact = null;
let serial = null;
let physicalIdentity = null;
let reuseInstalled = false;

async function runPhysicalMode(environment) {
  assert(["tailscale", "relay"].includes(environment.mode), "product_pairing_android_mode_invalid");
  await configureMethods(environment);
  const launched = await launchWebView(serial, androidArtifact, reuseInstalled);
  let fixture = null;
  let pairedDevice = null;
  let link = null;
  try {
    const before = await trustSummary(environment);
    link = await createOffer(environment, "full_control");
    const deepLink = productDeepLink(link);
    adb(
      serial,
      [
        "shell",
        "am",
        "start",
        "-W",
        "-a",
        "android.intent.action.VIEW",
        "-d",
        deepLink,
        "-n",
        `${APP_ID}/.MainActivity`
      ],
      "product_pairing_android_app_link_failed"
    );
    link = null;
    await waitPairingState(launched.page, "preview", "product_pairing_android_preview_missing");
    await launched.page.locator("#pairing-confirm").click();
    await waitForConnectedRuntime(launched.page);
    assert(
      await launched.page.evaluate(() => !location.hash.includes("/pair/")),
      "product_pairing_android_fragment_not_scrubbed"
    );
    const after = await trustSummary(environment);
    const delta = activeDeviceDelta(before, after);
    assert(delta.length === 1, "product_pairing_android_device_delta_invalid");
    pairedDevice = delta[0];
    await waitDesktop(
      environment,
      (snapshot) => snapshot.offerStatus === "claimed",
      "product_pairing_android_claim_not_observed"
    );
    fixture = await setupFixture(environment);
    await runProductMatrix(
      launched.page,
      recoveryDriver(environment, launched.page),
      "android_physical",
      environment.mode,
      null,
      environment.relayLogPath,
      fixture
    );
    await verifyMobileViewport(launched.page);
    const identity = await controlJson(environment, "/identity/summary");
    assert(
      identity.schemaVersion === "remote-access-identity-summary.v1" &&
        /^[0-9a-f]{64}$/.test(identity.serverIdentitySha256),
      "product_pairing_android_server_identity_invalid"
    );
    const candidate = resolveProductPairingCandidate(ROOT);
    const result = {
      transport: environment.mode,
      status: "passed",
      capturedAt: new Date().toISOString(),
      candidateDigest: candidate.candidateDigest,
      deviceIdentitySha256: physicalIdentity.deviceIdentitySha256,
      apkSha256: androidArtifact.sha256,
      serverIdentitySha256: identity.serverIdentitySha256,
      mobileViewport: "passed",
      productPairing: "passed",
      workflowSmoke: "passed",
      redactionScan: "passed"
    };
    if (environment.writeEvidence) {
      const path = join(ROOT, PRODUCT_PAIRING_EVIDENCE_PATH);
      const existing = existsSync(path) ? JSON.parse(readFileSync(path, "utf8")) : null;
      const evidence = mergeProductPairingPhysical(
        existing,
        candidate,
        environment.mode,
        result
      );
      writeFileSync(path, `${JSON.stringify(evidence, null, 2)}\n`);
    }
    return result;
  } finally {
    link = null;
    if (pairedDevice) {
      await revokeExact(environment, pairedDevice).catch(() => {});
      await clearRevokedDevice(launched.page).catch(() => {});
    }
    if (fixture) await cleanupFixture(environment).catch(() => {});
    await launched.browser.close().catch(() => {});
    adbMaybe(serial, ["forward", "--remove", `tcp:${launched.port}`]);
  }
}

async function main() {
  if (process.argv.includes("--record-unavailable")) {
    recordPhysicalUnavailability();
    return;
  }
  const transport = option("transport");
  assert(["tailscale", "relay"].includes(transport), "product_pairing_android_transport_required");
  if (transport === "relay") {
    assert(
      process.env.VIBEX_E2E_TLS_CERT &&
        process.env.VIBEX_E2E_TLS_KEY &&
        process.env.VIBEX_E2E_TLS_CA_CERT &&
        process.env.VIBEX_E2E_PUBLIC_HOST,
      "product_pairing_android_trusted_relay_tls_required"
    );
  }
  androidArtifact = buildAndroidArtifact();
  reuseInstalled = process.argv.includes("--reuse-installed");
  serial = authorizedDevice(option("device"));
  physicalIdentity = physicalDevice(serial);
  const { runProductPairingEnvironment } = await import(
    "./e2e-local-env/run-product-pairing.mjs"
  );
  await runProductPairingEnvironment({ runMode: runPhysicalMode });
}

try {
  await main();
} catch (error) {
  const code = /^[a-z0-9_]+$/.test(error?.code ?? "")
    ? error.code
    : "product_pairing_android_e2e_failed";
  console.error(`GPUI product pairing Android E2E failed: ${code}`);
  process.exitCode = 1;
}

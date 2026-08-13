import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { chromium } from "@playwright/test";
import { resolveGpuiSourceIdentities } from "./source-identities.mjs";
import {
  MOBILE_RUNTIME_SOURCE_INPUTS,
  sourceTreeSha256
} from "./wasm-source-tree.mjs";
import { startWasmServer } from "./mobile-wasm-test-server.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DIST = join(ROOT, "apps/mobile-wasm/dist");
const EVIDENCE_PATH = "docs/platform/evidence/mobile-wasm-host-gate.json";
const WRITE = process.argv.includes("--write");
const VIEWPORTS = Object.freeze({
  medium: { width: 1280, height: 800, deviceScaleFactor: 1 },
  compact: { width: 390, height: 844, deviceScaleFactor: 2 }
});

function fail(message) {
  throw new Error(message);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function repositoryFile(path) {
  return readFileSync(join(ROOT, path));
}

function diagnosticsUrl(origin) {
  const url = new URL(origin);
  url.searchParams.set("diagnostics", "gate");
  return url.href;
}

async function waitForReady(page) {
  await page.waitForFunction(
    () => ["ready", "unsupported", "error"].includes(document.body.dataset.gateState),
    null,
    { timeout: 45_000 }
  );
  const state = await page.evaluate(() => document.body.dataset.gateState);
  assert(state === "ready", `mobile WASM host reached ${state}`);
  await page.waitForFunction(
    () => {
      const probes = window.__VIBEX_GATE__?.probes;
      return [probes?.fetch?.status, probes?.webSocket?.status].every((status) =>
        ["passed", "failed", "unsupported"].includes(status)
      );
    },
    null,
    { timeout: 10_000 }
  );
  await page.waitForTimeout(100);
}

function pageDiagnostics(page) {
  const consoleMessages = [];
  const pageErrors = [];
  page.on("console", (message) => {
    const value = message.text();
    if (!value.includes("/__gate/") && consoleMessages.length < 40) {
      consoleMessages.push({ type: message.type(), text: value });
    }
  });
  page.on("pageerror", (error) => pageErrors.push(error.message));
  return { consoleMessages, pageErrors };
}

function assertCleanDiagnostics(diagnostics, label) {
  assert(diagnostics.pageErrors.length === 0, `${label} reported page errors`);
  assert(
    diagnostics.consoleMessages.every(
      (message) =>
        !message.text.includes("Failed to load a font") && message.type !== "error"
    ),
    `${label} reported browser console errors`
  );
}

async function inspectProductEntry(browser, origin, name, viewport, expectedShell) {
  const context = await browser.newContext({
    viewport: { width: viewport.width, height: viewport.height },
    deviceScaleFactor: viewport.deviceScaleFactor,
    hasTouch: name === "compact",
    isMobile: name === "compact"
  });
  const page = await context.newPage();
  const diagnostics = pageDiagnostics(page);
  try {
    await page.goto(origin, { waitUntil: "domcontentloaded", timeout: 45_000 });
    await waitForReady(page);
    const snapshot = await page.evaluate(() => {
      const runtime = window.__VIBEX_GATE__;
      const surfaces = ["agent", "files", "git", "terminal", "management"];
      const renderedSurfaces = surfaces.map((surface) => {
        runtime.workflowAction({ kind: "select_surface", surface });
        return runtime.workflowState().activeSurface;
      });
      runtime.workflowAction({ kind: "select_surface", surface: "agent" });
      const canvas = document.querySelector("canvas[data-vibex-gate-canvas]");
      const bounds = canvas.getBoundingClientRect();
      return {
        root: runtime.rootState(),
        workflow: runtime.workflowState(),
        remoteContract: runtime.remote.contract,
        hostContract: runtime.contract,
        runtimeBuild: runtime.build,
        host: runtime.hostSnapshot(),
        probes: runtime.probes,
        pixels: runtime.pixelMetrics,
        canvas: {
          width: canvas.width,
          height: canvas.height,
          cssWidth: bounds.width,
          cssHeight: bounds.height
        },
        pairingState: runtime.remote.pairing.state,
        renderedSurfaces
      };
    });

    assert(snapshot.root.mode === "workbench", `${name} default entry is not the workbench`);
    assert(snapshot.root.defaultMode === "workbench", `${name} default mode drifted`);
    assert(snapshot.root.gateFixtureIsProductSource === false, `${name} uses diagnostics as product data`);
    assert(snapshot.workflow.shell === expectedShell, `${name} resolved ${snapshot.workflow.shell}`);
    assert(snapshot.workflow.shell !== "wide", `${name} exposed Wide in the mobile runtime`);
    assert(
      JSON.stringify(snapshot.renderedSurfaces) ===
        JSON.stringify(["agent", "files", "git", "terminal", "management"]),
      `${name} did not retain the five mobile workflow surfaces`
    );
    assert(snapshot.remoteContract.deployment.runtimeRole === "capacitor_mobile_runtime", `${name} runtime role drifted`);
    assert(snapshot.remoteContract.deployment.browserHost === "development_and_test_only", `${name} host role drifted`);
    assert(snapshot.hostContract.breakpoints.maximumShell === "medium", `${name} can still select Wide`);
    assert(snapshot.runtimeBuild.runtimeRole === "capacitor_mobile_runtime", `${name} build role drifted`);
    assert(snapshot.runtimeBuild.browserHost === "development_and_test_only", `${name} build host role drifted`);
    assert(snapshot.pairingState === "unpaired", `${name} did not enter mobile pairing recovery`);
    assert(snapshot.probes.fetch.status === "passed", `${name} Fetch probe failed`);
    assert(snapshot.probes.webSocket.status === "passed", `${name} WebSocket probe failed`);
    assert(snapshot.pixels.uniqueColors >= 8, `${name} canvas is blank`);
    assert(snapshot.pixels.standardDeviation >= 2, `${name} canvas lacks pixel variance`);
    assert(snapshot.canvas.width > 0 && snapshot.canvas.height > 0, `${name} canvas is unsized`);
    assertCleanDiagnostics(diagnostics, name);
    return snapshot;
  } finally {
    await context.close();
  }
}

async function touchSwipe(cdp, x, startY, endY, steps = 6) {
  const point = (y) => [{ x, y, radiusX: 8, radiusY: 8, force: 0.8, id: 1 }];
  await cdp.send("Input.dispatchTouchEvent", {
    type: "touchStart",
    touchPoints: point(startY)
  });
  for (let step = 1; step <= steps; step += 1) {
    await cdp.send("Input.dispatchTouchEvent", {
      type: "touchMove",
      touchPoints: point(startY + ((endY - startY) * step) / steps)
    });
  }
  await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
}

async function inspectHostBridge(browser, origin) {
  const medium = await browser.newContext({
    viewport: { width: VIEWPORTS.medium.width, height: VIEWPORTS.medium.height },
    deviceScaleFactor: VIEWPORTS.medium.deviceScaleFactor,
    colorScheme: "light"
  });
  const page = await medium.newPage();
  const diagnostics = pageDiagnostics(page);
  try {
    await page.goto(diagnosticsUrl(origin), {
      waitUntil: "domcontentloaded",
      timeout: 45_000
    });
    await waitForReady(page);
    const initial = await page.evaluate(() => ({
      fixture: window.__VIBEX_GATE__.fixtureState(),
      host: window.__VIBEX_GATE__.hostSnapshot(),
      pairingState: window.__VIBEX_GATE__.remote.pairing.state,
      pairingHidden: document.querySelector("#pairing-layer").classList.contains("is-hidden")
    }));
    assert(initial.fixture.composerValue === "GPUI-WASM gate", "diagnostic input did not initialize");
    assert(initial.pairingState === "idle" && initial.pairingHidden, "diagnostics are blocked by pairing UI");

    await page.mouse.click(220, 343);
    const input = page.locator("input[data-vibex-gate-input]");
    await input.waitFor({ state: "attached" });
    await input.focus();
    await page.keyboard.press("Control+A");
    await page.keyboard.type("Mobile runtime input");
    await page.waitForTimeout(100);
    const afterInput = await page.evaluate(() => ({
      fixture: window.__VIBEX_GATE__.fixtureState(),
      interactions: window.__VIBEX_GATE__.interactions
    }));
    assert(
      afterInput.fixture.composerValue.includes("Mobile runtime input"),
      "keyboard input did not reach GPUI InputState"
    );
    assert(afterInput.interactions.keyboard > 0, "keyboard bridge did not record input");

    await page.emulateMedia({ colorScheme: "dark" });
    await page.waitForFunction(
      () =>
        window.__VIBEX_GATE__.hostSnapshot().darkMode === true &&
        window.__VIBEX_GATE__.fixtureState().themeDark === true
    );
    await page.emulateMedia({ colorScheme: "light" });
    await page.waitForFunction(
      () =>
        window.__VIBEX_GATE__.hostSnapshot().darkMode === false &&
        window.__VIBEX_GATE__.fixtureState().themeDark === false
    );

    const resumeBefore = initial.host.resumeCount;
    const lifecycle = await page.evaluate(() => {
      const runtime = window.__VIBEX_GATE__;
      runtime.emitHostEvent({ kind: "visibility", visible: false });
      runtime.emitHostEvent({ kind: "visibility", visible: true });
      return runtime.hostSnapshot();
    });
    assert(lifecycle.resumeCount === resumeBefore + 1, "lifecycle resume was not applied exactly once");
    assertCleanDiagnostics(diagnostics, "medium diagnostics");
  } finally {
    await medium.close();
  }

  const compact = await browser.newContext({
    viewport: { width: VIEWPORTS.compact.width, height: VIEWPORTS.compact.height },
    deviceScaleFactor: VIEWPORTS.compact.deviceScaleFactor,
    hasTouch: true,
    isMobile: true
  });
  const compactPage = await compact.newPage();
  const compactDiagnostics = pageDiagnostics(compactPage);
  let touch;
  try {
    await compactPage.goto(diagnosticsUrl(origin), {
      waitUntil: "domcontentloaded",
      timeout: 45_000
    });
    await waitForReady(compactPage);
    const before = await compactPage.evaluate(
      () => window.__VIBEX_GATE__.fixtureState().scrollOffsets.page[1]
    );
    const cdp = await compact.newCDPSession(compactPage);
    await compactPage.touchscreen.tap(195, 420);
    await touchSwipe(cdp, 195, 730, 280);
    await compactPage.waitForTimeout(100);
    touch = await compactPage.evaluate(() => ({
      after: window.__VIBEX_GATE__.fixtureState().scrollOffsets.page[1],
      interactions: window.__VIBEX_GATE__.interactions.touch,
      compatibility: window.__VIBEX_GATE__.compatibilitySnapshot().touch
    }));
    assert(touch.interactions > 0, "touch bridge did not observe an event");
    assert(touch.compatibility.scrollGestures > 0, "touch bridge did not classify a scroll");
    assert(touch.after < before, "touch swipe did not move GPUI content");
    touch.before = before;
    assertCleanDiagnostics(compactDiagnostics, "compact diagnostics");
  } finally {
    await compact.close();
  }

  const negative = await browser.newPage({ viewport: { width: 390, height: 844 } });
  try {
    await negative.goto(`${origin}/?forceUnsupported=1`, {
      waitUntil: "domcontentloaded",
      timeout: 45_000
    });
    await negative.waitForFunction(
      () => ["unsupported", "error"].includes(document.body.dataset.gateState)
    );
    const result = await negative.evaluate(() => ({
      state: document.body.dataset.gateState,
      code: document.querySelector("#gate-status-code").textContent,
      canvasCount: document.querySelectorAll("canvas").length
    }));
    assert(result.state === "unsupported", "forced WebGPU failure did not remain diagnostic");
    assert(result.code === "WEBGPU_FORCED_UNSUPPORTED", "forced WebGPU error code drifted");
    assert(result.canvasCount === 0, "unsupported path started GPUI");
    return {
      keyboard: "passed",
      appearance: "passed",
      lifecycle: "passed",
      touch: {
        status: "passed",
        before: touch.before,
        after: touch.after,
        scrollGestures: touch.compatibility.scrollGestures
      },
      negativePath: result
    };
  } finally {
    await negative.close();
  }
}

function runtimeBuild() {
  const path = join(DIST, "build.json");
  assert(existsSync(path), "apps/mobile-wasm/dist is missing");
  const build = JSON.parse(readFileSync(path, "utf8"));
  assert(build.schemaVersion === "vibex-mobile-wasm-build.v1", "runtime build schema drifted");
  assert(build.runtimeRole === "capacitor_mobile_runtime", "runtime build role drifted");
  assert(build.browserHost === "development_and_test_only", "runtime host role drifted");
  for (const retired of ["manifest.webmanifest", "offline.html", "service-worker.js"]) {
    assert(!existsSync(join(DIST, retired)), `mobile runtime still emits ${retired}`);
  }
  return build;
}

function validateEvidence(evidence, currentSourceHash) {
  assert(
    evidence.schemaVersion === "vibex-mobile-wasm-host-evidence.v1",
    "mobile WASM host evidence schema is invalid"
  );
  assert(evidence.purpose === "development_and_automation_only", "host evidence overclaims product scope");
  assert(evidence.releaseClaim === "none", "development host evidence makes a release claim");
  assert(evidence.source.sourceTreeSha256 === currentSourceHash, "mobile runtime source evidence is stale");
  assert(evidence.runtimeBuild.schemaVersion === "vibex-mobile-wasm-build.v1", "runtime build evidence is invalid");
  assert(evidence.runtimeBuild.runtimeRole === "capacitor_mobile_runtime", "runtime build evidence role drifted");
  assert(evidence.targets.chromium.status === "passed", "Chromium development-host gate failed");
  assert(evidence.targets.chromium.viewports.medium.shell === "medium", "Medium evidence drifted");
  assert(evidence.targets.chromium.viewports.compact.shell === "compact", "Compact evidence drifted");
  assert(evidence.targets.chromium.maximumShell === "medium", "host evidence permits Wide");
  assert(evidence.targets.chromium.hostBridge.touch.status === "passed", "touch evidence is missing");
  assert(evidence.targets.chromium.hostBridge.negativePath.state === "unsupported", "negative evidence is missing");
  assert(!JSON.stringify(evidence).includes("screenshot"), "development host evidence contains screenshots");
}

const build = runtimeBuild();
const server = await startWasmServer({ dist: DIST });
try {
  const browser = await chromium.launch({
    headless: true,
    args: ["--enable-unsafe-webgpu", "--use-angle=vulkan", "--enable-features=Vulkan"]
  });
  try {
    const medium = await inspectProductEntry(
      browser,
      server.origin,
      "medium",
      VIEWPORTS.medium,
      "medium"
    );
    const compact = await inspectProductEntry(
      browser,
      server.origin,
      "compact",
      VIEWPORTS.compact,
      "compact"
    );
    const hostBridge = await inspectHostBridge(browser, server.origin);
    const sourceIdentities = resolveGpuiSourceIdentities(ROOT);
    const currentSourceHash = sourceTreeSha256(ROOT, MOBILE_RUNTIME_SOURCE_INPUTS);
    const evidence = {
      schemaVersion: "vibex-mobile-wasm-host-evidence.v1",
      capturedAt: new Date().toISOString(),
      purpose: "development_and_automation_only",
      releaseClaim: "none",
      source: {
        ...sourceIdentities,
        sourceTreeSha256: currentSourceHash,
        cargoLockfileSha256: sha256(repositoryFile("Cargo.lock")),
        pnpmLockfileSha256: sha256(repositoryFile("pnpm-lock.yaml"))
      },
      runtimeBuild: build,
      targets: {
        chromium: {
          status: "passed",
          version: browser.version(),
          launchArguments: [
            "--enable-unsafe-webgpu",
            "--use-angle=vulkan",
            "--enable-features=Vulkan"
          ],
          maximumShell: "medium",
          viewports: {
            medium: {
              width: VIEWPORTS.medium.width,
              height: VIEWPORTS.medium.height,
              deviceScaleFactor: VIEWPORTS.medium.deviceScaleFactor,
              shell: medium.workflow.shell,
              pixels: medium.pixels,
              canvas: medium.canvas
            },
            compact: {
              width: VIEWPORTS.compact.width,
              height: VIEWPORTS.compact.height,
              deviceScaleFactor: VIEWPORTS.compact.deviceScaleFactor,
              shell: compact.workflow.shell,
              pixels: compact.pixels,
              canvas: compact.canvas
            }
          },
          hostBridge
        }
      }
    };

    validateEvidence(evidence, currentSourceHash);
    if (WRITE) {
      const absolute = join(ROOT, EVIDENCE_PATH);
      mkdirSync(dirname(absolute), { recursive: true });
      writeFileSync(absolute, `${JSON.stringify(evidence, null, 2)}\n`);
      console.log(`Wrote mobile WASM development-host evidence to ${EVIDENCE_PATH}`);
    } else {
      assert(existsSync(join(ROOT, EVIDENCE_PATH)), `${EVIDENCE_PATH} is missing; capture with --write`);
      validateEvidence(JSON.parse(repositoryFile(EVIDENCE_PATH)), currentSourceHash);
      console.log(`Mobile WASM development-host gate passed in Chromium ${browser.version()}`);
    }
  } finally {
    await browser.close();
  }
} finally {
  await server.close();
}

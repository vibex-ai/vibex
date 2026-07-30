import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { chromium, firefox } from "@playwright/test";
import { resolveGpuiSourceIdentities } from "./source-identities.mjs";
import {
  WEB_SOURCE_INPUTS,
  sourceTreeSha256
} from "./wasm-source-tree.mjs";
import { startWasmServer } from "./wasm-test-server.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DIST = join(ROOT, "apps/web/dist");
const EVIDENCE_PATH = "docs/platform/evidence/wasm-browser-gate.json";
const SCREENSHOTS = {
  workbench: "docs/parity/screenshots/current/spikes/web-workbench-default.png",
  wide: "docs/parity/screenshots/current/spikes/web-wide.png",
  compact: "docs/parity/screenshots/current/spikes/web-compact.png"
};
const write = process.argv.includes("--write");

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

function percentile(values, fraction) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * fraction))];
}

async function touchSwipe(cdp, x, startY, endY, steps = 6) {
  const point = (y) => [{ x, y, radiusX: 8, radiusY: 8, force: 0.8, id: 1 }];
  await cdp.send("Input.dispatchTouchEvent", {
    type: "touchStart",
    touchPoints: point(startY)
  });
  for (let step = 1; step <= steps; step += 1) {
    const y = startY + ((endY - startY) * step) / steps;
    await cdp.send("Input.dispatchTouchEvent", {
      type: "touchMove",
      touchPoints: point(y)
    });
  }
  await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
}

function imageMetrics(buffer) {
  const result = spawnSync(
    "magick",
    ["png:-", "-format", "%w\t%h\t%k\t%[entropy]\t%[standard-deviation]", "info:"],
    { cwd: ROOT, input: buffer, encoding: "utf8", maxBuffer: 1024 * 1024 }
  );
  if (result.error || result.status !== 0) {
    fail(`ImageMagick failed to inspect browser pixels: ${result.stderr ?? result.error?.message}`);
  }
  const [width, height, uniqueColors, entropy, standardDeviation] = result.stdout
    .trim()
    .split("\t")
    .map(Number);
  const metrics = { width, height, uniqueColors, entropy, standardDeviation };
  assert(Object.values(metrics).every(Number.isFinite), "ImageMagick returned invalid metrics");
  return metrics;
}

function capturePageDiagnostics(page) {
  const consoleMessages = [];
  const pageErrors = [];
  page.on("console", (message) => {
    const text = message.text();
    if (!text.includes("/__gate/") && consoleMessages.length < 80) {
      consoleMessages.push({ type: message.type(), text });
    }
  });
  page.on("pageerror", (error) => pageErrors.push(error.message));
  return { consoleMessages, pageErrors };
}

async function waitForTerminalState(page) {
  await page.waitForFunction(
    () => ["ready", "unsupported", "error"].includes(document.body.dataset.gateState),
    null,
    { timeout: 45_000 }
  );
  return page.evaluate(() => document.body.dataset.gateState);
}

async function waitForNetworkProbes(page) {
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
}

function gateDiagnosticsUrl(origin) {
  const url = new URL(origin);
  url.searchParams.set("diagnostics", "gate");
  return url.href;
}

async function verifyDefaultProductEntry(context, origin, screenshotPath = null) {
  const page = await context.newPage();
  try {
    await page.goto(origin, { waitUntil: "domcontentloaded", timeout: 45_000 });
    assert((await waitForTerminalState(page)) === "ready", "default GPUI product entry did not render");
    await waitForNetworkProbes(page);
    await page.waitForTimeout(250);
    const entry = await page.evaluate(() => {
      const runtime = window.__VIBEX_GATE__;
      const surfaces = ["agent", "files", "git", "terminal", "management"];
      const renderedSurfaces = surfaces.map((surface) => {
        runtime.workflowAction({ kind: "select_surface", surface });
        return runtime.workflowState().activeSurface;
      });
      runtime.workflowAction({ kind: "select_surface", surface: "agent" });
      return {
        root: runtime.rootState(),
        workflow: runtime.workflowState(),
        renderedSurfaces,
        pairing: runtime.remote.pairing,
        pairingLayerHidden: document.querySelector("#pairing-layer").classList.contains("is-hidden")
      };
    });
    const buffer = await page.screenshot();
    const pixels = imageMetrics(buffer);
    assert(entry.root.mode === "workbench", "default GPUI entry is not the workflow workbench");
    assert(entry.root.defaultMode === "workbench", "declared GPUI default entry is not the workbench");
    assert(entry.root.gateFixtureIsProductSource === false, "Gate fixture is still a product data source");
    assert(entry.workflow.connection === "offline", "unpaired default workbench is not explicitly offline");
    assert(entry.pairing.state === "unpaired", "default product entry does not require pairing");
    assert(entry.pairingLayerHidden === false, "default product entry hides the pairing recovery layer");
    assert(
      JSON.stringify(entry.renderedSurfaces) === JSON.stringify(["agent", "files", "git", "terminal", "management"]),
      `default workbench did not render all five workflow surfaces: ${JSON.stringify(entry.renderedSurfaces)}`
    );
    assert(pixels.uniqueColors > 100, "default workflow workbench does not contain credible GPUI pixels");
    const result = {
      ...entry,
      pixels
    };
    if (screenshotPath) {
      result.screenshot = screenshotPath;
      result.sha256 = sha256(buffer);
      result.buffer = buffer;
    }
    return result;
  } finally {
    await page.close();
  }
}

async function chromiumGate(origin) {
  const browser = await chromium.launch({
    headless: true,
    args: ["--enable-unsafe-webgpu", "--use-angle=vulkan", "--enable-features=Vulkan"]
  });
  try {
    const context = await browser.newContext({
      viewport: { width: 1280, height: 800 },
      deviceScaleFactor: 1,
      colorScheme: "light"
    });
    const defaultProductEntry = await verifyDefaultProductEntry(context, origin, SCREENSHOTS.workbench);
    const page = await context.newPage();
    const diagnostics = capturePageDiagnostics(page);
    const navigationStartedAt = performance.now();
    await page.goto(gateDiagnosticsUrl(origin), { waitUntil: "domcontentloaded", timeout: 45_000 });
    const state = await waitForTerminalState(page);
    assert(state === "ready", `Chromium GPUI state is ${state}`);
    await waitForNetworkProbes(page);
    await page.waitForTimeout(250);

    const initial = await page.evaluate(() => {
      const gate = window.__VIBEX_GATE__;
      return {
        runtime: {
          state: gate.state,
          firstFrameMs: gate.readyAt - gate.bootStartedAt,
          adapter: gate.adapter,
          pixelMetrics: gate.pixelMetrics,
          contract: gate.contract,
          compatibility: gate.compatibilitySnapshot(),
          build: gate.build,
          probes: gate.probes
        },
        host: gate.hostSnapshot(),
        fixture: gate.fixtureState(),
        canvas: (() => {
          const canvas = document.querySelector("canvas[data-vibex-gate-canvas]");
          const bounds = canvas.getBoundingClientRect();
          return {
            width: canvas.width,
            height: canvas.height,
            cssWidth: bounds.width,
            cssHeight: bounds.height,
            role: canvas.getAttribute("role")
          };
        })(),
        domAccessibility: {
          roleElements: document.querySelectorAll("[role]").length,
          labelledElements: document.querySelectorAll("[aria-label]").length
        },
        pairing: gate.remote.pairing,
        pairingLayerHidden: document.querySelector("#pairing-layer").classList.contains("is-hidden")
      };
    });
    assert(initial.host.storageStatus === "passed", "Web Storage probe did not pass");
    assert(initial.host.networkStatus === "passed", "Fetch/WebSocket bridge did not pass");
    assert(initial.pairing.state === "idle", "diagnostics Gate entered product pairing recovery");
    assert(initial.pairingLayerHidden === true, "diagnostics Gate pairing layer blocks GPUI interactions");
    assert(initial.runtime.probes.fetch.bytes <= 4096, "Fetch probe exceeded the bounded response budget");
    assert(initial.canvas.width === 1280 && initial.canvas.height === 800, "wide canvas size is invalid");
    assert(initial.fixture.composerValue === "GPUI-WASM gate", "shared Input fixture did not initialize");
    assert(initial.fixture.interfaceFont.tokenFamily === "Inter Variable", "shared font token changed");
    assert(initial.fixture.interfaceFont.resolvedFamily === "Inter", "shared Inter font was not resolved for Web");
    assert(initial.fixture.themeDark === false, "GPUI did not start with the requested light theme");
    assert(initial.runtime.contract.runtime.dispatcher === "single_threaded_web", "runtime is not single-threaded");
    assert(initial.runtime.contract.accessibility.releaseBlocking === true, "a11y blocker is not explicit");
    assert(
      initial.runtime.contract.compatibility.owner === "apps/web",
      "platform compatibility owner is not explicit"
    );
    assert(
      initial.runtime.compatibility.capabilities.inputEventFallback === true &&
        initial.runtime.compatibility.capabilities.touchScrollFallback === true,
      "browser compatibility fallbacks are unavailable"
    );

    const wideBuffer = await page.screenshot();
    const wideMetrics = imageMetrics(wideBuffer);
    assert(wideMetrics.uniqueColors > 100, "wide screenshot does not contain credible GPUI pixels");
    assert(wideMetrics.standardDeviation > 1000, "wide screenshot has insufficient pixel variance");

    const cdp = await context.newCDPSession(page);
    await cdp.send("Accessibility.enable");
    const accessibility = await cdp.send("Accessibility.getFullAXTree");
    const accessibilityRoles = accessibility.nodes
      .map((node) => node.role?.value)
      .filter(Boolean)
      .reduce((counts, role) => ({ ...counts, [role]: (counts[role] ?? 0) + 1 }), {});

    await page.mouse.click(220, 343);
    const gateInput = page.locator("input[data-vibex-gate-input]");
    await gateInput.waitFor({ state: "attached" });
    await gateInput.focus();
    assert(
      await gateInput.evaluate((input) => document.activeElement === input),
      "GPUI browser input proxy did not receive focus"
    );
    await page.keyboard.press("Control+A");
    await page.keyboard.type("Web keyboard 123");
    await page.evaluate(() => {
      const input = document.querySelector("input[data-vibex-gate-input]");
      const clipboard = new DataTransfer();
      clipboard.setData("text/plain", " pasted");
      input.dispatchEvent(new ClipboardEvent("paste", { bubbles: true, clipboardData: clipboard }));
      input.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true, data: "" }));
      input.dispatchEvent(new CompositionEvent("compositionupdate", { bubbles: true, data: "中文" }));
      input.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true, data: "中文" }));
    });
    await page.keyboard.type(" é");
    const inputPresentationMs = await page.evaluate(async () => {
      const input = document.querySelector("input[data-vibex-gate-input]");
      const startedAt = performance.now();
      input.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true, data: "" }));
      input.dispatchEvent(new CompositionEvent("compositionupdate", { bubbles: true, data: "验" }));
      input.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true, data: "验" }));
      input.value = "验";
      input.dispatchEvent(
        new InputEvent("input", {
          bubbles: true,
          inputType: "insertFromComposition",
          data: "验"
        })
      );
      await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      return performance.now() - startedAt;
    });
    await page.waitForTimeout(120);
    const inputBuffer = await page.screenshot();
    const afterInput = await page.evaluate(() => ({
      fixture: window.__VIBEX_GATE__.fixtureState(),
      interactions: window.__VIBEX_GATE__.interactions
    }));
    const composerValue = JSON.stringify(afterInput.fixture.composerValue);
    assert(
      afterInput.fixture.composerValue.includes("Web keyboard 123"),
      `keyboard input did not reach GPUI InputState: ${composerValue}`
    );
    assert(
      afterInput.fixture.composerValue.includes("pasted"),
      `paste did not reach GPUI InputState: ${composerValue}`
    );
    assert(
      afterInput.fixture.composerValue.includes("中文"),
      `composition did not reach GPUI InputState: ${composerValue}`
    );
    assert(afterInput.interactions.compositionEnd >= 1, "composition events were not observed");
    assert(afterInput.interactions.paste >= 1, "paste event was not observed");
    assert(sha256(inputBuffer) !== sha256(wideBuffer), "input did not change presented pixels");

    const residualInput = await page.evaluate(async () => {
      const gate = window.__VIBEX_GATE__;
      const input = document.querySelector("input[data-vibex-gate-input]");
      const before = gate.fixtureState().composerValue;
      input.value = "候选Z";
      input.dispatchEvent(
        new InputEvent("beforeinput", {
          bubbles: true,
          cancelable: true,
          inputType: "insertText",
          data: "候选Z"
        })
      );
      input.dispatchEvent(
        new InputEvent("input", { bubbles: true, inputType: "insertText", data: "候选Z" })
      );
      await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      const inserted = gate.fixtureState().composerValue;
      input.dispatchEvent(
        new InputEvent("beforeinput", {
          bubbles: true,
          cancelable: true,
          inputType: "deleteContentBackward"
        })
      );
      input.dispatchEvent(
        new InputEvent("input", { bubbles: true, inputType: "deleteContentBackward" })
      );
      await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      return {
        before,
        inserted,
        deleted: gate.fixtureState().composerValue,
        fixture: gate.fixtureState(),
        compatibility: gate.compatibilitySnapshot()
      };
    });
    assert(
      residualInput.inserted === `${residualInput.before}候选Z`,
      "residual insertText was not committed exactly once"
    );
    assert(
      residualInput.deleted === `${residualInput.before}候选`,
      "mobile deleteContentBackward fallback did not edit GPUI state"
    );
    assert(
      residualInput.compatibility.input.fallbackCommits >= 1 &&
        residualInput.compatibility.input.fallbackDeletes >= 1,
      "input compatibility diagnostics did not record fallback state changes"
    );

    await page.mouse.click(148, 518);
    await page.waitForTimeout(80);
    const approval = await page.evaluate(() => window.__VIBEX_GATE__.fixtureState().approval);
    assert(approval === "approved", "shared Button click did not update the permission card");

    const beforeDialog = await page.screenshot();
    await page.mouse.click(76, 387);
    await page.waitForTimeout(120);
    const dialogBuffer = await page.screenshot();
    assert(sha256(dialogBuffer) !== sha256(beforeDialog), "shared Dialog did not change presented pixels");
    assert(
      (await page.evaluate(() => window.__VIBEX_GATE__.fixtureState().overlay.dialogActive)) === true,
      "shared Dialog state did not become active"
    );
    await page.keyboard.press("Escape");
    await page.waitForTimeout(120);
    assert(
      (await page.evaluate(() => window.__VIBEX_GATE__.fixtureState().overlay.dialogActive)) === false,
      "Escape did not dismiss the shared Dialog"
    );

    await page.mouse.click(76, 387);
    await page.waitForTimeout(80);
    const dialogBack = await page.evaluate(() => window.__VIBEX_GATE__.platformBack());
    assert(dialogBack === "closed_dialog", "platform Back did not close the active Dialog");

    const beforeSheet = await page.screenshot();
    await page.mouse.click(210, 387);
    await page.waitForTimeout(120);
    const sheetBuffer = await page.screenshot();
    assert(sha256(sheetBuffer) !== sha256(beforeSheet), "shared Sheet did not change presented pixels");
    assert(
      (await page.evaluate(() => window.__VIBEX_GATE__.fixtureState().overlay.sheetActive)) === true,
      "shared Sheet state did not become active"
    );
    const sheetBack = await page.evaluate(() => window.__VIBEX_GATE__.platformBack());
    assert(sheetBack === "closed_sheet", "platform Back did not close the active Sheet");
    await page.waitForTimeout(120);
    const unhandledBack = await page.evaluate(() => window.__VIBEX_GATE__.platformBack());
    assert(unhandledBack === "unhandled", "platform Back swallowed an event without an overlay");

    const timelineBefore = await page.evaluate(
      () => window.__VIBEX_GATE__.fixtureState().scrollOffsets.timeline[1]
    );
    await page.mouse.move(930, 250);
    await page.mouse.wheel(0, 420);
    await page.waitForTimeout(100);
    const scrollState = await page.evaluate(() => ({
      interactions: window.__VIBEX_GATE__.interactions.wheel,
      offset: window.__VIBEX_GATE__.fixtureState().scrollOffsets.timeline[1]
    }));
    assert(scrollState.interactions >= 1, "timeline wheel input was not observed");
    assert(scrollState.offset < timelineBefore, "timeline wheel did not change GPUI scroll state");

    const lightThemeBuffer = await page.screenshot();
    await page.emulateMedia({ colorScheme: "dark" });
    await page.waitForFunction(() => {
      const gate = window.__VIBEX_GATE__;
      return gate.hostSnapshot().darkMode === true && gate.fixtureState().themeDark === true;
    });
    await page.waitForTimeout(120);
    const darkThemeBuffer = await page.screenshot();
    assert(sha256(darkThemeBuffer) !== sha256(lightThemeBuffer), "dark theme did not change GPUI pixels");
    await page.emulateMedia({ colorScheme: "light" });
    await page.waitForFunction(() => {
      const gate = window.__VIBEX_GATE__;
      return gate.hostSnapshot().darkMode === false && gate.fixtureState().themeDark === false;
    });

    await page.evaluate(() => {
      const canvas = document.querySelector("canvas[data-vibex-gate-canvas]");
      window.__VIBEX_GATE__.fullscreenProbe = new Promise((resolve) => {
        canvas.addEventListener(
          "pointerdown",
          async () => {
            try {
              await canvas.requestFullscreen();
              resolve(document.fullscreenElement === canvas);
            } catch {
              resolve(false);
            }
          },
          { capture: true, once: true }
        );
      });
    });
    await page.mouse.click(1200, 750);
    const fullscreenEntered = await page.evaluate(() => window.__VIBEX_GATE__.fullscreenProbe);
    assert(fullscreenEntered === true, "canvas did not enter browser fullscreen");
    await page.waitForFunction(() => window.__VIBEX_GATE__.hostSnapshot().fullscreen === true);
    await page.evaluate(() => document.exitFullscreen());
    await page.waitForFunction(() => window.__VIBEX_GATE__.hostSnapshot().fullscreen === false);

    await page.evaluate(() => window.dispatchEvent(new Event("blur")));
    assert((await page.evaluate(() => window.__VIBEX_GATE__.hostSnapshot().focused)) === false, "blur was not bridged");
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    assert((await page.evaluate(() => window.__VIBEX_GATE__.hostSnapshot().focused)) === true, "focus was not bridged");
    await page.evaluate(() => {
      const gate = window.__VIBEX_GATE__;
      for (let index = 0; index < 5; index += 1) {
        gate.emitHostEvent({ kind: "visibility", visible: false });
        gate.emitHostEvent({ kind: "visibility", visible: true });
      }
    });
    const lifecycle = await page.evaluate(() => window.__VIBEX_GATE__.hostSnapshot());
    assert(lifecycle.resumeCount === 5, "repeated lifecycle resumes were not counted exactly once");

    await page.setViewportSize({ width: 900, height: 700 });
    await page.waitForFunction(() => window.__VIBEX_GATE__.hostSnapshot().viewportWidth === 900);
    const medium = await page.evaluate(() => window.__VIBEX_GATE__.hostSnapshot());
    assert(medium.viewportWidth === 900, "medium resize was not bridged");

    const frameSamples = await page.evaluate(() => window.__VIBEX_GATE__.sampleFrames(120));
    const frameP95Ms = percentile(frameSamples, 0.95);
    assert(initial.runtime.firstFrameMs <= 5000, "first frame exceeded the gate budget");
    assert(frameP95Ms <= 50, `frame p95 exceeded the gate budget: ${frameP95Ms}`);
    assert(inputPresentationMs <= 500, "input presentation exceeded the gate budget");

    const compactContext = await browser.newContext({
      viewport: { width: 360, height: 800 },
      deviceScaleFactor: 2,
      hasTouch: true,
      isMobile: true
    });
    const compactDefaultProductEntry = await verifyDefaultProductEntry(compactContext, origin);
    assert(compactDefaultProductEntry.workflow.shell === "compact", "compact default entry did not use the Compact shell");
    const compactPage = await compactContext.newPage();
    const compactDiagnostics = capturePageDiagnostics(compactPage);
    await compactPage.goto(gateDiagnosticsUrl(origin), { waitUntil: "domcontentloaded", timeout: 45_000 });
    assert((await waitForTerminalState(compactPage)) === "ready", "compact GPUI page did not render");
    await waitForNetworkProbes(compactPage);
    await compactPage.waitForTimeout(250);
    const compactCdp = await compactContext.newCDPSession(compactPage);
    const compactInitial = await compactPage.evaluate(() => window.__VIBEX_GATE__.fixtureState());
    assert(compactInitial.overlay.layout.dialogWidth <= 328, "compact Dialog width is not viewport bounded");
    assert(compactInitial.overlay.layout.sheetPlacement === "bottom", "compact Sheet is not bottom placed");
    const pageScrollBefore = compactInitial.scrollOffsets.page[1];
    await compactPage.touchscreen.tap(180, 420);
    await touchSwipe(compactCdp, 180, 730, 280);
    await compactPage.waitForTimeout(100);
    const compactBuffer = await compactPage.screenshot();
    const compactMetrics = imageMetrics(compactBuffer);
    const compact = await compactPage.evaluate(() => ({
      host: window.__VIBEX_GATE__.hostSnapshot(),
      interactions: window.__VIBEX_GATE__.interactions,
      compatibility: window.__VIBEX_GATE__.compatibilitySnapshot(),
      fixture: window.__VIBEX_GATE__.fixtureState(),
      pixelMetrics: window.__VIBEX_GATE__.pixelMetrics,
      canvas: (() => {
        const canvas = document.querySelector("canvas");
        return { width: canvas.width, height: canvas.height };
      })()
    }));
    assert(compact.host.viewportWidth === 360 && compact.host.viewportHeight === 800, "compact viewport was not bridged");
    assert(compact.host.devicePixelRatio === 2, "compact DPR was not bridged");
    assert(compact.canvas.width >= 360 && compact.canvas.height >= 800, "compact canvas size is invalid");
    assert(compact.interactions.touch >= 1, "touch input was not observed");
    assert(compact.compatibility.touch.tapsReplayed >= 1, "short touch was not replayed as a GPUI tap");
    assert(compact.compatibility.touch.scrollGestures >= 1, "touch swipe was not classified as scrolling");
    assert(compact.fixture.scrollOffsets.page[1] < pageScrollBefore, "touch swipe did not scroll GPUI content");
    assert(compactMetrics.uniqueColors > 100, "compact screenshot does not contain credible GPUI pixels");

    await touchSwipe(compactCdp, 180, 730, 280);
    await touchSwipe(compactCdp, 180, 730, 280);
    await compactPage.waitForTimeout(100);
    assert(
      (await compactPage.evaluate(() => window.__VIBEX_GATE__.fixtureState().overlay.dialogActive)) ===
        false,
      "touch scrolling accidentally opened an overlay"
    );
    await compactPage.touchscreen.tap(180, 110);
    await compactPage.waitForTimeout(100);
    assert(
      (await compactPage.evaluate(() => window.__VIBEX_GATE__.fixtureState().overlay.dialogActive)) ===
        true,
      "replayed touch tap did not activate the GPUI Dialog button"
    );
    const compactTapBack = await compactPage.evaluate(() => window.__VIBEX_GATE__.platformBack());
    assert(compactTapBack === "closed_dialog", "platform Back did not close the touch-opened Dialog");
    const compactTouchFinal = await compactPage.evaluate(() => ({
      compatibility: window.__VIBEX_GATE__.compatibilitySnapshot().touch,
      fixture: window.__VIBEX_GATE__.fixtureState()
    }));
    assert(compactTouchFinal.compatibility.tapsReplayed >= 2, "stateful GPUI tap was not replayed");

    const negativePage = await context.newPage();
    await negativePage.goto(`${origin}/?forceUnsupported=1`, { waitUntil: "domcontentloaded" });
    assert((await waitForTerminalState(negativePage)) === "unsupported", "forced unsupported path did not render");
    const negative = await negativePage.evaluate(() => ({
      code: document.querySelector("#gate-status-code").textContent,
      title: document.querySelector("#gate-status-title").textContent,
      canvasCount: document.querySelectorAll("canvas").length
    }));
    assert(negative.code === "WEBGPU_FORCED_UNSUPPORTED", "unsupported error code is not diagnostic");
    assert(negative.canvasCount === 0, "unsupported path should not start GPUI");
    assert(
      diagnostics.consoleMessages.every((message) => !message.text.includes("Failed to load a font")),
      "Chromium reported a shared font loading failure"
    );

    await compactContext.close();
    await context.close();
    return {
      status: "passed",
      engine: "chromium",
      version: browser.version(),
      defaultProductEntry: { ...defaultProductEntry, buffer: undefined },
      compactDefaultProductEntry,
      launchArguments: ["--enable-unsafe-webgpu", "--use-angle=vulkan", "--enable-features=Vulkan"],
      navigationWallMs: performance.now() - navigationStartedAt,
      wide: { screenshot: SCREENSHOTS.wide, sha256: sha256(wideBuffer), metrics: wideMetrics },
      compact: {
        screenshot: SCREENSHOTS.compact,
        sha256: sha256(compactBuffer),
        metrics: compactMetrics,
        canvas: compact.canvas,
        reportedDevicePixelRatio: compact.host.devicePixelRatio,
        backingStoreMatchesDevicePixels:
          compact.canvas.width === compact.host.viewportWidth * compact.host.devicePixelRatio &&
          compact.canvas.height === compact.host.viewportHeight * compact.host.devicePixelRatio
      },
      runtime: initial.runtime,
      input: {
        inputPresentationMs,
        fixture: residualInput.fixture,
        interactions: afterInput.interactions,
        residualInput,
        pasteAutomation: "dom_clipboard_event_with_data_transfer",
        physicalClipboardClaimed: false
      },
      lifecycle: { resumeCount: lifecycle.resumeCount, focusEventBridge: true },
      resize: { medium },
      performance: { frameP95Ms, frameSampleCount: frameSamples.length },
      components: {
        dialogRendered: true,
        sheetRendered: true,
        platformBack: { dialog: dialogBack, sheet: sheetBack, empty: unhandledBack },
        compactLayout: compactInitial.overlay.layout
      },
      touch: {
        pageScrollBefore,
        pageScrollAfter: compact.fixture.scrollOffsets.page[1],
        compatibility: compact.compatibility.touch,
        statefulTap: {
          openedDialog: true,
          platformBack: compactTapBack,
          finalCompatibility: compactTouchFinal.compatibility
        }
      },
      appearance: { darkModeTransition: true, lightModeRestored: true },
      fullscreen: { entered: true, exited: true },
      accessibility: {
        canvasRole: initial.canvas.role,
        dom: initial.domAccessibility,
        axRoles: accessibilityRoles,
        gpuiSemanticTreeExposed: false
      },
      negativePath: negative,
      diagnostics: {
        consoleMessages: diagnostics.consoleMessages,
        pageErrors: diagnostics.pageErrors,
        compactConsoleMessages: compactDiagnostics.consoleMessages,
        compactPageErrors: compactDiagnostics.pageErrors
      },
      buffers: { workbench: defaultProductEntry.buffer, wide: wideBuffer, compact: compactBuffer }
    };
  } finally {
    await browser.close();
  }
}

async function firefoxGate(origin) {
  const browser = await firefox.launch({
    headless: true,
    firefoxUserPrefs: {
      "dom.webgpu.enabled": true,
      "gfx.webrender.all": true
    }
  });
  try {
    const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
    const diagnostics = capturePageDiagnostics(page);
    await page.goto(gateDiagnosticsUrl(origin), { waitUntil: "domcontentloaded", timeout: 45_000 });
    const state = await waitForTerminalState(page);
    await page.waitForTimeout(250);
    const disposition = await page.evaluate(() => ({
      state: document.body.dataset.gateState,
      code: document.querySelector("#gate-status-code").textContent,
      detail: document.querySelector("#gate-status-detail").textContent,
      gpuExposed: Boolean(navigator.gpu),
      runtime: window.__VIBEX_GATE__
        ? {
            adapter: window.__VIBEX_GATE__.adapter,
            errors: window.__VIBEX_GATE__.errors,
            pixelMetrics: window.__VIBEX_GATE__.pixelMetrics
          }
        : null
    }));
    assert(["ready", "unsupported", "error"].includes(state), "Firefox did not reach a diagnostic state");
    if (state === "ready") {
      const pixels = imageMetrics(await page.screenshot());
      assert(pixels.uniqueColors > 100, "Firefox ready state does not contain credible GPUI pixels");
      disposition.pixels = pixels;
    } else {
      assert(disposition.code !== "GATE_BOOT", "Firefox failure remained on the loading page");
    }
    assert(
      diagnostics.consoleMessages.every((message) => !message.text.includes("Failed to load a font")),
      "Firefox reported a shared font loading failure"
    );
    return {
      status: state === "ready" ? "passed" : "unsupported",
      engine: "firefox",
      version: browser.version(),
      disposition,
      diagnostics
    };
  } finally {
    await browser.close();
  }
}

function validateEvidence(evidence, currentSourceHash) {
  assert(evidence.schemaVersion === "vibex-wasm-browser-evidence.v1", "browser evidence schema is invalid");
  assert(evidence.source.sourceTreeSha256 === currentSourceHash, "browser evidence source identity is stale");
  assert(evidence.targets.chromium.status === "passed", "committed Chromium evidence did not pass");
  assert(evidence.targets.chromium.defaultProductEntry.root.mode === "workbench", "committed default entry is not the workflow workbench");
  assert(evidence.targets.chromium.defaultProductEntry.root.gateFixtureIsProductSource === false, "committed default entry still uses Gate fixture data");
  assert(evidence.targets.chromium.defaultProductEntry.renderedSurfaces.length === 5, "committed default entry did not render five workflows");
  assert(evidence.targets.chromium.compactDefaultProductEntry.root.mode === "workbench", "committed Compact entry is not the workflow workbench");
  assert(evidence.targets.chromium.compactDefaultProductEntry.workflow.shell === "compact", "committed Compact entry did not use the Compact shell");
  assert(evidence.targets.chromium.compactDefaultProductEntry.renderedSurfaces.length === 5, "committed Compact entry did not render five workflows");
  assert(evidence.targets.chromium.runtime.probes.fetch.status === "passed", "committed Fetch evidence did not pass");
  assert(evidence.targets.chromium.runtime.probes.webSocket.status === "passed", "committed WebSocket evidence did not pass");
  assert(evidence.decision.productionRelease === "no_go", "production release blocker must remain explicit");
  assert(evidence.decision.a11yReleaseBlocker === true, "a11y release blocker must remain explicit");
  for (const screenshot of Object.values(evidence.screenshots)) {
    assert(existsSync(join(ROOT, screenshot.path)), `browser screenshot is missing: ${screenshot.path}`);
    assert(sha256(repositoryFile(screenshot.path)) === screenshot.sha256, `browser screenshot hash is stale: ${screenshot.path}`);
  }
}

assert(existsSync(join(DIST, "build.json")), "apps/web/dist is missing; run the Web GPUI build first");
const server = await startWasmServer({ dist: DIST });
try {
  const chromiumResult = await chromiumGate(server.origin);
  const firefoxResult = await firefoxGate(server.origin);
  const sourceIdentities = resolveGpuiSourceIdentities(ROOT);
  const currentSourceHash = sourceTreeSha256(ROOT, WEB_SOURCE_INPUTS);
  const evidence = {
    schemaVersion: "vibex-wasm-browser-evidence.v1",
    capturedAt: new Date().toISOString(),
    source: {
      ...sourceIdentities,
      sourceTreeSha256: currentSourceHash,
      lockfileSha256: sha256(repositoryFile("Cargo.lock")),
      pnpmLockfileSha256: sha256(repositoryFile("pnpm-lock.yaml"))
    },
    requiredViewports: ["1280x800@1", "360x800@2"],
    targets: {
      chromium: { ...chromiumResult, buffers: undefined },
      firefox: firefoxResult
    },
    screenshots: {
      workbench: { path: SCREENSHOTS.workbench, sha256: chromiumResult.defaultProductEntry.sha256 },
      wide: { path: SCREENSHOTS.wide, sha256: chromiumResult.wide.sha256 },
      compact: { path: SCREENSHOTS.compact, sha256: chromiumResult.compact.sha256 }
    },
    decision: {
      technicalSpike: "go",
      productionRelease: "no_go",
      primaryChromiumWebUiValidated: true,
      additionalBrowserRendered: firefoxResult.status === "passed",
      a11yReleaseBlocker: true,
      physicalAndroidPending: true,
      physicalIosPending: true,
      reason: "Chromium renders the real five-surface workflow workbench by default and the Gate fixture only in explicit diagnostics mode, but the locked web platform has no accessibility adapter and physical mobile evidence is pending."
    }
  };

  if (write) {
    for (const [key, path] of Object.entries(SCREENSHOTS)) {
      const absolute = join(ROOT, path);
      mkdirSync(dirname(absolute), { recursive: true });
      writeFileSync(absolute, chromiumResult.buffers[key]);
    }
    const absoluteEvidence = join(ROOT, EVIDENCE_PATH);
    mkdirSync(dirname(absoluteEvidence), { recursive: true });
    writeFileSync(absoluteEvidence, `${JSON.stringify(evidence, null, 2)}\n`);
    validateEvidence(evidence, currentSourceHash);
    console.log(`Wrote GPUI-WASM browser evidence to ${EVIDENCE_PATH}`);
  } else {
    assert(existsSync(join(ROOT, EVIDENCE_PATH)), `${EVIDENCE_PATH} is missing; capture with --write`);
    validateEvidence(JSON.parse(repositoryFile(EVIDENCE_PATH)), currentSourceHash);
    console.log(
      `GPUI-WASM browser gate passed live: Chromium ${chromiumResult.version}; Firefox ${firefoxResult.status}`
    );
  }
} finally {
  await server.close();
}

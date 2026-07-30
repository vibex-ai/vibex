import { createHash } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { Buffer } from "node:buffer";
import console from "node:console";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync
} from "node:fs";
import { dirname, join, resolve, sep } from "node:path";
import { tmpdir } from "node:os";
import process from "node:process";
import { clearTimeout, setTimeout } from "node:timers";
import { fileURLToPath } from "node:url";
import { classifyGpuiEvidence } from "./evidence-applicability.mjs";
import { resolveGpuiSourceIdentities } from "./source-identities.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SOURCE_IDENTITIES = resolveGpuiSourceIdentities(ROOT);
const EVIDENCE_PATH = "docs/platform/evidence/composer-linux.json";
const PREEDIT_SCREENSHOT_PATH = "docs/parity/screenshots/current/spikes/linux-wayland-composer-preedit.png";
const FINAL_SCREENSHOT_PATH = "docs/parity/screenshots/current/spikes/linux-wayland-composer-final.png";
const BINARY_PATH = "target/debug/vibex-desktop";
const WINDOW_IDENTITY = "dev.vibex.desktop.preview";
const VIBEX_BASELINE_COMMIT = "f1c624f115401d6160a771545fa1ec73128394b1";
const WTYPE_COMMIT = "d71be3a7b3f93b534a2823fd68cabd7ac2a02359";
const WTYPE_DEFAULT_PATH = "/tmp/vibex-wtype-v0.4/build/wtype";
const EXPECTED_FINAL_TEXT = "\u4f60\u597d\nsecond /fi fixture-paste";
const SOURCE_INPUT_ROOTS = [
  "Cargo.lock",
  "Cargo.toml",
  "rust-toolchain.toml",
  "apps/desktop/Cargo.toml",
  "apps/desktop/src",
  "scripts/capture-composer-linux.mjs"
];
const CLAIMS = [
  ["native_cjk_preedit_commit", "passed"],
  ["multiline_shift_enter", "passed"],
  ["enter_submit_focus", "passed"],
  ["native_clipboard_paste_copy", "passed"],
  ["selection_undo_redo", "passed"],
  ["suggestion_menu", "passed"],
  ["multiline_accessibility_role", "passed"],
  ["inline_image_token", "passed"],
  ["drop_adapter_fixture", "passed"],
  ["native_wayland_file_drop", "blocked"]
];

function fail(message) {
  throw new Error(message);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function sleep(milliseconds) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}

function rootPath(path) {
  const absolute = resolve(ROOT, path);
  if (absolute !== ROOT && !absolute.startsWith(`${ROOT}${sep}`)) {
    fail(`path escapes repository: ${path}`);
  }
  return absolute;
}

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function readJson(path) {
  return JSON.parse(readFileSync(rootPath(path), "utf8"));
}

function runResult(command, args, options = {}) {
  return spawnSync(command, args, {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
    timeout: 120_000,
    ...options
  });
}

function run(command, args, options = {}) {
  const result = runResult(command, args, options);
  if (result.error) fail(`${command} failed to start: ${result.error.message}`);
  if (result.status !== 0) {
    fail(`${command} ${args.join(" ")} failed with status ${result.status ?? "unknown"}`);
  }
  return result.stdout ?? "";
}

function sourceFilesFor(path) {
  const absolute = rootPath(path);
  if (!existsSync(absolute)) fail(`source input is missing: ${path}`);
  if (statSync(absolute).isFile()) return [path];
  return readdirSync(absolute, { withFileTypes: true })
    .sort((left, right) => left.name.localeCompare(right.name))
    .flatMap((entry) => sourceFilesFor(`${path}/${entry.name}`));
}

function sourceInputTreeSha256() {
  const hash = createHash("sha256");
  for (const path of SOURCE_INPUT_ROOTS.flatMap(sourceFilesFor)) {
    hash.update(path);
    hash.update("\0");
    hash.update(readFileSync(rootPath(path)));
    hash.update("\0");
  }
  return hash.digest("hex");
}

function requireCommand(command) {
  run("sh", ["-c", `command -v ${command} >/dev/null`]);
}

function wtypeIdentity() {
  const binaryPath = process.env.VIBEX_WTYPE_BIN ?? WTYPE_DEFAULT_PATH;
  assert(existsSync(binaryPath) && statSync(binaryPath).isFile(), "pinned wtype binary is missing");
  const sourceRoot = resolve(dirname(binaryPath), "..");
  const commit = run("git", ["-C", sourceRoot, "rev-parse", "HEAD"]).trim();
  assert(commit === WTYPE_COMMIT, "wtype source revision is not pinned to v0.4");
  const license = readFileSync(join(sourceRoot, "LICENSE"), "utf8");
  assert(license.startsWith("MIT License"), "wtype license is not MIT");
  return {
    binaryPath,
    evidence: {
      name: "wtype",
      version: "0.4",
      sourceRevision: commit,
      licenseExpression: "MIT",
      repository: "https://github.com/atx/wtype",
      binarySha256: sha256(readFileSync(binaryPath)),
      packagedWithApplication: false
    }
  };
}

function hyprlandJson(kind) {
  return JSON.parse(run("hyprctl", ["-j", kind]));
}

async function waitForClient(app) {
  for (let attempt = 0; attempt < 300; attempt += 1) {
    const client = hyprlandJson("clients").find((candidate) => candidate.pid === app.pid);
    if (client) return client;
    if (app.exitCode !== null) fail("GPUI Composer exited before creating a window");
    await sleep(50);
  }
  fail("GPUI Composer window was not discovered by Hyprland");
}

async function waitForReport(path, app) {
  for (let attempt = 0; attempt < 600; attempt += 1) {
    if (existsSync(path)) return JSON.parse(readFileSync(path, "utf8"));
    if (app.exitCode !== null) fail("GPUI Composer exited without writing a report");
    await sleep(100);
  }
  fail("GPUI Composer report exceeded its bounded timeout");
}

async function waitForProgress(path, predicate, app, attempts = 60) {
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    if (existsSync(path)) {
      try {
        const progress = JSON.parse(readFileSync(path, "utf8"));
        if (progress.schemaVersion === "composer-progress.v1" && predicate(progress)) {
          return true;
        }
      } catch {
        // The foreground process may be between truncate and write; retry.
      }
    }
    if (app.exitCode !== null) fail("GPUI Composer exited before reaching expected progress");
    await sleep(100);
  }
  return false;
}

function waitForExit(app, timeoutMs) {
  if (app.exitCode !== null) return Promise.resolve({ code: app.exitCode, signal: app.signalCode });
  return new Promise((resolvePromise, rejectPromise) => {
    const timeout = setTimeout(() => rejectPromise(new Error("GPUI Composer did not exit")), timeoutMs);
    app.once("exit", (code, signal) => {
      clearTimeout(timeout);
      resolvePromise({ code, signal });
    });
    app.once("error", (error) => {
      clearTimeout(timeout);
      rejectPromise(error);
    });
  });
}

function parseImageMetrics(output) {
  const [width, height, uniqueColors, entropy, mean, standardDeviation] = output.trim().split("\t");
  const metrics = {
    width: Number(width),
    height: Number(height),
    uniqueColors: Number(uniqueColors),
    entropy: Number(entropy),
    mean: Number(mean),
    standardDeviation: Number(standardDeviation)
  };
  assert(Object.values(metrics).every(Number.isFinite), "invalid screenshot metrics");
  return metrics;
}

function captureWindow(client, path) {
  const screenshot = rootPath(path);
  mkdirSync(dirname(screenshot), { recursive: true });
  run("grim", [
    "-g",
    `${client.at[0]},${client.at[1]} ${client.size[0]}x${client.size[1]}`,
    screenshot
  ]);
  const metrics = parseImageMetrics(
    run("identify", [
      "-format",
      "%w\t%h\t%k\t%[entropy]\t%[fx:mean]\t%[fx:standard_deviation]",
      screenshot
    ])
  );
  assert(
    metrics.width >= 1198 &&
      metrics.width <= 1202 &&
      metrics.height === 780 &&
      metrics.uniqueColors > 1 &&
      metrics.entropy > 0 &&
      metrics.standardDeviation > 0,
    `${path} does not contain credible fixed-size pixels`
  );
  return {
    screenshotPath: path,
    screenshotSha256: sha256(readFileSync(screenshot)),
    ...metrics
  };
}

function exactKeys(value, expected, label) {
  const actual = Object.keys(value ?? {}).sort();
  const wanted = [...expected].sort();
  assert(JSON.stringify(actual) === JSON.stringify(wanted), `${label} keys are not exact`);
}

function validateRun(runReport) {
  assert(runReport?.schemaVersion === "composer-run.v1", "Composer run schema is invalid");
  assert(runReport.status === "passed" && runReport.failure === null, "Composer interaction run failed");
  const input = runReport.input;
  for (const [field, expected] of [
    ["nativeInputHandler", true],
    ["multiline", true],
    ["accessibilityRole", "multiline_text_input"],
    ["compositionObserved", true],
    ["cjkCommitObserved", true],
    ["shiftEnterObserved", true],
    ["enterSubmitObserved", true],
    ["pasteObserved", true],
    ["selectionObserved", true],
    ["undoObserved", true],
    ["redoObserved", true],
    ["rawTextStored", false]
  ]) {
    assert(input?.[field] === expected, `Composer input field ${field} is invalid`);
  }
  assert(input.markedFrameCount > 0, "GPUI did not observe an IME marked-text frame");
  assert(input.finalTextBytes === Buffer.byteLength(EXPECTED_FINAL_TEXT), "final Composer byte count is unexpected");
  assert(runReport.attachments?.inlineImageTokenRendered === true, "inline image token was not rendered");
  assert(runReport.attachments.dropAdapterFixtureAccepted === true, "drop adapter fixture failed");
  assert(runReport.attachments.nativeFileDropEventObserved === false, "native file drop was fabricated");
  assert(runReport.suggestions?.triggerObserved === true && runReport.suggestions.menuRendered === true, "suggestions were not rendered");
  assert(runReport.focus?.focusedAfterSubmit === true, "Composer focus was not retained after submit");
  exactKeys(
    input,
    [
      "nativeInputHandler",
      "multiline",
      "accessibilityRole",
      "compositionObserved",
      "markedFrameCount",
      "cjkCommitObserved",
      "shiftEnterObserved",
      "enterSubmitObserved",
      "pasteObserved",
      "selectionObserved",
      "undoObserved",
      "redoObserved",
      "finalTextBytes",
      "rawTextStored"
    ],
    "Composer input"
  );
}

function validateEvidenceObject(evidence) {
  assert(evidence?.schemaVersion === "composer-linux-evidence.v1", "evidence schema is invalid");
  assert(evidence.status === "partial", "Composer aggregate status must retain the file-drop blocker");
  assert(evidence.source?.vibexBaselineCommit === VIBEX_BASELINE_COMMIT, "baseline commit drifted");
  const applicability = classifyGpuiEvidence(ROOT, EVIDENCE_PATH, evidence.source);
  if (applicability === "current") {
    assert(evidence.source.zedRevision === SOURCE_IDENTITIES.zedRevision, "Zed revision drifted");
    assert(
      evidence.source.gpuiComponentRevision === SOURCE_IDENTITIES.gpuiComponentRevision,
      "component revision drifted"
    );
    assert(
      JSON.stringify(evidence.source.sourceInputRoots) === JSON.stringify(SOURCE_INPUT_ROOTS),
      "source input roots drifted"
    );
    assert(
      evidence.source.sourceInputTreeSha256 === sourceInputTreeSha256(),
      "Composer source identity is stale"
    );
    assert(
      evidence.source.lockfileSha256 === sha256(readFileSync(rootPath("Cargo.lock"))),
      "lockfile identity is stale"
    );
  }
  assert(evidence.runner?.platform === "linux", "Composer evidence is not from Linux");
  assert(evidence.runner.displayBackend === "wayland-hyprland" && evidence.runner.syntheticDisplay === false, "Composer evidence is not physical Wayland");
  assert(evidence.runner.inputMethod?.framework === "fcitx5", "Fcitx5 was not recorded");
  assert(evidence.runner.inputMethod.engine === "rime", "Rime was not recorded");
  assert(evidence.runner.inputInjection?.sourceRevision === WTYPE_COMMIT, "wtype revision drifted");
  assert(evidence.runner.inputInjection.licenseExpression === "MIT", "wtype license is invalid");
  assert(evidence.runner.inputInjection.packagedWithApplication === false, "capture tool entered the package");
  assert(evidence.window?.identity === WINDOW_IDENTITY, "window identity is invalid");
  assert(evidence.window.discovered === true && evidence.window.xwayland === false, "native Wayland window was not proven");
  assert(
    evidence.window.width >= 1198 && evidence.window.width <= 1202 && evidence.window.height === 780,
    "window geometry is unstable"
  );
  for (const capture of [evidence.captures?.preedit, evidence.captures?.final]) {
    const screenshot = rootPath(capture?.screenshotPath ?? "");
    assert(existsSync(screenshot), "Composer screenshot is missing");
    assert(capture.screenshotSha256 === sha256(readFileSync(screenshot)), "Composer screenshot hash is stale");
    assert(
      capture.width === evidence.window.width &&
        capture.height === evidence.window.height &&
        capture.uniqueColors > 1 &&
        capture.entropy > 0,
      "Composer screenshot metrics are invalid"
    );
  }
  assert(evidence.clipboard?.pasteRoundTrip === true, "native paste round trip failed");
  assert(evidence.clipboard.copyRoundTrip === true, "native copy round trip failed");
  assert(evidence.clipboard.rawTextStored === false, "clipboard text was retained");
  assert(evidence.process?.appExitCode === 0 && evidence.process.processExited === true, "Composer process did not exit cleanly");
  assert(evidence.process.panicMentioned === false, "Composer process panicked");
  validateRun(evidence.run);

  assert(
    JSON.stringify(evidence.claims.map((claim) => [claim.id, claim.status])) === JSON.stringify(CLAIMS),
    "Composer claim matrix drifted"
  );
  for (const claim of evidence.claims.slice(0, -1)) {
    assert(claim.decisionBearing === true && claim.blocker === null, `passed claim ${claim.id} is invalid`);
  }
  const fileDrop = evidence.claims.at(-1);
  assert(
    fileDrop.id === "native_wayland_file_drop" &&
      fileDrop.status === "blocked" &&
      fileDrop.decisionBearing === true &&
      fileDrop.blocker?.code === "native-file-drop-not-exercised",
    "native file-drop blocker is invalid"
  );
  assert(
    evidence.summary?.passedClaims === CLAIMS.length - 1 &&
      evidence.summary.blockedClaims === 1 &&
      evidence.summary.composerGateSatisfied === false,
    "Composer claim summary is inconsistent"
  );

  const serialized = JSON.stringify(evidence);
  for (const forbidden of [
    EXPECTED_FINAL_TEXT,
    "fixture-paste",
    ROOT,
    process.env.HOME,
    '"rawText"',
    '"clipboardText"',
    '"selectedText"'
  ]) {
    if (forbidden) assert(!serialized.includes(forbidden), "Composer evidence retained private or raw text");
  }
  return applicability;
}

function verifyEvidence() {
  assert(existsSync(rootPath(EVIDENCE_PATH)), `${EVIDENCE_PATH} is missing`);
  const evidence = readJson(EVIDENCE_PATH);
  const applicability = validateEvidenceObject(evidence);
  console.log(
    `GPUI Composer verified: ${evidence.summary.passedClaims} passed claims, ` +
      `${evidence.summary.blockedClaims} native file-drop blocker; applicability=${applicability}`
  );
}

function selfTest() {
  const evidence = readJson(EVIDENCE_PATH);
  validateEvidenceObject(evidence);
  for (const [label, mutate] of [
    ["missing composition", (copy) => (copy.run.input.compositionObserved = false)],
    ["fabricated file drop", (copy) => (copy.run.attachments.nativeFileDropEventObserved = true)],
    ["retained clipboard text", (copy) => (copy.clipboard.clipboardText = "fixture-paste")],
    ["removed blocker", (copy) => (copy.claims.at(-1).status = "passed")]
  ]) {
    const copy = JSON.parse(JSON.stringify(evidence));
    mutate(copy);
    let rejected = false;
    try {
      validateEvidenceObject(copy);
    } catch {
      rejected = true;
    }
    assert(rejected, `negative self-test was accepted: ${label}`);
  }
  console.log("GPUI Composer negative-case self-test passed");
}

function readClipboardText() {
  const result = runResult("wl-paste", ["--no-newline", "--type", "text/plain;charset=utf-8"]);
  if (result.error || result.status !== 0) return null;
  return result.stdout ?? "";
}

function writeClipboardText(value) {
  const clipboard = spawn(
    "wl-copy",
    ["--foreground", "--paste-once", "--type", "UTF8_STRING"],
    { stdio: ["pipe", "ignore", "ignore"] }
  );
  clipboard.stdin.end(value);
  return clipboard;
}

function restoreClipboardText(value) {
  const clipboard = spawn(
    "wl-copy",
    ["--foreground", "--type", "UTF8_STRING"],
    { detached: true, stdio: ["pipe", "ignore", "ignore"] }
  );
  clipboard.stdin.end(value);
  clipboard.unref();
}

async function capture() {
  assert(process.platform === "linux", "physical Composer capture requires Linux");
  assert(process.env.XDG_SESSION_TYPE === "wayland", "physical Composer capture requires Wayland");
  for (const command of [
    "cargo",
    "fcitx5",
    "fcitx5-remote",
    "git",
    "grim",
    "hyprctl",
    "identify",
    "rustc",
    "wl-copy",
    "wl-paste"
  ]) {
    requireCommand(command);
  }
  const wtype = wtypeIdentity();
  const previousClipboard = readClipboardText();
  run("cargo", ["build", "-p", "vibex-desktop", "--locked"], { stdio: "inherit" });
  const temporaryRoot = mkdtempSync(join(tmpdir(), "vibex-composer-"));
  const reportPath = join(temporaryRoot, "composer.json");
  const progressPath = join(temporaryRoot, "composer.progress.json");
  let stderr = "";
  let app;
  let fixtureClipboard;
  try {
    app = spawn(rootPath(BINARY_PATH), ["--spike-composer", reportPath], {
      cwd: ROOT,
      env: {
        ...process.env,
        XDG_SESSION_TYPE: "wayland",
        VIBEX_SPIKE_HOLD_MS: "3000"
      },
      stdio: ["ignore", "ignore", "pipe"]
    });
    app.stderr.on("data", (chunk) => {
      if (stderr.length < 64 * 1024) stderr += chunk.toString("utf8");
    });
    const client = await waitForClient(app);
    const monitor = hyprlandJson("monitors").find((candidate) => candidate.id === client.monitor);
    assert(monitor, "Composer window monitor was not found");
    const addressSelector = `address:${client.address}`;
    if (!client.floating) run("hyprctl", ["dispatch", "togglefloating", addressSelector]);
    run("hyprctl", ["dispatch", "resizewindowpixel", `exact 1200 780,${addressSelector}`]);
    run("hyprctl", [
      "dispatch",
      "movewindowpixel",
      `exact ${monitor.x + 100} ${monitor.y + 100},${addressSelector}`
    ]);
    run("hyprctl", ["dispatch", "focuswindow", addressSelector]);
    assert(
      await waitForProgress(progressPath, (progress) => progress.ready === true, app),
      "GPUI Composer input did not become ready"
    );
    await sleep(250);

    run("fcitx5-remote", ["-o"]);
    run(wtype.binaryPath, ["-d", "150", "nihao"]);
    let compositionObserved = await waitForProgress(
      progressPath,
      (progress) => progress.compositionObserved === true,
      app,
      30
    );
    if (!compositionObserved) {
      run(wtype.binaryPath, ["-k", "Escape"]);
      run(wtype.binaryPath, ["-M", "ctrl", "-k", "a", "-m", "ctrl"]);
      run(wtype.binaryPath, ["-k", "BackSpace"]);
      run(wtype.binaryPath, ["-M", "ctrl", "-k", "space", "-m", "ctrl"]);
      run(wtype.binaryPath, ["-d", "150", "nihao"]);
      compositionObserved = await waitForProgress(
        progressPath,
        (progress) => progress.compositionObserved === true,
        app,
        30
      );
    }
    assert(compositionObserved, "Fcitx5/Rime did not produce GPUI marked text");
    await sleep(350);
    let finalClient = hyprlandJson("clients").find((candidate) => candidate.address === client.address);
    assert(finalClient, "Composer window disappeared during preedit");
    const preeditCapture = captureWindow(finalClient, PREEDIT_SCREENSHOT_PATH);

    run(wtype.binaryPath, ["-k", "space"]);
    assert(
      await waitForProgress(
        progressPath,
        (progress) => progress.compositionActive === false,
        app,
        20
      ),
      "Rime did not commit and clear the marked-text range"
    );
    run("fcitx5-remote", ["-c"]);
    await sleep(250);
    run(wtype.binaryPath, ["-M", "shift", "-k", "Return", "-m", "shift"]);
    // InputState intentionally groups edits made within one second. Start the
    // paste as a distinct history entry so undo/redo validates the paste
    // operation instead of replaying the preceding IME commit as one batch.
    await sleep(1_100);
    run(wtype.binaryPath, ["-M", "ctrl", "-k", "v", "-m", "ctrl"]);
    assert(
      await waitForProgress(
        progressPath,
        (progress) => progress.valueBytes === Buffer.byteLength(EXPECTED_FINAL_TEXT),
        app,
        50
      ),
      "GPUI did not insert the native clipboard fixture"
    );
    await sleep(1_100);
    run(wtype.binaryPath, ["-M", "ctrl", "-k", "z", "-m", "ctrl"]);
    await sleep(550);
    run(wtype.binaryPath, ["-M", "ctrl", "-k", "y", "-m", "ctrl"]);
    await sleep(550);
    // GPUI exposes both conventional Linux redo bindings. Exercise the
    // cross-platform Ctrl+Shift+Z path as well; it is a no-op when Ctrl+Y
    // already restored the latest history entry.
    run(wtype.binaryPath, ["-M", "ctrl", "-M", "shift", "-k", "z", "-m", "shift", "-m", "ctrl"]);
    await sleep(550);
    run(wtype.binaryPath, ["-M", "ctrl", "-k", "a", "-m", "ctrl"]);
    let fullSelectionObserved = await waitForProgress(
      progressPath,
      (progress) => progress.fullSelectionActive === true,
      app,
      20
    );
    if (!fullSelectionObserved) {
      run("hyprctl", ["dispatch", "focuswindow", addressSelector]);
      run(wtype.binaryPath, ["-M", "ctrl", "-k", "a", "-m", "ctrl"]);
      fullSelectionObserved = await waitForProgress(
        progressPath,
        (progress) => progress.fullSelectionActive === true,
        app,
        20
      );
    }
    assert(fullSelectionObserved, "GPUI did not expose a full-text selection after Ctrl+A");
    run(wtype.binaryPath, ["-M", "ctrl", "-k", "c", "-m", "ctrl"]);
    await sleep(750);
    const copiedText = readClipboardText();
    assert(
      copiedText === EXPECTED_FINAL_TEXT,
      "native clipboard copy did not match the sanitized fixture: " +
        `actualBytes=${copiedText === null ? "unavailable" : Buffer.byteLength(copiedText)}, ` +
        `actualSha256=${copiedText === null ? "unavailable" : sha256(copiedText)}, ` +
        `expectedBytes=${Buffer.byteLength(EXPECTED_FINAL_TEXT)}, expectedSha256=${sha256(EXPECTED_FINAL_TEXT)}`
    );
    run(wtype.binaryPath, ["-k", "Return"]);

    const runReport = await waitForReport(reportPath, app);
    validateRun(runReport);
    finalClient = hyprlandJson("clients").find((candidate) => candidate.address === client.address);
    assert(finalClient, "Composer window disappeared before final capture");
    const finalCapture = captureWindow(finalClient, FINAL_SCREENSHOT_PATH);
    const exit = await waitForExit(app, 10_000);
    assert(exit.code === 0 && exit.signal === null, "Composer process did not exit cleanly");

    const evidence = {
      schemaVersion: "composer-linux-evidence.v1",
      status: "partial",
      capturedAt: new Date().toISOString(),
      source: {
        vibexBaselineCommit: VIBEX_BASELINE_COMMIT,
        captureParentCommit: run("git", ["rev-parse", "HEAD"]).trim(),
        dependencySourcePolicy: SOURCE_IDENTITIES.dependencySourcePolicy,
        zedRevision: SOURCE_IDENTITIES.zedRevision,
        gpuiComponentRevision: SOURCE_IDENTITIES.gpuiComponentRevision,
        rustToolchain: run("rustc", ["--version"]).trim(),
        lockfileSha256: sha256(readFileSync(rootPath("Cargo.lock"))),
        sourceInputRoots: SOURCE_INPUT_ROOTS,
        sourceInputTreeSha256: sourceInputTreeSha256()
      },
      runner: {
        platform: process.platform,
        architecture: process.arch,
        kernelRelease: run("uname", ["-r"]).trim(),
        displayBackend: "wayland-hyprland",
        compositor: "Hyprland",
        syntheticDisplay: false,
        inputMethod: {
          framework: "fcitx5",
          frameworkVersion: run("fcitx5", ["--version"]).trim().slice(0, 128),
          engine: "rime"
        },
        inputInjection: wtype.evidence
      },
      window: {
        identity: WINDOW_IDENTITY,
        discovered: true,
        xwayland: false,
        width: finalClient.size[0],
        height: finalClient.size[1]
      },
      captures: {
        preedit: preeditCapture,
        final: finalCapture
      },
      clipboard: {
        pasteRoundTrip: true,
        copyRoundTrip: copiedText === EXPECTED_FINAL_TEXT,
        rawTextStored: false,
        priorTextRestored: previousClipboard !== null,
        source: "gpui_platform_clipboard_fixture"
      },
      process: {
        processExited: true,
        appExitCode: exit.code,
        panicMentioned: /panicked at|thread '.+' panicked/i.test(stderr)
      },
      run: runReport,
      claims: CLAIMS.map(([id, status]) => ({
        id,
        status,
        decisionBearing: true,
        blocker:
          id === "native_wayland_file_drop"
            ? {
                code: "native-file-drop-not-exercised",
                summary: "The keyboard/clipboard injector cannot originate a Wayland file drag from another application surface.",
                action: "Repeat the installed Composer flow with a real file-manager drag and verify nativeFileDropEventObserved before closing this claim."
              }
            : null
      })),
      summary: {
        passedClaims: CLAIMS.length - 1,
        blockedClaims: 1,
        composerGateSatisfied: false
      },
      limitations: [
        "The committed screenshots use a sanitized input and attachment fixture.",
        "The paste fixture is written through GPUI's native platform clipboard; external Wayland clipboard-owner interoperability is not claimed.",
        "wtype is an MIT capture-only prerequisite and is not linked or packaged with Vibex.",
        "Native Wayland file drag-and-drop remains decision-bearing and blocked."
      ]
    };
    if (evidence.process.panicMentioned) {
      console.error(stderr.slice(-8_000));
    }
    validateEvidenceObject(evidence);
    mkdirSync(dirname(rootPath(EVIDENCE_PATH)), { recursive: true });
    writeFileSync(rootPath(EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
    verifyEvidence();
  } finally {
    if (app && app.exitCode === null) {
      app.kill("SIGTERM");
      await sleep(250);
      if (app.exitCode === null) app.kill("SIGKILL");
    }
    if (fixtureClipboard && fixtureClipboard.exitCode === null) fixtureClipboard.kill("SIGTERM");
    if (previousClipboard !== null) {
      restoreClipboardText(previousClipboard);
    } else {
      runResult("wl-copy", ["--clear"]);
    }
    rmSync(temporaryRoot, { recursive: true, force: true });
  }
}

const mode = process.argv[2];
if (mode === "--write") {
  await capture();
} else if (mode === "--self-test") {
  selfTest();
} else if (mode === undefined) {
  verifyEvidence();
} else {
  fail("usage: node scripts/capture-composer-linux.mjs [--write|--self-test]");
}

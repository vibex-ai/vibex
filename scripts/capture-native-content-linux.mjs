import { createHash } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
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
import { tmpdir } from "node:os";
import { dirname, join, resolve, sep } from "node:path";
import process from "node:process";
import { clearTimeout, setTimeout } from "node:timers";
import { fileURLToPath } from "node:url";
import { classifyGpuiEvidence } from "./evidence-applicability.mjs";
import { resolveGpuiSourceIdentities } from "./source-identities.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SOURCE_IDENTITIES = resolveGpuiSourceIdentities(ROOT);
const EVIDENCE_PATH = "docs/platform/evidence/native-content-linux.json";
const SCREENSHOT_PATH =
  "docs/parity/screenshots/current/native-content/linux-wayland-terminal.png";
const BINARY_PATH = "target/debug/vibex-desktop";
const WINDOW_IDENTITY = "dev.vibex.desktop.preview";
const WTYPE_COMMIT = "d71be3a7b3f93b534a2823fd68cabd7ac2a02359";
const WTYPE_DEFAULT_PATH = "/tmp/vibex-wtype-v0.4/build/wtype";
const SOURCE_INPUT_ROOTS = [
  "Cargo.lock",
  "Cargo.toml",
  "apps/desktop/Cargo.toml",
  "apps/desktop/src/main.rs",
  "apps/desktop/src/document_interaction.rs",
  "apps/desktop/src/native_content.rs",
  "apps/desktop/src/office_surface.rs",
  "apps/desktop/src/pdf_surface.rs",
  "apps/desktop/src/pdf_worker.rs",
  "apps/desktop/src/terminal_surface.rs",
  "crates/content/Cargo.toml",
  "crates/content/src",
  "crates/terminal/Cargo.toml",
  "crates/terminal/src",
  "docs/platform/fixtures/office-interaction.docx",
  "scripts/capture-document-interaction-linux.mjs",
  "scripts/capture-native-content-linux.mjs",
  "scripts/generate-office-fixtures.mjs"
];
const BASE_CLAIMS = [
  ["native_wayland_window", "passed"],
  ["live_pty_command_round_trip", "passed"],
  ["raw_snapshot_frame", "passed"],
  ["web_zero_allocation", "passed"],
  ["diagnostics_redacted", "passed"],
  ["clean_close_cleanup", "passed"],
  ["pdf_office_interaction", "passed"]
];
const TERMINAL_STRESS_EVIDENCE_PATH = "docs/platform/evidence/terminal-stress-linux.json";
const X11_EVIDENCE_PATH = "docs/platform/evidence/native-content-x11-linux.json";

function validateExternalEvidence() {
  run("node", ["scripts/capture-terminal-stress-linux.mjs"]);
  run("node", ["scripts/capture-native-content-x11-linux.mjs"]);
  const terminalStress = readJson(TERMINAL_STRESS_EVIDENCE_PATH);
  const x11 = readJson(X11_EVIDENCE_PATH);
  assert(
    terminalStress.status === "passed" &&
      terminalStress.summary?.terminalStressGateSatisfied === true &&
      terminalStress.run?.soakObservedSeconds >= 300,
    "terminal stress evidence is incomplete"
  );
  assert(
    (x11.status === "passed" && x11.summary?.x11NativeGateSatisfied === true) ||
      (x11.status === "blocked" && x11.summary?.x11NativeGateSatisfied === false),
    "X11 evidence status is inconsistent"
  );
  return {
    terminalStress: {
      path: TERMINAL_STRESS_EVIDENCE_PATH,
      sha256: sha256(readFileSync(rootPath(TERMINAL_STRESS_EVIDENCE_PATH))),
      status: terminalStress.status
    },
    x11: {
      path: X11_EVIDENCE_PATH,
      sha256: sha256(readFileSync(rootPath(X11_EVIDENCE_PATH))),
      status: x11.status,
      blocker: x11.claims?.[0]?.blocker ?? null
    }
  };
}

function claimMatrix(externalEvidence) {
  return [
    ...BASE_CLAIMS.slice(0, 6),
    ["x11_native_matrix", externalEvidence.x11.status],
    ["terminal_stress_and_soak", externalEvidence.terminalStress.status],
    ...BASE_CLAIMS.slice(6)
  ];
}

function validateDocumentInteractionEvidence() {
  run("node", ["scripts/capture-document-interaction-linux.mjs"]);
  const evidence = readJson("docs/platform/evidence/document-interaction-linux.json");
  assert(evidence.status === "passed", "PDF/Office physical interaction evidence did not pass");
  assert(
    evidence.runner?.activeMonitorObserved === true &&
      evidence.window?.xwayland === false &&
      evidence.run?.pdf?.currentPage === 1 &&
      evidence.run?.pdf?.zoomLabel === "125%" &&
      evidence.run?.office?.closeCommandObserved === true &&
      evidence.run?.office?.finalResidentItems === 0 &&
      evidence.run?.office?.finalResidentBytes === 0 &&
      evidence.capture?.pdfRegion?.uniqueColors >= 100 &&
      evidence.capture.pdfRegion.entropy > 0.02 &&
      evidence.capture.pdfRegion.standardDeviation > 0.02 &&
      evidence.process?.appExitCode === 0 &&
      evidence.process?.panicMentioned === false,
    "PDF/Office physical interaction contract is incomplete"
  );
  const screenshot = rootPath(evidence.capture?.screenshotPath ?? "");
  assert(
    existsSync(screenshot) && evidence.capture.screenshotSha256 === sha256(readFileSync(screenshot)),
    "PDF/Office physical screenshot identity drifted"
  );
}

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
    fail(
      `${command} ${args.join(" ")} failed with status ${result.status ?? "unknown"}:\n` +
        (result.stderr || result.stdout || "")
    );
  }
  return result.stdout ?? "";
}

function requireCommand(command) {
  run("sh", ["-c", `command -v ${command} >/dev/null`]);
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

function wtypeIdentity() {
  const binaryPath = process.env.VIBEX_WTYPE_BIN ?? WTYPE_DEFAULT_PATH;
  assert(existsSync(binaryPath) && statSync(binaryPath).isFile(), "pinned wtype binary is missing");
  const sourceRoot = resolve(dirname(binaryPath), "..");
  const commit = run("git", ["-C", sourceRoot, "rev-parse", "HEAD"]).trim();
  assert(commit === WTYPE_COMMIT, "wtype source revision is not pinned to v0.4");
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

function sourceEvidence() {
  return {
    captureParentCommit: run("git", ["rev-parse", "HEAD"]).trim(),
    dependencySourcePolicy: SOURCE_IDENTITIES.dependencySourcePolicy,
    zedRevision: SOURCE_IDENTITIES.zedRevision,
    gpuiComponentRevision: SOURCE_IDENTITIES.gpuiComponentRevision,
    rustToolchain: run("rustc", ["--version"]).trim(),
    lockfileSha256: sha256(readFileSync(rootPath("Cargo.lock"))),
    sourceInputRoots: SOURCE_INPUT_ROOTS,
    sourceInputTreeSha256: sourceInputTreeSha256()
  };
}

function runnerEvidence(wtype, activeMonitorObserved) {
  return {
    platform: process.platform,
    architecture: process.arch,
    kernelRelease: run("uname", ["-r"]).trim(),
    displayBackend: "wayland-hyprland",
    compositor: "Hyprland",
    syntheticDisplay: false,
    activeMonitorObserved,
    inputInjection: wtype.evidence
  };
}

function hyprlandJson(kind) {
  return JSON.parse(run("hyprctl", ["-j", kind]));
}

async function waitForClient(app) {
  for (let attempt = 0; attempt < 300; attempt += 1) {
    const client = hyprlandJson("clients").find(
      (candidate) => candidate.pid === app.pid && candidate.class === WINDOW_IDENTITY
    );
    if (client) return client;
    if (app.exitCode !== null) fail("GPUI Native Content exited before creating a window");
    await sleep(50);
  }
  fail("GPUI Native Content window was not discovered by Hyprland");
}

async function waitForProgress(path, predicate, app) {
  for (let attempt = 0; attempt < 300; attempt += 1) {
    if (existsSync(path)) {
      try {
        const progress = JSON.parse(readFileSync(path, "utf8"));
        if (progress.schemaVersion === "native-content-progress.v1" && predicate(progress)) {
          return progress;
        }
      } catch {
        // The foreground process may be between truncate and write; retry.
      }
    }
    if (app.exitCode !== null) fail("GPUI Native Content exited before reaching expected progress");
    await sleep(100);
  }
  fail("GPUI Native Content progress exceeded its bounded timeout");
}

async function waitForReport(path, app) {
  for (let attempt = 0; attempt < 300; attempt += 1) {
    if (existsSync(path)) return JSON.parse(readFileSync(path, "utf8"));
    if (app.exitCode !== null) fail("GPUI Native Content exited without writing a report");
    await sleep(100);
  }
  fail("GPUI Native Content report exceeded its bounded timeout");
}

function waitForExit(app, timeoutMs) {
  if (app.exitCode !== null) return Promise.resolve({ code: app.exitCode, signal: app.signalCode });
  return new Promise((resolvePromise, rejectPromise) => {
    const timeout = setTimeout(
      () => rejectPromise(new Error("GPUI Native Content did not exit")),
      timeoutMs
    );
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
  const [width, height, uniqueColors, entropy, mean, standardDeviation] = output
    .trim()
    .split("\t")
    .map(Number);
  const metrics = { width, height, uniqueColors, entropy, mean, standardDeviation };
  assert(Object.values(metrics).every(Number.isFinite), "invalid screenshot metrics");
  return metrics;
}

function captureWindow(client) {
  const screenshot = rootPath(SCREENSHOT_PATH);
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
      metrics.height >= 898 &&
      metrics.height <= 902 &&
      metrics.uniqueColors > 1 &&
      metrics.entropy > 0 &&
      metrics.standardDeviation > 0,
    "Native Content screenshot does not contain credible fixed-size pixels"
  );
  return {
    screenshotPath: SCREENSHOT_PATH,
    screenshotSha256: sha256(readFileSync(screenshot)),
    ...metrics
  };
}

function validateRun(runReport) {
  assert(runReport?.schemaVersion === "native-content-run.v1", "run schema is invalid");
  assert(runReport.status === "passed", "Native Content run did not pass");
  assert(
    runReport.terminal?.ptyCreated === true &&
      runReport.terminal.rawByteSnapshots === true &&
      runReport.terminal.imeCapableInput === true &&
      runReport.terminal.commandSubmitted === true &&
      runReport.terminal.commandMarkerObserved === true &&
      Number.isInteger(runReport.terminal.frameRows) &&
      runReport.terminal.frameRows >= 16 &&
      runReport.terminal.frameRows <= 64 &&
      Number.isInteger(runReport.terminal.frameColumns) &&
      runReport.terminal.frameColumns >= 60 &&
      runReport.terminal.frameColumns <= 240 &&
      runReport.terminal.ingestedBytes > 0 &&
      runReport.terminal.nonBlankCells > 0 &&
      runReport.terminal.cursorPresent === true &&
      runReport.terminal.fullRepaints >= 1 &&
      runReport.terminal.terminalOutputStored === false,
    "live terminal observation is incomplete"
  );
  assert(
    runReport.web?.typedUnsupportedState === true &&
      runReport.web.ordinaryNativeSurfaceAllocated === false &&
      runReport.web.rightRailNativeSurfaceAllocated === false &&
      runReport.web.profileOrCacheAllocated === false &&
      runReport.web.networkTaskAllocated === false,
    "Web zero-allocation observation is incomplete"
  );
  assert(
    Object.values(runReport.privacy ?? {}).every((value) => value === false),
    "Native Content run retained private content"
  );
  const serialized = JSON.stringify(runReport);
  assert(!serialized.includes("vibex-native-content-ok"), "run report retained terminal output");
  assert(!serialized.includes("example.com"), "run report retained a URL");
}

function validateEvidenceObject(evidence) {
  assert(
    evidence?.schemaVersion === "native-content-linux-evidence.v1",
    "Native Content evidence schema is invalid"
  );
  assert(
    evidence.status === "passed" || evidence.status === "partial" || evidence.status === "blocked",
    "Native Content evidence status is invalid"
  );
  const applicability = classifyGpuiEvidence(ROOT, EVIDENCE_PATH, evidence.source);
  if (applicability === "current") {
    assert(evidence.source.zedRevision === SOURCE_IDENTITIES.zedRevision, "Zed revision drifted");
    assert(
      evidence.source.gpuiComponentRevision === SOURCE_IDENTITIES.gpuiComponentRevision,
      "gpui-component revision drifted"
    );
    assert(
      evidence.source.lockfileSha256 === sha256(readFileSync(rootPath("Cargo.lock"))),
      "lockfile identity drifted"
    );
    assert(
      JSON.stringify(evidence.source.sourceInputRoots) === JSON.stringify(SOURCE_INPUT_ROOTS) &&
        evidence.source.sourceInputTreeSha256 === sourceInputTreeSha256(),
      "Native Content source identity drifted"
    );
  }
  assert(
    evidence.runner?.platform === "linux" &&
      evidence.runner.displayBackend === "wayland-hyprland" &&
      evidence.runner.syntheticDisplay === false,
    "Native Content evidence is not physical Linux Wayland"
  );
  assert(
    evidence.window?.identity === WINDOW_IDENTITY &&
      evidence.window.discovered === true &&
      evidence.window.xwayland === false,
    "Native Content native Wayland window is invalid"
  );
  if (evidence.status === "blocked") {
    assert(
      evidence.runner.activeMonitorObserved === false &&
        evidence.window.monitorId === -1 &&
        evidence.capture === null &&
        evidence.run === null &&
        evidence.process?.processExited === true &&
        evidence.process.appExitCode === 0 &&
        evidence.process.panicMentioned === false,
      "blocked Native Content evidence fabricated capture or failed cleanup"
    );
    assert(
      evidence.claims?.length === 1 &&
        evidence.claims[0].id === "active_physical_output" &&
        evidence.claims[0].status === "blocked" &&
        evidence.claims[0].blocker?.code === "active-wayland-monitor-unavailable" &&
        evidence.summary?.passedClaims === 0 &&
        evidence.summary.blockedClaims === 1 &&
        evidence.summary.nativeContentGateSatisfied === false,
      "blocked Native Content claim is invalid"
    );
    const blockedSerialized = JSON.stringify(evidence);
    for (const forbidden of ["vibex-native-content-ok", "example.com", ROOT, process.env.HOME]) {
      if (forbidden) {
        assert(!blockedSerialized.includes(forbidden), "blocked evidence retained private content");
      }
    }
    return applicability;
  }
  assert(evidence.runner.activeMonitorObserved === true, "captured evidence lacks an active monitor");
  const externalEvidence = validateExternalEvidence();
  assert(
    evidence.externalEvidence?.terminalStress?.path === externalEvidence.terminalStress.path &&
      evidence.externalEvidence.terminalStress.sha256 === externalEvidence.terminalStress.sha256 &&
      evidence.externalEvidence.terminalStress.status === externalEvidence.terminalStress.status &&
      evidence.externalEvidence?.x11?.path === externalEvidence.x11.path &&
      evidence.externalEvidence.x11.sha256 === externalEvidence.x11.sha256 &&
      evidence.externalEvidence.x11.status === externalEvidence.x11.status,
    "Native Content external evidence identity drifted"
  );
  const screenshot = rootPath(evidence.capture?.screenshotPath ?? "");
  assert(existsSync(screenshot), "Native Content screenshot is missing");
  assert(
    evidence.capture.screenshotSha256 === sha256(readFileSync(screenshot)) &&
      evidence.capture.uniqueColors > 1 &&
      evidence.capture.entropy > 0,
    "Native Content screenshot identity is stale"
  );
  validateRun(evidence.run);
  validateDocumentInteractionEvidence();
  assert(
    evidence.process?.processExited === true &&
      evidence.process.appExitCode === 0 &&
      evidence.process.panicMentioned === false,
    "Native Content process did not close cleanly"
  );
  assert(
    JSON.stringify(evidence.claims?.map((claim) => [claim.id, claim.status])) ===
      JSON.stringify(claimMatrix(externalEvidence)),
    "Native Content claim matrix drifted"
  );
  const blockedClaims = claimMatrix(externalEvidence).filter(([, status]) => status === "blocked").length;
  const passedClaims = claimMatrix(externalEvidence).length - blockedClaims;
  assert(
    evidence.summary?.passedClaims === passedClaims &&
      evidence.summary.blockedClaims === blockedClaims &&
      evidence.summary.nativeContentGateSatisfied === (blockedClaims === 0) &&
      evidence.status === (blockedClaims === 0 ? "passed" : "partial"),
    "Native Content summary is inconsistent"
  );
  const serialized = JSON.stringify(evidence);
  for (const forbidden of ["vibex-native-content-ok", "example.com", ROOT, process.env.HOME]) {
    if (forbidden) assert(!serialized.includes(forbidden), "Native Content evidence retained private content");
  }
  return applicability;
}

function verifyEvidence() {
  assert(existsSync(rootPath(EVIDENCE_PATH)), `${EVIDENCE_PATH} is missing`);
  const evidence = readJson(EVIDENCE_PATH);
  const applicability = validateEvidenceObject(evidence);
  console.log(
    `GPUI Native Content Linux verified: ${evidence.summary.passedClaims} passed, ` +
      `${evidence.summary.blockedClaims} blocked; applicability=${applicability}`
  );
}

function selfTest() {
  const evidence = readJson(EVIDENCE_PATH);
  validateEvidenceObject(evidence);
  const mutations =
    evidence.status === "blocked"
      ? [
          ["fabricated active monitor", (copy) => (copy.runner.activeMonitorObserved = true)],
          ["fabricated screenshot", (copy) => (copy.capture = { screenshotPath: "fake.png" })],
          ["removed monitor blocker", (copy) => (copy.claims[0].status = "passed")],
          ["retained terminal output", (copy) => (copy.rawTerminalOutput = "vibex-native-content-ok")]
        ]
      : [
          ["missing terminal marker", (copy) => (copy.run.terminal.commandMarkerObserved = false)],
          ["fabricated Web allocation", (copy) => (copy.run.web.ordinaryNativeSurfaceAllocated = true)],
          ["retained terminal output", (copy) => (copy.rawTerminalOutput = "vibex-native-content-ok")],
          [
            "fabricated X11 pass",
            (copy) => (copy.claims.find((claim) => claim.id === "x11_native_matrix").status = "passed")
          ],
          [
            "removed terminal stress",
            (copy) => (copy.claims.find((claim) => claim.id === "terminal_stress_and_soak").status = "blocked")
          ],
          [
            "removed document interaction",
            (copy) => (copy.claims.find((claim) => claim.id === "pdf_office_interaction").status = "blocked")
          ]
        ];
  for (const [label, mutate] of mutations) {
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
  console.log("GPUI Native Content Linux negative-case self-test passed");
}

function capturePrerequisites() {
  assert(process.platform === "linux", "Native Content capture requires Linux");
  assert(process.env.XDG_SESSION_TYPE === "wayland", "Native Content capture requires Wayland");
  for (const command of ["cargo", "git", "grim", "hyprctl", "identify", "rustc"]) {
    requireCommand(command);
  }
  return wtypeIdentity();
}

async function capture() {
  const wtype = capturePrerequisites();
  run("cargo", ["build", "-p", "vibex-desktop", "--locked"], { stdio: "inherit" });
  const temporaryRoot = mkdtempSync(join(tmpdir(), "vibex-native-content-"));
  const isolatedHome = join(temporaryRoot, "home");
  const isolatedConfig = join(temporaryRoot, "config");
  const isolatedData = join(temporaryRoot, "data");
  const isolatedCache = join(temporaryRoot, "cache");
  const isolatedWorkspace = join(temporaryRoot, "workspace");
  for (const directory of [isolatedHome, isolatedConfig, isolatedData, isolatedCache, isolatedWorkspace]) {
    mkdirSync(directory, { recursive: true });
  }
  const reportPath = join(temporaryRoot, "native-content.json");
  const progressPath = join(temporaryRoot, "native-content.progress.json");
  let stderr = "";
  let app;
  try {
    app = spawn(rootPath(BINARY_PATH), ["--native-content-workbench", reportPath], {
      cwd: ROOT,
      env: {
        ...process.env,
        HOME: isolatedHome,
        XDG_CONFIG_HOME: isolatedConfig,
        XDG_DATA_HOME: isolatedData,
        XDG_CACHE_HOME: isolatedCache,
        XDG_SESSION_TYPE: "wayland",
        VIBEX_NATIVE_CONTENT_WORKSPACE_ROOT: isolatedWorkspace,
        TERM: "xterm-256color",
        SHELL: "/bin/sh",
        USER: "vibex",
        LOGNAME: "vibex",
        HOSTNAME: "vibex-native"
      },
      stdio: ["ignore", "ignore", "pipe"]
    });
    app.stderr.on("data", (chunk) => {
      if (stderr.length < 64 * 1024) stderr += chunk.toString("utf8");
    });
    let client = await waitForClient(app);
    const monitor = hyprlandJson("monitors").find((candidate) => candidate.id === client.monitor);
    const selector = `address:${client.address}`;
    if (!monitor) {
      run("hyprctl", ["dispatch", "closewindow", selector]);
      const exit = await waitForExit(app, 10_000);
      assert(exit.code === 0 && exit.signal === null, "blocked capture process did not exit cleanly");
      const evidence = {
        schemaVersion: "native-content-linux-evidence.v1",
        status: "blocked",
        capturedAt: new Date().toISOString(),
        source: sourceEvidence(),
        runner: runnerEvidence(wtype, false),
        window: {
          identity: WINDOW_IDENTITY,
          discovered: true,
          xwayland: client.xwayland,
          monitorId: client.monitor,
          width: client.size[0],
          height: client.size[1]
        },
        capture: null,
        process: {
          processExited: true,
          appExitCode: exit.code,
          panicMentioned: /panicked at|thread '.+' panicked/i.test(stderr)
        },
        run: null,
        claims: [
          {
            id: "active_physical_output",
            status: "blocked",
            decisionBearing: true,
            blocker: {
              code: "active-wayland-monitor-unavailable",
              owner: "desktop-native-content",
              action:
                "Repeat the capture in a physical Hyprland session with an active monitor; do not create a headless output."
            }
          }
        ],
        summary: {
          passedClaims: 0,
          blockedClaims: 1,
          nativeContentGateSatisfied: false
        },
        limitations: [
          "A native Wayland window existed on monitor -1, but no active physical output was available.",
          "No input was injected, no screenshot was captured, and no Terminal/Web behavior is claimed.",
          "Synthetic or headless compositor output is explicitly not accepted as physical evidence."
        ]
      };
      validateEvidenceObject(evidence);
      mkdirSync(dirname(rootPath(EVIDENCE_PATH)), { recursive: true });
      writeFileSync(rootPath(EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
      verifyEvidence();
      return;
    }
    if (!client.floating) run("hyprctl", ["dispatch", "togglefloating", selector]);
    run("hyprctl", ["dispatch", "resizewindowpixel", `exact 1200 900,${selector}`]);
    run("hyprctl", [
      "dispatch",
      "movewindowpixel",
      `exact ${monitor.x + 80} ${monitor.y + 60},${selector}`
    ]);
    run("hyprctl", ["dispatch", "focuswindow", selector]);
    await waitForProgress(progressPath, (progress) => progress.ready === true, app);
    await sleep(300);
    run("hyprctl", ["dispatch", "sendshortcut", `,t,${selector}`]);
    await waitForProgress(
      progressPath,
      (progress) => progress.commandMarkerObserved === true,
      app
    );
    const runReport = await waitForReport(reportPath, app);
    validateRun(runReport);
    const externalEvidence = validateExternalEvidence();
    const claims = claimMatrix(externalEvidence);
    await sleep(300);
    client = hyprlandJson("clients").find((candidate) => candidate.address === client.address);
    assert(client && client.xwayland === false, "Native Content window lost native Wayland identity");
    const capture = captureWindow(client);
    run("hyprctl", ["dispatch", "closewindow", selector]);
    const exit = await waitForExit(app, 10_000);
    assert(exit.code === 0 && exit.signal === null, "Native Content process did not exit cleanly");

    const evidence = {
      schemaVersion: "native-content-linux-evidence.v1",
      status: claims.some(([, status]) => status === "blocked") ? "partial" : "passed",
      capturedAt: new Date().toISOString(),
      source: sourceEvidence(),
      runner: runnerEvidence(wtype, true),
      window: {
        identity: WINDOW_IDENTITY,
        discovered: true,
        xwayland: false,
        monitorId: client.monitor,
        width: client.size[0],
        height: client.size[1]
      },
      capture,
      process: {
        processExited: true,
        appExitCode: exit.code,
        panicMentioned: /panicked at|thread '.+' panicked/i.test(stderr)
      },
      run: runReport,
      externalEvidence: {
        terminalStress: externalEvidence.terminalStress,
        x11: {
          path: externalEvidence.x11.path,
          sha256: externalEvidence.x11.sha256,
          status: externalEvidence.x11.status
        }
      },
      claims: claims.map(([id, status]) => ({
        id,
        status,
        decisionBearing: true,
        blocker:
          status === "blocked"
            ? {
                code: id.replaceAll("_", "-"),
                owner: "desktop-native-content",
                action:
                  id === "x11_native_matrix"
                    ? externalEvidence.x11.blocker?.action ??
                      "Repeat the native content interaction protocol on an authorized physical Xorg session."
                    : null
              }
            : null
      })),
      summary: {
        passedClaims: claims.filter(([, status]) => status === "passed").length,
        blockedClaims: claims.filter(([, status]) => status === "blocked").length,
        nativeContentGateSatisfied: claims.every(([, status]) => status === "passed")
      },
      limitations: [
        "This capture proves one physical native Wayland frame and one live PTY command round trip.",
        "The evidence stores no terminal output, URL, PDF content, or Office content.",
        "wtype is an MIT capture-only prerequisite and is not linked or packaged with Vibex.",
        externalEvidence.x11.status === "passed"
          ? "Independent physical Xorg and the five-minute Terminal stress matrix passed separate evidence protocols."
          : "The five-minute Terminal stress matrix passed; independent physical Xorg remains a decision-bearing blocker."
      ]
    };
    if (evidence.process.panicMentioned) console.error(stderr.slice(-8_000));
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
    rmSync(temporaryRoot, { recursive: true, force: true });
  }
}

const mode = process.argv[2];
if (mode === "--write") await capture();
else if (mode === "--self-test") selfTest();
else if (mode === "--preflight") {
  capturePrerequisites();
  console.log("GPUI Native Content Linux capture prerequisites verified");
}
else if (mode === undefined) verifyEvidence();
else fail("usage: node scripts/capture-native-content-linux.mjs [--write|--self-test|--preflight]");

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

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const EVIDENCE_PATH = "docs/platform/evidence/native-content-x11-linux.json";
const SCREENSHOT_PATH = "docs/parity/screenshots/current/native-content/linux-x11-terminal.png";
const BINARY_PATH = "target/debug/vibex-desktop";
const WINDOW_CLASS = "dev.vibex.desktop.preview";
const SOURCE_INPUT_ROOTS = [
  "Cargo.lock",
  "Cargo.toml",
  "apps/desktop/Cargo.toml",
  "apps/desktop/src/main.rs",
  "apps/desktop/src/native_content.rs",
  "apps/desktop/src/terminal_surface.rs",
  "crates/content/Cargo.toml",
  "crates/content/src/terminal.rs",
  "crates/terminal/Cargo.toml",
  "crates/terminal/src",
  "scripts/capture-native-content-x11-linux.mjs",
  "scripts/x11-x-test-input.py"
];

function fail(message) {
  throw new Error(message);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function rootPath(path) {
  const absolute = resolve(ROOT, path);
  if (absolute !== ROOT && !absolute.startsWith(`${ROOT}${sep}`)) fail(`path escapes repository: ${path}`);
  return absolute;
}

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function readJson(path) {
  return JSON.parse(readFileSync(rootPath(path), "utf8"));
}

function sleep(milliseconds) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}

function x11Environment(display, xauthority, extra = {}) {
  const env = { ...process.env, ...extra, DISPLAY: display, XDG_SESSION_TYPE: "x11" };
  delete env.WAYLAND_DISPLAY;
  delete env.HYPRLAND_INSTANCE_SIGNATURE;
  if (xauthority) env.XAUTHORITY = xauthority;
  else delete env.XAUTHORITY;
  return env;
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
    fail(`${command} ${args.join(" ")} failed with status ${result.status ?? "unknown"}:\n${result.stderr || result.stdout || ""}`);
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

function sourceEvidence() {
  return {
    captureParentCommit: run("git", ["rev-parse", "HEAD"]).trim(),
    rustToolchain: run("rustc", ["--version"]).trim(),
    lockfileSha256: sha256(readFileSync(rootPath("Cargo.lock"))),
    sourceInputRoots: SOURCE_INPUT_ROOTS,
    sourceInputTreeSha256: sourceInputTreeSha256()
  };
}

function displayServerKind(display) {
  const displayNumber = display.match(/^:(\d+)/)?.[1];
  const processes = run("ps", ["-eo", "args="]).split("\n");
  const matching = processes.filter((line) =>
    displayNumber === undefined ? false : new RegExp(`(?:^|\\s):${displayNumber}(?:\\s|$)`).test(line)
  );
  if (matching.some((line) => /(?:^|\/)Xwayland(?:\s|$)/.test(line))) return "xwayland";
  if (matching.some((line) => /(?:^|\/)(?:Xvfb|Xephyr)(?:\s|$)/.test(line))) return "synthetic-x11";
  if (matching.some((line) => /(?:^|\/)Xorg(?:\s|$)/.test(line))) return "xorg";
  if (
    displayNumber === "0" &&
    processes.some((line) => /(?:^|\/)Xorg(?:\s|$)/.test(line) && !/Xwayland/.test(line))
  ) {
    return "xorg";
  }
  return "unknown";
}

function displayProbe(display, xauthority) {
  const env = x11Environment(display, xauthority);
  const serverKind = displayServerKind(display);
  const info = runResult("xdpyinfo", [], { env, timeout: 5_000 });
  if (info.status !== 0) {
    return {
      display,
      authorized: false,
      serverKind,
      synthetic: serverKind === "synthetic-x11",
      xwayland: null,
      independentXorg: false,
      physicalConnector: null,
      dri3: false,
      xtest: false
    };
  }
  const output = info.stdout ?? "";
  const xwayland = /^\s*XWAYLAND\s*$/m.test(output);
  const randr = runResult("xrandr", ["--query"], { env, timeout: 5_000 });
  const connector = (randr.stdout ?? "")
    .split("\n")
    .map((line) => line.match(/^([^ ]+) connected(?: primary)? /)?.[1])
    .find((name) => name && /^(?:e?DP|HDMI|DVI|VGA|USB-C|DisplayPort)[-_]/i.test(name)) ?? null;
  return {
    display,
    authorized: true,
    serverKind,
    synthetic: serverKind === "synthetic-x11",
    xwayland,
    independentXorg: !xwayland && serverKind === "xorg",
    physicalConnector: connector,
    dri3: /^\s*DRI3\s*$/m.test(output),
    xtest: /^\s*XTEST\s*$/m.test(output)
  };
}

function discoverDisplays() {
  const candidates = new Set();
  if (process.env.VIBEX_X11_DISPLAY) candidates.add(process.env.VIBEX_X11_DISPLAY);
  if (process.env.DISPLAY) candidates.add(process.env.DISPLAY.replace(/\.\d+$/, ""));
  if (existsSync("/tmp/.X11-unix")) {
    for (const entry of readdirSync("/tmp/.X11-unix")) {
      const number = entry.match(/^X(\d+)$/)?.[1];
      if (number !== undefined) candidates.add(`:${number}`);
    }
  }
  const xauthority = process.env.VIBEX_X11_XAUTHORITY ?? process.env.XAUTHORITY ?? null;
  return [...candidates].sort().map((display) => displayProbe(display, xauthority));
}

function physicalXorgProcessObserved() {
  const output = run("ps", ["-eo", "args="]);
  return output.split("\n").some((line) => /(?:^|\/)Xorg(?:\s|$)/.test(line) && !/Xwayland/.test(line));
}

function parseImageMetrics(output) {
  const [width, height, uniqueColors, entropy, mean, standardDeviation] = output
    .trim()
    .split("\t")
    .map(Number);
  const metrics = { width, height, uniqueColors, entropy, mean, standardDeviation };
  assert(Object.values(metrics).every(Number.isFinite), "invalid X11 screenshot metrics");
  return metrics;
}

function findWindowIds(display, xauthority) {
  const env = x11Environment(display, xauthority);
  const tree = runResult("xwininfo", ["-root", "-tree"], { env, timeout: 5_000 });
  if (tree.status !== 0) return [];
  return [...(tree.stdout ?? "").matchAll(/^\s*(0x[0-9a-f]+) /gim)].map((match) => match[1]);
}

async function waitForWindow(app, display, xauthority) {
  const env = x11Environment(display, xauthority);
  for (let attempt = 0; attempt < 300; attempt += 1) {
    for (const id of findWindowIds(display, xauthority)) {
      const properties = runResult("xprop", ["-id", id, "WM_CLASS", "_NET_WM_PID"], {
        env,
        timeout: 5_000
      });
      const text = properties.stdout ?? "";
      if (text.includes(WINDOW_CLASS) && text.includes(String(app.pid))) return id;
    }
    if (app.exitCode !== null) fail("GPUI Native Content exited before creating an X11 window");
    await sleep(50);
  }
  fail("GPUI Native Content X11 window was not discovered");
}

async function waitForJson(path, predicate, app, label) {
  for (let attempt = 0; attempt < 600; attempt += 1) {
    if (existsSync(path)) {
      try {
        const value = JSON.parse(readFileSync(path, "utf8"));
        if (predicate(value)) return value;
      } catch {
        // Retry while the app atomically advances its bounded report.
      }
    }
    if (app.exitCode !== null) fail(`GPUI Native Content exited before ${label}`);
    await sleep(50);
  }
  fail(`GPUI Native Content timed out before ${label}`);
}

function waitForExit(app, timeoutMs) {
  if (app.exitCode !== null) return Promise.resolve({ code: app.exitCode, signal: app.signalCode });
  return new Promise((resolvePromise, rejectPromise) => {
    const timeout = setTimeout(() => rejectPromise(new Error("GPUI Native Content X11 did not exit")), timeoutMs);
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

function validateRun(runReport) {
  assert(runReport?.schemaVersion === "native-content-run.v1" && runReport.status === "passed", "X11 run schema is invalid");
  assert(
    runReport.terminal?.ptyCreated === true &&
      runReport.terminal.commandSubmitted === true &&
      runReport.terminal.commandMarkerObserved === true &&
      runReport.terminal.ingestedBytes > 0 &&
      runReport.terminal.cursorPresent === true &&
      runReport.terminal.terminalOutputStored === false,
    "X11 terminal observation is incomplete"
  );
  assert(Object.values(runReport.privacy ?? {}).every((value) => value === false), "X11 run retained private content");
}

function validateEvidence(evidence) {
  assert(evidence?.schemaVersion === "native-content-x11-linux-evidence.v1", "X11 evidence schema drifted");
  assert(evidence.status === "passed" || evidence.status === "blocked", "X11 evidence status is invalid");
  const applicability = classifyGpuiEvidence(ROOT, EVIDENCE_PATH, evidence.source);
  assert(
    JSON.stringify(evidence.source?.sourceInputRoots) === JSON.stringify(SOURCE_INPUT_ROOTS) &&
      (applicability !== "current" ||
        evidence.source.sourceInputTreeSha256 === sourceInputTreeSha256()),
    "X11 source identity drifted"
  );
  assert(evidence.runner?.platform === "linux" && evidence.runner.syntheticDisplay === false, "X11 runner identity is invalid");
  if (evidence.status === "blocked") {
    assert(
      evidence.runner.independentXorgAuthorized === false &&
        evidence.capture === null &&
        evidence.run === null &&
        evidence.process === null &&
        evidence.claims?.length === 1 &&
        evidence.claims[0].id === "x11_native_matrix" &&
        evidence.claims[0].status === "blocked" &&
        ["physical-xorg-authorization-unavailable", "physical-xorg-session-unavailable"].includes(evidence.claims[0].blocker?.code) &&
        evidence.summary?.passedClaims === 0 &&
        evidence.summary.blockedClaims === 1 &&
        evidence.summary.x11NativeGateSatisfied === false,
      "blocked X11 evidence fabricated a physical result"
    );
  } else {
    assert(
      evidence.runner.displayBackend === "x11-xorg" &&
        evidence.runner.serverKind === "xorg" &&
        evidence.runner.independentXorgAuthorized === true &&
        evidence.runner.xwaylandDetected === false &&
        typeof evidence.runner.physicalConnector === "string" &&
        evidence.runner.physicalConnector.length > 0 &&
        evidence.runner.dri3Observed === true &&
        evidence.runner.xtestObserved === true,
      "passed X11 evidence is not an independent physical Xorg run"
    );
    const screenshot = rootPath(evidence.capture?.screenshotPath ?? "");
    assert(
      existsSync(screenshot) &&
        evidence.capture.screenshotSha256 === sha256(readFileSync(screenshot)) &&
        evidence.capture.uniqueColors > 1 &&
        evidence.capture.entropy > 0,
      "X11 screenshot identity is stale"
    );
    validateRun(evidence.run);
    assert(
      evidence.process?.processExited === true &&
        evidence.process.appExitCode === 0 &&
        evidence.process.panicMentioned === false &&
        evidence.claims?.length === 1 &&
        evidence.claims[0].status === "passed" &&
        evidence.summary?.passedClaims === 1 &&
        evidence.summary.blockedClaims === 0 &&
        evidence.summary.x11NativeGateSatisfied === true,
      "passed X11 process/claim summary is invalid"
    );
  }
  const serialized = JSON.stringify(evidence);
  for (const forbidden of ["vibex-native-content-ok", "example.com", ROOT, process.env.HOME, "/run/sddm/xauth_"]) {
    if (forbidden) assert(!serialized.includes(forbidden), "X11 evidence retained private content or credentials");
  }
  return applicability;
}

function verify() {
  assert(existsSync(rootPath(EVIDENCE_PATH)), `${EVIDENCE_PATH} is missing`);
  const evidence = readJson(EVIDENCE_PATH);
  const applicability = validateEvidence(evidence);
  console.log(
    `GPUI Native Content X11 verified: ${evidence.status}; ` +
      `backend=${evidence.runner.displayBackend}; applicability=${applicability}`
  );
}

function selfTest() {
  const evidence = readJson(EVIDENCE_PATH);
  validateEvidence(evidence);
  const mutations = evidence.status === "blocked"
    ? [
        ["fabricated authorization", (copy) => (copy.runner.independentXorgAuthorized = true)],
        ["fabricated pass", (copy) => (copy.claims[0].status = "passed")],
        ["fabricated screenshot", (copy) => (copy.capture = { screenshotPath: "fake.png" })],
        ["private auth path", (copy) => (copy.authorizationPath = "/run/sddm/xauth_secret")]
      ]
    : [
        ["XWayland substitution", (copy) => (copy.runner.xwaylandDetected = true)],
        ["missing connector", (copy) => (copy.runner.physicalConnector = null)],
        ["missing marker", (copy) => (copy.run.terminal.commandMarkerObserved = false)],
        ["stale screenshot", (copy) => (copy.capture.screenshotSha256 = "0".repeat(64))]
      ];
  for (const [label, mutate] of mutations) {
    const copy = JSON.parse(JSON.stringify(evidence));
    mutate(copy);
    let rejected = false;
    try {
      validateEvidence(copy);
    } catch {
      rejected = true;
    }
    assert(rejected, `negative self-test was accepted: ${label}`);
  }
  console.log("GPUI Native Content X11 negative-case self-test passed");
}

function writeBlocked(displays, physicalXorgObserved) {
  const xwayland = displays.find((display) => display.authorized && display.xwayland === true);
  const blockerCode = physicalXorgObserved
    ? "physical-xorg-authorization-unavailable"
    : "physical-xorg-session-unavailable";
  const evidence = {
    schemaVersion: "native-content-x11-linux-evidence.v1",
    status: "blocked",
    capturedAt: new Date().toISOString(),
    source: sourceEvidence(),
    runner: {
      platform: process.platform,
      architecture: process.arch,
      kernelRelease: run("uname", ["-r"]).trim(),
        displayBackend: xwayland ? "xwayland-rejected" : "x11-unavailable",
      serverKind: xwayland?.serverKind ?? "unavailable",
      syntheticDisplay: false,
      physicalXorgProcessObserved: physicalXorgObserved,
      independentXorgAuthorized: false,
      xwaylandDetected: Boolean(xwayland),
      physicalConnector: null,
      dri3Observed: false,
      xtestObserved: false,
      probedDisplayCount: displays.length
    },
    capture: null,
    process: null,
    run: null,
    claims: [
      {
        id: "x11_native_matrix",
        status: "blocked",
        decisionBearing: true,
        blocker: {
          code: blockerCode,
          owner: "desktop-native-content",
          action: "Run the protocol from an authorized, active physical Xorg session; XWayland and synthetic X servers are rejected."
        }
      }
    ],
    summary: { passedClaims: 0, blockedClaims: 1, x11NativeGateSatisfied: false },
    limitations: [
      "The active desktop is Wayland and its DISPLAY endpoint is XWayland, which is not reported as independent Xorg evidence.",
      "A separate physical Xorg server process may exist, but this user session cannot authenticate to or prove an active output on it.",
      "No window, pixels, keyboard input, or terminal behavior is claimed for X11."
    ]
  };
  validateEvidence(evidence);
  mkdirSync(dirname(rootPath(EVIDENCE_PATH)), { recursive: true });
  writeFileSync(rootPath(EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
  verify();
}

async function capturePassed(probe) {
  assert(probe.authorized && probe.independentXorg && probe.physicalConnector, "selected X11 display is not physical Xorg");
  assert(probe.dri3 && probe.xtest, "selected X11 display lacks DRI3 or XTEST");
  run("cargo", ["build", "-p", "vibex-desktop", "--locked"], { stdio: "inherit" });
  const temporary = mkdtempSync(join(tmpdir(), "vibex-native-content-x11-"));
  const isolatedHome = join(temporary, "home");
  const isolatedWorkspace = join(temporary, "workspace");
  for (const directory of [isolatedHome, isolatedWorkspace]) mkdirSync(directory, { recursive: true });
  const reportPath = join(temporary, "run.json");
  const progressPath = join(temporary, "run.progress.json");
  const xauthority = process.env.VIBEX_X11_XAUTHORITY ?? process.env.XAUTHORITY ?? null;
  const env = x11Environment(probe.display, xauthority, {
    HOME: isolatedHome,
    XDG_CONFIG_HOME: join(temporary, "config"),
    XDG_DATA_HOME: join(temporary, "data"),
    XDG_CACHE_HOME: join(temporary, "cache"),
    VIBEX_NATIVE_CONTENT_WORKSPACE_ROOT: isolatedWorkspace,
    TERM: "xterm-256color",
    SHELL: "/bin/sh",
    USER: "vibex",
    LOGNAME: "vibex",
    HOSTNAME: "vibex-native"
  });
  let stderr = "";
  let app;
  try {
    app = spawn(rootPath(BINARY_PATH), ["--native-content-workbench", reportPath], {
      cwd: ROOT,
      env,
      stdio: ["ignore", "ignore", "pipe"]
    });
    app.stderr.on("data", (chunk) => {
      if (stderr.length < 64 * 1024) stderr += chunk.toString("utf8");
    });
    const windowId = await waitForWindow(app, probe.display, xauthority);
    await waitForJson(progressPath, (progress) => progress.ready === true, app, "ready progress");
    run("python3", [rootPath("scripts/x11-x-test-input.py"), windowId, "marker"], { env });
    await waitForJson(
      progressPath,
      (progress) => progress.commandMarkerObserved === true,
      app,
      "terminal marker"
    );
    const runReport = await waitForJson(reportPath, (report) => report.status === "passed", app, "run report");
    validateRun(runReport);
    const screenshot = rootPath(SCREENSHOT_PATH);
    mkdirSync(dirname(screenshot), { recursive: true });
    run("import", ["-window", windowId, screenshot], { env });
    const metrics = parseImageMetrics(
      run("identify", ["-format", "%w\t%h\t%k\t%[entropy]\t%[fx:mean]\t%[fx:standard_deviation]", screenshot])
    );
    assert(metrics.uniqueColors > 1 && metrics.entropy > 0 && metrics.standardDeviation > 0, "X11 screenshot is blank");
    run("python3", [rootPath("scripts/x11-x-test-input.py"), windowId, "close"], { env });
    const exit = await waitForExit(app, 10_000);
    assert(exit.code === 0 && exit.signal === null, "X11 app did not close cleanly");
    const evidence = {
      schemaVersion: "native-content-x11-linux-evidence.v1",
      status: "passed",
      capturedAt: new Date().toISOString(),
      source: sourceEvidence(),
      runner: {
        platform: process.platform,
        architecture: process.arch,
        kernelRelease: run("uname", ["-r"]).trim(),
        displayBackend: "x11-xorg",
        serverKind: probe.serverKind,
        syntheticDisplay: false,
        physicalXorgProcessObserved: true,
        independentXorgAuthorized: true,
        xwaylandDetected: false,
        physicalConnector: probe.physicalConnector,
        dri3Observed: true,
        xtestObserved: true,
        probedDisplayCount: 1
      },
      capture: { screenshotPath: SCREENSHOT_PATH, screenshotSha256: sha256(readFileSync(screenshot)), ...metrics },
      process: {
        processExited: true,
        appExitCode: exit.code,
        panicMentioned: /panicked at|thread '.+' panicked/i.test(stderr)
      },
      run: runReport,
      claims: [{ id: "x11_native_matrix", status: "passed", decisionBearing: true, blocker: null }],
      summary: { passedClaims: 1, blockedClaims: 0, x11NativeGateSatisfied: true },
      limitations: [
        "This proves one authorized independent Xorg terminal interaction and pixel capture on the named physical connector.",
        "Wayland, PDF, Office, and longer terminal stress use separate evidence protocols."
      ]
    };
    validateEvidence(evidence);
    mkdirSync(dirname(rootPath(EVIDENCE_PATH)), { recursive: true });
    writeFileSync(rootPath(EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
    verify();
  } finally {
    if (app && app.exitCode === null) {
      app.kill("SIGTERM");
      await sleep(250);
      if (app.exitCode === null) app.kill("SIGKILL");
    }
    rmSync(temporary, { recursive: true, force: true });
  }
}

async function capture() {
  assert(process.platform === "linux", "X11 capture requires Linux");
  for (const command of ["cargo", "git", "identify", "import", "ps", "python3", "rustc", "xdpyinfo", "xprop", "xrandr", "xwininfo"]) {
    const result = runResult("sh", ["-c", `command -v ${command} >/dev/null`]);
    assert(result.status === 0, `required command is missing: ${command}`);
  }
  const displays = discoverDisplays();
  const physical = displays.find(
    (display) =>
      display.authorized &&
      display.independentXorg &&
      display.synthetic === false &&
      display.physicalConnector
  );
  if (physical) await capturePassed(physical);
  else writeBlocked(displays, physicalXorgProcessObserved());
}

const mode = process.argv[2];
try {
  if (mode === "--write") await capture();
  else if (mode === "--self-test") selfTest();
  else if (mode === undefined) verify();
  else fail("usage: node scripts/capture-native-content-x11-linux.mjs [--write|--self-test]");
} catch (error) {
  console.error(error.message);
  process.exit(1);
}

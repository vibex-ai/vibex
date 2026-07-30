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
import { basename, dirname, join, resolve, sep } from "node:path";
import { tmpdir } from "node:os";
import process from "node:process";
import { clearInterval, clearTimeout, setInterval, setTimeout } from "node:timers";
import { fileURLToPath } from "node:url";
import { classifyGpuiEvidence } from "./evidence-applicability.mjs";
import { resolveGpuiSourceIdentities } from "./source-identities.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SOURCE_IDENTITIES = resolveGpuiSourceIdentities(ROOT);
const EVIDENCE_PATH = "docs/platform/evidence/acp-lifecycle-linux.json";
const SCREENSHOT_PATH = "docs/parity/screenshots/current/spikes/linux-wayland-acp-lifecycle.png";
const BINARY_PATH = "target/debug/vibex-desktop";
const WINDOW_IDENTITY = "dev.vibex.desktop.preview";
const VIBEX_BASELINE_COMMIT = "f1c624f115401d6160a771545fa1ec73128394b1";
const SOURCE_INPUT_ROOTS = [
  "Cargo.lock",
  "Cargo.toml",
  "rust-toolchain.toml",
  "apps/desktop/Cargo.toml",
  "apps/desktop/src",
  "crates/agent/Cargo.toml",
  "crates/agent/src",
  "crates/agent-acp/Cargo.toml",
  "crates/agent-acp/src",
  "crates/config-switch/Cargo.toml",
  "crates/config-switch/src",
  "crates/core/Cargo.toml",
  "crates/core/src",
  "scripts/capture-acp-lifecycle.mjs"
];
const EXPECTED_EVENT_KEYS = ["agent_message_delta", "reasoning"];
const PROMPT = "Reply with exactly: vibex-acp-ok. Do not run tools or edit files.";

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

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
    timeout: 30_000,
    ...options
  });
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

function processInfo(pid) {
  try {
    const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
    const commandEnd = stat.lastIndexOf(")");
    const fields = stat.slice(commandEnd + 2).trim().split(/\s+/);
    const args = readFileSync(`/proc/${pid}/cmdline`)
      .toString("utf8")
      .split("\0")
      .filter(Boolean);
    return {
      pid,
      parentPid: Number(fields[1]),
      startTime: fields[19],
      args
    };
  } catch {
    return null;
  }
}

function processTable() {
  return readdirSync("/proc")
    .filter((entry) => /^\d+$/.test(entry))
    .map((entry) => processInfo(Number(entry)))
    .filter(Boolean);
}

function processIdentity(processEntry) {
  return `${processEntry.pid}:${processEntry.startTime}`;
}

function isOpenCodeAcp(processEntry) {
  return (
    processEntry.args.some((argument) => basename(argument) === "opencode") &&
    processEntry.args.includes("acp")
  );
}

function descendantProcesses(rootPid, table) {
  const descendants = new Set([rootPid]);
  let changed = true;
  while (changed) {
    changed = false;
    for (const processEntry of table) {
      if (!descendants.has(processEntry.pid) && descendants.has(processEntry.parentPid)) {
        descendants.add(processEntry.pid);
        changed = true;
      }
    }
  }
  descendants.delete(rootPid);
  return table.filter((processEntry) => descendants.has(processEntry.pid));
}

function hyprlandJson(kind) {
  return JSON.parse(run("hyprctl", ["-j", kind]));
}

async function waitForClient(app) {
  for (let attempt = 0; attempt < 300; attempt += 1) {
    const client = hyprlandJson("clients").find((candidate) => candidate.pid === app.pid);
    if (client) return client;
    if (app.exitCode !== null) fail("GPUI lifecycle process exited before creating a window");
    await sleep(50);
  }
  fail("GPUI lifecycle window was not discovered by Hyprland");
}

async function waitForReport(path, app) {
  for (let attempt = 0; attempt < 1_200; attempt += 1) {
    if (existsSync(path)) return JSON.parse(readFileSync(path, "utf8"));
    if (app.exitCode !== null) fail("GPUI lifecycle process exited without writing its report");
    await sleep(100);
  }
  fail("GPUI lifecycle report exceeded its bounded timeout");
}

function waitForExit(app, timeoutMs) {
  if (app.exitCode !== null) return Promise.resolve({ code: app.exitCode, signal: app.signalCode });
  return new Promise((resolvePromise, rejectPromise) => {
    const timeout = setTimeout(() => rejectPromise(new Error("GPUI lifecycle process did not exit")), timeoutMs);
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
  assert(
    Object.values(metrics).every(Number.isFinite),
    "ImageMagick returned invalid screenshot metrics"
  );
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
    metrics.uniqueColors > 1 && metrics.entropy > 0 && metrics.standardDeviation > 0,
    "GPUI lifecycle screenshot is uniform"
  );
  return {
    screenshotPath: SCREENSHOT_PATH,
    screenshotSha256: sha256(readFileSync(screenshot)),
    ...metrics
  };
}

function exactKeys(value, expected, label) {
  const actual = Object.keys(value ?? {}).sort();
  const wanted = [...expected].sort();
  assert(JSON.stringify(actual) === JSON.stringify(wanted), `${label} keys are not exact`);
}

function validateLifecycle(lifecycle) {
  assert(lifecycle?.schemaVersion === "acp-lifecycle-run.v1", "lifecycle schema is invalid");
  assert(lifecycle.status === "passed" && lifecycle.failure === null, "lifecycle did not pass");
  assert(lifecycle.provider?.id === "opencode", "lifecycle provider is not OpenCode");
  assert(/^[0-9A-Za-z][0-9A-Za-z.+_-]{0,63}$/.test(lifecycle.provider.version), "provider version is not bounded");
  assert(lifecycle.provider.realProcess === true, "real provider process was not recorded");
  assert(lifecycle.provider.adapterBoundary === true, "AgentProvider boundary was not exercised");
  assert(lifecycle.provider.transport === "acp", "runtime transport is not ACP");
  assert(lifecycle.provider.rawOutputStored === false, "raw provider output was retained");
  assert(lifecycle.gpui?.tokioTaskOwned === true, "GPUI Tokio ownership was not recorded");
  assert(lifecycle.gpui.foregroundEntityUpdates > 0, "GPUI foreground Entity was not updated");
  assert(lifecycle.gpui.responseTextStored === false, "response text was retained");
  assert(lifecycle.session?.created === true, "ACP session was not created");
  assert(lifecycle.session.nativeIdPresent === true, "ACP native session identity was not observed");
  assert(lifecycle.session.completed === true, "ACP turn did not complete");
  assert(lifecycle.session.streamedEventCount > 0, "no streamed ACP events were observed");
  assert(lifecycle.session.streamedTextBytes > 0, "no streamed response bytes were observed");
  assert(
    Object.values(lifecycle.session.eventCounts).reduce((sum, count) => sum + count, 0) ===
      lifecycle.session.streamedEventCount,
    "streamed event count is inconsistent"
  );
  assert(
    Object.keys(lifecycle.session.eventCounts).every((key) => EXPECTED_EVENT_KEYS.includes(key)),
    "unexpected provider event category was retained"
  );
  assert(lifecycle.errorSurface?.exercised === true, "structured error path was not exercised");
  assert(
    lifecycle.errorSurface.structuredErrorCode === "acp_binary_missing",
    "structured error code is not deterministic"
  );
  assert(lifecycle.errorSurface.rawProviderErrorStored === false, "raw provider error was retained");
  assert(lifecycle.shutdown?.sessionClosed === true, "ACP session close did not complete");
  assert(lifecycle.shutdown.sweepCompleted === true, "lifecycle sweep did not complete");
  assert(Number.isInteger(lifecycle.shutdown.processesRemoved), "sweep process count is invalid");
  assert(lifecycle.shutdown.elapsedMs > 0 && lifecycle.shutdown.elapsedMs < 120_000, "shutdown was not bounded");
  assert(lifecycle.shutdown.bounded === true, "bounded shutdown flag is false");
  assert(lifecycle.shutdown.temporaryRootRemoved === true, "temporary runtime root remains");
  exactKeys(
    lifecycle.session,
    [
      "created",
      "nativeIdPresent",
      "completed",
      "streamedEventCount",
      "streamedTextBytes",
      "eventCounts"
    ],
    "lifecycle session"
  );
}

function validateEvidenceObject(evidence) {
  assert(evidence?.schemaVersion === "acp-lifecycle-evidence.v1", "evidence schema is invalid");
  assert(evidence.status === "passed", "ACP lifecycle evidence did not pass");
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
      "source input identity is stale"
    );
    assert(
      evidence.source.lockfileSha256 === sha256(readFileSync(rootPath("Cargo.lock"))),
      "lockfile identity is stale"
    );
  }
  assert(evidence.runner?.platform === "linux", "evidence is not from Linux");
  assert(evidence.runner.displayBackend === "wayland-hyprland", "evidence is not native Wayland");
  assert(evidence.runner.syntheticDisplay === false, "evidence used a synthetic display");
  assert(evidence.window?.identity === WINDOW_IDENTITY, "window identity is invalid");
  assert(evidence.window.discovered === true && evidence.window.xwayland === false, "native Wayland window was not proven");
  assert(evidence.window.width === 1200 && evidence.window.height === 780, "window geometry is not stable");
  const screenshot = rootPath(evidence.capture?.screenshotPath ?? "");
  assert(evidence.capture.screenshotPath === SCREENSHOT_PATH && existsSync(screenshot), "lifecycle screenshot is missing");
  assert(evidence.capture.screenshotSha256 === sha256(readFileSync(screenshot)), "lifecycle screenshot hash is stale");
  assert(
    evidence.capture.width > 0 &&
      evidence.capture.height > 0 &&
      evidence.capture.uniqueColors > 1 &&
      evidence.capture.entropy > 0 &&
      evidence.capture.standardDeviation > 0,
    "lifecycle screenshot metrics are not credible"
  );
  assert(evidence.process?.providerProcessObserved === true, "OpenCode ACP child process was not observed");
  assert(evidence.process.observedProviderProcessCount > 0, "observed provider process count is empty");
  assert(evidence.process.observedProviderProcessesExited === true, "observed provider process remains");
  assert(evidence.process.unexpectedProviderProcessesAfterExit === 0, "unexpected ACP process remains");
  assert(evidence.process.appExitCode === 0 && evidence.process.panicMentioned === false, "GPUI process exit was not clean");
  validateLifecycle(evidence.lifecycle);

  const serialized = JSON.stringify(evidence);
  for (const forbidden of [
    PROMPT,
    "vibex-acp-ok",
    ROOT,
    process.env.HOME,
    '"nativeSessionId"',
    '"workspaceRoot"',
    '"textDelta"',
    '"responseText"'
  ]) {
    if (forbidden) assert(!serialized.includes(forbidden), "evidence retained a forbidden sensitive field or value");
  }
  return applicability;
}

function verifyEvidence() {
  assert(existsSync(rootPath(EVIDENCE_PATH)), `${EVIDENCE_PATH} is missing`);
  const evidence = readJson(EVIDENCE_PATH);
  const applicability = validateEvidenceObject(evidence);
  console.log(
    `GPUI ACP lifecycle verified: ${evidence.lifecycle.session.streamedEventCount} events, ` +
      `${evidence.process.observedProviderProcessCount} provider process(es), bounded cleanup; ` +
      `applicability=${applicability}`
  );
}

function selfTest() {
  const evidence = readJson(EVIDENCE_PATH);
  validateEvidenceObject(evidence);
  for (const [label, mutate] of [
    ["retained response", (copy) => (copy.lifecycle.responseText = "sensitive")],
    ["missing adapter boundary", (copy) => (copy.lifecycle.provider.adapterBoundary = false)],
    ["unclosed session", (copy) => (copy.lifecycle.shutdown.sessionClosed = false)],
    ["remaining provider process", (copy) => (copy.process.unexpectedProviderProcessesAfterExit = 1)]
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
  console.log("GPUI ACP lifecycle negative-case self-test passed");
}

async function capture() {
  assert(process.platform === "linux", "physical lifecycle capture requires Linux");
  assert(process.env.XDG_SESSION_TYPE === "wayland", "physical lifecycle capture requires a Wayland session");
  for (const command of ["cargo", "grim", "hyprctl", "identify", "opencode", "rustc"]) {
    run("sh", ["-c", `command -v ${command} >/dev/null`]);
  }

  run("cargo", ["build", "-p", "vibex-desktop", "--locked"], { stdio: "inherit" });
  const binary = rootPath(BINARY_PATH);
  const temporaryRoot = mkdtempSync(join(tmpdir(), "vibex-acp-evidence-"));
  const reportPath = join(temporaryRoot, "lifecycle.json");
  const providerBaseline = new Set(processTable().filter(isOpenCodeAcp).map(processIdentity));
  const observedProviders = new Map();
  let stderr = "";
  let app;
  let observer;
  try {
    app = spawn(binary, ["--spike-acp-lifecycle", reportPath], {
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
    observer = setInterval(() => {
      for (const processEntry of descendantProcesses(app.pid, processTable()).filter(isOpenCodeAcp)) {
        observedProviders.set(processIdentity(processEntry), processEntry);
      }
    }, 25);

    const client = await waitForClient(app);
    const monitor = hyprlandJson("monitors").find((candidate) => candidate.id === client.monitor);
    assert(monitor, "window monitor was not found");
    const addressSelector = `address:${client.address}`;
    if (!client.floating) run("hyprctl", ["dispatch", "togglefloating", addressSelector]);
    run("hyprctl", ["dispatch", "resizewindowpixel", `exact 1200 780,${addressSelector}`]);
    run("hyprctl", [
      "dispatch",
      "movewindowpixel",
      `exact ${monitor.x + 100} ${monitor.y + 100},${addressSelector}`
    ]);
    await sleep(250);
    const lifecycle = await waitForReport(reportPath, app);
    const finalClient = hyprlandJson("clients").find((candidate) => candidate.address === client.address);
    assert(finalClient, "GPUI lifecycle window disappeared before capture");
    assert(finalClient.xwayland === false, "GPUI lifecycle window did not use native Wayland");
    const capture = captureWindow(finalClient);
    const exit = await waitForExit(app, 10_000);
    clearInterval(observer);
    observer = null;
    assert(exit.code === 0 && exit.signal === null, "GPUI lifecycle process did not exit cleanly");

    for (let attempt = 0; attempt < 50; attempt += 1) {
      const liveIdentities = new Set(processTable().map(processIdentity));
      if ([...observedProviders].every(([identity]) => !liveIdentities.has(identity))) break;
      await sleep(100);
    }
    const after = processTable();
    const liveIdentities = new Set(after.map(processIdentity));
    const observedExited = [...observedProviders].every(([identity]) => !liveIdentities.has(identity));
    const unexpectedAfter = after
      .filter(isOpenCodeAcp)
      .map(processIdentity)
      .filter((identity) => !providerBaseline.has(identity));
    assert(observedProviders.size > 0, "no OpenCode ACP descendant was observed");
    assert(observedExited, "an observed OpenCode ACP process remains after app exit");
    assert(unexpectedAfter.length === 0, "an unexpected OpenCode ACP process remains after app exit");

    const binaryBytes = readFileSync(binary);
    const evidence = {
      schemaVersion: "acp-lifecycle-evidence.v1",
      status: "passed",
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
      artifact: {
        path: BINARY_PATH,
        bytes: binaryBytes.length,
        sha256: sha256(binaryBytes)
      },
      runner: {
        platform: process.platform,
        architecture: process.arch,
        kernelRelease: run("uname", ["-r"]).trim(),
        displayBackend: "wayland-hyprland",
        compositor: "Hyprland",
        syntheticDisplay: false,
        monitor: {
          width: monitor.width,
          height: monitor.height,
          refreshRateHz: monitor.refreshRate,
          scaleFactor: monitor.scale,
          transform: monitor.transform
        }
      },
      window: {
        identity: WINDOW_IDENTITY,
        discovered: true,
        xwayland: false,
        width: finalClient.size[0],
        height: finalClient.size[1]
      },
      capture,
      process: {
        providerProcessObserved: true,
        observedProviderProcessCount: observedProviders.size,
        observedProviderProcessesExited: observedExited,
        unexpectedProviderProcessesAfterExit: unexpectedAfter.length,
        appExitCode: exit.code,
        panicMentioned: /panicked at|thread '.+' panicked/i.test(stderr)
      },
      lifecycle,
      limitations: [
        "This physical Linux spike exercises one authenticated OpenCode ACP turn, not every Agent or provider.",
        "The screenshot proves the final GPUI status surface only; response text is counted and discarded.",
        "macOS and Windows GUI behavior is outside this Linux evidence record."
      ]
    };
    validateEvidenceObject(evidence);
    mkdirSync(dirname(rootPath(EVIDENCE_PATH)), { recursive: true });
    writeFileSync(rootPath(EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
    verifyEvidence();
  } finally {
    if (observer) clearInterval(observer);
    if (app && app.exitCode === null) {
      app.kill("SIGTERM");
      await sleep(250);
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
  fail("usage: node scripts/capture-acp-lifecycle.mjs [--write|--self-test]");
}

import { createHash } from "node:crypto";
import console from "node:console";
import {
  accessSync,
  constants,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  statSync,
  writeFileSync
} from "node:fs";
import { delimiter, dirname, join, resolve } from "node:path";
import process from "node:process";
import { spawn, spawnSync } from "node:child_process";
import { setTimeout } from "node:timers";
import { fileURLToPath } from "node:url";
import { classifyGpuiEvidence } from "./evidence-applicability.mjs";
import { resolveGpuiSourceIdentities } from "./source-identities.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SOURCE_IDENTITIES = resolveGpuiSourceIdentities(ROOT);
const EVIDENCE_PATH = "docs/platform/evidence/linux-x11-first-frame-xvfb.json";
const SCREENSHOT_PATH = "docs/parity/screenshots/current/spikes/linux-x11-xvfb-black.png";
const NATIVE_EVIDENCE_PATH = "docs/platform/evidence/linux-native-first-frame.json";
const HOSTED_POLICY_PATH = "docs/platform/hosted-runner-policy.json";
const NATIVE_SCREENSHOTS = {
  linux_x11: "docs/parity/screenshots/current/spikes/linux-x11-native.png",
  linux_wayland: "docs/parity/screenshots/current/spikes/linux-wayland-native.png"
};
const WINDOW_IDENTITY = "dev.vibex.desktop.preview";
const SOURCE_INPUT_ROOTS = ["Cargo.lock", "Cargo.toml", "apps/desktop/Cargo.toml", "apps/desktop/src"];

function fail(message) {
  throw new Error(message);
}

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function repositoryFile(path) {
  return readFileSync(join(ROOT, path));
}

function sourceFilesFor(path) {
  const absolute = join(ROOT, path);
  if (!existsSync(absolute)) fail(`source input is missing: ${path}`);
  if (statSync(absolute).isFile()) return [path];
  return readdirSync(absolute, { withFileTypes: true })
    .sort((left, right) => left.name.localeCompare(right.name))
    .flatMap((entry) => sourceFilesFor(`${path}/${entry.name}`));
}

function sourceTreeSha256() {
  const hash = createHash("sha256");
  for (const path of SOURCE_INPUT_ROOTS.flatMap(sourceFilesFor)) {
    hash.update(path);
    hash.update("\0");
    hash.update(repositoryFile(path));
    hash.update("\0");
  }
  return hash.digest("hex");
}

function commandPath(command) {
  for (const path of (process.env.PATH ?? "").split(delimiter)) {
    const candidate = join(path, command);
    try {
      accessSync(candidate, constants.X_OK);
      return candidate;
    } catch {
      // Continue searching PATH.
    }
  }
  return null;
}

function requireCommand(command) {
  const path = commandPath(command);
  if (!path) fail(`required capture command is unavailable: ${command}`);
  return path;
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
    fail(`${command} ${args.join(" ")} failed:\n${result.stderr ?? result.stdout ?? ""}`);
  }
  return result.stdout ?? "";
}

function sleep(milliseconds) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}

function freeDisplay() {
  for (let display = 97; display <= 127; display += 1) {
    if (
      !existsSync(`/tmp/.X${display}-lock`) &&
      !existsSync(`/tmp/.X11-unix/X${display}`)
    ) {
      return display;
    }
  }
  fail("no free X11 display was found in :97-:127");
}

async function waitForDisplay(display, xvfb) {
  for (let attempt = 0; attempt < 80; attempt += 1) {
    if (xvfb.exitCode !== null) fail(`Xvfb exited before display ${display} was ready`);
    const probe = spawnSync("xdpyinfo", ["-display", display], {
      encoding: "utf8",
      timeout: 2_000
    });
    if (probe.status === 0) return;
    await sleep(100);
  }
  fail(`Xvfb display ${display} did not become ready`);
}

async function waitForWindow(display, app) {
  for (let attempt = 0; attempt < 120; attempt += 1) {
    if (app.exitCode !== null) fail("GPUI process exited before creating its X11 window");
    const tree = spawnSync("xwininfo", ["-display", display, "-root", "-tree"], {
      encoding: "utf8",
      timeout: 2_000
    });
    if (tree.status === 0) {
      const line = tree.stdout
        .split("\n")
        .find((candidate) => candidate.includes(WINDOW_IDENTITY));
      const match = line?.match(/^\s*(0x[0-9a-f]+)\s+/i);
      if (match) return match[1];
    }
    await sleep(100);
  }
  fail(`GPUI window ${WINDOW_IDENTITY} was not found on ${display}`);
}

function terminate(processHandle, signal = "SIGTERM") {
  if (processHandle && processHandle.exitCode === null) {
    processHandle.kill(signal);
  }
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
  if (Object.values(metrics).some((value) => !Number.isFinite(value))) {
    fail(`ImageMagick returned invalid metrics: ${output}`);
  }
  return metrics;
}

function readJson(path) {
  return JSON.parse(repositoryFile(path).toString("utf8"));
}

function hostedPixelDisposition() {
  const policy = readJson(HOSTED_POLICY_PATH);
  const claim = policy.requiredSkippedClaims?.find(
    (candidate) => candidate.id === "real_window_screenshots_native_pixels"
  );
  if (
    policy.schemaVersion !== "hosted-runner-policy.v1" ||
    JSON.stringify(policy.requiredTargets) !== JSON.stringify(["macos", "windows"]) ||
    claim?.status !== "skipped_by_product_decision" ||
    claim.decisionImpact !== false ||
    claim.decisionDenominator !== "excluded" ||
    claim.notEvidenceOfParity !== true
  ) {
    fail(`${HOSTED_POLICY_PATH} does not contain the approved hosted pixel exclusion`);
  }
  return {
    policyPath: HOSTED_POLICY_PATH,
    targets: policy.requiredTargets,
    claimId: claim.id,
    status: claim.status,
    decisionImpact: claim.decisionImpact,
    decisionDenominator: claim.decisionDenominator,
    notEvidenceOfParity: claim.notEvidenceOfParity,
    reason: claim.reason
  };
}

function verifyNativeEvidence() {
  if (!existsSync(join(ROOT, NATIVE_EVIDENCE_PATH))) {
    fail(`${NATIVE_EVIDENCE_PATH} is missing`);
  }
  const evidence = readJson(NATIVE_EVIDENCE_PATH);
  if (evidence.schemaVersion !== "linux-native-frame-evidence.v2") {
    fail(`${NATIVE_EVIDENCE_PATH} has an unsupported schemaVersion`);
  }
  const applicability = classifyGpuiEvidence(ROOT, NATIVE_EVIDENCE_PATH, evidence.source);
  if (applicability === "current") {
    if (
      evidence.source.zedRevision !== SOURCE_IDENTITIES.zedRevision ||
      evidence.source.gpuiComponentRevision !== SOURCE_IDENTITIES.gpuiComponentRevision ||
      evidence.source.lockfileSha256 !== sha256(repositoryFile("Cargo.lock")) ||
      evidence.source.gpuiShellTreeSha256 !== sourceTreeSha256() ||
      evidence.source.hostedRunnerPolicySha256 !== sha256(repositoryFile(HOSTED_POLICY_PATH))
    ) {
      fail(`${NATIVE_EVIDENCE_PATH} source identity is stale`);
    }
  }
  const expectedTargets = ["linux_x11", "linux_wayland"];
  if (JSON.stringify(evidence.requiredTargets) !== JSON.stringify(expectedTargets)) {
    fail(`${NATIVE_EVIDENCE_PATH} requiredTargets are incomplete`);
  }
  if (evidence.targets.length !== expectedTargets.length) {
    fail(`${NATIVE_EVIDENCE_PATH} target count is invalid`);
  }
  for (const [index, id] of expectedTargets.entries()) {
    const target = evidence.targets[index];
    if (target.id !== id) fail(`${NATIVE_EVIDENCE_PATH} target order is invalid`);
    const expectedBackend = id === "linux_x11" ? "x11-xwayland-hyprland" : "wayland-hyprland";
    if (
      target.status !== "captured_physical" ||
      target.runner.syntheticDisplay !== false ||
      target.runner.displayBackend !== expectedBackend ||
      target.window.xwayland !== (id === "linux_x11") ||
      target.nativePixelsVerified !== true ||
      target.blocker !== null ||
      target.processStderrSignals?.panicMentioned !== false
    ) {
      fail(`${NATIVE_EVIDENCE_PATH} ${id} does not contain physical native pixels`);
    }
    if (
      target.capture.width !== 1200 ||
      target.capture.height !== 780 ||
      target.capture.uniqueColors <= 1 ||
      target.capture.entropy <= 0 ||
      target.capture.standardDeviation <= 0
    ) {
      fail(`${NATIVE_EVIDENCE_PATH} ${id} image metrics are not credible`);
    }
    if (target.capture.screenshotPath !== NATIVE_SCREENSHOTS[id]) {
      fail(`${NATIVE_EVIDENCE_PATH} ${id} screenshot path is invalid`);
    }
    const screenshot = join(ROOT, target.capture.screenshotPath);
    if (!existsSync(screenshot)) fail(`${target.capture.screenshotPath} is missing`);
    if (target.capture.screenshotSha256 !== sha256(readFileSync(screenshot))) {
      fail(`${target.capture.screenshotPath} hash does not match the evidence record`);
    }
  }
  if (
    evidence.linuxNativePixelGateSatisfied !== true ||
    JSON.stringify(evidence.hostedPixelDisposition) !== JSON.stringify(hostedPixelDisposition())
  ) {
    fail(`${NATIVE_EVIDENCE_PATH} aggregate gates contradict target evidence`);
  }
  console.log(
    `GPUI physical Linux first-frame evidence verified: X11 and Wayland captured; ` +
      `applicability=${applicability}`
  );
}

function verifyEvidence() {
  if (!existsSync(join(ROOT, EVIDENCE_PATH))) fail(`${EVIDENCE_PATH} is missing`);
  const evidence = JSON.parse(repositoryFile(EVIDENCE_PATH).toString("utf8"));
  if (evidence.schemaVersion !== "native-frame-evidence.v1") {
    fail(`${EVIDENCE_PATH} has an unsupported schemaVersion`);
  }
  if (!new Set(["blocked", "captured_synthetic"]).has(evidence.status)) {
    fail(`${EVIDENCE_PATH} has an invalid status`);
  }
  const applicability = classifyGpuiEvidence(ROOT, EVIDENCE_PATH, evidence.source);
  if (applicability === "current") {
    if (
      evidence.source.zedRevision !== SOURCE_IDENTITIES.zedRevision ||
      evidence.source.gpuiComponentRevision !== SOURCE_IDENTITIES.gpuiComponentRevision
    ) {
      fail(`${EVIDENCE_PATH} source revisions are stale`);
    }
    if (evidence.source.lockfileSha256 !== sha256(repositoryFile("Cargo.lock"))) {
      fail(`${EVIDENCE_PATH} lockfile identity is stale`);
    }
    if (evidence.source.gpuiShellTreeSha256 !== sourceTreeSha256()) {
      fail(`${EVIDENCE_PATH} GPUI shell identity is stale`);
    }
  }
  const screenshot = join(ROOT, evidence.capture.screenshotPath);
  if (!existsSync(screenshot)) fail(`${evidence.capture.screenshotPath} is missing`);
  if (evidence.capture.screenshotSha256 !== sha256(readFileSync(screenshot))) {
    fail(`${evidence.capture.screenshotPath} hash does not match the evidence record`);
  }
  if (evidence.nativePixelsVerified !== (evidence.status === "captured_synthetic")) {
    fail(`${EVIDENCE_PATH} status and nativePixelsVerified disagree`);
  }
  console.log(
    `GPUI X11 evidence verified: ${evidence.status}, ` +
      `${evidence.capture.width}x${evidence.capture.height}, entropy ${evidence.capture.entropy}; ` +
      `applicability=${applicability}`
  );
  verifyNativeEvidence();
}

function hyprlandJson(args) {
  return JSON.parse(run("hyprctl", [...args, "-j"]));
}

async function waitForHyprlandClient(app) {
  for (let attempt = 0; attempt < 120; attempt += 1) {
    if (app.exitCode !== null) fail("GPUI process exited before creating its native window");
    const client = hyprlandJson(["clients"]).find((candidate) => candidate.pid === app.pid);
    if (client) return client;
    await sleep(100);
  }
  fail(`GPUI process ${app.pid} did not create a Hyprland client`);
}

function sanitizedVulkanDevice() {
  const summary = run("vulkaninfo", ["--summary"]);
  const value = (name) => summary.match(new RegExp(`^\\s*${name}\\s*=\\s*(.+)$`, "m"))?.[1]?.trim() ?? "unreported";
  return {
    deviceName: value("deviceName"),
    driverName: value("driverName"),
    driverInfo: value("driverInfo")
  };
}

async function capturePhysicalTarget(id, buildPath, monitor, vulkanDevice) {
  const isX11 = id === "linux_x11";
  const env = { ...process.env, XDG_SESSION_TYPE: isX11 ? "x11" : "wayland" };
  if (isX11) delete env.WAYLAND_DISPLAY;
  let app;
  let stderr = "";
  try {
    app = spawn(buildPath, [], {
      cwd: ROOT,
      env,
      stdio: ["ignore", "ignore", "pipe"]
    });
    app.stderr.on("data", (chunk) => {
      if (stderr.length < 64 * 1024) stderr += chunk.toString("utf8");
    });
    let client = await waitForHyprlandClient(app);
    const addressSelector = `address:${client.address}`;
    if (!client.floating) run("hyprctl", ["dispatch", "togglefloating", addressSelector]);
    run("hyprctl", ["dispatch", "resizewindowpixel", `exact 1200 780,${addressSelector}`]);
    run("hyprctl", [
      "dispatch",
      "movewindowpixel",
      `exact ${monitor.x + 100} ${monitor.y + 100},${addressSelector}`
    ]);
    await sleep(2_000);
    client = hyprlandJson(["clients"]).find((candidate) => candidate.address === client.address);
    if (!client) fail(`${id} GPUI client disappeared before capture`);
    if (client.size[0] !== 1200 || client.size[1] !== 780) {
      fail(`${id} GPUI client has unexpected size ${client.size.join("x")}`);
    }
    if (client.xwayland !== isX11) {
      fail(`${id} GPUI client selected the wrong display backend`);
    }
    const screenshotPath = NATIVE_SCREENSHOTS[id];
    const screenshot = join(ROOT, screenshotPath);
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
    if (metrics.uniqueColors <= 1 || metrics.entropy <= 0 || metrics.standardDeviation <= 0) {
      fail(`${id} physical capture is uniform`);
    }
    return {
      id,
      status: "captured_physical",
      runner: {
        platform: process.platform,
        architecture: process.arch,
        displayBackend: isX11 ? "x11-xwayland-hyprland" : "wayland-hyprland",
        compositor: "Hyprland",
        syntheticDisplay: false,
        gpu: vulkanDevice,
        monitor: {
          width: monitor.width,
          height: monitor.height,
          physicalWidthMm: monitor.physicalWidth,
          physicalHeightMm: monitor.physicalHeight,
          refreshRateHz: monitor.refreshRate,
          scaleFactor: monitor.scale,
          transform: monitor.transform
        }
      },
      window: {
        identity: WINDOW_IDENTITY,
        discovered: true,
        borderlessRequested: true,
        xwayland: isX11,
        width: client.size[0],
        height: client.size[1]
      },
      capture: {
        screenshotPath,
        screenshotSha256: sha256(readFileSync(screenshot)),
        ...metrics
      },
      nativePixelsVerified: true,
      blocker: null,
      limitations: [
        "This first-frame capture proves application pixels only, not keyboard, pointer, clipboard, accessibility, or IME behavior.",
        "One Linux workstation cannot substitute for macOS, Windows, another compositor, packaging, or multi-monitor DPI transition evidence."
      ],
      processStderrSignals: {
        dri3Mentioned: /DRI3/i.test(stderr),
        panicMentioned: /panicked at|thread '.+' panicked/i.test(stderr)
      }
    };
  } finally {
    terminate(app);
    await sleep(300);
    terminate(app, "SIGKILL");
  }
}

async function captureNativeLinux() {
  for (const command of ["grim", "hyprctl", "identify", "vulkaninfo"]) requireCommand(command);
  if (process.platform !== "linux" || process.env.XDG_SESSION_TYPE !== "wayland") {
    fail("physical Linux capture requires a Linux Wayland desktop session");
  }
  const build = spawnSync("cargo", ["build", "-p", "vibex-desktop", "--locked"], {
    cwd: ROOT,
    stdio: "inherit"
  });
  if (build.error) fail(`cargo build failed to start: ${build.error.message}`);
  if (build.status !== 0) fail(`cargo build failed with exit code ${build.status ?? 1}`);

  const monitors = hyprlandJson(["monitors"]);
  const monitor = monitors.find((candidate) => candidate.focused) ?? monitors[0];
  if (!monitor || monitor.width < 1400 || monitor.height < 980 || monitor.scale !== 1) {
    fail("physical capture requires a scale-1 monitor large enough for an isolated 1200x780 window");
  }
  const buildPath = join(ROOT, "target/debug/vibex-desktop");
  const vulkanDevice = sanitizedVulkanDevice();
  const targets = [];
  targets.push(await capturePhysicalTarget("linux_x11", buildPath, monitor, vulkanDevice));
  targets.push(await capturePhysicalTarget("linux_wayland", buildPath, monitor, vulkanDevice));
  const evidence = {
    schemaVersion: "linux-native-frame-evidence.v2",
    capturedAt: new Date().toISOString(),
    source: {
      vibexBaselineCommit: "f1c624f115401d6160a771545fa1ec73128394b1",
      captureParentCommit: run("git", ["rev-parse", "HEAD"]).trim(),
      dependencySourcePolicy: SOURCE_IDENTITIES.dependencySourcePolicy,
      zedRevision: SOURCE_IDENTITIES.zedRevision,
      gpuiComponentRevision: SOURCE_IDENTITIES.gpuiComponentRevision,
      lockfileSha256: sha256(repositoryFile("Cargo.lock")),
      gpuiShellTreeSha256: sourceTreeSha256(),
      hostedRunnerPolicySha256: sha256(repositoryFile(HOSTED_POLICY_PATH))
    },
    requiredTargets: ["linux_x11", "linux_wayland"],
    targets,
    linuxNativePixelGateSatisfied: true,
    hostedPixelDisposition: hostedPixelDisposition()
  };
  writeFileSync(join(ROOT, NATIVE_EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
  verifyNativeEvidence();
}

async function capture() {
  for (const command of ["Xvfb", "identify", "import", "xdpyinfo", "xwininfo"]) {
    requireCommand(command);
  }
  const build = spawnSync("cargo", ["build", "-p", "vibex-desktop", "--locked"], {
    cwd: ROOT,
    stdio: "inherit"
  });
  if (build.error) fail(`cargo build failed to start: ${build.error.message}`);
  if (build.status !== 0) fail(`cargo build failed with exit code ${build.status ?? 1}`);

  const displayNumber = freeDisplay();
  const display = `:${displayNumber}`;
  let xvfb;
  let app;
  let appStderr = "";
  try {
    xvfb = spawn("Xvfb", [display, "-screen", "0", "1200x780x24", "-nolisten", "tcp"], {
      cwd: ROOT,
      stdio: ["ignore", "ignore", "pipe"]
    });
    await waitForDisplay(display, xvfb);
    const appEnvironment = {
      ...process.env,
      DISPLAY: display,
      XDG_SESSION_TYPE: "x11"
    };
    delete appEnvironment.WAYLAND_DISPLAY;
    app = spawn(join(ROOT, "target/debug/vibex-desktop"), [], {
      cwd: ROOT,
      env: appEnvironment,
      stdio: ["ignore", "ignore", "pipe"]
    });
    app.stderr.on("data", (chunk) => {
      if (appStderr.length < 64 * 1024) appStderr += chunk.toString("utf8");
    });
    const windowId = await waitForWindow(display, app);
    await sleep(1_500);

    const screenshot = join(ROOT, SCREENSHOT_PATH);
    mkdirSync(dirname(screenshot), { recursive: true });
    run("import", ["-display", display, "-window", windowId, screenshot]);
    const metrics = parseImageMetrics(
      run("identify", [
        "-format",
        "%w\t%h\t%k\t%[entropy]\t%[fx:mean]\t%[fx:standard_deviation]",
        screenshot
      ])
    );
    const nativePixelsVerified =
      metrics.uniqueColors > 1 && metrics.entropy > 0 && metrics.standardDeviation > 0;
    const evidence = {
      schemaVersion: "native-frame-evidence.v1",
      status: nativePixelsVerified ? "captured_synthetic" : "blocked",
      capturedAt: new Date().toISOString(),
      source: {
        vibexBaselineCommit: "f1c624f115401d6160a771545fa1ec73128394b1",
        dependencySourcePolicy: SOURCE_IDENTITIES.dependencySourcePolicy,
        zedRevision: SOURCE_IDENTITIES.zedRevision,
        gpuiComponentRevision: SOURCE_IDENTITIES.gpuiComponentRevision,
        lockfileSha256: sha256(repositoryFile("Cargo.lock")),
        gpuiShellTreeSha256: sourceTreeSha256()
      },
      runner: {
        platform: process.platform,
        architecture: process.arch,
        displayBackend: "x11-xvfb",
        displaySize: "1200x780x24",
        scaleFactor: 1,
        syntheticDisplay: true
      },
      window: {
        identity: WINDOW_IDENTITY,
        discovered: true,
        borderlessRequested: true
      },
      capture: {
        screenshotPath: SCREENSHOT_PATH,
        screenshotSha256: sha256(readFileSync(screenshot)),
        ...metrics
      },
      nativePixelsVerified,
      blocker: nativePixelsVerified
        ? null
        : {
            code: "xvfb-frame-is-uniform",
            summary: "The GPUI X11 window exists, but its captured client area is one uniform color.",
            dri3MentionedByProcess: /DRI3/i.test(appStderr),
            disposition: "Keep Linux X11 native pixels blocked until a DRI3/Vulkan-capable native runner captures non-uniform application pixels."
          },
      limitations: [
        "Xvfb is a synthetic X11 display and cannot prove physical-display DPI behavior.",
        "This capture does not prove pointer, keyboard, clipboard, focus, accessibility, or IME behavior.",
        "A discovered native window is not equivalent to rendered application pixels."
      ]
    };
    mkdirSync(dirname(join(ROOT, EVIDENCE_PATH)), { recursive: true });
    writeFileSync(join(ROOT, EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
  } finally {
    terminate(app);
    terminate(xvfb);
    await sleep(200);
    terminate(app, "SIGKILL");
    terminate(xvfb, "SIGKILL");
  }
  verifyEvidence();
}

try {
  const args = process.argv.slice(2);
  if (args.length === 0) {
    verifyEvidence();
  } else if (args.length === 1 && args[0] === "--write") {
    await capture();
  } else if (args.length === 1 && args[0] === "--write-linux-native") {
    await captureNativeLinux();
  } else {
    fail("usage: node scripts/capture-x11-first-frame.mjs [--write|--write-linux-native]");
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}

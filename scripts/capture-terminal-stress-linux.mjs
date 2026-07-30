import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
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
import { fileURLToPath } from "node:url";
import { classifyGpuiEvidence } from "./evidence-applicability.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const EVIDENCE_PATH = "docs/platform/evidence/terminal-stress-linux.json";
const SOURCE_INPUT_ROOTS = [
  "Cargo.lock",
  "Cargo.toml",
  "crates/content/Cargo.toml",
  "crates/content/src/terminal.rs",
  "crates/content/src/terminal_stress.rs",
  "crates/content/src/bin/vibex-terminal-stress.rs",
  "crates/terminal/Cargo.toml",
  "crates/terminal/src/emulator.rs",
  "crates/terminal/src/lib.rs",
  "scripts/capture-terminal-stress-linux.mjs"
];
const CLAIM_IDS = [
  "terminal_10_mib_no_loss",
  "terminal_120_fps_burst",
  "terminal_10000_line_scrollback",
  "terminal_repeated_lifecycle",
  "terminal_resize_reflow",
  "terminal_sequence_rebuild",
  "terminal_five_minute_activity",
  "terminal_fd_child_rss_bounded",
  "terminal_stress_privacy"
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

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
    timeout: 4 * 60 * 60 * 1000,
    ...options
  });
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

function validateRun(report, requireFiveMinutes = true) {
  assert(report?.schemaVersion === "terminal-stress-linux-run.v1", "terminal stress run schema drifted");
  assert(report.status === "passed", "terminal stress run did not pass");
  assert(report.platform === "linux", "terminal stress run is not Linux");
  assert(
    report.throughput?.fixtureBytes === 10 * 1024 * 1024 &&
      report.throughput.fixtureSha256 === report.throughput.observedSha256 &&
      report.throughput.dataLossObserved === false &&
      report.throughput.rawDroppedChunks === 0,
    "10 MiB throughput contract failed"
  );
  assert(
    report.burst?.requestedFrames === 120 &&
      report.burst.observedFrames === 120 &&
      report.burst.sourceFramesPerSecond >= 100 &&
      report.burst.sourceFramesPerSecond <= 130 &&
      report.burst.dataLossObserved === false &&
      report.burst.maxSnapshotMs <= 50 &&
      report.burst.renderUpdates > 0 &&
      report.burst.renderUpdates <= 120 &&
      report.burst.fullRepaints <= 2 &&
      report.burst.partialRepaints > 0 &&
      report.burst.changedRows <= (report.burst.renderUpdates + 1) * 24 &&
      report.burst.maxParseFrameMs <= 50 &&
      report.burst.boundedRepaint === true,
    "120 FPS burst contract failed"
  );
  assert(
    report.scrollback?.requestedLines === 10_000 &&
      report.scrollback.observedHistoryLines >= 9_000 &&
      report.scrollback.observedHistoryLines <= 10_000 &&
      report.scrollback.modelResidentBytes <= report.scrollback.modelBudgetBytes &&
      report.scrollback.modelBudgetBytes === 128 * 1024 * 1024 &&
      report.scrollback.bounded === true,
    "10,000-line scrollback contract failed"
  );
  assert(
    report.lifecycle?.createCount === 101 &&
      report.lifecycle.restoreCount === 100 &&
      report.lifecycle.killCount === 101 &&
      report.lifecycle.allSessionsClosed === true &&
      report.lifecycle.statusesValid === true,
    "repeated lifecycle contract failed"
  );
  assert(
    report.resize?.requestedRows === report.resize.observedRows &&
      report.resize.requestedColumns === report.resize.observedColumns &&
      report.resize.reflowMarkerObserved === true,
    "resize/reflow contract failed"
  );
  assert(
    report.sequenceRebuild?.gapInjected === true &&
      report.sequenceRebuild.emptyIncrementalSnapshot === true &&
      report.sequenceRebuild.incrementalBytes > 0 &&
      report.sequenceRebuild.incrementalBytes < report.sequenceRebuild.fullRetainedBytes &&
      report.sequenceRebuild.incrementalSnapshotBounded === true &&
      report.sequenceRebuild.rebuildObserved === true &&
      report.sequenceRebuild.dataLossObserved === false,
    "sequence rebuild contract failed"
  );
  assert(
    report.soak?.completedRequestedDuration === true &&
      report.soak.sequenceGaps === 0 &&
      report.soak.rawDroppedChunks === 0 &&
      report.soak.snapshots >= report.soak.activityTicks &&
      report.soak.renderUpdates === report.soak.activityTicks &&
      report.soak.fullRepaints <= 2 &&
      (report.soak.requestedSeconds === 0 || report.soak.partialRepaints > 0),
    "activity soak contract failed"
  );
  if (requireFiveMinutes) {
    assert(
      report.soakRequestedSeconds >= 300 &&
        report.soakObservedSeconds >= 300 &&
        report.soak.requestedSeconds >= 300 &&
        report.soak.observedSeconds >= 300 &&
        report.soak.activityTicks >= 1_000,
      "committed evidence did not execute five minutes of activity"
    );
  }
  assert(
    report.resources?.procfsAvailable === true &&
      report.resources.fdLeakObserved === false &&
      report.resources.childLeakObserved === false &&
      report.resources.finalFdCount <= report.resources.baselineFdCount + 2 &&
      report.resources.finalChildCount <= report.resources.baselineChildCount &&
      report.resources.rssGrowthBytes <= report.resources.rssBudgetBytes &&
      report.resources.rssBudgetBytes === 64 * 1024 * 1024,
    "terminal process resource contract failed"
  );
  assert(
    Object.values(report.privacy ?? {}).every((value) => value === false),
    "terminal stress report retained private output"
  );
  const serialized = JSON.stringify(report);
  for (const forbidden of ["VIBEX_TP_BEGIN", "VIBEX_SOAK_TICK", ROOT, process.env.HOME]) {
    if (forbidden) assert(!serialized.includes(forbidden), "terminal stress report retained sensitive text");
  }
}

function validateEvidence(evidence) {
  assert(evidence?.schemaVersion === "terminal-stress-linux-evidence.v1", "terminal stress evidence schema drifted");
  assert(evidence.status === "passed", "terminal stress evidence did not pass");
  const applicability = classifyGpuiEvidence(ROOT, EVIDENCE_PATH, evidence.source);
  assert(
    JSON.stringify(evidence.source?.sourceInputRoots) === JSON.stringify(SOURCE_INPUT_ROOTS) &&
      (applicability !== "current" ||
        evidence.source.sourceInputTreeSha256 === sourceInputTreeSha256()),
    "terminal stress source identity drifted"
  );
  assert(
    evidence.runner?.platform === "linux" &&
      evidence.runner.syntheticPty === false &&
      evidence.runner.procfsObserved === true,
    "terminal stress runner identity is invalid"
  );
  validateRun(evidence.run, true);
  assert(
    JSON.stringify(evidence.claims?.map((claim) => claim.id)) === JSON.stringify(CLAIM_IDS) &&
      evidence.claims.every((claim) => claim.status === "passed" && claim.decisionBearing === true),
    "terminal stress claim matrix drifted"
  );
  assert(
    evidence.summary?.passedClaims === CLAIM_IDS.length &&
      evidence.summary.failedClaims === 0 &&
      evidence.summary.terminalStressGateSatisfied === true,
    "terminal stress summary drifted"
  );
  const serialized = JSON.stringify(evidence);
  for (const forbidden of ["VIBEX_TP_BEGIN", "VIBEX_SOAK_TICK", ROOT, process.env.HOME]) {
    if (forbidden) assert(!serialized.includes(forbidden), "terminal stress evidence retained sensitive text");
  }
  return applicability;
}

function verify() {
  assert(existsSync(rootPath(EVIDENCE_PATH)), `${EVIDENCE_PATH} is missing`);
  const evidence = readJson(EVIDENCE_PATH);
  const applicability = validateEvidence(evidence);
  console.log(
    `GPUI terminal stress verified: ${evidence.run.soakObservedSeconds}s, ` +
      `${evidence.run.soak.activityTicks} activity ticks; applicability=${applicability}`
  );
}

function selfTest() {
  const evidence = readJson(EVIDENCE_PATH);
  validateEvidence(evidence);
  const mutations = [
    ["short soak", (copy) => (copy.run.soakObservedSeconds = 299)],
    ["throughput hash mismatch", (copy) => (copy.run.throughput.observedSha256 = "0".repeat(64))],
    ["burst frame loss", (copy) => (copy.run.burst.observedFrames = 119)],
    ["unbounded repaint", (copy) => (copy.run.burst.boundedRepaint = false)],
    ["missed soak frame", (copy) => (copy.run.soak.renderUpdates -= 1)],
    ["unbounded soak repaint", (copy) => (copy.run.soak.fullRepaints = 3)],
    ["unbounded raw clone", (copy) => (copy.run.sequenceRebuild.incrementalSnapshotBounded = false)],
    ["child leak", (copy) => (copy.run.resources.childLeakObserved = true)],
    ["private output", (copy) => (copy.rawTerminalOutput = "VIBEX_SOAK_TICK")],
    ["source drift", (copy) => (copy.source.sourceInputTreeSha256 = "0".repeat(64))]
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
  console.log("GPUI terminal stress negative-case self-test passed");
}

function capture() {
  assert(process.platform === "linux", "terminal stress capture requires Linux");
  const temporary = mkdtempSync(join(tmpdir(), "vibex-terminal-stress-evidence-"));
  try {
    const reportPath = join(temporary, "run.json");
    run("cargo", [
      "run",
      "-p",
      "vibex-content",
      "--bin",
      "vibex-terminal-stress",
      "--locked",
      "--",
      "--soak-seconds",
      "300",
      "--output",
      reportPath
    ], { stdio: "inherit" });
    const report = JSON.parse(readFileSync(reportPath, "utf8"));
    validateRun(report, true);
    const evidence = {
      schemaVersion: "terminal-stress-linux-evidence.v1",
      status: "passed",
      capturedAt: new Date().toISOString(),
      source: {
        captureParentCommit: run("git", ["rev-parse", "HEAD"]).trim(),
        rustToolchain: run("rustc", ["--version"]).trim(),
        lockfileSha256: sha256(readFileSync(rootPath("Cargo.lock"))),
        sourceInputRoots: SOURCE_INPUT_ROOTS,
        sourceInputTreeSha256: sourceInputTreeSha256()
      },
      runner: {
        platform: process.platform,
        architecture: process.arch,
        kernelRelease: run("uname", ["-r"]).trim(),
        syntheticPty: false,
        ptyBackend: "portable-pty-openpty",
        procfsObserved: report.resources.procfsAvailable
      },
      run: report,
      claims: CLAIM_IDS.map((id) => ({ id, status: "passed", decisionBearing: true })),
      summary: {
        passedClaims: CLAIM_IDS.length,
        failedClaims: 0,
        terminalStressGateSatisfied: true
      },
      limitations: [
        "This is Linux PTY, parser, lifecycle, and process-tree evidence; macOS and Windows physical results are not inferred.",
        "The report retains counts, hashes, timings, and resource metrics only; raw terminal output is discarded."
      ]
    };
    validateEvidence(evidence);
    mkdirSync(dirname(rootPath(EVIDENCE_PATH)), { recursive: true });
    writeFileSync(rootPath(EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
    verify();
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

const mode = process.argv[2];
try {
  if (mode === "--write") capture();
  else if (mode === "--self-test") selfTest();
  else if (mode === undefined) verify();
  else fail("usage: node scripts/capture-terminal-stress-linux.mjs [--write|--self-test]");
} catch (error) {
  console.error(error.message);
  process.exit(1);
}

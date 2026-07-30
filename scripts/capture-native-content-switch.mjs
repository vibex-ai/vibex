import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import console from "node:console";
import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve, sep } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { classifyGpuiEvidence } from "./evidence-applicability.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const EVIDENCE_PATH = "docs/platform/evidence/native-content-switch.json";
const BINARY_PATH = "target/debug/vibex-desktop";
const SOURCE_INPUTS = [
  "Cargo.lock",
  "Cargo.toml",
  "apps/desktop/Cargo.toml",
  "apps/desktop/src/main.rs",
  "apps/desktop/src/native_content.rs",
  "crates/content/Cargo.toml",
  "crates/content/src/lifecycle.rs",
  "crates/content/src/web.rs",
  "scripts/capture-native-content-switch.mjs"
];

function fail(message) {
  throw new Error(message);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function rootPath(path) {
  const absolute = resolve(ROOT, path);
  if (absolute !== ROOT && !absolute.startsWith(`${ROOT}${sep}`)) {
    fail(`path escapes repository: ${path}`);
  }
  return absolute;
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function identity(path) {
  const bytes = readFileSync(rootPath(path));
  return { path, bytes: bytes.length, sha256: sha256(bytes) };
}

function readJson(path) {
  return JSON.parse(readFileSync(rootPath(path), "utf8"));
}

function run(command, args, timeout = 300_000) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024,
    timeout
  });
  if (result.error) fail(`${command} failed to start: ${result.error.message}`);
  if (result.status !== 0) {
    fail(
      `${command} ${args.join(" ")} failed with status ${result.status ?? "unknown"}:\n${result.stderr || result.stdout || ""}`
    );
  }
}

function validateRun(report) {
  assert(
    report?.schemaVersion === "native-content-switch-contract.v1",
    "Native Content switch schema drifted"
  );
  assert(report.status === "passed", "Native Content switch contract did not pass");
  assert(report.targetsActivated === 5, "Native Content target coverage drifted");
  assert(report.rapidSwitches === 100, "Native Content rapid-switch count drifted");
  assert(report.staleCallbacksIgnored === 3, "stale callbacks were not fenced");
  assert(report.closeCallbacksIgnored === 3, "close callbacks were not fenced");
  assert(
    report.overlayHidden === true &&
      report.focusReturnPendingObserved === true &&
      report.focusRestored === true,
    "overlay/focus-return lifecycle failed"
  );
  assert(report.latestBoundsPreserved === true, "stale bounds replaced current bounds");
  assert(
    report.closedSurfaceRemainedClosed === true,
    "same-generation callback reopened a closed surface"
  );
  assert(report.crashRecoveryPassed === true, "content lifecycle crash recovery failed");
  assert(
    report.visibleSurfaceCount === 1 &&
      report.focusedSurfaceCount === 1 &&
      report.finalActiveKind === "office",
    "final active/focus ownership drifted"
  );
  assert(
    report.webZeroAllocation === true && report.rightRailWebZeroAllocation === true,
    "Web switching allocated an unsupported native surface"
  );
  assert(
    Object.values(report.privacy ?? {}).every((value) => value === false),
    "Native Content switch report leaked target or content data"
  );
  assert(
    report.limitations?.some((entry) => entry.includes("code-workbench task")),
    "Native Content switch report omitted its reducer ownership boundary"
  );
}

function validateEvidence(evidence) {
  assert(
    evidence?.schemaVersion === "native-content-switch-evidence.v1",
    "Native Content switch evidence schema drifted"
  );
  assert(evidence.status === "passed", "Native Content switch evidence did not pass");
  const capturedLock = evidence.sourceInputs?.find((input) => input.path === "Cargo.lock");
  const applicability = classifyGpuiEvidence(ROOT, EVIDENCE_PATH, {
    cargoLockSha256: capturedLock?.sha256
  });
  if (applicability === "current") {
    assert(
      JSON.stringify(evidence.sourceInputs) === JSON.stringify(SOURCE_INPUTS.map(identity)),
      "Native Content switch source identity drifted; recapture is required"
    );
  }
  validateRun(evidence.run);
  return applicability;
}

function capture() {
  run("cargo", ["build", "-p", "vibex-desktop", "--locked"]);
  const temporary = mkdtempSync(join(tmpdir(), "vibex-native-content-switch-"));
  try {
    const output = join(temporary, "switch.json");
    run(rootPath(BINARY_PATH), ["--native-content-switch-contract", output]);
    const runReport = JSON.parse(readFileSync(output, "utf8"));
    validateRun(runReport);
    const evidence = {
      schemaVersion: "native-content-switch-evidence.v1",
      status: "passed",
      capturedAt: new Date().toISOString(),
      sourceInputs: SOURCE_INPUTS.map(identity),
      run: runReport
    };
    validateEvidence(evidence);
    writeFileSync(rootPath(EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

function selfTest(evidence) {
  const mutations = [
    (copy) => (copy.run.staleCallbacksIgnored = 2),
    (copy) => (copy.run.closeCallbacksIgnored = 2),
    (copy) => (copy.run.focusRestored = false),
    (copy) => (copy.run.visibleSurfaceCount = 2),
    (copy) => (copy.run.webZeroAllocation = false),
    (copy) => (copy.run.privacy.urlStored = true)
  ];
  for (const mutate of mutations) {
    const copy = structuredClone(evidence);
    mutate(copy);
    let rejected = false;
    try {
      validateEvidence(copy);
    } catch {
      rejected = true;
    }
    assert(rejected, "Native Content switch negative self-test accepted invalid evidence");
  }
}

try {
  if (process.argv.includes("--write")) capture();
  const evidence = readJson(EVIDENCE_PATH);
  const applicability = validateEvidence(evidence);
  if (process.argv.includes("--self-test")) selfTest(evidence);
  const size = statSync(rootPath(EVIDENCE_PATH)).size;
  console.log(`GPUI Native Content switch evidence verified (${size} bytes); applicability=${applicability}`);
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}

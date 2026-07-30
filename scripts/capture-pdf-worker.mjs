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
const EVIDENCE_PATH = "docs/platform/evidence/pdf-worker-linux.json";
const LIBRARY_PATH = "target/native/pdfium/linux-x86_64/lib/libpdfium.so";
const FIXTURE_PATH = "docs/platform/fixtures/pdf-feasibility.pdf";
const REVIEW_PATH = "docs/licenses/pdfium-7881-review.json";
const BINARY_PATH = "target/debug/vibex-desktop";
const SOURCE_INPUTS = [
  "Cargo.lock",
  "Cargo.toml",
  "apps/desktop/Cargo.toml",
  "apps/desktop/src/main.rs",
  "apps/desktop/src/pdf_worker.rs",
  "crates/content/Cargo.toml",
  "crates/content/src/lifecycle.rs",
  "crates/content/src/pdf.rs",
  FIXTURE_PATH,
  REVIEW_PATH,
  "scripts/capture-pdf-worker.mjs",
  "scripts/prepare-pdfium-runtime.mjs"
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
      `${command} ${args.join(" ")} failed with status ${result.status ?? "unknown"}:\n${result.stderr ?? ""}${result.stdout ?? ""}`
    );
  }
}

function validateRun(report) {
  assert(
    report?.schemaVersion === "pdf-worker-supervisor-run.v1",
    "PDF worker supervisor schema drifted"
  );
  assert(report.status === "passed", "PDF worker supervisor did not pass");
  assert(report.normalRenderPassed === true, "PDF worker normal render failed");
  assert(
    report.crashDetected === true && report.crashErrorCode === "pdf_worker_crashed",
    "PDF worker crash was not contained"
  );
  assert(
    report.timeoutDetected === true && report.timeoutErrorCode === "pdf_worker_timeout",
    "PDF worker hard timeout was not enforced"
  );
  assert(
    report.recoveryAfterCrashPassed === true && report.recoveryAfterTimeoutPassed === true,
    "PDF worker did not recover after a failed child"
  );
  assert(
    report.childrenStarted === 5 && report.childrenReaped === 5,
    "PDF worker supervisor leaked or failed to start a child"
  );
  assert(
    Object.values(report.privacy ?? {}).every((value) => value === false),
    "PDF worker supervisor retained a path, page content, or raw stderr"
  );
}

function validateSoak(report) {
  assert(report?.schemaVersion === "pdf-worker-soak-run.v1", "PDF worker soak schema drifted");
  assert(report.status === "passed", "PDF worker soak did not pass");
  assert(
    report.iterations === 49 &&
      report.normalRequests === 37 &&
      report.cancellations === 4 &&
      report.crashes === 4 &&
      report.timeouts === 4 &&
      report.recoveriesPassed === 12 &&
      report.unexpectedFailures === 0,
    "PDF worker soak request matrix drifted"
  );
  assert(
    report.childrenStarted === report.iterations && report.childrenReaped === report.iterations,
    "PDF worker soak leaked or failed to start a child"
  );
  assert(
    report.parentRssGrowthBytes <= report.rssGrowthBudgetBytes &&
      report.rssGrowthBudgetBytes === 64 * 1024 * 1024 &&
      report.peakParentRssBytes >= report.initialParentRssBytes &&
      report.peakParentRssBytes >= report.finalParentRssBytes,
    "PDF worker soak exceeded its parent RSS budget"
  );
  assert(report.finalOpenFds <= report.initialOpenFds + 1, "PDF worker soak leaked file descriptors");
  assert(
    report.finalDirectChildren === report.initialDirectChildren,
    "PDF worker soak retained a direct child process"
  );
  assert(
    report.finalWorkerTempDirectories === report.initialWorkerTempDirectories,
    "PDF worker soak retained temporary directories"
  );
  assert(
    report.currentResources?.residentItems === 0 &&
      report.currentResources.residentBytes === 0 &&
      report.currentResources.budgetItems === 4 &&
      report.currentResources.budgetBytes === 48 * 1024 * 1024,
    "PDF worker soak retained current native resources"
  );
  assert(
    report.lastWorkerResources?.residentItems > 0 &&
      report.lastWorkerResources.residentItems <= report.lastWorkerResources.budgetItems &&
      report.lastWorkerResources.residentBytes > 0 &&
      report.lastWorkerResources.residentBytes <= report.lastWorkerResources.budgetBytes,
    "PDF worker soak last-worker metrics are invalid"
  );
  assert(
    Number.isInteger(report.longestRequestMs) && report.longestRequestMs > 0 && report.longestRequestMs < 15_000,
    "PDF worker soak request duration is invalid"
  );
  assert(
    Object.values(report.privacy ?? {}).every((value) => value === false),
    "PDF worker soak retained a path, page content, raw stderr, or temporary path"
  );
}

function validateEvidence(evidence) {
  assert(
    evidence?.schemaVersion === "pdf-worker-evidence.v1",
    "PDF worker evidence schema drifted"
  );
  assert(evidence.status === "passed", "PDF worker evidence did not pass");
  const capturedLock = evidence.sourceInputs?.find((input) => input.path === "Cargo.lock");
  const applicability = classifyGpuiEvidence(ROOT, EVIDENCE_PATH, {
    cargoLockSha256: capturedLock?.sha256
  });
  if (applicability === "current") {
    assert(
      JSON.stringify(evidence.sourceInputs) === JSON.stringify(SOURCE_INPUTS.map(identity)),
      "PDF worker source identity drifted; recapture is required"
    );
  }
  const review = readJson(REVIEW_PATH);
  const linux = review.archives?.find((archive) => archive.target === "linux-x86_64");
  assert(linux, "reviewed Linux PDFium input is missing");
  assert(
    evidence.nativeRuntime?.target === "linux-x86_64" &&
      evidence.nativeRuntime.libraryBytes === linux.libraryBytes &&
      evidence.nativeRuntime.librarySha256 === linux.librarySha256,
    "PDF worker runtime identity drifted"
  );
  validateRun(evidence.run);
  validateSoak(evidence.soak);
  return applicability;
}

function capture() {
  assert(
    process.platform === "linux" && process.arch === "x64",
    "PDF worker capture requires Linux x86_64"
  );
  run("node", ["scripts/prepare-pdfium-runtime.mjs", "--offline"]);
  run("cargo", ["build", "-p", "vibex-desktop", "--locked"]);
  const temporary = mkdtempSync(join(tmpdir(), "vibex-pdf-worker-"));
  try {
    const output = join(temporary, "supervisor.json");
    const soakOutput = join(temporary, "soak.json");
    run(rootPath(BINARY_PATH), [
      "--native-content-pdf-worker-supervisor",
      rootPath(LIBRARY_PATH),
      rootPath(FIXTURE_PATH),
      output
    ]);
    const runReport = JSON.parse(readFileSync(output, "utf8"));
    validateRun(runReport);
    run(rootPath(BINARY_PATH), [
      "--native-content-pdf-worker-soak",
      rootPath(LIBRARY_PATH),
      rootPath(FIXTURE_PATH),
      soakOutput
    ]);
    const soakReport = JSON.parse(readFileSync(soakOutput, "utf8"));
    validateSoak(soakReport);
    const library = readFileSync(rootPath(LIBRARY_PATH));
    const evidence = {
      schemaVersion: "pdf-worker-evidence.v1",
      status: "passed",
      capturedAt: new Date().toISOString(),
      sourceInputs: SOURCE_INPUTS.map(identity),
      nativeRuntime: {
        target: "linux-x86_64",
        libraryBytes: library.length,
        librarySha256: sha256(library)
      },
      run: runReport,
      soak: soakReport
    };
    validateEvidence(evidence);
    writeFileSync(rootPath(EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

function selfTest(evidence) {
  const mutations = [
    (copy) => (copy.run.crashDetected = false),
    (copy) => (copy.run.timeoutDetected = false),
    (copy) => (copy.run.childrenReaped = 4),
    (copy) => (copy.run.recoveryAfterCrashPassed = false),
    (copy) => (copy.run.recoveryAfterTimeoutPassed = false),
    (copy) => (copy.run.privacy.documentPathStored = true),
    (copy) => (copy.soak.childrenReaped = 48),
    (copy) => (copy.soak.parentRssGrowthBytes = copy.soak.rssGrowthBudgetBytes + 1),
    (copy) => (copy.soak.finalOpenFds = copy.soak.initialOpenFds + 2),
    (copy) => (copy.soak.finalWorkerTempDirectories = copy.soak.initialWorkerTempDirectories + 1),
    (copy) => (copy.soak.privacy.temporaryPathStored = true)
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
    assert(rejected, "PDF worker negative self-test accepted invalid evidence");
  }
}

try {
  if (process.argv.includes("--write")) capture();
  const evidence = readJson(EVIDENCE_PATH);
  const applicability = validateEvidence(evidence);
  if (process.argv.includes("--self-test")) selfTest(evidence);
  const size = statSync(rootPath(EVIDENCE_PATH)).size;
  console.log(`GPUI PDF worker evidence verified (${size} bytes); applicability=${applicability}`);
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}

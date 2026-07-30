import { createHash } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import console from "node:console";
import {
  closeSync,
  existsSync,
  ftruncateSync,
  mkdtempSync,
  openSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, resolve, sep } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { ENCRYPTED_PDF_FIXTURE_PATH } from "./pdf-encrypted-fixture-contract.mjs";
import {
  EXTREME_PAGE_PDF_FIXTURE_PATH,
  TOO_MANY_PAGES_PDF_FIXTURE_PATH
} from "./pdf-large-fixture-contract.mjs";
import { classifyGpuiEvidence } from "./evidence-applicability.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const EVIDENCE_PATH = "docs/platform/evidence/pdf-surface-linux.json";
const LIBRARY_PATH = "target/native/pdfium/linux-x86_64/lib/libpdfium.so";
const FIXTURE_PATH = "docs/platform/fixtures/pdf-feasibility.pdf";
const ENCRYPTED_FIXTURE_PATH = ENCRYPTED_PDF_FIXTURE_PATH;
const TOO_MANY_PAGES_FIXTURE_PATH = TOO_MANY_PAGES_PDF_FIXTURE_PATH;
const EXTREME_PAGE_FIXTURE_PATH = EXTREME_PAGE_PDF_FIXTURE_PATH;
const OVERSIZED_SOURCE_BYTES = 256 * 1024 * 1024 + 1;
const REVIEW_PATH = "docs/licenses/pdfium-7881-review.json";
const BINARY_PATH = "target/debug/vibex-desktop";
const SOURCE_INPUTS = [
  "Cargo.lock",
  "Cargo.toml",
  "apps/desktop/Cargo.toml",
  "apps/desktop/src/main.rs",
  "apps/desktop/src/native_content.rs",
  "apps/desktop/src/pdf_surface.rs",
  "apps/desktop/src/pdf_worker.rs",
  "crates/content/src/lifecycle.rs",
  "crates/content/src/pdf.rs",
  FIXTURE_PATH,
  ENCRYPTED_FIXTURE_PATH,
  TOO_MANY_PAGES_FIXTURE_PATH,
  EXTREME_PAGE_FIXTURE_PATH,
  REVIEW_PATH,
  "scripts/capture-pdf-surface.mjs",
  "scripts/generate-pdf-encrypted-fixture.mjs",
  "scripts/pdf-encrypted-fixture-contract.mjs",
  "scripts/generate-pdf-large-fixtures.mjs",
  "scripts/pdf-large-fixture-contract.mjs",
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

function validateMetrics(metrics, itemLimit, byteLimit, label) {
  assert(metrics?.budgetItems === itemLimit, `${label} item budget drifted`);
  assert(metrics.budgetBytes === byteLimit, `${label} byte budget drifted`);
  assert(metrics.residentItems > 0 && metrics.residentItems <= itemLimit, `${label} item count is invalid`);
  assert(metrics.residentBytes > 0 && metrics.residentBytes <= byteLimit, `${label} byte count is invalid`);
}

function validateReleasedMetrics(metrics, itemLimit, byteLimit, label) {
  assert(metrics?.budgetItems === itemLimit, `${label} item budget drifted`);
  assert(metrics.budgetBytes === byteLimit, `${label} byte budget drifted`);
  assert(metrics.residentItems === 0, `${label} retained items after failure`);
  assert(metrics.residentBytes === 0, `${label} retained bytes after failure`);
}

function validateWorkerProcesses(processes, label) {
  assert(processes?.currentProcesses === 0, `${label} retained an active worker`);
  assert(
    processes.childrenStarted === 1 && processes.childrenReaped === 1,
    `${label} did not start and reap exactly one worker`
  );
  assert(
    processes.cleanExits === 1 &&
      processes.cancellations === 0 &&
      processes.timeouts === 0 &&
      processes.crashes === 0 &&
      processes.protocolFailures === 0 &&
      processes.lastDisposition === "clean_exit",
    `${label} worker disposition drifted`
  );
}

function validateRun(report) {
  assert(report?.schemaVersion === "pdf-surface-run.v1", "PDF surface run schema drifted");
  assert(report.status === "ready", "PDF surface did not reach ready state");
  assert(report.pageCount === 12 && report.currentPage === 0, "PDF surface page state drifted");
  assert(
    Number.isInteger(report.targetWidth) && report.targetWidth >= 64 && report.targetWidth <= 2048,
    "PDF surface target width is outside the UI bound"
  );
  assert(
    JSON.stringify(report.renderedPageIndexes) === JSON.stringify([0, 1]),
    "PDF surface current-page overscan drifted"
  );
  assert(report.zoomMode === "Fit width", "PDF surface initial zoom mode drifted");
  assert(
    Object.values(report.controls ?? {}).every((value) => value === true),
    "PDF surface controls are incomplete"
  );
  validateReleasedMetrics(report.resources, 4, 48 * 1024 * 1024, "current worker cache");
  validateMetrics(report.lastWorkerResources, 4, 48 * 1024 * 1024, "last worker peak cache");
  validateWorkerProcesses(report.workerProcesses, "ready PDF surface");
  validateMetrics(report.uiImages, 3, 72 * 1024 * 1024, "GPUI image cache");
  assert(report.error === null, "ready PDF surface unexpectedly reported an error");
  assert(
    Object.values(report.privacy ?? {}).every((value) => value === false),
    "PDF surface report leaked document identity or content"
  );
  assert(
    report.limitations?.some((entry) => entry.includes("not native pixels or physical input")),
    "PDF surface run omitted its physical-evidence limitation"
  );
}

function validateErrorRun(report, expectedCode, label) {
  assert(report?.schemaVersion === "pdf-surface-run.v1", `${label} schema drifted`);
  assert(report.status === "error", `${label} did not reach error state`);
  assert(
    report.pageCount === 0 &&
      report.currentPage === 0 &&
      JSON.stringify(report.renderedPageIndexes) === JSON.stringify([]),
    `${label} retained page state`
  );
  assert(
    report.error?.code === expectedCode &&
      report.error.retryAvailable === true &&
      report.error.explicitSystemOpenAvailable === true,
    `${label} recovery contract failed`
  );
  assert(
    Object.values(report.controls ?? {}).every((value) => value === true),
    `${label} controls are incomplete`
  );
  validateReleasedMetrics(report.resources, 4, 48 * 1024 * 1024, `${label} controller cache`);
  validateReleasedMetrics(
    report.lastWorkerResources,
    4,
    48 * 1024 * 1024,
    `${label} last worker cache`
  );
  validateWorkerProcesses(report.workerProcesses, label);
  validateReleasedMetrics(report.uiImages, 3, 72 * 1024 * 1024, `${label} GPUI image cache`);
  assert(
    Object.values(report.privacy ?? {}).every((value) => value === false),
    `${label} report leaked document identity or content`
  );
  assert(
    report.limitations?.some((entry) => entry.includes("not native pixels or physical input")),
    `${label} omitted its physical-evidence limitation`
  );
}

function validateEvidence(evidence) {
  assert(evidence?.schemaVersion === "pdf-surface-evidence.v1", "PDF surface evidence schema drifted");
  assert(evidence.status === "ready_not_physical", "PDF surface evidence status drifted");
  const capturedLock = evidence.sourceInputs?.find((input) => input.path === "Cargo.lock");
  const applicability = classifyGpuiEvidence(ROOT, EVIDENCE_PATH, {
    cargoLockSha256: capturedLock?.sha256
  });
  if (applicability === "current") {
    assert(
      JSON.stringify(evidence.sourceInputs) === JSON.stringify(SOURCE_INPUTS.map(identity)),
      "PDF surface source identity drifted; recapture is required"
    );
  }
  const review = readJson(REVIEW_PATH);
  const linux = review.archives?.find((archive) => archive.target === "linux-x86_64");
  assert(linux, "reviewed Linux PDFium input is missing");
  assert(
    evidence.nativeRuntime?.target === "linux-x86_64" &&
      evidence.nativeRuntime.libraryBytes === linux.libraryBytes &&
      evidence.nativeRuntime.librarySha256 === linux.librarySha256,
    "PDF surface runtime identity drifted"
  );
  assert(
    evidence.physical?.nativePixelsClaimed === false &&
      evidence.physical.inputClaimed === false &&
      evidence.physical.screenshot === null,
    "PDF surface evidence made an unsupported physical claim"
  );
  validateRun(evidence.run);
  validateErrorRun(evidence.encryptedErrorRun, "pdf_password_required", "encrypted PDF surface");
  validateErrorRun(
    evidence.largeInputRuns?.oversizedSource,
    "pdf_source_size_invalid",
    "oversized-source PDF surface"
  );
  validateErrorRun(
    evidence.largeInputRuns?.tooManyPages,
    "pdf_page_count_unsupported",
    "too-many-pages PDF surface"
  );
  validateErrorRun(
    evidence.largeInputRuns?.extremePage,
    "pdf_page_exceeds_cache_budget",
    "extreme-page PDF surface"
  );
  return applicability;
}

const wait = (milliseconds) => new Promise((resolveWait) => setTimeout(resolveWait, milliseconds));

async function captureSurfaceRun(fixturePath, output, label) {
  const documentPath = isAbsolute(fixturePath) ? fixturePath : rootPath(fixturePath);
  const child = spawn(
    rootPath(BINARY_PATH),
    [
      "--native-content-pdf-workbench",
      rootPath(LIBRARY_PATH),
      documentPath,
      output
    ],
    { cwd: ROOT, stdio: ["ignore", "pipe", "pipe"] }
  );
  let stderr = "";
  child.stderr.on("data", (chunk) => {
    stderr += chunk.toString();
  });
  try {
    for (let attempt = 0; attempt < 150 && !existsSync(output); attempt += 1) {
      if (child.exitCode !== null) fail(`PDF surface exited before ${label}: ${stderr}`);
      await wait(100);
    }
    assert(existsSync(output) && statSync(output).size > 0, `PDF surface ${label} report timed out`);
    return JSON.parse(readFileSync(output, "utf8"));
  } finally {
    child.kill("SIGTERM");
    await Promise.race([new Promise((resolveExit) => child.once("exit", resolveExit)), wait(2_000)]);
    if (child.exitCode === null) child.kill("SIGKILL");
  }
}

async function capture() {
  assert(process.platform === "linux" && process.arch === "x64", "PDF surface capture requires Linux x86_64");
  run("node", ["scripts/prepare-pdfium-runtime.mjs", "--offline"]);
  run("node", ["scripts/generate-pdf-encrypted-fixture.mjs"]);
  run("node", ["scripts/generate-pdf-large-fixtures.mjs"]);
  run("cargo", ["build", "-p", "vibex-desktop", "--locked"]);
  const temporary = mkdtempSync(join(tmpdir(), "vibex-pdf-surface-"));
  try {
    const oversizedSource = join(temporary, "oversized-source.pdf");
    const oversizedFd = openSync(oversizedSource, "w");
    try {
      ftruncateSync(oversizedFd, OVERSIZED_SOURCE_BYTES);
    } finally {
      closeSync(oversizedFd);
    }
    const report = await captureSurfaceRun(FIXTURE_PATH, join(temporary, "ready.json"), "ready");
    validateRun(report);
    const encryptedErrorRun = await captureSurfaceRun(
      ENCRYPTED_FIXTURE_PATH,
      join(temporary, "encrypted-error.json"),
      "encrypted error"
    );
    validateErrorRun(encryptedErrorRun, "pdf_password_required", "encrypted PDF surface");
    const largeInputRuns = {
      oversizedSource: await captureSurfaceRun(
        oversizedSource,
        join(temporary, "oversized-error.json"),
        "oversized-source error"
      ),
      tooManyPages: await captureSurfaceRun(
        TOO_MANY_PAGES_FIXTURE_PATH,
        join(temporary, "too-many-pages-error.json"),
        "too-many-pages error"
      ),
      extremePage: await captureSurfaceRun(
        EXTREME_PAGE_FIXTURE_PATH,
        join(temporary, "extreme-page-error.json"),
        "extreme-page error"
      )
    };
    validateErrorRun(
      largeInputRuns.oversizedSource,
      "pdf_source_size_invalid",
      "oversized-source PDF surface"
    );
    validateErrorRun(
      largeInputRuns.tooManyPages,
      "pdf_page_count_unsupported",
      "too-many-pages PDF surface"
    );
    validateErrorRun(
      largeInputRuns.extremePage,
      "pdf_page_exceeds_cache_budget",
      "extreme-page PDF surface"
    );
    const library = readFileSync(rootPath(LIBRARY_PATH));
    const evidence = {
      schemaVersion: "pdf-surface-evidence.v1",
      status: "ready_not_physical",
      capturedAt: new Date().toISOString(),
      sourceInputs: SOURCE_INPUTS.map(identity),
      nativeRuntime: {
        target: "linux-x86_64",
        libraryBytes: library.length,
        librarySha256: sha256(library)
      },
      physical: {
        nativePixelsClaimed: false,
        inputClaimed: false,
        screenshot: null,
        followUp: "Run the PDF interaction protocol on an active physical Linux output."
      },
      run: report,
      encryptedErrorRun,
      largeInputRuns
    };
    validateEvidence(evidence);
    writeFileSync(rootPath(EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

function selfTest(evidence) {
  const mutations = [
    (copy) => (copy.physical.nativePixelsClaimed = true),
    (copy) => (copy.encryptedErrorRun.error.code = "pdf_document_corrupt"),
    (copy) => (copy.encryptedErrorRun.resources.residentBytes = 4),
    (copy) => (copy.run.workerProcesses.childrenReaped = 0),
    (copy) => (copy.run.resources.residentItems = 1),
    (copy) => (copy.largeInputRuns.extremePage.error.code = "pdf_page_render_failed"),
    (copy) => (copy.largeInputRuns.tooManyPages.uiImages.residentItems = 1)
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
    assert(rejected, "PDF surface negative self-test accepted invalid evidence");
  }
}

try {
  if (process.argv.includes("--write")) await capture();
  const evidence = readJson(EVIDENCE_PATH);
  const applicability = validateEvidence(evidence);
  if (process.argv.includes("--self-test")) selfTest(evidence);
  console.log(
    "GPUI PDF surface evidence verified: ready, encrypted, and large-input states passed; " +
      `pixels/input remain unclaimed; applicability=${applicability}`
  );
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}

import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import console from "node:console";
import {
  closeSync,
  ftruncateSync,
  mkdtempSync,
  openSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve, sep } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import {
  ENCRYPTED_PDF_FIXTURE_PAGE_COUNT,
  ENCRYPTED_PDF_FIXTURE_PASSWORD,
  ENCRYPTED_PDF_FIXTURE_PATH
} from "./pdf-encrypted-fixture-contract.mjs";
import {
  EXTREME_PAGE_PDF_FIXTURE_PATH,
  TOO_MANY_PAGES_PDF_FIXTURE_PATH,
  TOO_MANY_PAGES_PDF_PAGE_COUNT
} from "./pdf-large-fixture-contract.mjs";
import { classifyGpuiEvidence } from "./evidence-applicability.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const EVIDENCE_PATH = "docs/platform/evidence/pdf-controller-linux.json";
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
  "apps/desktop/src/pdf_controller.rs",
  "crates/content/Cargo.toml",
  "crates/content/src/lifecycle.rs",
  "crates/content/src/pdf.rs",
  FIXTURE_PATH,
  ENCRYPTED_FIXTURE_PATH,
  TOO_MANY_PAGES_FIXTURE_PATH,
  EXTREME_PAGE_FIXTURE_PATH,
  REVIEW_PATH,
  "scripts/capture-pdf-controller.mjs",
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

function fileIdentity(path) {
  const bytes = readFileSync(rootPath(path));
  return { path, bytes: bytes.length, sha256: sha256(bytes) };
}

function readJson(path) {
  return JSON.parse(readFileSync(rootPath(path), "utf8"));
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024,
    timeout: 300_000,
    ...options
  });
  if (result.error) fail(`${command} failed to start: ${result.error.message}`);
  if (result.status !== 0) {
    fail(
      `${command} ${args.join(" ")} failed with status ${result.status ?? "unknown"}:\n${result.stderr ?? ""}${result.stdout ?? ""}`
    );
  }
}

function validateRender(render, expectedWidth) {
  assert(render?.pageIndex === 0, "PDF controller render page index drifted");
  assert(render.width === expectedWidth, `PDF controller render width ${expectedWidth} drifted`);
  assert(Number.isInteger(render.height) && render.height > 0, "PDF controller render height is invalid");
  assert(render.rgbaBytes === render.width * render.height * 4, "PDF controller RGBA size is invalid");
  assert(/^[a-f0-9]{64}$/.test(render.rgbaSha256), "PDF controller RGBA hash is invalid");
  assert(render.sampledUniqueColors >= 16, "PDF controller render lacks credible color diversity");
}

function validateRun(report) {
  assert(report?.schemaVersion === "vibex-pdf-controller-run.v1", "PDF controller schema drifted");
  assert(report.status === "passed", "PDF controller run did not pass");
  assert(report.platform === "linux" && report.architecture === "x86_64", "PDF controller platform drifted");
  assert(
    report.engine?.backend === "pdfium-render" &&
      report.engine.wrapperVersion === "0.9.3" &&
      report.engine.pdfiumVersion === "7881" &&
      report.engine.binding === "explicit-dynamic-library",
    "PDF controller engine identity drifted"
  );
  const fixture = readFileSync(rootPath(FIXTURE_PATH));
  assert(report.fixture?.bytes === fixture.length, "PDF controller fixture size drifted");
  assert(report.fixture.sha256 === sha256(fixture), "PDF controller fixture hash drifted");
  assert(report.fixture.pageCount === 12, "PDF controller fixture page count drifted");
  const encryptedFixture = readFileSync(rootPath(ENCRYPTED_FIXTURE_PATH));
  assert(
    report.encryptedFixture?.bytes === encryptedFixture.length &&
      report.encryptedFixture.sha256 === sha256(encryptedFixture) &&
      report.encryptedFixture.pageCount === ENCRYPTED_PDF_FIXTURE_PAGE_COUNT &&
      report.encryptedFixture.missingPasswordErrorCode === "pdf_password_required" &&
      report.encryptedFixture.incorrectPasswordErrorCode === "pdf_password_required" &&
      report.encryptedFixture.failedOpenClearedDocument === true &&
      report.encryptedFixture.failedOpenClearedCache === true &&
      report.encryptedFixture.correctPasswordOpened === true &&
      report.encryptedFixture.correctPasswordRendered === true &&
      report.encryptedFixture.loadFailuresAfterPasswordAttempts === 4,
    "PDF controller encrypted/password fixture contract failed"
  );
  const tooManyPagesFixture = readFileSync(rootPath(TOO_MANY_PAGES_FIXTURE_PATH));
  const extremePageFixture = readFileSync(rootPath(EXTREME_PAGE_FIXTURE_PATH));
  assert(
    report.largeInputs?.oversizedSourceBytes === OVERSIZED_SOURCE_BYTES &&
      report.largeInputs.oversizedSourceErrorCode === "pdf_source_size_invalid" &&
      report.largeInputs.tooManyPagesFixture?.bytes === tooManyPagesFixture.length &&
      report.largeInputs.tooManyPagesFixture.sha256 === sha256(tooManyPagesFixture) &&
      report.largeInputs.tooManyPagesFixture.pageCount === TOO_MANY_PAGES_PDF_PAGE_COUNT &&
      report.largeInputs.tooManyPagesErrorCode === "pdf_page_count_unsupported" &&
      report.largeInputs.tooManyPagesClearedDocument === true &&
      report.largeInputs.tooManyPagesClearedCache === true &&
      report.largeInputs.extremePageFixture?.bytes === extremePageFixture.length &&
      report.largeInputs.extremePageFixture.sha256 === sha256(extremePageFixture) &&
      report.largeInputs.extremePageFixture.pageCount === 1 &&
      report.largeInputs.extremePageRenderErrorCode === "pdf_page_exceeds_cache_budget" &&
      report.largeInputs.extremePageRenderRequests === 0 &&
      report.largeInputs.extremePageCacheEmpty === true &&
      report.largeInputs.loadFailuresAfterLargeAttempts === 5,
    "PDF controller large-input contract failed"
  );
  assert(
    report.opening?.lifecycleActive === true &&
      report.opening.metadataComplete === true &&
      report.opening.positivePageDimensions === true,
    "PDF controller open/metadata contract failed"
  );
  validateRender(report.viewport?.fit, 960);
  validateRender(report.viewport?.zoom150Percent, 1440);
  assert(
    JSON.stringify(report.viewport.overscanPageIndexes) === JSON.stringify([0, 1]) &&
      report.viewport.aspectRatioPreserved === true &&
      report.viewport.distinctZoomOutput === true &&
      report.viewport.repeatedViewportReusedCache === true,
    "PDF controller viewport/overscan/cache-reuse contract failed"
  );
  assert(
    report.cache?.pageLimit === 3 &&
      report.cache.budgetBytes === 64 * 1024 * 1024 &&
      report.cache.renderedPageRequests > 0 &&
      report.cache.evictions > 0 &&
      report.cache.residentPages <= report.cache.pageLimit &&
      report.cache.residentBytes <= report.cache.budgetBytes &&
      report.cache.budgetRespected === true,
    "PDF controller decoded cache contract failed"
  );
  assert(
    report.cancellation?.preCancelledRenderRejected === true &&
      report.cancellation.errorCode === "pdf_render_cancelled" &&
      report.cancellation.cancelledRequests === 1,
    "PDF controller cancellation contract failed"
  );
  assert(
    report.failures?.sourceSizeErrorCode === "pdf_source_size_invalid" &&
      report.failures.corruptErrorCode === "pdf_document_corrupt" &&
      report.failures.lifecycleError === true &&
      report.failures.failedReloadClearedDocument === true &&
      report.failures.failedReloadClearedCache === true &&
      report.failures.loadFailures === 2,
    "PDF controller typed failure/failed-reload contract failed"
  );
  assert(
    report.close?.lifecycleClosed === true &&
      report.close.documentReleased === true &&
      report.close.cacheReleased === true,
    "PDF controller close contract failed"
  );
  assert(
    Object.values(report.privacy ?? {}).every((value) => value === false),
    "PDF controller diagnostics leaked document identity or content"
  );
  assert(
    JSON.stringify(report.limitations) ===
      JSON.stringify([
        "this controller run is headless and does not claim GPUI page controls or physical input",
        "PDFium native-call crash and hard-timeout isolation remain open and are not claimed by typed error tests"
      ]),
    "PDF controller evidence limitations drifted"
  );
}

function validateEvidence(evidence) {
  assert(evidence?.schemaVersion === "vibex-pdf-controller-evidence.v1", "PDF controller evidence schema drifted");
  assert(evidence.status === "passed", "PDF controller evidence did not pass");
  const capturedLock = evidence.sourceInputs?.find((input) => input.path === "Cargo.lock");
  const applicability = classifyGpuiEvidence(ROOT, EVIDENCE_PATH, {
    cargoLockSha256: capturedLock?.sha256
  });
  if (applicability === "current") {
    assert(
      JSON.stringify(evidence.sourceInputs) === JSON.stringify(SOURCE_INPUTS.map(fileIdentity)),
      "PDF controller evidence source inputs drifted; recapture is required"
    );
  }
  const review = readJson(REVIEW_PATH);
  const linuxReview = review.archives?.find((archive) => archive.target === "linux-x86_64");
  assert(linuxReview, "PDFium Linux review entry is missing");
  assert(
    evidence.nativeRuntime?.target === "linux-x86_64" &&
      evidence.nativeRuntime.librarySha256 === linuxReview.librarySha256 &&
      evidence.nativeRuntime.libraryBytes === linuxReview.libraryBytes,
    "PDF controller native runtime identity drifted"
  );
  validateRun(evidence.run);
  return applicability;
}

function capture() {
  run("node", ["scripts/prepare-pdfium-runtime.mjs", "--offline"]);
  run("node", ["scripts/generate-pdf-encrypted-fixture.mjs"]);
  run("node", ["scripts/generate-pdf-large-fixtures.mjs"]);
  run("cargo", ["build", "-p", "vibex-desktop", "--locked"]);
  const temp = mkdtempSync(join(tmpdir(), "vibex-pdf-controller-"));
  try {
    const output = join(temp, "run.json");
    const oversizedSource = join(temp, "oversized-source.pdf");
    const oversizedFd = openSync(oversizedSource, "w");
    try {
      ftruncateSync(oversizedFd, OVERSIZED_SOURCE_BYTES);
    } finally {
      closeSync(oversizedFd);
    }
    run(rootPath(BINARY_PATH), [
      "--native-content-pdf-controller",
      rootPath(LIBRARY_PATH),
      rootPath(FIXTURE_PATH),
      rootPath(ENCRYPTED_FIXTURE_PATH),
      rootPath(TOO_MANY_PAGES_FIXTURE_PATH),
      rootPath(EXTREME_PAGE_FIXTURE_PATH),
      oversizedSource,
      output
    ], {
      env: {
        ...process.env,
        VIBEX_PDF_ENCRYPTED_FIXTURE_PASSWORD: ENCRYPTED_PDF_FIXTURE_PASSWORD
      }
    });
    const runReport = JSON.parse(readFileSync(output, "utf8"));
    validateRun(runReport);
    const library = readFileSync(rootPath(LIBRARY_PATH));
    const evidence = {
      schemaVersion: "vibex-pdf-controller-evidence.v1",
      status: "passed",
      capturedAt: new Date().toISOString(),
      sourceInputs: SOURCE_INPUTS.map(fileIdentity),
      nativeRuntime: {
        target: "linux-x86_64",
        libraryBytes: library.length,
        librarySha256: sha256(library)
      },
      run: runReport
    };
    validateEvidence(evidence);
    writeFileSync(rootPath(EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
}

function selfTest(evidence) {
  const mutated = structuredClone(evidence);
  const mutations = [
    (copy) => (copy.run.failures.failedReloadClearedCache = false),
    (copy) => (copy.run.encryptedFixture.correctPasswordRendered = false),
    (copy) => (copy.run.largeInputs.extremePageRenderRequests = 1),
    (copy) => (copy.run.largeInputs.tooManyPagesClearedCache = false),
    (copy) => (copy.run.privacy.diagnosticsContainPassword = true)
  ];
  for (const mutate of mutations) {
    const copy = structuredClone(mutated);
    mutate(copy);
    let rejected = false;
    try {
      validateEvidence(copy);
    } catch {
      rejected = true;
    }
    assert(rejected, "PDF controller negative self-test accepted invalid encrypted evidence");
  }
}

try {
  if (process.argv.includes("--write")) capture();
  const evidence = readJson(EVIDENCE_PATH);
  const applicability = validateEvidence(evidence);
  if (process.argv.includes("--self-test")) selfTest(evidence);
  const size = statSync(rootPath(EVIDENCE_PATH)).size;
  console.log(`GPUI PDF controller evidence verified (${size} bytes); applicability=${applicability}`);
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}

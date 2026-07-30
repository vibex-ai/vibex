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
import { dirname, join, resolve, sep } from "node:path";
import { tmpdir } from "node:os";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { classifyGpuiEvidence } from "./evidence-applicability.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const EVIDENCE_PATH = "docs/platform/evidence/pdf-linux.json";
const PREVIEW_PATH = "docs/parity/screenshots/current/spikes/linux-pdfium-page-1.png";
const FIXTURE_PATH = "docs/platform/fixtures/pdf-feasibility.pdf";
const REVIEW_PATH = "docs/licenses/pdfium-7881-review.json";
const BINARY_PATH = "target/debug/vibex-desktop";
const BASELINE_COMMIT = "f1c624f115401d6160a771545fa1ec73128394b1";
const PDFIUM_RELEASE_URL =
  "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F7881";
const SOURCE_INPUT_ROOTS = [
  "Cargo.lock",
  "Cargo.toml",
  "rust-toolchain.toml",
  "apps/desktop/Cargo.toml",
  "apps/desktop/src",
  REVIEW_PATH,
  FIXTURE_PATH,
  "scripts/generate-pdf-fixture.mjs",
  "scripts/capture-pdf.mjs"
];
const EXPECTED_CLAIMS = [
  ["native_pdfium_bind", "passed", true],
  ["page_render", "passed", true],
  ["cjk_embedded_font", "passed", true],
  ["zoom_and_fit", "passed", true],
  ["page_virtualization", "passed", true],
  ["bounded_decode_cache", "passed", true],
  ["invalid_document_error", "passed", true],
  ["linux_process_memory", "passed", true],
  ["native_distribution_license_review", "passed", true],
  ["macos_windows_native_runtime", "blocked", true],
  ["webview_pdf_comparison", "deferred_by_user_request", false]
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

function readJson(path) {
  return JSON.parse(readFileSync(rootPath(path), "utf8"));
}

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024,
    timeout: 180_000,
    ...options
  });
  if (result.error) fail(`${command} failed to start: ${result.error.message}`);
  if (result.status !== 0) {
    fail(`${command} ${args.join(" ")} failed with status ${result.status}:\n${result.stderr ?? ""}`);
  }
  return result;
}

function sourceFilesFor(path) {
  const absolute = rootPath(path);
  assert(existsSync(absolute), `source input is missing: ${path}`);
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

function validateReview(review) {
  assert(review?.schemaVersion === "vibex-pdfium-native-review.v1", "PDFium review schema is invalid");
  assert(review.engine?.build === "7881", "PDFium review build drifted");
  assert(review.engine?.wrapper === "pdfium-render 0.9.3", "PDFium wrapper review drifted");
  const targets = review.archives?.map((archive) => archive.target);
  assert(
    JSON.stringify(targets) ===
      JSON.stringify(["linux-x86_64", "macos-x86_64", "macos-aarch64", "windows-x86_64"]),
    "PDFium archive target matrix is incomplete"
  );
  for (const archive of review.archives) {
    for (const field of ["archiveSha256", "librarySha256"]) {
      assert(/^[a-f0-9]{64}$/.test(archive[field]), `PDFium ${archive.target} ${field} is invalid`);
    }
    assert(archive.archiveBytes > 0 && archive.libraryBytes > 0, `PDFium ${archive.target} sizes are invalid`);
  }
  assert(review.licenseFiles?.length === 16, "PDFium license file inventory is incomplete");
  assert(
    review.review?.status === "approved_for_linux_distribution" &&
      review.review.productionPolicyModified === true &&
      review.review.nativeBinaryRegisteredForDistribution === true &&
      JSON.stringify(review.review.registeredTargets) === JSON.stringify(["linux-x86_64"]),
    "PDFium Linux distribution approval is invalid"
  );
  assert(
    JSON.stringify(review.review.unapprovedExpressions) === JSON.stringify([]),
    "PDFium license approval retained unapproved expressions"
  );
  return review;
}

function validateRun(report) {
  assert(report?.schemaVersion === "vibex-pdf-feasibility-run.v1", "PDF run schema is invalid");
  assert(report.status === "passed", "PDF technical run did not pass");
  assert(report.platform === "linux" && report.architecture === "x86_64", "PDF run platform is invalid");
  assert(
    report.engine?.wrapper === "pdfium-render" &&
      report.engine.wrapperVersion === "0.9.3" &&
      report.engine.pdfiumVersion === "7881" &&
      report.engine.binding === "explicit-dynamic-library" &&
      report.engine.processModel === "in-process-native-library" &&
      report.engine.childProcessesStarted === 0,
    "PDF engine identity is invalid"
  );
  const fixture = readFileSync(rootPath(FIXTURE_PATH));
  assert(report.fixture?.bytes === fixture.length, "PDF fixture byte count drifted");
  assert(report.fixture.sha256 === sha256(fixture), "PDF fixture hash drifted");
  assert(report.fixture.pageCount === 12, "PDF fixture page count drifted");
  assert(report.fixture.cjkTextExtracted === true, "PDF CJK extraction did not pass");
  assert(report.fixture.embeddedFontMarkerPresent === true, "PDF embedded font marker is absent");
  for (const render of [report.rendering?.fit, report.rendering?.zoom150Percent]) {
    assert(Number.isInteger(render?.width) && render.width > 0, "PDF render width is invalid");
    assert(Number.isInteger(render.height) && render.height > 0, "PDF render height is invalid");
    assert(render.rgbaBytes === render.width * render.height * 4, "PDF RGBA byte count is invalid");
    assert(/^[a-f0-9]{64}$/.test(render.rgbaSha256), "PDF RGBA hash is invalid");
    assert(render.sampledUniqueColors >= 16, "PDF render has insufficient color diversity");
    assert(Number.isFinite(render.elapsedMs) && render.elapsedMs > 0, "PDF render duration is invalid");
  }
  assert(
    report.rendering.fit.width === 960 &&
      report.rendering.zoom150Percent.width === 1440 &&
      report.rendering.aspectRatioPreserved === true &&
      report.rendering.distinctZoomOutput === true &&
      report.rendering.previewRawRgbaWritten === true,
    "PDF fit/zoom contract is invalid"
  );
  assert(
    report.virtualization?.strategy === "visible-two-pages-plus-one-page-overscan-lru" &&
      report.virtualization.visiblePages === 2 &&
      report.virtualization.overscanPagesPerSide === 1 &&
      report.virtualization.cacheBudgetBytes === 24 * 1024 * 1024 &&
      report.virtualization.viewportSteps === 24 &&
      report.virtualization.cacheHits > 0 &&
      report.virtualization.cacheMisses > 0 &&
      report.virtualization.evictions > 0 &&
      report.virtualization.maximumResidentBytes <= report.virtualization.cacheBudgetBytes &&
      report.virtualization.cacheBudgetRespected === true,
    "PDF virtualization evidence is invalid"
  );
  assert(
    report.errorHandling?.invalidDocumentRejected === true &&
      report.errorHandling.loadingErrorIsStructured === true,
    "PDF invalid-document behavior did not pass"
  );
  assert(
    report.memory?.measurementSource === "proc-self-status" &&
      Number.isInteger(report.memory.currentRssBeforeKib) &&
      Number.isInteger(report.memory.currentRssAfterKib) &&
      Number.isInteger(report.memory.processPeakRssKib),
    "PDF memory observation is invalid"
  );
  assert(
    report.privacy?.documentTextStored === false &&
      report.privacy.nativeLibraryPathStored === false &&
      report.privacy.fixturePathStored === false,
    "PDF run retained private paths or document text"
  );
}

function validateEvidence(evidence) {
  const review = validateReview(readJson(REVIEW_PATH));
  assert(evidence?.schemaVersion === "pdf-linux-evidence.v1", "PDF evidence schema is invalid");
  assert(evidence.status === "partial", "PDF evidence must retain unresolved distribution and hosted work");
  assert(evidence.source?.vibexBaselineCommit === BASELINE_COMMIT, "PDF baseline identity drifted");
  const applicability = classifyGpuiEvidence(ROOT, EVIDENCE_PATH, evidence.source);
  if (applicability === "current") {
    assert(
      JSON.stringify(evidence.source.sourceInputRoots) === JSON.stringify(SOURCE_INPUT_ROOTS),
      "PDF source roots drifted"
    );
    assert(
      evidence.source.sourceInputTreeSha256 === sourceInputTreeSha256(),
      "PDF source identity is stale"
    );
    assert(
      evidence.source.lockfileSha256 === sha256(readFileSync(rootPath("Cargo.lock"))),
      "PDF lockfile is stale"
    );
  }
  const linuxArchive = review.archives[0];
  assert(
    evidence.nativeInput?.target === linuxArchive.target &&
      evidence.nativeInput.archiveSha256 === linuxArchive.archiveSha256 &&
      evidence.nativeInput.archiveBytes === linuxArchive.archiveBytes &&
      evidence.nativeInput.librarySha256 === linuxArchive.librarySha256 &&
      evidence.nativeInput.libraryBytes === linuxArchive.libraryBytes,
    "PDF Linux native input identity drifted"
  );
  validateRun(evidence.run);
  const preview = rootPath(PREVIEW_PATH);
  assert(existsSync(preview), "PDF preview PNG is missing");
  assert(evidence.preview?.path === PREVIEW_PATH, "PDF preview path drifted");
  assert(evidence.preview.sha256 === sha256(readFileSync(preview)), "PDF preview hash drifted");
  assert(
    evidence.preview.width === evidence.run.rendering.fit.width &&
      evidence.preview.height === evidence.run.rendering.fit.height &&
      evidence.preview.uniqueColors > 16 &&
      evidence.preview.entropy > 0,
    "PDF preview pixels are not credible"
  );
  assert(Number.isInteger(evidence.process?.maximumResidentSetKib) && evidence.process.maximumResidentSetKib > 0, "PDF process RSS is invalid");
  assert(
    JSON.stringify(evidence.claims.map((claim) => [claim.id, claim.status, claim.decisionBearing])) ===
      JSON.stringify(EXPECTED_CLAIMS),
    "PDF claim matrix drifted"
  );
  for (const claim of evidence.claims.slice(0, 9)) {
    assert(claim.blocker === null, `passed PDF claim ${claim.id} retained a blocker`);
  }
  assert(
    evidence.claims[9].blocker?.code === "hosted-pdfium-runtime-not-run" &&
      evidence.claims[10].blocker === null,
    "PDF blocker/defer semantics are invalid"
  );
  assert(
    evidence.webviewComparison?.status === "deferred_by_user_request" &&
      evidence.webviewComparison.tested === false &&
      evidence.webviewComparison.inferredResult === null,
    "PDF evidence fabricated a WebView comparison result"
  );
  assert(
    evidence.summary?.passedClaims === 9 &&
      evidence.summary.blockedClaims === 1 &&
      evidence.summary.deferredClaims === 1 &&
      evidence.summary.technicalRouteProvenOnLinux === true &&
      evidence.summary.pdfGateSatisfied === false,
    "PDF evidence summary is inconsistent"
  );
  assert(
    evidence.recommendation?.route === "pdfium-native-linux" &&
      evidence.recommendation.selectedForProduction === true &&
      JSON.stringify(evidence.recommendation.productionTargets) ===
        JSON.stringify(["linux-x86_64"]) &&
      evidence.recommendation.productionEstimateEngineerWeeks?.min === 2 &&
      evidence.recommendation.productionEstimateEngineerWeeks?.max === 4,
    "PDF recommendation is incomplete"
  );
  const serialized = JSON.stringify(evidence);
  for (const forbidden of [ROOT, process.env.HOME, "/tmp/", "FIT-ZOOM-CACHE-", "PDFIUM_LIB_PATH"]) {
    if (forbidden) assert(!serialized.includes(forbidden), "PDF evidence retained a path or document text");
  }
  return applicability;
}

function imageMetrics(path) {
  const output = run("magick", [
    "identify",
    "-format",
    "%w\t%h\t%k\t%[entropy]",
    path
  ]).stdout.trim();
  const [width, height, uniqueColors, entropy] = output.split("\t").map(Number);
  assert([width, height, uniqueColors, entropy].every(Number.isFinite), "ImageMagick returned invalid PDF metrics");
  return { width, height, uniqueColors, entropy };
}

function capture() {
  assert(process.platform === "linux" && process.arch === "x64", "local PDF capture requires Linux x86_64");
  run(process.execPath, ["scripts/generate-pdf-fixture.mjs"]);
  const review = validateReview(readJson(REVIEW_PATH));
  const nativeInput = review.archives[0];
  const temporaryRoot = mkdtempSync(join(tmpdir(), "vibex-pdf-"));
  const archive = join(temporaryRoot, nativeInput.asset);
  const extracted = join(temporaryRoot, "extracted");
  const reportPath = join(temporaryRoot, "report.json");
  const rawPath = join(temporaryRoot, "page-1.rgba");
  try {
    mkdirSync(extracted, { recursive: true });
    run("curl", [
      "-fL",
      "--retry",
      "3",
      "--output",
      archive,
      `${PDFIUM_RELEASE_URL}/${nativeInput.asset}`
    ]);
    const archiveContent = readFileSync(archive);
    assert(archiveContent.length === nativeInput.archiveBytes, "PDFium archive size did not match review");
    assert(sha256(archiveContent) === nativeInput.archiveSha256, "PDFium archive hash did not match review");
    run("tar", ["-xzf", archive, "-C", extracted]);
    const library = join(extracted, nativeInput.libraryPath);
    const libraryContent = readFileSync(library);
    assert(libraryContent.length === nativeInput.libraryBytes, "PDFium library size did not match review");
    assert(sha256(libraryContent) === nativeInput.librarySha256, "PDFium library hash did not match review");
    const licensePaths = ["LICENSE", ...review.licenseFiles.slice(1).map((entry) => entry.path)];
    for (const path of licensePaths) {
      assert(existsSync(join(extracted, path)), `PDFium archive is missing ${path}`);
    }

    run("cargo", ["build", "-p", "vibex-desktop", "--locked"], { stdio: "inherit" });
    run(rootPath(BINARY_PATH), [
      "--spike-pdf",
      library,
      rootPath(FIXTURE_PATH),
      reportPath,
      rawPath
    ]);
    const report = JSON.parse(readFileSync(reportPath, "utf8"));
    validateRun(report);
    const maximumResidentSetKib = report.memory.processPeakRssKib;
    assert(Number.isInteger(maximumResidentSetKib) && maximumResidentSetKib > 0, "PDF process did not report VmHWM");
    assert(readFileSync(rawPath).length === report.rendering.fit.rgbaBytes, "PDF raw preview size is invalid");
    mkdirSync(dirname(rootPath(PREVIEW_PATH)), { recursive: true });
    run("magick", [
      "-size",
      `${report.rendering.fit.width}x${report.rendering.fit.height}`,
      "-depth",
      "8",
      `rgba:${rawPath}`,
      "-strip",
      rootPath(PREVIEW_PATH)
    ]);
    const metrics = imageMetrics(rootPath(PREVIEW_PATH));
    const passedClaim = (id) => ({ id, status: "passed", decisionBearing: true, blocker: null });
    const evidence = {
      schemaVersion: "pdf-linux-evidence.v1",
      status: "partial",
      capturedAt: new Date().toISOString(),
      source: {
        vibexBaselineCommit: BASELINE_COMMIT,
        captureParentCommit: run("git", ["rev-parse", "HEAD"]).stdout.trim(),
        rustToolchain: run("rustc", ["--version"]).stdout.trim(),
        lockfileSha256: sha256(readFileSync(rootPath("Cargo.lock"))),
        sourceInputRoots: SOURCE_INPUT_ROOTS,
        sourceInputTreeSha256: sourceInputTreeSha256()
      },
      runner: {
        platform: "linux",
        architecture: "x86_64",
        syntheticDocument: true,
        displayRequired: false,
        systemPopplerUsed: false
      },
      nativeInput: {
        target: nativeInput.target,
        release: review.engine.binaryRelease,
        asset: nativeInput.asset,
        archiveSha256: nativeInput.archiveSha256,
        archiveBytes: nativeInput.archiveBytes,
        librarySha256: nativeInput.librarySha256,
        libraryBytes: nativeInput.libraryBytes,
        runtimeDependencies: ["libpthread", "libm", "libgcc_s", "libc"]
      },
      run: report,
      process: {
        measurement: "proc-self-status-vmhwm",
        maximumResidentSetKib,
        processTreeMembers: 1,
        childProcessesObserved: 0
      },
      preview: {
        path: PREVIEW_PATH,
        bytes: statSync(rootPath(PREVIEW_PATH)).size,
        sha256: sha256(readFileSync(rootPath(PREVIEW_PATH))),
        ...metrics
      },
      licenseReview: {
        path: REVIEW_PATH,
        status: review.review.status,
        prohibitedGplOrAgplComponentDetected: review.review.prohibitedGplOrAgplComponentDetected,
        unapprovedExpressions: review.review.unapprovedExpressions,
        productionPolicyModified: review.review.productionPolicyModified,
        nativeBinaryRegisteredForDistribution: review.review.nativeBinaryRegisteredForDistribution,
        registeredTargets: review.review.registeredTargets
      },
      webviewComparison: {
        status: "deferred_by_user_request",
        tested: false,
        inferredResult: null
      },
      capabilityDisposition: {
        pageListAndScroll: "passed_by_bounded_virtualization_model",
        zoomAndFit: "passed",
        loadingAndError: "passed",
        systemOpen: "retained_platform_shell_requirement_not_engine_dependent",
        searchAnnotationsAndForms: "outside_current_parity_scope"
      },
      claims: [
        ...[
          "native_pdfium_bind",
          "page_render",
          "cjk_embedded_font",
          "zoom_and_fit",
          "page_virtualization",
          "bounded_decode_cache",
          "invalid_document_error",
          "linux_process_memory"
        ].map(passedClaim),
        {
          id: "native_distribution_license_review",
          status: "passed",
          decisionBearing: true,
          blocker: null
        },
        {
          id: "macos_windows_native_runtime",
          status: "blocked",
          decisionBearing: true,
          blocker: {
            code: "hosted-pdfium-runtime-not-run",
            owner: "desktop-platform",
            detail: "Pinned native archive identities exist, but macOS and Windows runtime rendering has not run."
          }
        },
        {
          id: "webview_pdf_comparison",
          status: "deferred_by_user_request",
          decisionBearing: false,
          blocker: null
        }
      ],
      summary: {
        passedClaims: 9,
        blockedClaims: 1,
        deferredClaims: 1,
        technicalRouteProvenOnLinux: true,
        pdfGateSatisfied: false
      },
      recommendation: {
        route: "pdfium-native-linux",
        selectedForProduction: true,
        productionTargets: ["linux-x86_64"],
        deferredTargets: ["macos-x86_64", "macos-aarch64", "windows-x86_64"],
        compressedNativeInputBytes: nativeInput.archiveBytes,
        installedNativeLibraryBytes: nativeInput.libraryBytes,
        cacheBudgetBytes: report.virtualization.cacheBudgetBytes,
        productionEstimateEngineerWeeks: { min: 2, max: 4 },
        prerequisites: [
          "hosted macOS and Windows headless render probes"
        ]
      }
    };
    mkdirSync(dirname(rootPath(EVIDENCE_PATH)), { recursive: true });
    writeFileSync(rootPath(EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
    validateEvidence(evidence);
    console.log(
      `GPUI PDF captured: ${report.fixture.pageCount} pages, ` +
        `${report.virtualization.maximumResidentBytes} cache bytes, ${maximumResidentSetKib} KiB peak RSS`
    );
  } finally {
    rmSync(temporaryRoot, { recursive: true, force: true });
  }
}

function verify() {
  run(process.execPath, ["scripts/generate-pdf-fixture.mjs"]);
  assert(existsSync(rootPath(EVIDENCE_PATH)), `${EVIDENCE_PATH} is missing`);
  const applicability = validateEvidence(readJson(EVIDENCE_PATH));
  console.log(
    "GPUI PDF evidence verified: Linux native route and distribution license passed; hosted runtime remains blocked; " +
      `applicability=${applicability}`
  );
}

function selfTest() {
  const evidence = readJson(EVIDENCE_PATH);
  validateEvidence(evidence);
  for (const [label, mutate] of [
    ["stale native binary", (copy) => (copy.nativeInput.librarySha256 = "0".repeat(64))],
    ["unbounded cache", (copy) => (copy.run.virtualization.cacheBudgetRespected = false)],
    ["license regression", (copy) => (copy.claims[8].status = "blocked")],
    ["fabricated WebView result", (copy) => (copy.webviewComparison.tested = true)]
  ]) {
    const copy = JSON.parse(JSON.stringify(evidence));
    mutate(copy);
    let rejected = false;
    try {
      validateEvidence(copy);
    } catch {
      rejected = true;
    }
    assert(rejected, `PDF negative self-test was accepted: ${label}`);
  }
  console.log("GPUI PDF negative-case self-test passed");
}

if (process.argv.includes("--write")) capture();
else {
  verify();
  if (process.argv.includes("--self-test")) selfTest();
}

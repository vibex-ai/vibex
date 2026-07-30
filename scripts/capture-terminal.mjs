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
import { resolveGpuiSourceIdentities } from "./source-identities.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SOURCE_IDENTITIES = resolveGpuiSourceIdentities(ROOT);
const TERMY_ROOT = resolve(ROOT, "../gpui/termy");
const EVIDENCE_PATH = "docs/platform/evidence/terminal-linux.json";
const BINARY_PATH = "target/debug/vibex-desktop";
const VIBEX_BASELINE_COMMIT = "f1c624f115401d6160a771545fa1ec73128394b1";
const TERMY_REVISION = "03df18575fdda54619b86e277e7389d30972c302";
const TERMY_GPUI_REVISION = "c8656ac96d2344fc288b551943cc12fcb6ef56ad";
const ALACRITTY_VERSION = "0.26.0";
const ZERO_10_MIB_SHA256 = "e5b844cc57f57094ea4585e235f36c78c1cd222262bb89d53c94dcb4d6b3e55d";
const SOURCE_INPUT_ROOTS = [
  "Cargo.lock",
  "Cargo.toml",
  "rust-toolchain.toml",
  "apps/desktop/Cargo.toml",
  "apps/desktop/src",
  "crates/terminal/Cargo.toml",
  "crates/terminal/src",
  "scripts/capture-terminal.mjs"
];
const CLAIMS = [
  ["termy_reference_boundary", "passed"],
  ["alacritty_core_fallback", "passed"],
  ["vibex_pty_raw_bytes", "passed"],
  ["cjk_round_trip", "passed"],
  ["selection_copy_model", "passed"],
  ["resize_round_trip", "passed"],
  ["alternate_screen", "passed"],
  ["linux_10mb_no_loss", "passed"],
  ["windows_conpty", "blocked"]
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
    timeout: 120_000,
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

function exactKeys(value, expected, label) {
  const actual = Object.keys(value ?? {}).sort();
  const wanted = [...expected].sort();
  assert(JSON.stringify(actual) === JSON.stringify(wanted), `${label} keys are not exact`);
}

function validateRun(report, expectedPlatform = "linux") {
  assert(report?.schemaVersion === "vibex-terminal-feasibility-run.v1", "terminal run schema is invalid");
  assert(report.status === "passed", "terminal run did not pass");
  assert(report.platform === expectedPlatform, `terminal run platform must be ${expectedPlatform}`);
  assert(/^[a-z0-9_]{2,32}$/.test(report.architecture), "terminal run architecture is invalid");
  assert(report.engine?.name === "alacritty_terminal", "terminal engine name drifted");
  assert(report.engine.version === ALACRITTY_VERSION, "terminal engine version drifted");
  assert(
    report.engine.integration === "termy-compatible-bounded-core-fallback",
    "terminal integration boundary drifted"
  );
  assert(report.pty?.backend === "portable-pty-openpty", "Linux PTY backend is invalid");
  assert(report.pty.windowsConptyExercised === false, "Linux run fabricated Windows ConPTY evidence");
  for (const field of [
    "rawBytesObserved",
    "invalidUtf8Observed",
    "cjkObserved",
    "resizeObserved",
    "processExited"
  ]) {
    assert(report.pty[field] === true, `terminal PTY field ${field} did not pass`);
  }
  assert(
    report.pty.resizeRequested?.rows === 42 && report.pty.resizeRequested?.columns === 132,
    "terminal PTY resize fixture drifted"
  );
  assert(
    Number.isInteger(report.pty.inputBytesWritten) &&
      report.pty.inputBytesWritten > 0 &&
      report.pty.inputBytesWritten < 1024,
    "terminal input byte count is invalid"
  );
  assert(report.pty.rawDroppedChunks === 0, "raw PTY chunks were dropped");
  for (const field of [
    "cjkCellsObserved",
    "selectionCopyObserved",
    "alternateScreenEntered",
    "primaryScreenRestored",
    "resizeObserved"
  ]) {
    assert(report.emulator?.[field] === true, `terminal emulator field ${field} did not pass`);
  }
  assert(
    Number.isInteger(report.emulator.ingestedBytes) && report.emulator.ingestedBytes > 0,
    "terminal emulator byte count is invalid"
  );
  assert(report.throughput?.fixtureBytes === 10 * 1024 * 1024, "terminal throughput size drifted");
  assert(report.throughput.fixtureSha256 === ZERO_10_MIB_SHA256, "terminal throughput hash drifted");
  assert(
    Number.isInteger(report.throughput.elapsedMs) && report.throughput.elapsedMs > 0,
    "terminal throughput duration is invalid"
  );
  assert(
    Number.isFinite(report.throughput.mebibytesPerSecond) && report.throughput.mebibytesPerSecond > 0,
    "terminal throughput rate is invalid"
  );
  assert(report.throughput.dataLossObserved === false, "terminal throughput lost data");
  assert(report.rawTextStored === false, "terminal report retained raw text");
  exactKeys(
    report,
    [
      "schemaVersion",
      "status",
      "platform",
      "architecture",
      "engine",
      "pty",
      "emulator",
      "throughput",
      "rawTextStored"
    ],
    "terminal run"
  );
}

function dependencyIdentity() {
  const metadata = JSON.parse(run("cargo", ["metadata", "--locked", "--format-version", "1"]));
  const packages = metadata.packages ?? [];
  const desktop = packages.find((entry) => entry.name === "vibex-desktop");
  const terminal = packages.find((entry) => entry.name === "vibex-terminal");
  const alacritty = packages.filter((entry) => entry.name === "alacritty_terminal");
  assert(desktop && terminal, "production-shaped terminal packages are missing");
  assert(
    desktop.dependencies.some((entry) => entry.name === "vibex-terminal" && entry.path),
    "GPUI desktop does not directly link vibex-terminal"
  );
  assert(
    terminal.dependencies.some(
      (entry) => entry.name === "alacritty_terminal" && entry.req === "^0.26"
    ),
    "vibex-terminal does not pin the Alacritty fallback"
  );
  assert(alacritty.length === 1, "locked graph must contain one alacritty_terminal package");
  assert(alacritty[0].version === ALACRITTY_VERSION, "locked Alacritty version drifted");
  assert(alacritty[0].license === "Apache-2.0", "Alacritty license is not approved");
  assert(
    alacritty[0].source?.startsWith("registry+https://github.com/rust-lang/crates.io-index"),
    "Alacritty source is not crates.io"
  );
  return {
    name: alacritty[0].name,
    version: alacritty[0].version,
    source: alacritty[0].source,
    licenseExpression: alacritty[0].license,
    directFromVibexTerminal: true,
    linkedFromDesktop: true
  };
}

function termyReference() {
  assert(existsSync(TERMY_ROOT) && statSync(TERMY_ROOT).isDirectory(), "local Termy reference is missing");
  const repository = run("git", ["-C", TERMY_ROOT, "remote", "get-url", "origin"]).trim();
  run("git", ["-C", TERMY_ROOT, "cat-file", "-e", `${TERMY_REVISION}^{commit}`]);
  const pinnedFile = (path) => run("git", ["-C", TERMY_ROOT, "show", `${TERMY_REVISION}:${path}`]);
  const license = pinnedFile("LICENSE");
  const workspace = pinnedFile("Cargo.toml");
  const core = pinnedFile("crates/core/Cargo.toml");
  const terminalUi = pinnedFile("crates/terminal_ui/Cargo.toml");
  assert(repository === "https://github.com/lassejlv/termy.git", "Termy repository drifted");
  assert(license.startsWith("MIT License"), "Termy reference license is not MIT");
  assert(workspace.includes(`alacritty_terminal = "0.26"`), "Termy engine version drifted");
  assert(workspace.includes(`rev = "${TERMY_GPUI_REVISION}"`), "Termy GPUI revision drifted");
  for (const dependency of ["termy_config_core", "termy_search", "termy_themes"]) {
    assert(core.includes(dependency), `Termy core coupling ${dependency} is missing`);
  }
  assert(terminalUi.includes("gpui = { workspace = true }"), "Termy terminal UI is no longer GPUI-coupled");
  assert(
    TERMY_GPUI_REVISION !== SOURCE_IDENTITIES.zedRevision,
    "Termy and Vibex GPUI revisions unexpectedly match"
  );
  return {
    repository,
    revision: TERMY_REVISION,
    licenseExpression: "MIT",
    engine: `alacritty_terminal ${ALACRITTY_VERSION}`,
    gpuiRevision: TERMY_GPUI_REVISION,
    vibexGpuiRevision: SOURCE_IDENTITIES.zedRevision,
    directImportSelected: false,
    sourceCodeCopied: false,
    coupling: ["termy_config_core", "termy_search", "termy_themes", "termy_gpui_revision"]
  };
}

function validateEvidence(evidence) {
  assert(evidence?.schemaVersion === "terminal-linux-evidence.v1", "terminal evidence schema is invalid");
  assert(evidence.status === "partial", "terminal evidence must retain the hosted ConPTY blocker");
  assert(evidence.source?.vibexBaselineCommit === VIBEX_BASELINE_COMMIT, "terminal baseline drifted");
  const applicability = classifyGpuiEvidence(ROOT, EVIDENCE_PATH, evidence.source);
  if (applicability === "current") {
    assert(
      JSON.stringify(evidence.source.sourceInputRoots) === JSON.stringify(SOURCE_INPUT_ROOTS),
      "terminal source roots drifted"
    );
    assert(
      evidence.source.sourceInputTreeSha256 === sourceInputTreeSha256(),
      "terminal source identity is stale"
    );
    assert(
      evidence.source.lockfileSha256 === sha256(readFileSync(rootPath("Cargo.lock"))),
      "terminal lockfile identity is stale"
    );
  }
  assert(evidence.runner?.platform === "linux" && evidence.runner.syntheticPty === false, "terminal runner is invalid");
  assert(evidence.reference?.revision === TERMY_REVISION, "terminal Termy reference drifted");
  assert(evidence.reference.directImportSelected === false, "terminal evidence imported Termy wholesale");
  assert(evidence.reference.sourceCodeCopied === false, "terminal evidence copied Termy source");
  assert(evidence.dependency?.version === ALACRITTY_VERSION, "terminal dependency version drifted");
  assert(evidence.dependency.licenseExpression === "Apache-2.0", "terminal dependency license drifted");
  validateRun(evidence.run);
  assert(
    JSON.stringify(evidence.claims.map((claim) => [claim.id, claim.status])) === JSON.stringify(CLAIMS),
    "terminal claim matrix drifted"
  );
  for (const claim of evidence.claims.slice(0, -1)) {
    assert(claim.decisionBearing === true && claim.blocker === null, `terminal claim ${claim.id} is invalid`);
  }
  const conpty = evidence.claims.at(-1);
  assert(
    conpty.id === "windows_conpty" &&
      conpty.status === "blocked" &&
      conpty.decisionBearing === true &&
      conpty.blocker?.code === "hosted-conpty-not-run",
    "terminal ConPTY blocker is invalid"
  );
  assert(
    evidence.summary?.passedClaims === 8 &&
      evidence.summary.blockedClaims === 1 &&
      evidence.summary.terminalGateSatisfied === false,
    "terminal evidence summary is inconsistent"
  );
  assert(
    evidence.recommendation?.route === "alacritty-core-adapter" &&
      evidence.recommendation.productionEstimateEngineerWeeks?.min === 2 &&
      evidence.recommendation.productionEstimateEngineerWeeks?.max === 4,
    "terminal recommendation is incomplete"
  );
  const serialized = JSON.stringify(evidence);
  for (const forbidden of [ROOT, process.env.HOME, "VIBEX_RAW_BEGIN", "VIBEX_CJK:", '"rawText"']) {
    if (forbidden) assert(!serialized.includes(forbidden), "terminal evidence retained private or raw data");
  }
  return applicability;
}

function verifyEvidence() {
  assert(existsSync(rootPath(EVIDENCE_PATH)), `${EVIDENCE_PATH} is missing`);
  const evidence = readJson(EVIDENCE_PATH);
  const applicability = validateEvidence(evidence);
  console.log(
    `GPUI Terminal verified: ${evidence.summary.passedClaims} Linux claims passed, ` +
      `${evidence.summary.blockedClaims} hosted ConPTY blocker; applicability=${applicability}`
  );
}

function selfTest() {
  const evidence = readJson(EVIDENCE_PATH);
  validateEvidence(evidence);
  for (const [label, mutate] of [
    ["dropped bytes", (copy) => (copy.run.pty.rawDroppedChunks = 1)],
    ["data loss", (copy) => (copy.run.throughput.dataLossObserved = true)],
    ["Termy import", (copy) => (copy.reference.directImportSelected = true)],
    ["fabricated ConPTY pass", (copy) => (copy.claims.at(-1).status = "passed")]
  ]) {
    const copy = JSON.parse(JSON.stringify(evidence));
    mutate(copy);
    let rejected = false;
    try {
      validateEvidence(copy);
    } catch {
      rejected = true;
    }
    assert(rejected, `terminal negative self-test was accepted: ${label}`);
  }
  console.log("GPUI Terminal negative-case self-test passed");
}

function capture() {
  assert(process.platform === "linux", "local Terminal capture requires Linux");
  const temporaryRoot = mkdtempSync(join(tmpdir(), "vibex-terminal-"));
  const reportPath = join(temporaryRoot, "terminal.json");
  try {
    run("cargo", ["build", "-p", "vibex-desktop", "--locked"], { stdio: "inherit" });
    run(rootPath(BINARY_PATH), ["--spike-terminal", reportPath], { timeout: 60_000 });
    const report = JSON.parse(readFileSync(reportPath, "utf8"));
    validateRun(report);
    const evidence = {
      schemaVersion: "terminal-linux-evidence.v1",
      status: "partial",
      capturedAt: new Date().toISOString(),
      source: {
        vibexBaselineCommit: VIBEX_BASELINE_COMMIT,
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
        fixtureProvider: "python-stdlib"
      },
      reference: termyReference(),
      dependency: dependencyIdentity(),
      run: report,
      claims: CLAIMS.map(([id, status]) => ({
        id,
        status,
        decisionBearing: true,
        blocker:
          id === "windows_conpty"
            ? {
                code: "hosted-conpty-not-run",
                summary: "Windows ConPTY must execute on the pinned windows-2022 hosted runner.",
                action: "Run the hosted native gate and merge its terminal probe evidence."
              }
            : null
      })),
      summary: {
        passedClaims: 8,
        blockedClaims: 1,
        terminalGateSatisfied: false
      },
      recommendation: {
        route: "alacritty-core-adapter",
        termyDisposition: "reference-only",
        rationale: "Termy confirms the mature engine choice, but its core and renderer are coupled to Termy configuration/search/themes and a different GPUI revision.",
        productionEstimateEngineerWeeks: { min: 2, max: 4 }
      },
      remainingRisks: [
        "Windows ConPTY execution remains pending on the hosted runner.",
        "The spike proves the emulator model and GPUI binary linkage, not a production terminal grid renderer or native clipboard UI."
      ]
    };
    validateEvidence(evidence);
    mkdirSync(dirname(rootPath(EVIDENCE_PATH)), { recursive: true });
    writeFileSync(rootPath(EVIDENCE_PATH), `${JSON.stringify(evidence, null, 2)}\n`);
    verifyEvidence();
  } finally {
    rmSync(temporaryRoot, { recursive: true, force: true });
  }
}

const mode = process.argv[2];
if (mode === "--write") {
  capture();
} else if (mode === "--self-test") {
  selfTest();
} else if (mode === undefined) {
  verifyEvidence();
} else {
  fail("usage: node scripts/capture-terminal.mjs [--write|--self-test]");
}

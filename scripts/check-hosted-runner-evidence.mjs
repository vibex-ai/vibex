import { createHash } from "node:crypto";
import { Buffer } from "node:buffer";
import { get } from "node:https";
import console from "node:console";
import {
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readlinkSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync
} from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { spawn, spawnSync } from "node:child_process";
import process from "node:process";
import { setTimeout } from "node:timers";
import { fileURLToPath, URL } from "node:url";
import {
  GPUI_DEPENDENCY_SOURCE_POLICY,
  resolveGpuiSourceIdentities
} from "./source-identities.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SOURCE_IDENTITIES = resolveGpuiSourceIdentities(ROOT);
const POLICY_PATH = "docs/platform/hosted-runner-policy.json";
const WORKFLOW_PATH = ".github/workflows/native-gate.yml";
const PACKAGER_CONFIG_PATH = "apps/desktop/Packager.toml";
const MATRIX_PATH = "docs/platform/evidence/hosted-runner-matrix.json";
const PDF_FIXTURE_PATH = "docs/platform/fixtures/pdf-feasibility.pdf";
const PDF_REVIEW_PATH = "docs/licenses/pdfium-7881-review.json";
const PDFIUM_RELEASE_URL =
  "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F7881";
const PLATFORM_SETTLE_MS = 5000;
const COMMAND_BUFFER_BYTES = 128 * 1024 * 1024;
const ALACRITTY_VERSION = "0.26.0";
const ZERO_10_MIB_SHA256 = "e5b844cc57f57094ea4585e235f36c78c1cd222262bb89d53c94dcb4d6b3e55d";

const EXPECTED_TARGETS = {
  macos: { runnerLabel: "macos-15", runnerOs: "macOS", nodePlatform: "darwin", packageFormat: "app" },
  windows: {
    runnerLabel: "windows-2022",
    runnerOs: "Windows",
    nodePlatform: "win32",
    packageFormat: "nsis"
  }
};

const EXPECTED_ACTIONS = {
  checkout: ["actions/checkout", "11bd71901bbe5b1630ceea73d27597364c9af683", "v4.2.2"],
  pnpm_setup: ["pnpm/action-setup", "7088e561eb65bb68695d245aa206f005ef30921d", "v4.1.0"],
  node_setup: ["actions/setup-node", "49933ea5288caeca8642d1e84afbd3f7d6820020", "v4.4.0"],
  rust_toolchain: ["dtolnay/rust-toolchain", "35a842e360814583e976785eeda0bd0655cb8e83", "1.97.0"],
  upload_artifact: [
    "actions/upload-artifact",
    "ea165f8d65b6e75b540449e92b4886f43607fa02",
    "v4.6.2"
  ],
  download_artifact: [
    "actions/download-artifact",
    "d3f86a106a0bac45b974a628896c90dbdf5c8093",
    "v4.3.0"
  ]
};

const EXPECTED_CHECK_IDS = [
  "pinned_toolchain",
  "locked_cargo_metadata",
  "locked_gpui_source_identity",
  "workspace_rust_tests",
  "desktop_tests",
  "frontend_quality",
  "supply_chain",
  "release_link",
  "direct_probe",
  "terminal_feasibility",
  "pdfium_headless_runtime",
  "platform_initialization",
  "minimal_package",
  "install_lifecycle",
  "packaged_probe",
  "artifact_hashes",
  "uninstall_lifecycle"
];

const EXPECTED_SKIP_IDS = [
  "real_window_screenshots_native_pixels",
  "ime_composition",
  "keyboard_pointer_clipboard_drag_drop_input",
  "dpi_scale_transitions",
  "multi_monitor_behavior"
];

const SOURCE_INPUT_PATHS = [
  "Cargo.lock",
  "Cargo.toml",
  "rust-toolchain.toml",
  "package.json",
  "pnpm-lock.yaml",
  "apps/desktop/Cargo.toml",
  "apps/desktop/Packager.toml",
  "apps/desktop/src/acp_lifecycle.rs",
  "apps/desktop/src/composer_spike.rs",
  "apps/desktop/src/lib.rs",
  "apps/desktop/src/main.rs",
  "apps/desktop/src/pdf_spike.rs",
  "crates/terminal/Cargo.toml",
  "crates/terminal/src/bin/vibex-terminal-feasibility.rs",
  "crates/terminal/src/emulator.rs",
  "crates/terminal/src/feasibility.rs",
  "crates/terminal/src/lib.rs",
  "crates/vibex-terminal-ui/Cargo.toml",
  "crates/vibex-terminal-ui/src",
  "docs/licenses/desktop-policy.json",
  "docs/licenses/desktop.cdx.json",
  "docs/licenses/desktop-third-party-notices.md",
  PDF_REVIEW_PATH,
  PDF_FIXTURE_PATH,
  "scripts/check-graph.mjs",
  "scripts/check-licenses.mjs",
  "scripts/capture-terminal.mjs",
  "scripts/check-hosted-runner-evidence.mjs",
  ".github/workflows/native-gate.yml"
];

function fail(message) {
  throw new Error(message);
}

function rootPath(path) {
  const absolute = resolve(ROOT, path);
  if (absolute !== ROOT && !absolute.startsWith(`${ROOT}${sep}`)) {
    fail(`path escapes repository: ${path}`);
  }
  return absolute;
}

function posixPath(path) {
  return path.split(sep).join("/");
}

function read(path) {
  return readFileSync(rootPath(path), "utf8");
}

function readJson(path) {
  return JSON.parse(read(path));
}

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function fileIdentity(path) {
  const absolute = rootPath(path);
  if (!existsSync(absolute) || !statSync(absolute).isFile()) {
    fail(`required source input is missing: ${path}`);
  }
  const content = readFileSync(absolute);
  return { path, bytes: content.length, sha256: sha256(content) };
}

function sourceInputs() {
  return SOURCE_INPUT_PATHS.map(fileIdentity);
}

function sameJson(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function deepClone(value) {
  return JSON.parse(JSON.stringify(value));
}

function replaceControlCharacters(value) {
  return Array.from(String(value), (character) => {
    const code = character.codePointAt(0);
    return code < 32 || code === 127 ? " " : character;
  }).join("");
}

function containsControlCharacters(value) {
  return Array.from(value).some((character) => {
    const code = character.codePointAt(0);
    return code < 32 || code === 127;
  });
}

function sanitizedFailureSummary(value) {
  let summary = replaceControlCharacters(value ?? "unspecified failure")
    .replace(/\s+/g, " ")
    .trim();
  for (const [path, replacement] of [
    [ROOT, "<repository>"],
    [process.env.RUNNER_TEMP, "<runner-temp>"],
    [process.env.HOME, "<home>"],
    [process.env.USERPROFILE, "<home>"]
  ]) {
    if (path) summary = summary.split(path).join(replacement);
  }
  return summary.slice(0, 512) || "unspecified failure";
}

function assertExactIds(actual, expected, label) {
  if (!sameJson(actual, expected)) {
    fail(`${label} must be exactly ${expected.join(", ")}; found ${actual.join(", ")}`);
  }
}

function validatePolicy(policy) {
  if (policy.schemaVersion !== "hosted-runner-policy.v1") {
    fail(`${POLICY_PATH} has an unsupported schemaVersion`);
  }
  if (policy.policyDecision?.status !== "approved" || policy.policyDecision?.linuxScopeUnchanged !== true) {
    fail(`${POLICY_PATH} must retain the approved hosted scope without weakening Linux`);
  }
  if (
    policy.toolchain?.rust !== "1.97.0" ||
    policy.toolchain?.nodeMajor !== 22 ||
    policy.toolchain?.pnpm !== "11.3.0" ||
    policy.toolchain?.cargoPackager !== "0.11.8"
  ) {
    fail(`${POLICY_PATH} toolchain drifted from the reviewed versions`);
  }

  assertExactIds((policy.actions ?? []).map((action) => action.id), Object.keys(EXPECTED_ACTIONS), "actions");
  for (const action of policy.actions) {
    const expected = EXPECTED_ACTIONS[action.id];
    if (
      action.repository !== expected[0] ||
      action.revision !== expected[1] ||
      action.release !== expected[2]
    ) {
      fail(`action ${action.id} drifted from the reviewed revision`);
    }
  }

  assertExactIds(policy.requiredTargets ?? [], Object.keys(EXPECTED_TARGETS), "requiredTargets");
  assertExactIds(
    (policy.requiredDecisionChecks ?? []).map((check) => check.id),
    EXPECTED_CHECK_IDS,
    "requiredDecisionChecks"
  );
  for (const check of policy.requiredDecisionChecks) {
    if (!check.description?.trim()) fail(`decision check ${check.id} is missing a description`);
  }

  assertExactIds(
    (policy.requiredSkippedClaims ?? []).map((claim) => claim.id),
    EXPECTED_SKIP_IDS,
    "requiredSkippedClaims"
  );
  for (const claim of policy.requiredSkippedClaims) {
    if (
      claim.status !== "skipped_by_product_decision" ||
      claim.decisionImpact !== false ||
      claim.decisionDenominator !== "excluded" ||
      claim.notEvidenceOfParity !== true ||
      !claim.reason?.trim()
    ) {
      fail(`skip ${claim.id} must be an explicit non-decision, non-parity exclusion`);
    }
  }

  const targets = policy.targets ?? [];
  assertExactIds(targets.map((target) => target.id), Object.keys(EXPECTED_TARGETS), "targets");
  for (const target of targets) {
    const expected = EXPECTED_TARGETS[target.id];
    for (const field of ["runnerLabel", "runnerOs", "nodePlatform", "packageFormat"]) {
      if (target[field] !== expected[field]) {
        fail(`target ${target.id} ${field} must be ${expected[field]}`);
      }
    }
    if (!Array.isArray(target.allowedArchitectures) || target.allowedArchitectures.length === 0) {
      fail(`target ${target.id} has no allowed architecture`);
    }
    if (!target.productName?.trim() || !target.installedBinaryRelativePath?.trim()) {
      fail(`target ${target.id} is missing package lifecycle identity`);
    }
  }
  return policy;
}

function validateWorkflowBinding(policy) {
  if (!existsSync(rootPath(WORKFLOW_PATH))) fail(`${WORKFLOW_PATH} is missing`);
  const workflow = read(WORKFLOW_PATH);
  for (const required of [
    "workflow_dispatch:",
    "runs-on: ${{ matrix.runner }}",
    "--run-target ${{ matrix.id }}",
    "--merge",
    `CARGO_PACKAGER_VERSION: "${policy.toolchain.cargoPackager}"`,
    `node-version: ${policy.toolchain.nodeMajor}`,
    `version: ${policy.toolchain.pnpm}`
  ]) {
    if (!workflow.includes(required)) fail(`${WORKFLOW_PATH} is missing ${JSON.stringify(required)}`);
  }
  for (const target of policy.targets) {
    if (!workflow.includes(`id: ${target.id}`) || !workflow.includes(`runner: ${target.runnerLabel}`)) {
      fail(`${WORKFLOW_PATH} does not bind ${target.id} to ${target.runnerLabel}`);
    }
  }
  for (const action of policy.actions) {
    if (!workflow.includes(`${action.repository}@${action.revision}`)) {
      fail(`${WORKFLOW_PATH} does not pin ${action.repository} at ${action.revision}`);
    }
  }
  for (const forbidden of ["screenshot", "ime-test", "input-test", "dpi-test", "multi-monitor-test"]) {
    if (workflow.toLowerCase().includes(forbidden)) {
      fail(`${WORKFLOW_PATH} contains forbidden hosted GUI test token ${forbidden}`);
    }
  }
}

function loadPolicy() {
  return validatePolicy(readJson(POLICY_PATH));
}

function execute(command, args, options = {}) {
  const started = Date.now();
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? ROOT,
    env: options.env ?? process.env,
    encoding: "utf8",
    maxBuffer: COMMAND_BUFFER_BYTES,
    shell: options.shell ?? false,
    windowsHide: true
  });
  const stdout = result.stdout ?? "";
  const stderr = result.stderr ?? "";
  if (options.echoOutput !== false) {
    if (stdout) process.stdout.write(stdout);
    if (stderr) process.stderr.write(stderr);
  }
  return {
    status: result.status,
    signal: result.signal,
    error: result.error?.message ?? null,
    stdout,
    stderr,
    durationMs: Date.now() - started
  };
}

function checkById(evidence, id) {
  return evidence.checks.find((check) => check.id === id);
}

function dependenciesPassed(evidence, dependencies) {
  return dependencies.every((id) => checkById(evidence, id)?.status === "passed");
}

function blockedCheck(evidence, id, dependencies, command) {
  const check = {
    id,
    status: "blocked_by_failed_dependency",
    decisionImpact: true,
    command,
    exitCode: null,
    durationMs: 0,
    blockedBy: dependencies.filter((dependency) => checkById(evidence, dependency)?.status !== "passed")
  };
  evidence.checks.push(check);
  return { check, result: null };
}

function runCommandCheck(evidence, id, command, args, options = {}) {
  const dependencies = options.dependencies ?? [];
  const displayCommand = options.displayCommand ?? [command, ...args].join(" ");
  if (!dependenciesPassed(evidence, dependencies)) {
    return blockedCheck(evidence, id, dependencies, displayCommand);
  }
  console.log(`\n[${id}] ${displayCommand}`);
  const result = execute(command, args, options);
  const passed = result.status === 0 && !result.error;
  const check = {
    id,
    status: passed ? "passed" : "failed",
    decisionImpact: true,
    command: displayCommand,
    exitCode: result.status,
    durationMs: result.durationMs
  };
  if (!passed) {
    check.failure = {
      kind: result.error ? "command_start_failed" : "nonzero_exit",
      summary: sanitizedFailureSummary(
        result.error ?? `command exited with ${result.status ?? result.signal ?? "unknown status"}`
      )
    };
  }
  evidence.checks.push(check);
  return { check, result };
}

function markFailed(check, summary) {
  check.status = "failed";
  check.failure = { kind: "invalid_observation", summary: sanitizedFailureSummary(summary) };
}

function addObservedCheck(evidence, id, passed, observation, failureSummary = null) {
  const check = {
    id,
    status: passed ? "passed" : "failed",
    decisionImpact: true,
    command: "structured runner observation",
    exitCode: passed ? 0 : 1,
    durationMs: observation.durationMs ?? 0,
    observation
  };
  if (!passed) {
    check.failure = {
      kind: "invalid_observation",
      summary: sanitizedFailureSummary(failureSummary)
    };
  }
  evidence.checks.push(check);
  return check;
}

function commandName(name) {
  return process.platform === "win32" && name === "pnpm" ? "pnpm.cmd" : name;
}

function firstLine(value) {
  return value.trim().split(/\r?\n/, 1)[0] ?? "";
}

function collectToolchain(policy, evidence) {
  const commands = [
    ["rustc", ["--version"], "rustc"],
    ["cargo", ["--version"], "cargo"],
    [process.execPath, ["--version"], "node"],
    [commandName("pnpm"), ["--version"], "pnpm"],
    ["cargo", ["packager", "--version"], "cargoPackager"]
  ];
  const values = {};
  const failures = [];
  let durationMs = 0;
  for (const [command, args, key] of commands) {
    const result = execute(command, args, { shell: process.platform === "win32" && key === "pnpm" });
    durationMs += result.durationMs;
    values[key] = firstLine(result.stdout || result.stderr);
    if (result.status !== 0 || result.error) failures.push(key);
  }
  const matches =
    failures.length === 0 &&
    values.rustc.startsWith(`rustc ${policy.toolchain.rust} `) &&
    values.cargo.startsWith(`cargo ${policy.toolchain.rust} `) &&
    Number(values.node.match(/^v(\d+)/)?.[1]) === policy.toolchain.nodeMajor &&
    values.pnpm === policy.toolchain.pnpm &&
    values.cargoPackager.endsWith(` ${policy.toolchain.cargoPackager}`);
  evidence.toolchain = values;
  addObservedCheck(
    evidence,
    "pinned_toolchain",
    matches,
    { ...values, durationMs },
    failures.length ? `tool commands failed: ${failures.join(", ")}` : "tool versions do not match policy"
  );
}

function validateProbe(probe, targetId, architecture) {
  if (
    probe?.schemaVersion !== "first-frame-probe.v1" ||
    probe.status !== "compiled_probe" ||
    probe.dependencySourcePolicy !== GPUI_DEPENDENCY_SOURCE_POLICY ||
    probe.platform !== targetId ||
    probe.architecture !== architecture ||
    typeof probe.displayBackend !== "string" ||
    probe.defaultWidth !== 1200 ||
    probe.defaultHeight !== 780 ||
    probe.minWidth !== 360 ||
    probe.minHeight !== 620 ||
    probe.borderless !== true ||
    probe.nativePixelsVerified !== false ||
    probe.componentStory !== "root_button_status"
  ) {
    fail(`invalid GPUI probe for ${targetId}`);
  }
  return probe;
}

function runProbeCheck(evidence, id, binaryPath, targetId, architecture, dependency) {
  if (!dependenciesPassed(evidence, [dependency])) {
    return blockedCheck(evidence, id, [dependency], "vibex-desktop --probe");
  }
  const result = execute(binaryPath, ["--probe"], { echoOutput: true });
  const check = {
    id,
    status: result.status === 0 ? "passed" : "failed",
    decisionImpact: true,
    command: "vibex-desktop --probe",
    exitCode: result.status,
    durationMs: result.durationMs
  };
  if (check.status === "passed") {
    try {
      evidence.probes[id === "direct_probe" ? "direct" : "packaged"] = validateProbe(
        JSON.parse(result.stdout),
        targetId,
        architecture
      );
    } catch (error) {
      markFailed(check, error.message);
    }
  } else {
    check.failure = { kind: "nonzero_exit", summary: `probe exited with ${result.status}` };
  }
  evidence.checks.push(check);
  return { check, result };
}

function validateTerminalProbe(report, targetId, architecture) {
  const expectedBackend = targetId === "windows" ? "portable-pty-conpty" : "portable-pty-openpty";
  if (
    report?.schemaVersion !== "vibex-terminal-feasibility-run.v1" ||
    report.status !== "passed" ||
    report.platform !== targetId ||
    report.architecture !== architecture ||
    report.engine?.name !== "alacritty_terminal" ||
    report.engine.version !== ALACRITTY_VERSION ||
    report.engine.integration !== "termy-compatible-bounded-core-fallback" ||
    report.pty?.backend !== expectedBackend ||
    report.pty.windowsConptyExercised !== (targetId === "windows") ||
    report.pty.rawBytesObserved !== true ||
    report.pty.invalidUtf8Observed !== true ||
    report.pty.cjkObserved !== true ||
    report.pty.resizeRequested?.rows !== 42 ||
    report.pty.resizeRequested?.columns !== 132 ||
    report.pty.resizeObserved !== true ||
    report.pty.processExited !== true ||
    report.pty.rawDroppedChunks !== 0 ||
    report.emulator?.cjkCellsObserved !== true ||
    report.emulator.selectionCopyObserved !== true ||
    report.emulator.alternateScreenEntered !== true ||
    report.emulator.primaryScreenRestored !== true ||
    report.emulator.resizeObserved !== true ||
    report.throughput?.fixtureBytes !== 10 * 1024 * 1024 ||
    report.throughput.fixtureSha256 !== ZERO_10_MIB_SHA256 ||
    !Number.isInteger(report.throughput.elapsedMs) ||
    report.throughput.elapsedMs <= 0 ||
    !Number.isFinite(report.throughput.mebibytesPerSecond) ||
    report.throughput.mebibytesPerSecond <= 0 ||
    report.throughput.dataLossObserved !== false ||
    report.rawTextStored !== false
  ) {
    fail(`invalid Terminal feasibility probe for ${targetId}`);
  }
  const serialized = JSON.stringify(report);
  for (const forbidden of ["VIBEX_RAW_BEGIN", "VIBEX_CJK:", '"rawText"']) {
    if (serialized.includes(forbidden)) fail(`Terminal probe for ${targetId} retained raw text`);
  }
  return report;
}

function runTerminalProbeCheck(evidence, binaryPath, targetId, architecture) {
  if (!dependenciesPassed(evidence, ["release_link"])) {
    return blockedCheck(
      evidence,
      "terminal_feasibility",
      ["release_link"],
      "vibex-desktop --spike-terminal <sanitized-output>"
    );
  }
  const output = rootPath(`target/hosted-evidence/${targetId}-terminal.json`);
  mkdirSync(dirname(output), { recursive: true });
  rmSync(output, { force: true });
  const result = execute(binaryPath, ["--spike-terminal", output]);
  const check = {
    id: "terminal_feasibility",
    status: result.status === 0 && !result.error ? "passed" : "failed",
    decisionImpact: true,
    command: "vibex-desktop --spike-terminal <sanitized-output>",
    exitCode: result.status,
    durationMs: result.durationMs
  };
  if (check.status === "passed") {
    try {
      if (!existsSync(output)) fail("Terminal probe did not write its report");
      evidence.terminalProbe = validateTerminalProbe(
        JSON.parse(readFileSync(output, "utf8")),
        targetId,
        architecture
      );
    } catch (error) {
      markFailed(check, error.message);
      check.exitCode = 1;
    }
  } else {
    check.failure = {
      kind: result.error ? "command_start_failed" : "nonzero_exit",
      summary: sanitizedFailureSummary(
        result.error ?? `terminal probe exited with ${result.status ?? result.signal ?? "unknown status"}`
      )
    };
  }
  rmSync(output, { force: true });
  evidence.checks.push(check);
  return { check, result };
}

function validatePdfProbe(probe, targetId, architecture) {
  const review = readJson(PDF_REVIEW_PATH);
  const expectedTarget = `${targetId}-${architecture}`;
  const archive = review.archives?.find((candidate) => candidate.target === expectedTarget);
  const report = probe?.run;
  const fixture = readFileSync(rootPath(PDF_FIXTURE_PATH));
  if (
    review.schemaVersion !== "vibex-pdfium-native-review.v1" ||
    review.engine?.build !== "7881" ||
    review.review?.status !== "approved_for_linux_distribution" ||
    review.review.nativeBinaryRegisteredForDistribution !== true ||
    !review.review.deferredRuntimeTargets?.includes(expectedTarget) ||
    !archive ||
    !sameJson(probe.nativeInput, {
      target: archive.target,
      release: review.engine.binaryRelease,
      asset: archive.asset,
      archiveSha256: archive.archiveSha256,
      archiveBytes: archive.archiveBytes,
      librarySha256: archive.librarySha256,
      libraryBytes: archive.libraryBytes
    }) ||
    report?.schemaVersion !== "vibex-pdf-feasibility-run.v1" ||
    report.status !== "passed" ||
    report.platform !== targetId ||
    report.architecture !== architecture ||
    report.engine?.wrapper !== "pdfium-render" ||
    report.engine.wrapperVersion !== "0.9.3" ||
    report.engine.pdfiumVersion !== "7881" ||
    report.engine.binding !== "explicit-dynamic-library" ||
    report.engine.processModel !== "in-process-native-library" ||
    report.engine.childProcessesStarted !== 0 ||
    report.fixture?.bytes !== fixture.length ||
    report.fixture.sha256 !== sha256(fixture) ||
    report.fixture.pageCount !== 12 ||
    report.fixture.cjkTextExtracted !== true ||
    report.fixture.embeddedFontMarkerPresent !== true ||
    report.rendering?.fit?.width !== 960 ||
    report.rendering.zoom150Percent?.width !== 1440 ||
    report.rendering.fit.rgbaBytes !==
      report.rendering.fit.width * report.rendering.fit.height * 4 ||
    report.rendering.zoom150Percent.rgbaBytes !==
      report.rendering.zoom150Percent.width * report.rendering.zoom150Percent.height * 4 ||
    report.rendering.fit.sampledUniqueColors < 16 ||
    report.rendering.zoom150Percent.sampledUniqueColors < 16 ||
    !/^[a-f0-9]{64}$/.test(report.rendering.fit.rgbaSha256 ?? "") ||
    !/^[a-f0-9]{64}$/.test(report.rendering.zoom150Percent.rgbaSha256 ?? "") ||
    !Number.isFinite(report.rendering.fit.elapsedMs) ||
    report.rendering.fit.elapsedMs <= 0 ||
    !Number.isFinite(report.rendering.zoom150Percent.elapsedMs) ||
    report.rendering.zoom150Percent.elapsedMs <= 0 ||
    report.rendering.aspectRatioPreserved !== true ||
    report.rendering.distinctZoomOutput !== true ||
    report.rendering.previewRawRgbaWritten !== true ||
    report.virtualization?.strategy !== "visible-two-pages-plus-one-page-overscan-lru" ||
    report.virtualization.visiblePages !== 2 ||
    report.virtualization.overscanPagesPerSide !== 1 ||
    report.virtualization.cacheBudgetBytes !== 24 * 1024 * 1024 ||
    report.virtualization.viewportSteps !== 24 ||
    report.virtualization.cacheHits <= 0 ||
    report.virtualization.cacheMisses <= 0 ||
    report.virtualization.evictions <= 0 ||
    report.virtualization.maximumResidentBytes > report.virtualization.cacheBudgetBytes ||
    report.virtualization.cacheBudgetRespected !== true ||
    report.errorHandling?.invalidDocumentRejected !== true ||
    report.errorHandling.loadingErrorIsStructured !== true ||
    report.memory?.currentRssBeforeKib !== null ||
    report.memory.currentRssAfterKib !== null ||
    report.memory.processPeakRssKib !== null ||
    report.memory.measurementSource !== "unavailable-on-this-platform" ||
    report.privacy?.documentTextStored !== false ||
    report.privacy.nativeLibraryPathStored !== false ||
    report.privacy.fixturePathStored !== false
  ) {
    fail(`invalid PDFium feasibility probe for ${targetId}`);
  }
  const serialized = JSON.stringify(probe);
  for (const forbidden of [ROOT, process.env.HOME, process.env.USERPROFILE, "PDFIUM_LIB_PATH"]) {
    if (forbidden && serialized.includes(forbidden)) {
      fail(`PDFium probe for ${targetId} retained a private path`);
    }
  }
  return probe;
}

async function downloadPdfiumArchive(url) {
  let lastError;
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    try {
      return await downloadHttps(url);
    } catch (error) {
      lastError = error;
      if (attempt < 3) await delay(attempt * 1000);
    }
  }
  throw lastError;
}

function downloadHttps(url, redirects = 0) {
  if (redirects > 5) return Promise.reject(new Error("PDFium download exceeded five redirects"));
  return new Promise((resolveDownload, rejectDownload) => {
    const request = get(url, { headers: { "user-agent": "vibex-hosted-gate" } }, (response) => {
      if (
        response.statusCode >= 300 &&
        response.statusCode < 400 &&
        response.headers.location
      ) {
        response.resume();
        resolveDownload(
          downloadHttps(new URL(response.headers.location, url).toString(), redirects + 1)
        );
        return;
      }
      if (response.statusCode !== 200) {
        response.resume();
        rejectDownload(new Error(`PDFium download returned HTTP ${response.statusCode}`));
        return;
      }
      const chunks = [];
      let bytes = 0;
      response.on("data", (chunk) => {
        bytes += chunk.length;
        if (bytes > 16 * 1024 * 1024) {
          request.destroy(new Error("PDFium download exceeded 16 MiB"));
          return;
        }
        chunks.push(chunk);
      });
      response.on("end", () => resolveDownload(Buffer.concat(chunks)));
      response.on("error", rejectDownload);
    });
    request.setTimeout(60_000, () => request.destroy(new Error("PDFium download timed out")));
    request.on("error", rejectDownload);
  });
}

async function runPdfProbeCheck(evidence, binaryPath, targetId, architecture) {
  if (!dependenciesPassed(evidence, ["release_link"])) {
    return blockedCheck(
      evidence,
      "pdfium_headless_runtime",
      ["release_link"],
      "vibex-desktop --spike-pdf <pinned-library> <fixture> <sanitized-output>"
    );
  }
  const started = Date.now();
  const check = {
    id: "pdfium_headless_runtime",
    status: "failed",
    decisionImpact: true,
    command: "vibex-desktop --spike-pdf <pinned-library> <fixture> <sanitized-output>",
    exitCode: 1,
    durationMs: 0
  };
  const temporaryRoot = rootPath(`target/hosted-pdf/${targetId}`);
  try {
    const review = readJson(PDF_REVIEW_PATH);
    const target = `${targetId}-${architecture}`;
    const nativeInput = review.archives?.find((archive) => archive.target === target);
    if (!nativeInput) fail(`PDFium review has no archive for ${target}`);
    rmSync(temporaryRoot, { recursive: true, force: true });
    const extracted = join(temporaryRoot, "extracted");
    mkdirSync(extracted, { recursive: true });
    const archivePath = join(temporaryRoot, nativeInput.asset);
    const archiveBytes = await downloadPdfiumArchive(`${PDFIUM_RELEASE_URL}/${nativeInput.asset}`);
    if (
      archiveBytes.length !== nativeInput.archiveBytes ||
      sha256(archiveBytes) !== nativeInput.archiveSha256
    ) {
      fail(`PDFium archive identity differs for ${target}`);
    }
    writeFileSync(archivePath, archiveBytes);
    const extraction = execute("tar", ["-xzf", archivePath, "-C", extracted], {
      echoOutput: false
    });
    if (extraction.status !== 0 || extraction.error) {
      fail(extraction.error ?? `PDFium archive extraction exited with ${extraction.status}`);
    }
    const libraryPath = join(extracted, nativeInput.libraryPath);
    const libraryBytes = readFileSync(libraryPath);
    if (
      libraryBytes.length !== nativeInput.libraryBytes ||
      sha256(libraryBytes) !== nativeInput.librarySha256
    ) {
      fail(`PDFium library identity differs for ${target}`);
    }
    const reportPath = join(temporaryRoot, "report.json");
    const previewPath = join(temporaryRoot, "page-1.rgba");
    const result = execute(binaryPath, [
      "--spike-pdf",
      libraryPath,
      rootPath(PDF_FIXTURE_PATH),
      reportPath,
      previewPath
    ]);
    check.exitCode = result.status;
    if (result.status !== 0 || result.error) {
      fail(result.error ?? `PDFium probe exited with ${result.status ?? result.signal}`);
    }
    if (!existsSync(reportPath) || !existsSync(previewPath)) {
      fail("PDFium probe did not write its structured report and raw preview");
    }
    const probe = {
      nativeInput: {
        target: nativeInput.target,
        release: review.engine.binaryRelease,
        asset: nativeInput.asset,
        archiveSha256: nativeInput.archiveSha256,
        archiveBytes: nativeInput.archiveBytes,
        librarySha256: nativeInput.librarySha256,
        libraryBytes: nativeInput.libraryBytes
      },
      run: JSON.parse(readFileSync(reportPath, "utf8"))
    };
    validatePdfProbe(probe, targetId, architecture);
    if (statSync(previewPath).size !== probe.run.rendering.fit.rgbaBytes) {
      fail("PDFium raw preview byte count is invalid");
    }
    evidence.pdfProbe = probe;
    check.status = "passed";
    check.exitCode = 0;
  } catch (error) {
    check.failure = {
      kind: "pdfium_probe_failed",
      summary: sanitizedFailureSummary(error.message)
    };
  } finally {
    check.durationMs = Date.now() - started;
    rmSync(temporaryRoot, { recursive: true, force: true });
  }
  evidence.checks.push(check);
  return { check };
}

function delay(ms) {
  return new Promise((resolveDelay) => setTimeout(resolveDelay, ms));
}

async function terminateObservedProcess(child, state) {
  if (state.exited) return true;
  child.kill("SIGTERM");
  await Promise.race([state.exitPromise, delay(2000)]);
  if (state.exited) return true;
  if (process.platform === "win32") {
    execute("taskkill.exe", ["/PID", String(child.pid), "/T", "/F"]);
  } else {
    child.kill("SIGKILL");
  }
  await Promise.race([state.exitPromise, delay(3000)]);
  return state.exited;
}

async function runPlatformInitialization(evidence, binaryPath) {
  if (!dependenciesPassed(evidence, ["release_link"])) {
    return blockedCheck(
      evidence,
      "platform_initialization",
      ["release_link"],
      "vibex-desktop (bounded initialization observation)"
    );
  }
  const started = Date.now();
  let exited = false;
  let exitCode = null;
  let exitSignal = null;
  let spawnError = null;
  const child = spawn(binaryPath, [], { cwd: ROOT, env: process.env, windowsHide: true });
  child.stdout?.on("data", (chunk) => process.stdout.write(chunk));
  child.stderr?.on("data", (chunk) => process.stderr.write(chunk));
  let resolveExit;
  const exitPromise = new Promise((resolvePromise) => {
    resolveExit = resolvePromise;
  });
  const state = {
    get exited() {
      return exited;
    },
    exitPromise
  };
  child.once("error", (error) => {
    spawnError = error.message;
    exited = true;
    resolveExit();
  });
  child.once("exit", (code, signal) => {
    exitCode = code;
    exitSignal = signal;
    exited = true;
    resolveExit();
  });

  await Promise.race([exitPromise, delay(PLATFORM_SETTLE_MS)]);
  const aliveAfterSettle = !exited && !spawnError;
  const processExitedAfterHarness = await terminateObservedProcess(child, state);
  const observation = {
    durationMs: Date.now() - started,
    settleMs: PLATFORM_SETTLE_MS,
    processAliveAfterSettle: aliveAfterSettle,
    terminatedByHarness: aliveAfterSettle && processExitedAfterHarness,
    processExitedAfterHarness,
    exitBeforeSettle: aliveAfterSettle ? null : { code: exitCode, signal: exitSignal },
    screenshotCaptured: false,
    windowPixelsObserved: false,
    inputInjected: false
  };
  return addObservedCheck(
    evidence,
    "platform_initialization",
    aliveAfterSettle && processExitedAfterHarness,
    observation,
    spawnError ??
      (aliveAfterSettle
        ? "GPUI process did not exit after bounded harness termination"
        : "GPUI process exited before the bounded initialization interval")
  );
}

function walkIdentity(root) {
  const entries = [];
  function walk(directory) {
    for (const name of readdirSync(directory).sort()) {
      const absolute = join(directory, name);
      const stat = lstatSync(absolute);
      const path = posixPath(relative(root, absolute));
      if (stat.isDirectory()) {
        entries.push({ path, type: "directory", bytes: 0, content: null });
        walk(absolute);
      } else if (stat.isSymbolicLink()) {
        const content = Buffer.from(readlinkSync(absolute));
        entries.push({ path, type: "symlink", bytes: content.length, content });
      } else if (stat.isFile()) {
        const content = readFileSync(absolute);
        entries.push({ path, type: "file", bytes: content.length, content });
      }
    }
  }
  walk(root);
  const hash = createHash("sha256");
  let bytes = 0;
  for (const entry of entries) {
    hash.update(entry.path);
    hash.update("\0");
    hash.update(entry.type);
    hash.update("\0");
    if (entry.content) hash.update(entry.content);
    hash.update("\0");
    bytes += entry.bytes;
  }
  return { kind: "directory", entries: entries.length, bytes, sha256: hash.digest("hex") };
}

function artifactIdentity(path) {
  const stat = lstatSync(path);
  if (stat.isDirectory()) return walkIdentity(path);
  if (!stat.isFile()) fail(`unsupported package artifact type: ${path}`);
  const content = readFileSync(path);
  return { kind: "file", entries: 1, bytes: content.length, sha256: sha256(content) };
}

function repositoryArtifactPath(path) {
  return posixPath(relative(ROOT, path));
}

function findPackageArtifact(packageDir, target) {
  if (target.packageFormat === "app") {
    const app = join(packageDir, `${target.productName}.app`);
    if (!existsSync(app) || !lstatSync(app).isDirectory()) fail(`macOS app bundle was not produced: ${app}`);
    const icon = join(app, "Contents", "Resources", `${target.productName}.icns`);
    if (!existsSync(icon) || !statSync(icon).isFile()) fail("macOS package is missing the registered icon");
    return { artifactPath: app, iconPath: icon };
  }
  const installers = readdirSync(packageDir)
    .filter((name) => name.endsWith("-setup.exe"))
    .map((name) => join(packageDir, name));
  if (installers.length !== 1) fail(`expected one NSIS installer, found ${installers.length}`);
  return { artifactPath: installers[0], iconPath: rootPath("apps/desktop/assets/app-icons/icon.ico") };
}

function installMacPackage(artifactPath, installRoot, target) {
  const applications = join(installRoot, "Applications");
  const installedApp = join(applications, `${target.productName}.app`);
  rmSync(installRoot, { recursive: true, force: true });
  mkdirSync(applications, { recursive: true });
  cpSync(artifactPath, installedApp, { recursive: true, errorOnExist: true });
  const binaryPath = join(installRoot, target.installedBinaryRelativePath);
  if (!existsSync(binaryPath)) fail("installed macOS package binary is missing");
  return binaryPath;
}

function installWindowsPackage(artifactPath, installRoot, target) {
  rmSync(installRoot, { recursive: true, force: true });
  mkdirSync(dirname(installRoot), { recursive: true });
  const result = execute(artifactPath, ["/S", "/NS", `/D=${installRoot}`]);
  if (result.status !== 0 || result.error) {
    fail(result.error ?? `NSIS installer exited with ${result.status}`);
  }
  const binaryPath = join(installRoot, target.installedBinaryRelativePath);
  if (!existsSync(binaryPath)) fail("installed Windows package binary is missing");
  if (!existsSync(join(installRoot, "uninstall.exe"))) fail("NSIS uninstaller is missing");
  return binaryPath;
}

function runInstallCheck(evidence, artifactPath, installRoot, target) {
  if (!dependenciesPassed(evidence, ["minimal_package"])) {
    return blockedCheck(
      evidence,
      "install_lifecycle",
      ["minimal_package"],
      `${target.packageFormat} isolated install`
    );
  }
  const started = Date.now();
  const check = {
    id: "install_lifecycle",
    status: "passed",
    decisionImpact: true,
    command: `${target.packageFormat} isolated install`,
    exitCode: 0,
    durationMs: 0
  };
  let binaryPath = null;
  try {
    binaryPath =
      target.id === "macos"
        ? installMacPackage(artifactPath, installRoot, target)
        : installWindowsPackage(artifactPath, installRoot, target);
  } catch (error) {
    markFailed(check, error.message);
    check.exitCode = 1;
  }
  check.durationMs = Date.now() - started;
  check.observation = {
    isolation: "runner_temp",
    packageFormat: target.packageFormat,
    installedBinaryRelativePath: target.installedBinaryRelativePath,
    installedBinaryPresent: Boolean(binaryPath)
  };
  evidence.checks.push(check);
  return { check, binaryPath };
}

async function waitForRemoval(path, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (existsSync(path) && Date.now() < deadline) await delay(250);
  return !existsSync(path);
}

async function runUninstallCheck(evidence, installRoot, target) {
  if (!dependenciesPassed(evidence, ["install_lifecycle"])) {
    return blockedCheck(
      evidence,
      "uninstall_lifecycle",
      ["install_lifecycle"],
      `${target.packageFormat} isolated uninstall`
    );
  }
  const started = Date.now();
  let passed = false;
  let failure = null;
  try {
    if (target.id === "macos") {
      rmSync(installRoot, { recursive: true, force: true });
      passed = !existsSync(installRoot);
    } else {
      const uninstaller = join(installRoot, "uninstall.exe");
      const result = execute(uninstaller, ["/S"]);
      if (result.status !== 0 || result.error) {
        fail(result.error ?? `NSIS uninstaller exited with ${result.status}`);
      }
      passed = await waitForRemoval(installRoot, 30000);
    }
    if (!passed) failure = "isolated installation still exists after uninstall";
  } catch (error) {
    failure = error.message;
  }
  const observation = {
    durationMs: Date.now() - started,
    installationRemoved: passed,
    installedBinaryRemoved: !existsSync(join(installRoot, target.installedBinaryRelativePath))
  };
  return addObservedCheck(
    evidence,
    "uninstall_lifecycle",
    passed && observation.installedBinaryRemoved,
    observation,
    failure ?? "installed binary remains after uninstall"
  );
}

function gitValue(args) {
  const result = execute("git", args, { echoOutput: false });
  if (result.status !== 0) fail(`git ${args.join(" ")} failed`);
  return result.stdout.trim();
}

function rustArchitecture() {
  if (process.arch === "x64") return "x86_64";
  if (process.arch === "arm64") return "aarch64";
  return process.arch;
}

function createTargetEvidence(policy, target) {
  const commit = process.env.GITHUB_SHA || gitValue(["rev-parse", "HEAD"]);
  return {
    schemaVersion: "hosted-runner-target-evidence.v1",
    generatedAt: new Date().toISOString(),
    policy: fileIdentity(POLICY_PATH),
    source: {
      commit,
      ref: process.env.GITHUB_REF ?? "local",
      inputs: sourceInputs()
    },
    target: {
      id: target.id,
      runnerLabel: target.runnerLabel,
      os: target.runnerOs,
      architecture: rustArchitecture(),
      packageFormat: target.packageFormat
    },
    runner: {
      provider: "github-hosted",
      githubActions: process.env.GITHUB_ACTIONS === "true",
      runnerOs: process.env.RUNNER_OS ?? "unreported",
      runnerArch: process.env.RUNNER_ARCH ?? "unreported",
      imageOs: process.env.ImageOS ?? "unreported",
      imageVersion: process.env.ImageVersion ?? "unreported"
    },
    toolchain: null,
    checks: [],
    skippedClaims: policy.requiredSkippedClaims,
    probes: {},
    terminalProbe: null,
    pdfProbe: null,
    package: null,
    decisionSummary: null,
    limitations: [
      "No real-window screenshot or native pixel comparison was captured.",
      "No IME, real input, DPI/scale transition, or multi-monitor behavior was exercised.",
      "A bounded live process observation proves initialization only, not window correctness or parity."
    ]
  };
}

function decisionSummary(checks, skipCount) {
  const count = (status) => checks.filter((check) => check.status === status).length;
  const passed = count("passed");
  const failed = count("failed");
  const blocked = count("blocked_by_failed_dependency");
  return {
    decisionBearingPassed: passed,
    decisionBearingFailed: failed,
    decisionBearingBlocked: blocked,
    nonDecisionSkipped: skipCount,
    decisionBearingSatisfied: passed === EXPECTED_CHECK_IDS.length && failed === 0 && blocked === 0
  };
}

function validateSource(source, checkCurrentInputs) {
  if (!/^[a-f0-9]{40}$/.test(source?.commit ?? "")) fail("source commit must be a full Git SHA");
  if (!source.ref?.trim()) fail("source ref is missing");
  const expectedPaths = SOURCE_INPUT_PATHS;
  assertExactIds((source.inputs ?? []).map((input) => input.path), expectedPaths, "source inputs");
  for (const input of source.inputs) {
    if (!Number.isInteger(input.bytes) || input.bytes <= 0 || !/^[a-f0-9]{64}$/.test(input.sha256)) {
      fail(`invalid source input identity for ${input.path}`);
    }
  }
  if (checkCurrentInputs && !sameJson(source.inputs, sourceInputs())) {
    fail("hosted evidence source inputs are stale");
  }
}

function validateChecks(checks, policy) {
  assertExactIds(checks.map((check) => check.id), EXPECTED_CHECK_IDS, "target checks");
  for (const check of checks) {
    if (!policy.requiredDecisionChecks.some((required) => required.id === check.id)) {
      fail(`unknown decision-bearing check ${check.id}`);
    }
    if (!new Set(["passed", "failed", "blocked_by_failed_dependency"]).has(check.status)) {
      fail(`invalid status ${check.status} for ${check.id}`);
    }
    if (check.decisionImpact !== true || !check.command?.trim()) {
      fail(`check ${check.id} must remain decision-bearing and name its command`);
    }
    if (!Number.isInteger(check.durationMs) || check.durationMs < 0) {
      fail(`check ${check.id} has an invalid duration`);
    }
    if (check.status === "passed" && (check.exitCode !== 0 || check.failure || check.blockedBy)) {
      fail(`passed check ${check.id} retains failure state`);
    }
    if (
      check.status === "failed" &&
      (!check.failure?.summary?.trim() ||
        check.failure.summary.length > 512 ||
        containsControlCharacters(check.failure.summary))
    ) {
      fail(`failed check ${check.id} has an invalid failure summary`);
    }
    if (
      check.status === "blocked_by_failed_dependency" &&
      (!Array.isArray(check.blockedBy) ||
        check.blockedBy.length === 0 ||
        check.exitCode !== null ||
        check.blockedBy.some((id) => !EXPECTED_CHECK_IDS.includes(id) || checkById({ checks }, id)?.status === "passed"))
    ) {
      fail(`blocked check ${check.id} is missing its failed dependency`);
    }
  }
}

function validateArtifactIdentity(identity, label) {
  if (
    !new Set(["file", "directory"]).has(identity?.kind) ||
    !Number.isInteger(identity.entries) ||
    identity.entries < 1 ||
    !Number.isInteger(identity.bytes) ||
    identity.bytes < 1 ||
    !/^[a-f0-9]{64}$/.test(identity.sha256 ?? "")
  ) {
    fail(`${label} has an invalid artifact identity`);
  }
}

function validateStoredRelativePath(path, label) {
  if (
    typeof path !== "string" ||
    !path.trim() ||
    path.startsWith("/") ||
    /^[A-Za-z]:[\\/]/.test(path) ||
    path.split(/[\\/]/).includes("..")
  ) {
    fail(`${label} must be a repository-relative or package-relative path`);
  }
}

function validateTargetEvidence(evidence, policy, options = {}) {
  if (evidence.schemaVersion !== "hosted-runner-target-evidence.v1") {
    fail("unsupported hosted target evidence schemaVersion");
  }
  const currentPolicyIdentity = fileIdentity(POLICY_PATH);
  if (!sameJson(evidence.policy, currentPolicyIdentity)) fail("target evidence policy identity is stale");
  validateSource(evidence.source, options.checkCurrentInputs !== false);
  const target = policy.targets.find((entry) => entry.id === evidence.target?.id);
  if (!target) fail(`unknown hosted target ${evidence.target?.id}`);
  if (
    evidence.target.runnerLabel !== target.runnerLabel ||
    evidence.target.os !== target.runnerOs ||
    evidence.target.packageFormat !== target.packageFormat ||
    !target.allowedArchitectures.includes(evidence.target.architecture)
  ) {
    fail(`target identity does not match policy for ${target.id}`);
  }
  if (
    evidence.runner?.provider !== "github-hosted" ||
    evidence.runner.githubActions !== true ||
    evidence.runner.runnerOs !== target.runnerOs
  ) {
    fail(`target ${target.id} was not recorded by its GitHub-hosted runner OS`);
  }
  const expectedRunnerArch =
    evidence.target.architecture === "x86_64"
      ? "X64"
      : evidence.target.architecture === "aarch64"
        ? "ARM64"
        : null;
  if (evidence.runner.runnerArch !== expectedRunnerArch) {
    fail(`target ${target.id} runner architecture does not match its binary architecture`);
  }
  validateChecks(evidence.checks ?? [], policy);
  for (const key of ["rustc", "cargo", "node", "pnpm", "cargoPackager"]) {
    if (typeof evidence.toolchain?.[key] !== "string") {
      fail(`target ${target.id} toolchain is missing ${key}`);
    }
  }
  const toolchainMatches =
    evidence.toolchain.rustc.startsWith(`rustc ${policy.toolchain.rust} `) &&
    evidence.toolchain.cargo.startsWith(`cargo ${policy.toolchain.rust} `) &&
    Number(evidence.toolchain.node.match(/^v(\d+)/)?.[1]) === policy.toolchain.nodeMajor &&
    evidence.toolchain.pnpm === policy.toolchain.pnpm &&
    evidence.toolchain.cargoPackager.endsWith(` ${policy.toolchain.cargoPackager}`);
  const toolchainCheck = checkById(evidence, "pinned_toolchain");
  if (
    !["rustc", "cargo", "node", "pnpm", "cargoPackager"].every(
      (key) => toolchainCheck.observation?.[key] === evidence.toolchain[key]
    )
  ) {
    fail(`target ${target.id} toolchain observation differs from its top-level identity`);
  }
  if (toolchainCheck.status === "passed" && !toolchainMatches) {
    fail(`target ${target.id} passed a toolchain check with mismatched versions`);
  }
  if (!sameJson(evidence.skippedClaims, policy.requiredSkippedClaims)) {
    fail(`target ${target.id} hosted skips drifted from policy`);
  }
  for (const claim of evidence.skippedClaims) {
    if (claim.decisionImpact !== false || claim.status !== "skipped_by_product_decision") {
      fail(`target ${target.id} skip ${claim.id} entered the decision denominator`);
    }
  }

  for (const [checkId, probeKey] of [
    ["direct_probe", "direct"],
    ["packaged_probe", "packaged"]
  ]) {
    if (checkById(evidence, checkId).status === "passed") {
      validateProbe(evidence.probes?.[probeKey], target.id, evidence.target.architecture);
    }
  }
  if (
    checkById(evidence, "direct_probe").status === "passed" &&
    checkById(evidence, "packaged_probe").status === "passed" &&
    !sameJson(evidence.probes.direct, evidence.probes.packaged)
  ) {
    fail(`target ${target.id} direct and packaged probes differ`);
  }
  if (checkById(evidence, "terminal_feasibility").status === "passed") {
    validateTerminalProbe(evidence.terminalProbe, target.id, evidence.target.architecture);
  }
  if (checkById(evidence, "pdfium_headless_runtime").status === "passed") {
    validatePdfProbe(evidence.pdfProbe, target.id, evidence.target.architecture);
  }
  if (checkById(evidence, "platform_initialization").status === "passed") {
    const observation = checkById(evidence, "platform_initialization").observation;
    if (
      observation?.processAliveAfterSettle !== true ||
      observation.settleMs !== PLATFORM_SETTLE_MS ||
      observation.processExitedAfterHarness !== true ||
      observation.terminatedByHarness !== true ||
      observation.screenshotCaptured !== false ||
      observation.windowPixelsObserved !== false ||
      observation.inputInjected !== false
    ) {
      fail(`target ${target.id} has an invalid platform initialization observation`);
    }
  }
  if (checkById(evidence, "minimal_package").status === "passed") {
    if (evidence.package?.format !== target.packageFormat) fail(`target ${target.id} package format drifted`);
    validateArtifactIdentity(evidence.package.artifact, `${target.id} package`);
    validateStoredRelativePath(evidence.package.artifact.path, `${target.id} package artifact`);
    if (!/^[a-f0-9]{64}$/.test(evidence.package.packageIcon?.sha256 ?? "")) {
      fail(`target ${target.id} package icon identity is missing`);
    }
    validateStoredRelativePath(evidence.package.packageIcon.path, `${target.id} package icon`);
  }
  if (checkById(evidence, "install_lifecycle").status === "passed") {
    const observation = checkById(evidence, "install_lifecycle").observation;
    if (
      observation?.isolation !== "runner_temp" ||
      observation.packageFormat !== target.packageFormat ||
      observation.installedBinaryRelativePath !== target.installedBinaryRelativePath ||
      observation.installedBinaryPresent !== true
    ) {
      fail(`target ${target.id} install observation is incomplete`);
    }
  }
  if (checkById(evidence, "artifact_hashes").status === "passed") {
    validateArtifactIdentity(evidence.package?.releaseBinary, `${target.id} release binary`);
    validateArtifactIdentity(evidence.package?.installedBinary, `${target.id} installed binary`);
    if (
      evidence.package.releaseBinary.sha256 !== evidence.package.installedBinary.sha256 ||
      evidence.package.binaryIdentityPreserved !== true
    ) {
      fail(`target ${target.id} installed binary identity differs from release`);
    }
    const expectedReleasePath =
      target.id === "windows"
        ? "target/release/vibex-desktop.exe"
        : "target/release/vibex-desktop";
    if (
      evidence.package.releaseBinary.path !== expectedReleasePath ||
      evidence.package.installedBinary.path !== target.installedBinaryRelativePath ||
      !sameJson(evidence.package.packagerConfig, fileIdentity(PACKAGER_CONFIG_PATH)) ||
      !sameJson(evidence.package.iconInputs, [
        fileIdentity("apps/desktop/assets/app-icons/icon.png"),
        fileIdentity("apps/desktop/assets/app-icons/icon.ico")
      ])
    ) {
      fail(`target ${target.id} package input identities are stale or incomplete`);
    }
  }
  if (checkById(evidence, "uninstall_lifecycle").status === "passed") {
    const observation = checkById(evidence, "uninstall_lifecycle").observation;
    if (observation?.installationRemoved !== true || observation.installedBinaryRemoved !== true) {
      fail(`target ${target.id} uninstall observation is incomplete`);
    }
  }
  const expectedSummary = decisionSummary(evidence.checks, policy.requiredSkippedClaims.length);
  if (!sameJson(evidence.decisionSummary, expectedSummary)) {
    fail(`target ${target.id} decision summary is inconsistent`);
  }
  return evidence;
}

function validateMatrixEvidence(matrix, policy, options = {}) {
  if (matrix.schemaVersion !== "hosted-runner-matrix-evidence.v1") {
    fail("unsupported hosted matrix evidence schemaVersion");
  }
  if (!sameJson(matrix.policy, fileIdentity(POLICY_PATH))) fail("matrix policy identity is stale");
  validateSource(matrix.source, options.checkCurrentInputs !== false);
  assertExactIds(matrix.requiredTargets ?? [], policy.requiredTargets, "matrix requiredTargets");
  assertExactIds((matrix.targets ?? []).map((entry) => entry.target.id), policy.requiredTargets, "matrix targets");
  for (const target of matrix.targets) validateTargetEvidence(target, policy, options);
  const satisfied = matrix.targets.every((target) => target.decisionSummary.decisionBearingSatisfied);
  const expectedSummary = {
    targetCount: policy.requiredTargets.length,
    decisionBearingTargetsPassed: matrix.targets.filter(
      (target) => target.decisionSummary.decisionBearingSatisfied
    ).length,
    decisionBearingTargetsFailed: matrix.targets.filter(
      (target) => !target.decisionSummary.decisionBearingSatisfied
    ).length,
    nonDecisionSkippedClaims: policy.requiredSkippedClaims.length * policy.requiredTargets.length,
    hostedGateSatisfied: satisfied
  };
  if (!sameJson(matrix.decisionSummary, expectedSummary) || matrix.hostedGateSatisfied !== satisfied) {
    fail("hosted matrix decision summary is inconsistent");
  }
  if (!sameJson(matrix.skippedClaims, policy.requiredSkippedClaims)) {
    fail("hosted matrix skip contract drifted from policy");
  }
  return matrix;
}

function writeJson(path, value) {
  const absolute = rootPath(path);
  mkdirSync(dirname(absolute), { recursive: true });
  writeFileSync(absolute, `${JSON.stringify(value, null, 2)}\n`);
}

async function runTarget(targetId, outputPath) {
  const policy = loadPolicy();
  validateWorkflowBinding(policy);
  const target = policy.targets.find((entry) => entry.id === targetId);
  if (!target) fail(`unknown target ${targetId}`);
  if (process.platform !== target.nodePlatform) {
    fail(`target ${targetId} must run on ${target.nodePlatform}, found ${process.platform}`);
  }
  const evidence = createTargetEvidence(policy, target);
  collectToolchain(policy, evidence);

  const metadata = runCommandCheck(
    evidence,
    "locked_cargo_metadata",
    "cargo",
    ["metadata", "--locked", "--format-version", "1"],
    { echoOutput: false }
  );
  if (metadata.check.status === "passed") {
    try {
      const graph = JSON.parse(metadata.result.stdout);
      if (!graph.workspace_members?.length || !graph.packages?.length) fail("Cargo metadata graph is empty");
    } catch (error) {
      markFailed(metadata.check, `Cargo metadata JSON is invalid: ${error.message}`);
    }
  }
  runCommandCheck(
    evidence,
    "locked_gpui_source_identity",
    process.execPath,
    ["scripts/check-graph.mjs"],
    { dependencies: ["locked_cargo_metadata"], displayCommand: "node scripts/check-graph.mjs" }
  );
  runCommandCheck(evidence, "workspace_rust_tests", "cargo", ["test", "--workspace", "--locked"]);
  runCommandCheck(evidence, "desktop_tests", "cargo", ["test", "-p", "vibex-desktop", "--locked"]);
  runCommandCheck(evidence, "frontend_quality", commandName("pnpm"), ["check:frontend"], {
    displayCommand: "pnpm check:frontend",
    shell: process.platform === "win32"
  });
  runCommandCheck(evidence, "supply_chain", process.execPath, ["scripts/check-licenses.mjs"], {
    dependencies: ["locked_cargo_metadata"],
    displayCommand: "node scripts/check-licenses.mjs"
  });
  runCommandCheck(
    evidence,
    "release_link",
    "cargo",
    ["build", "-p", "vibex-desktop", "--release", "--locked"]
  );

  const releaseBinary = rootPath(
    process.platform === "win32"
      ? "target/release/vibex-desktop.exe"
      : "target/release/vibex-desktop"
  );
  runProbeCheck(
    evidence,
    "direct_probe",
    releaseBinary,
    target.id,
    evidence.target.architecture,
    "release_link"
  );
  runTerminalProbeCheck(evidence, releaseBinary, target.id, evidence.target.architecture);
  await runPdfProbeCheck(evidence, releaseBinary, target.id, evidence.target.architecture);
  await runPlatformInitialization(evidence, releaseBinary);

  const packageDir = rootPath("target/hosted-packages");
  rmSync(packageDir, { recursive: true, force: true });
  mkdirSync(packageDir, { recursive: true });
  const packageResult = runCommandCheck(
    evidence,
    "minimal_package",
    "cargo",
    [
      "packager",
      "--config",
      PACKAGER_CONFIG_PATH,
      "--formats",
      target.packageFormat,
      "--out-dir",
      packageDir
    ],
    {
      dependencies: ["release_link"],
      displayCommand: `cargo packager --config ${PACKAGER_CONFIG_PATH} --formats ${target.packageFormat} --out-dir target/hosted-packages`
    }
  );

  let artifactPath = null;
  let packageIconPath = null;
  if (packageResult.check.status === "passed") {
    try {
      const found = findPackageArtifact(packageDir, target);
      artifactPath = found.artifactPath;
      packageIconPath = found.iconPath;
      const identity = artifactIdentity(artifactPath);
      evidence.package = {
        format: target.packageFormat,
        artifact: { path: repositoryArtifactPath(artifactPath), ...identity },
        packageIcon: {
          path:
            target.id === "macos"
              ? posixPath(relative(artifactPath, packageIconPath))
              : "apps/desktop/assets/app-icons/icon.ico",
          sha256: sha256(readFileSync(packageIconPath))
        },
        releaseBinary: null,
        installedBinary: null,
        binaryIdentityPreserved: null
      };
    } catch (error) {
      markFailed(packageResult.check, error.message);
    }
  }

  const installRoot = join(process.env.RUNNER_TEMP || rootPath("target"), "hosted-install", target.id);
  const installResult = runInstallCheck(evidence, artifactPath, installRoot, target);
  const installedBinary = installResult.binaryPath;
  runProbeCheck(
    evidence,
    "packaged_probe",
    installedBinary,
    target.id,
    evidence.target.architecture,
    "install_lifecycle"
  );

  if (!dependenciesPassed(evidence, ["minimal_package", "install_lifecycle"])) {
    blockedCheck(
      evidence,
      "artifact_hashes",
      ["minimal_package", "install_lifecycle"],
      "SHA-256 package and binary identity"
    );
  } else {
    const started = Date.now();
    let passed = false;
    let failure = null;
    try {
      const releaseIdentity = artifactIdentity(releaseBinary);
      const installedIdentity = artifactIdentity(installedBinary);
      evidence.package.releaseBinary = {
        path: process.platform === "win32" ? "target/release/vibex-desktop.exe" : "target/release/vibex-desktop",
        ...releaseIdentity
      };
      evidence.package.installedBinary = {
        path: target.installedBinaryRelativePath,
        ...installedIdentity
      };
      evidence.package.packagerConfig = fileIdentity(PACKAGER_CONFIG_PATH);
      evidence.package.iconInputs = [
        fileIdentity("apps/desktop/assets/app-icons/icon.png"),
        fileIdentity("apps/desktop/assets/app-icons/icon.ico")
      ];
      passed = releaseIdentity.sha256 === installedIdentity.sha256;
      evidence.package.binaryIdentityPreserved = passed;
      if (!passed) failure = "installed binary SHA-256 differs from the linked release binary";
    } catch (error) {
      failure = error.message;
    }
    addObservedCheck(
      evidence,
      "artifact_hashes",
      passed,
      {
        durationMs: Date.now() - started,
        algorithm: "SHA-256",
        binaryIdentityPreserved: passed
      },
      failure
    );
  }

  await runUninstallCheck(evidence, installRoot, target);
  evidence.decisionSummary = decisionSummary(evidence.checks, policy.requiredSkippedClaims.length);
  validateTargetEvidence(evidence, policy);
  writeJson(outputPath, evidence);
  console.log(
    `Hosted ${target.id} evidence written to ${outputPath}; decision-bearing gate ` +
      `${evidence.decisionSummary.decisionBearingSatisfied ? "passed" : "failed"}`
  );
  if (!evidence.decisionSummary.decisionBearingSatisfied) process.exitCode = 1;
}

function mergeTargets(inputPaths, outputPath) {
  const policy = loadPolicy();
  validateWorkflowBinding(policy);
  const targets = inputPaths.map((path) => validateTargetEvidence(readJson(path), policy));
  targets.sort(
    (left, right) => policy.requiredTargets.indexOf(left.target.id) - policy.requiredTargets.indexOf(right.target.id)
  );
  assertExactIds(targets.map((target) => target.target.id), policy.requiredTargets, "merge inputs");
  const source = targets[0].source;
  for (const target of targets.slice(1)) {
    if (!sameJson(target.source, source)) fail("hosted targets were not produced from identical source inputs");
  }
  const hostedGateSatisfied = targets.every((target) => target.decisionSummary.decisionBearingSatisfied);
  const matrix = {
    schemaVersion: "hosted-runner-matrix-evidence.v1",
    generatedAt: new Date().toISOString(),
    policy: fileIdentity(POLICY_PATH),
    source,
    requiredTargets: policy.requiredTargets,
    targets,
    skippedClaims: policy.requiredSkippedClaims,
    hostedGateSatisfied,
    decisionSummary: {
      targetCount: policy.requiredTargets.length,
      decisionBearingTargetsPassed: targets.filter(
        (target) => target.decisionSummary.decisionBearingSatisfied
      ).length,
      decisionBearingTargetsFailed: targets.filter(
        (target) => !target.decisionSummary.decisionBearingSatisfied
      ).length,
      nonDecisionSkippedClaims: policy.requiredSkippedClaims.length * policy.requiredTargets.length,
      hostedGateSatisfied
    },
    limitations: [
      "This matrix makes no macOS or Windows real-window pixel, IME, input, DPI, or multi-monitor parity claim.",
      "Skipped claims are excluded from the decision denominator and do not weaken Linux evidence requirements."
    ]
  };
  validateMatrixEvidence(matrix, policy);
  writeJson(outputPath, matrix);
  console.log(`Hosted matrix written to ${outputPath}; gate ${hostedGateSatisfied ? "passed" : "failed"}`);
  if (!hostedGateSatisfied) process.exitCode = 1;
}

function syntheticTerminalProbe(target) {
  return {
    schemaVersion: "vibex-terminal-feasibility-run.v1",
    status: "passed",
    platform: target.id,
    architecture: target.allowedArchitectures[0],
    engine: {
      name: "alacritty_terminal",
      version: ALACRITTY_VERSION,
      integration: "termy-compatible-bounded-core-fallback"
    },
    pty: {
      backend: target.id === "windows" ? "portable-pty-conpty" : "portable-pty-openpty",
      windowsConptyExercised: target.id === "windows",
      rawBytesObserved: true,
      invalidUtf8Observed: true,
      cjkObserved: true,
      resizeRequested: { rows: 42, columns: 132 },
      resizeObserved: true,
      inputBytesWritten: 1,
      processExited: true,
      rawDroppedChunks: 0
    },
    emulator: {
      cjkCellsObserved: true,
      selectionCopyObserved: true,
      alternateScreenEntered: true,
      primaryScreenRestored: true,
      resizeObserved: true,
      ingestedBytes: 1
    },
    throughput: {
      fixtureBytes: 10 * 1024 * 1024,
      fixtureSha256: ZERO_10_MIB_SHA256,
      elapsedMs: 1,
      mebibytesPerSecond: 10,
      dataLossObserved: false
    },
    rawTextStored: false
  };
}

function syntheticPdfProbe(target) {
  const review = readJson(PDF_REVIEW_PATH);
  const architecture = target.allowedArchitectures[0];
  const archive = review.archives.find((candidate) => candidate.target === `${target.id}-${architecture}`);
  const hash = "a".repeat(64);
  return {
    nativeInput: {
      target: archive.target,
      release: review.engine.binaryRelease,
      asset: archive.asset,
      archiveSha256: archive.archiveSha256,
      archiveBytes: archive.archiveBytes,
      librarySha256: archive.librarySha256,
      libraryBytes: archive.libraryBytes
    },
    run: {
      schemaVersion: "vibex-pdf-feasibility-run.v1",
      status: "passed",
      platform: target.id,
      architecture,
      engine: {
        wrapper: "pdfium-render",
        wrapperVersion: "0.9.3",
        pdfiumVersion: "7881",
        binding: "explicit-dynamic-library",
        processModel: "in-process-native-library",
        childProcessesStarted: 0
      },
      fixture: {
        bytes: statSync(rootPath(PDF_FIXTURE_PATH)).size,
        sha256: sha256(readFileSync(rootPath(PDF_FIXTURE_PATH))),
        pageCount: 12,
        cjkTextExtracted: true,
        embeddedFontMarkerPresent: true
      },
      rendering: {
        fit: {
          width: 960,
          height: 1358,
          rgbaBytes: 960 * 1358 * 4,
          rgbaSha256: hash,
          sampledUniqueColors: 16,
          elapsedMs: 1
        },
        zoom150Percent: {
          width: 1440,
          height: 2037,
          rgbaBytes: 1440 * 2037 * 4,
          rgbaSha256: "b".repeat(64),
          sampledUniqueColors: 16,
          elapsedMs: 1
        },
        aspectRatioPreserved: true,
        distinctZoomOutput: true,
        previewRawRgbaWritten: true
      },
      virtualization: {
        strategy: "visible-two-pages-plus-one-page-overscan-lru",
        visiblePages: 2,
        overscanPagesPerSide: 1,
        cacheBudgetBytes: 24 * 1024 * 1024,
        viewportSteps: 24,
        renderRequests: 88,
        cacheHits: 44,
        cacheMisses: 44,
        evictions: 40,
        maximumResidentPages: 4,
        maximumResidentBytes: 20_858_880,
        cacheBudgetRespected: true
      },
      errorHandling: { invalidDocumentRejected: true, loadingErrorIsStructured: true },
      memory: {
        currentRssBeforeKib: null,
        currentRssAfterKib: null,
        processPeakRssKib: null,
        measurementSource: "unavailable-on-this-platform"
      },
      privacy: {
        documentTextStored: false,
        nativeLibraryPathStored: false,
        fixturePathStored: false
      }
    }
  };
}

function syntheticTarget(policy, target) {
  const hash = "a".repeat(64);
  const probe = {
    schemaVersion: "first-frame-probe.v1",
    status: "compiled_probe",
    dependencySourcePolicy: GPUI_DEPENDENCY_SOURCE_POLICY,
    zedRevision: SOURCE_IDENTITIES.zedRevision,
    gpuiComponentRevision: SOURCE_IDENTITIES.gpuiComponentRevision,
    platform: target.id,
    architecture: target.allowedArchitectures[0],
    displayBackend: "unreported",
    defaultWidth: 1200,
    defaultHeight: 780,
    minWidth: 360,
    minHeight: 620,
    borderless: true,
    componentStory: "root_button_status",
    nativePixelsVerified: false
  };
  const checks = EXPECTED_CHECK_IDS.map((id) => ({
    id,
    status: "passed",
    decisionImpact: true,
    command: "synthetic self-test observation",
    exitCode: 0,
    durationMs: 1,
    ...(id === "pinned_toolchain"
      ? {
          observation: {
            rustc: `rustc ${policy.toolchain.rust} (self-test)`,
            cargo: `cargo ${policy.toolchain.rust} (self-test)`,
            node: `v${policy.toolchain.nodeMajor}.0.0`,
            pnpm: policy.toolchain.pnpm,
            cargoPackager: `cargo-packager ${policy.toolchain.cargoPackager}`
          }
        }
      : {}),
    ...(id === "platform_initialization"
      ? {
          observation: {
            processAliveAfterSettle: true,
            settleMs: PLATFORM_SETTLE_MS,
            processExitedAfterHarness: true,
            terminatedByHarness: true,
            screenshotCaptured: false,
            windowPixelsObserved: false,
            inputInjected: false
          }
        }
      : {}),
    ...(id === "install_lifecycle"
      ? {
          observation: {
            isolation: "runner_temp",
            packageFormat: target.packageFormat,
            installedBinaryRelativePath: target.installedBinaryRelativePath,
            installedBinaryPresent: true
          }
        }
      : {}),
    ...(id === "uninstall_lifecycle"
      ? { observation: { installationRemoved: true, installedBinaryRemoved: true } }
      : {})
  }));
  const identity = { kind: "file", entries: 1, bytes: 1, sha256: hash };
  return {
    schemaVersion: "hosted-runner-target-evidence.v1",
    generatedAt: new Date().toISOString(),
    policy: fileIdentity(POLICY_PATH),
    source: { commit: "b".repeat(40), ref: "self-test", inputs: sourceInputs() },
    target: {
      id: target.id,
      runnerLabel: target.runnerLabel,
      os: target.runnerOs,
      architecture: target.allowedArchitectures[0],
      packageFormat: target.packageFormat
    },
    runner: {
      provider: "github-hosted",
      githubActions: true,
      runnerOs: target.runnerOs,
      runnerArch: target.allowedArchitectures[0] === "x86_64" ? "X64" : "ARM64",
      imageOs: "self-test",
      imageVersion: "self-test"
    },
    toolchain: {
      rustc: `rustc ${policy.toolchain.rust} (self-test)`,
      cargo: `cargo ${policy.toolchain.rust} (self-test)`,
      node: `v${policy.toolchain.nodeMajor}.0.0`,
      pnpm: policy.toolchain.pnpm,
      cargoPackager: `cargo-packager ${policy.toolchain.cargoPackager}`
    },
    checks,
    skippedClaims: policy.requiredSkippedClaims,
    probes: { direct: probe, packaged: probe },
    terminalProbe: syntheticTerminalProbe(target),
    pdfProbe: syntheticPdfProbe(target),
    package: {
      format: target.packageFormat,
      artifact: { path: "self-test-package", ...identity },
      packageIcon: { path: "self-test-icon", sha256: hash },
      releaseBinary: {
        path:
          target.id === "windows"
            ? "target/release/vibex-desktop.exe"
            : "target/release/vibex-desktop",
        ...identity
      },
      installedBinary: { path: target.installedBinaryRelativePath, ...identity },
      packagerConfig: fileIdentity(PACKAGER_CONFIG_PATH),
      iconInputs: [
        fileIdentity("apps/desktop/assets/app-icons/icon.png"),
        fileIdentity("apps/desktop/assets/app-icons/icon.ico")
      ],
      binaryIdentityPreserved: true
    },
    decisionSummary: decisionSummary(checks, policy.requiredSkippedClaims.length),
    limitations: ["self-test"]
  };
}

function expectRejected(callback, label) {
  try {
    callback();
  } catch {
    return;
  }
  fail(`self-test did not reject ${label}`);
}

function selfTest() {
  const policy = loadPolicy();
  validateWorkflowBinding(policy);
  const targets = policy.targets.map((target) => syntheticTarget(policy, target));
  for (const target of targets) validateTargetEvidence(target, policy);
  const matrix = {
    schemaVersion: "hosted-runner-matrix-evidence.v1",
    generatedAt: new Date().toISOString(),
    policy: fileIdentity(POLICY_PATH),
    source: targets[0].source,
    requiredTargets: policy.requiredTargets,
    targets,
    skippedClaims: policy.requiredSkippedClaims,
    hostedGateSatisfied: true,
    decisionSummary: {
      targetCount: policy.requiredTargets.length,
      decisionBearingTargetsPassed: policy.requiredTargets.length,
      decisionBearingTargetsFailed: 0,
      nonDecisionSkippedClaims: policy.requiredSkippedClaims.length * policy.requiredTargets.length,
      hostedGateSatisfied: true
    },
    limitations: ["self-test"]
  };
  validateMatrixEvidence(matrix, policy);
  const failedToolchain = deepClone(targets[0]);
  failedToolchain.toolchain.rustc = "rustc 0.0.0 (self-test)";
  failedToolchain.checks[0].observation.rustc = "rustc 0.0.0 (self-test)";
  failedToolchain.checks[0].status = "failed";
  failedToolchain.checks[0].exitCode = 1;
  failedToolchain.checks[0].failure = {
    kind: "invalid_observation",
    summary: "synthetic toolchain mismatch"
  };
  failedToolchain.decisionSummary = decisionSummary(
    failedToolchain.checks,
    policy.requiredSkippedClaims.length
  );
  validateTargetEvidence(failedToolchain, policy);
  const failedMatrix = deepClone(matrix);
  failedMatrix.targets[0] = failedToolchain;
  failedMatrix.hostedGateSatisfied = false;
  failedMatrix.decisionSummary = {
    targetCount: policy.requiredTargets.length,
    decisionBearingTargetsPassed: 1,
    decisionBearingTargetsFailed: 1,
    nonDecisionSkippedClaims: policy.requiredSkippedClaims.length * policy.requiredTargets.length,
    hostedGateSatisfied: false
  };
  validateMatrixEvidence(failedMatrix, policy);
  const badSkip = deepClone(targets[0]);
  badSkip.skippedClaims[0].decisionImpact = true;
  expectRejected(() => validateTargetEvidence(badSkip, policy), "decision-bearing hosted skip");
  const badRunner = deepClone(targets[0]);
  badRunner.target.runnerLabel = "macos-latest";
  expectRejected(() => validateTargetEvidence(badRunner, policy), "unpinned runner label");
  const badProbe = deepClone(targets[0]);
  badProbe.probes.direct.nativePixelsVerified = true;
  expectRejected(() => validateTargetEvidence(badProbe, policy), "probe native-pixel claim");
  const badTerminal = deepClone(targets.find((target) => target.target.id === "windows"));
  badTerminal.terminalProbe.pty.windowsConptyExercised = false;
  expectRejected(() => validateTargetEvidence(badTerminal, policy), "fabricated Windows ConPTY route");
  const badPdf = deepClone(targets[0]);
  badPdf.pdfProbe.run.fixture.cjkTextExtracted = false;
  expectRejected(() => validateTargetEvidence(badPdf, policy), "fabricated PDF CJK result");
  const missingTarget = deepClone(matrix);
  missingTarget.targets.pop();
  expectRejected(() => validateMatrixEvidence(missingTarget, policy), "incomplete hosted matrix");
  const privateSummary = sanitizedFailureSummary(
    `${ROOT} ${process.env.RUNNER_TEMP ?? ""} ${process.env.HOME ?? ""}`
  );
  if (privateSummary.includes(ROOT) || (process.env.HOME && privateSummary.includes(process.env.HOME))) {
    fail("self-test retained a private path in a failure summary");
  }
  console.log("GPUI hosted-runner evidence self-test passed");
}

function validatePath(path) {
  const policy = loadPolicy();
  validateWorkflowBinding(policy);
  const evidence = readJson(path);
  if (evidence.schemaVersion === "hosted-runner-target-evidence.v1") {
    validateTargetEvidence(evidence, policy);
  } else if (evidence.schemaVersion === "hosted-runner-matrix-evidence.v1") {
    validateMatrixEvidence(evidence, policy);
  } else {
    fail(`unsupported evidence at ${path}`);
  }
  console.log(`${path} is valid`);
}

async function main() {
  const args = process.argv.slice(2);
  if (sameJson(args, ["--policy"])) {
    const policy = loadPolicy();
    validateWorkflowBinding(policy);
    console.log(
      `Hosted-runner policy verified: ${policy.requiredTargets.join(", ")}; ` +
        `${policy.requiredDecisionChecks.length} decision checks; ` +
        `${policy.requiredSkippedClaims.length} non-decision skips`
    );
    return;
  }
  if (sameJson(args, ["--self-test"])) {
    selfTest();
    return;
  }
  if (args.length === 4 && args[0] === "--run-target" && args[2] === "--output") {
    await runTarget(args[1], args[3]);
    return;
  }
  if (args[0] === "--merge") {
    const outputIndex = args.indexOf("--output");
    if (outputIndex < 3 || outputIndex !== args.length - 2) {
      fail("--merge requires two or more input files followed by --output <path>");
    }
    mergeTargets(args.slice(1, outputIndex), args[outputIndex + 1]);
    return;
  }
  if (args.length === 2 && args[0] === "--validate") {
    validatePath(args[1]);
    return;
  }
  if (args.length === 0 && existsSync(rootPath(MATRIX_PATH))) {
    validatePath(MATRIX_PATH);
    return;
  }
  fail(
    "usage: node scripts/check-hosted-runner-evidence.mjs " +
      "--policy | --self-test | --run-target <macos|windows> --output <path> | " +
      "--merge <target...> --output <path> | --validate <path>"
  );
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});

import { createHash } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
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
import { fileURLToPath } from "node:url";
import { classifyGpuiEvidence } from "./evidence-applicability.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const EVIDENCE = "docs/platform/evidence/document-interaction-linux.json";
const SCREENSHOT = "docs/parity/screenshots/current/native-content/linux-wayland-pdf-office.png";
const LIBRARY = "target/native/pdfium/linux-x86_64/lib/libpdfium.so";
const PDF = "docs/platform/fixtures/pdf-feasibility.pdf";
const OFFICE = "docs/platform/fixtures/office-interaction.docx";
const BINARY = "target/debug/vibex-desktop";
const WTYPE = process.env.VIBEX_WTYPE_BIN ?? "/tmp/vibex-wtype-v0.4/build/wtype";
const WINDOW_IDENTITY = "dev.vibex.desktop.preview";
const SCREEN_LOCKERS = ["hyprlock", "swaylock", "gtklock", "waylock"];
const SOURCE_INPUT_ROOTS = [
  "Cargo.lock",
  "Cargo.toml",
  "apps/desktop/Cargo.toml",
  "apps/desktop/src/document_interaction.rs",
  "apps/desktop/src/main.rs",
  "apps/desktop/src/office_surface.rs",
  "apps/desktop/src/pdf_controller.rs",
  "apps/desktop/src/pdf_surface.rs",
  "apps/desktop/src/pdf_worker.rs",
  "crates/content/Cargo.toml",
  "crates/content/src/lifecycle.rs",
  "crates/content/src/office.rs",
  "crates/content/src/pdf.rs",
  "scripts/capture-document-interaction-linux.mjs",
  "scripts/generate-office-fixtures.mjs",
  "scripts/generate-pdf-fixture.mjs"
];

const root = (path) => {
  const absolute = resolve(ROOT, path);
  if (absolute !== ROOT && !absolute.startsWith(`${ROOT}${sep}`)) {
    throw new Error(`path escapes repository: ${path}`);
  }
  return absolute;
};
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const run = (command, args, options = {}) => {
  const result = spawnSync(command, args, { cwd: ROOT, encoding: "utf8", timeout: 300_000, ...options });
  if (result.error || result.status !== 0) throw new Error(`${command} failed: ${result.error?.message ?? result.stderr}`);
  return result.stdout ?? "";
};
const assertSessionUnlocked = () => {
  for (const locker of SCREEN_LOCKERS) {
    const result = spawnSync("pgrep", ["-x", locker], { encoding: "utf8" });
    if (result.status === 0) throw new Error(`physical capture refused while ${locker} is active`);
    if (result.status !== 1) throw new Error(`unable to determine whether ${locker} is active`);
  }
};
const sleep = (ms) => new Promise((resolvePromise) => setTimeout(resolvePromise, ms));
const hypr = (kind) => JSON.parse(run("hyprctl", ["-j", kind]));
const readJsonIfReady = (path) => {
  if (!existsSync(path)) return null;
  try { return JSON.parse(readFileSync(path, "utf8")); } catch { return null; }
};

function sourceFilesFor(path) {
  const absolute = root(path);
  if (!existsSync(absolute)) throw new Error(`source input is missing: ${path}`);
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
    hash.update(readFileSync(root(path)));
    hash.update("\0");
  }
  return hash.digest("hex");
}

function sourceEvidence() {
  return {
    captureParentCommit: run("git", ["rev-parse", "HEAD"]).trim(),
    rustToolchain: run("rustc", ["--version"]).trim(),
    lockfileSha256: sha256(readFileSync(root("Cargo.lock"))),
    sourceInputRoots: SOURCE_INPUT_ROOTS,
    sourceInputTreeSha256: sourceInputTreeSha256()
  };
}

function pdfRegionMetrics(screenshot, region) {
  const output = run("magick", [
    screenshot,
    "-crop",
    `${region.width}x${region.height}+${region.x}+${region.y}`,
    "-colorspace",
    "gray",
    "-format",
    "%w\t%h\t%k\t%[entropy]\t%[fx:standard_deviation]",
    "info:"
  ]);
  const [width, height, uniqueColors, entropy, standardDeviation] = output
    .trim()
    .split("\t")
    .map(Number);
  const metrics = { ...region, width, height, uniqueColors, entropy, standardDeviation };
  if (!Object.values(metrics).every(Number.isFinite)) {
    throw new Error("invalid PDF screenshot-region metrics");
  }
  return metrics;
}

async function waitFor(predicate, app, label) {
  for (let attempt = 0; attempt < 400; attempt += 1) {
    if (attempt % 20 === 0) assertSessionUnlocked();
    const value = predicate();
    if (value) return value;
    if (app.exitCode !== null) throw new Error(`app exited before ${label}`);
    await sleep(50);
  }
  throw new Error(`timeout waiting for ${label}`);
}

async function capture() {
  run("node", ["scripts/generate-office-fixtures.mjs"]);
  run("node", ["scripts/prepare-pdfium-runtime.mjs"]);
  run("cargo", ["build", "-p", "vibex-desktop", "--locked"], { stdio: "inherit" });
  assertSessionUnlocked();
  const temporary = mkdtempSync(join(tmpdir(), "vibex-document-interaction-"));
  const reportPath = join(temporary, "run.json");
  const progressPath = join(temporary, "run.progress.json");
  let stderr = "";
  let app;
  try {
    app = spawn(root(BINARY), ["--native-content-document-interaction", root(LIBRARY), root(PDF), root(OFFICE), reportPath], {
      cwd: ROOT, env: { ...process.env, XDG_SESSION_TYPE: "wayland" }, stdio: ["ignore", "ignore", "pipe"]
    });
    app.stderr.on("data", (chunk) => { if (stderr.length < 65536) stderr += chunk.toString(); });
    let client = await waitFor(() => hypr("clients").find((item) => item.pid === app.pid && item.class === WINDOW_IDENTITY), app, "window");
    const monitor = hypr("monitors").find((item) => item.id === client.monitor);
    if (!monitor) throw new Error("active physical monitor unavailable");
    const selector = `address:${client.address}`;
    if (!client.floating) run("hyprctl", ["dispatch", "togglefloating", selector]);
    run("hyprctl", ["dispatch", "resizewindowpixel", `exact 1200 900,${selector}`]);
    run("hyprctl", ["dispatch", "movewindowpixel", `exact ${monitor.x + 80} ${monitor.y + 60},${selector}`]);
    run("hyprctl", ["dispatch", "focuswindow", selector]);
    await waitFor(() => readJsonIfReady(progressPath)?.ready === true, app, "ready models");
    const commandSteps = [
      ["n", "nextPageCommandObserved"],
      ["z", "zoomCommandObserved"],
      ["c", "officeCloseCommandObserved"]
    ];
    let capture = null;
    for (const [command, progressField] of commandSteps) {
      run("hyprctl", ["dispatch", "sendshortcut", `,${command},${selector}`]);
      await waitFor(
        () => {
          const progress = readJsonIfReady(progressPath);
          return progress?.[progressField] === true && (command === "c" || progress.ready === true);
        },
        app,
        progressField
      );
      await sleep(750);
      if (command === "n") {
        assertSessionUnlocked();
        mkdirSync(dirname(root(SCREENSHOT)), { recursive: true });
        client = hypr("clients").find((item) => item.address === client.address);
        run("grim", ["-g", `${client.at[0]},${client.at[1]} ${client.size[0]}x${client.size[1]}`, root(SCREENSHOT)]);
        const region = {
          x: 150,
          y: 220,
          width: Math.floor(client.size[0] * 3 / 5) - 150,
          height: client.size[1] - 260
        };
        const pdfRegion = pdfRegionMetrics(root(SCREENSHOT), region);
        if (pdfRegion.uniqueColors < 100 || pdfRegion.entropy <= 0.02 || pdfRegion.standardDeviation <= 0.02) {
          throw new Error("PDF screenshot region is blank or lacks rendered page detail");
        }
        capture = {
          screenshotPath: SCREENSHOT,
          screenshotSha256: sha256(readFileSync(root(SCREENSHOT))),
          interactionState: { currentPage: 2, zoomLabel: "Fit width", officeVisible: true },
          pdfRegion
        };
      }
    }
    if (!capture) throw new Error("PDF interaction screenshot was not captured");
    const report = await waitFor(() => existsSync(reportPath) && JSON.parse(readFileSync(reportPath, "utf8")), app, "interaction report");
    if (report.status !== "passed" || report.pdf.currentPage !== 1 || report.pdf.zoomLabel !== "125%" ||
        report.office.closeCommandObserved !== true || report.office.finalResidentItems !== 0 || report.office.finalResidentBytes !== 0) {
      throw new Error(`interaction report did not meet the contract: ${JSON.stringify(report)}`);
    }
    run("hyprctl", ["dispatch", "closewindow", selector]);
    await waitFor(() => app.exitCode !== null, app, "clean exit");
    const evidence = {
      schemaVersion: "document-interaction-linux-evidence.v1",
      status: "passed",
      capturedAt: new Date().toISOString(),
      source: sourceEvidence(),
      runner: { platform: process.platform, architecture: process.arch, displayBackend: "wayland-hyprland", syntheticDisplay: false, activeMonitorObserved: true },
      window: { identity: WINDOW_IDENTITY, xwayland: false, monitorId: client.monitor, width: client.size[0], height: client.size[1] },
      fixtures: { pdf: { bytes: readFileSync(root(PDF)).length, sha256: sha256(readFileSync(root(PDF))) }, office: { bytes: readFileSync(root(OFFICE)).length, sha256: sha256(readFileSync(root(OFFICE))) } },
      capture,
      run: report,
      process: { processExited: true, appExitCode: app.exitCode, panicMentioned: /panicked at|thread '.+' panicked/i.test(stderr) },
      privacy: { documentPathsStored: false, pdfContentStored: false, officeContentStored: false },
      limitations: ["Physical Wayland only; X11 remains untested.", "The Office physical slice uses DOCX; XLSX, ODS, and PPTX remain controller-tested."]
    };
    validateEvidence(evidence);
    writeFileSync(root(EVIDENCE), `${JSON.stringify(evidence, null, 2)}\n`);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    const diagnostic = stderr.trim();
    throw new Error(diagnostic ? `${message}\nGPUI stderr:\n${diagnostic}` : message);
  } finally {
    if (app && app.exitCode === null) app.kill("SIGKILL");
    rmSync(temporary, { recursive: true, force: true });
  }
}

function validateEvidence(evidence) {
  const applicability = classifyGpuiEvidence(ROOT, EVIDENCE, evidence.source);
  if (
    applicability === "current" &&
    (JSON.stringify(evidence.source.sourceInputRoots) !== JSON.stringify(SOURCE_INPUT_ROOTS) ||
      evidence.source.sourceInputTreeSha256 !== sourceInputTreeSha256())
  ) {
    throw new Error("GPUI document interaction source identity is stale");
  }
  if (
    evidence?.schemaVersion !== "document-interaction-linux-evidence.v1" ||
    evidence.status !== "passed" ||
    evidence.runner?.activeMonitorObserved !== true ||
    evidence.runner.syntheticDisplay !== false ||
    evidence.window?.xwayland !== false ||
    evidence.window.width !== 1200 ||
    evidence.window.height < 900 ||
    evidence.fixtures?.pdf?.sha256 !== sha256(readFileSync(root(PDF))) ||
    evidence.fixtures.office?.sha256 !== sha256(readFileSync(root(OFFICE))) ||
    evidence.capture?.screenshotSha256 !== sha256(readFileSync(root(evidence.capture.screenshotPath))) ||
    evidence.capture.interactionState?.currentPage !== 2 ||
    evidence.capture.interactionState.zoomLabel !== "Fit width" ||
    evidence.capture.interactionState.officeVisible !== true ||
    evidence.capture.pdfRegion?.uniqueColors < 100 ||
    evidence.capture.pdfRegion.entropy <= 0.02 ||
    evidence.capture.pdfRegion.standardDeviation <= 0.02 ||
    evidence.run?.status !== "passed" ||
    evidence.run.pdf?.pageCount !== 12 ||
    evidence.run.pdf.currentPage !== 1 ||
    evidence.run.pdf.zoomLabel !== "125%" ||
    evidence.run.pdf.renderedPages < 2 ||
    evidence.run.pdf.nextPageCommandObserved !== true ||
    evidence.run.pdf.zoomCommandObserved !== true ||
    evidence.run.pdf.workerActive !== false ||
    evidence.run.office?.initialKind !== "docx" ||
    evidence.run.office.initialVisibleItems < 1 ||
    evidence.run.office.systemOpenAvailable !== true ||
    evidence.run.office.closeCommandObserved !== true ||
    evidence.run.office.finalResidentItems !== 0 ||
    evidence.run.office.finalResidentBytes !== 0 ||
    evidence.process?.processExited !== true ||
    evidence.process.appExitCode !== 0 ||
    evidence.process.panicMentioned !== false ||
    Object.values(evidence.privacy ?? {}).some((value) => value !== false)
  ) {
    throw new Error("GPUI document interaction evidence is invalid");
  }
  const serialized = JSON.stringify(evidence);
  for (const forbidden of [ROOT, process.env.HOME, "GPUI Office physical fixture", "VIBEX PDF FEASIBILITY"]) {
    if (forbidden && serialized.includes(forbidden)) throw new Error("evidence retained private content");
  }
  return applicability;
}

function verify() {
  const evidence = JSON.parse(readFileSync(root(EVIDENCE), "utf8"));
  const applicability = validateEvidence(evidence);
  console.log(`GPUI PDF/Office physical interaction evidence verified; applicability=${applicability}`);
}

function selfTest() {
  const evidence = JSON.parse(readFileSync(root(EVIDENCE), "utf8"));
  validateEvidence(evidence);
  const mutations = [
    ["stale source", (copy) => (copy.source.sourceInputTreeSha256 = "0".repeat(64))],
    ["blank PDF region", (copy) => (copy.capture.pdfRegion.uniqueColors = 1)],
    ["stale screenshot", (copy) => (copy.capture.screenshotSha256 = "0".repeat(64))],
    ["missing page input", (copy) => (copy.run.pdf.nextPageCommandObserved = false)],
    ["Office cache leak", (copy) => (copy.run.office.finalResidentBytes = 1)],
    ["retained content", (copy) => (copy.rawDocumentContent = "GPUI Office physical fixture")]
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
    if (!rejected) throw new Error(`negative self-test was accepted: ${label}`);
  }
  console.log("GPUI PDF/Office physical interaction negative-case self-test passed");
}

const mode = process.argv[2];
if (mode === "--write") await capture();
else if (mode === "--self-test") selfTest();
else if (mode === undefined) verify();
else throw new Error("usage: node scripts/capture-document-interaction-linux.mjs [--write|--self-test]");

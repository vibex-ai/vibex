import { spawnSync } from "node:child_process";
import console from "node:console";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function fail(message) {
  console.error(message);
  process.exit(1);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024,
    timeout: 180_000
  });
  if (result.error) fail(`${command} failed to start: ${result.error.message}`);
  if (result.status !== 0) {
    process.stderr.write(result.stderr ?? "");
    process.stdout.write(result.stdout ?? "");
    fail(`${command} ${args.join(" ")} failed with status ${result.status ?? "unknown"}`);
  }
}

function exactKeys(value, expected, label) {
  const actual = Object.keys(value ?? {}).sort();
  const wanted = [...expected].sort();
  assert(JSON.stringify(actual) === JSON.stringify(wanted), `${label} keys drifted`);
}

function validateSurface(surface, expected) {
  exactKeys(
    surface,
    [
      "kind",
      "lifecyclePhase",
      "backend",
      "explicitLoadRequired",
      "nativeSurfaceAllocated",
      "resourceBudgeted",
      "diagnosticsRedacted",
      "supported",
      "notes"
    ],
    `${expected.kind} surface`
  );
  assert(surface.kind === expected.kind, `${expected.kind} kind drifted`);
  assert(surface.backend === expected.backend, `${expected.kind} backend drifted`);
  assert(surface.explicitLoadRequired === expected.explicitLoadRequired, `${expected.kind} explicit-load drifted`);
  assert(surface.nativeSurfaceAllocated === expected.nativeSurfaceAllocated, `${expected.kind} allocation drifted`);
  assert(surface.resourceBudgeted === true, `${expected.kind} resource budget did not pass`);
  assert(surface.diagnosticsRedacted === true, `${expected.kind} diagnostics are not redacted`);
  assert(surface.supported === expected.supported, `${expected.kind} support state drifted`);
  assert(Array.isArray(surface.notes) && surface.notes.length > 0, `${expected.kind} notes are missing`);
}

function validate(report) {
  exactKeys(
    report,
    [
      "schemaVersion",
      "status",
      "platform",
      "architecture",
      "terminal",
      "web",
      "rightRailWebPlugin",
      "pdf",
      "office",
      "privacy"
    ],
    "native content report"
  );
  assert(report.schemaVersion === "native-content-contract.v1", "native content schema drifted");
  assert(report.status === "passed", "native content contract did not pass");
  assert(/^[a-z0-9_]+$/.test(report.platform), "platform is invalid");
  assert(/^[a-z0-9_]+$/.test(report.architecture), "architecture is invalid");
  validateSurface(report.terminal, {
    kind: "terminal",
    backend: "alacritty-terminal",
    explicitLoadRequired: false,
    nativeSurfaceAllocated: true,
    supported: true
  });
  validateSurface(report.web, {
    kind: "web",
    backend: "unsupported-no-allocation",
    explicitLoadRequired: true,
    nativeSurfaceAllocated: false,
    supported: false
  });
  validateSurface(report.rightRailWebPlugin, {
    kind: "right_rail_web_plugin",
    backend: "dom-iframe-external-open-boundary",
    explicitLoadRequired: true,
    nativeSurfaceAllocated: false,
    supported: false
  });
  assert(report.rightRailWebPlugin.lifecyclePhase === "Unsupported", "right-rail Web plugin must stay unsupported");
  validateSurface(report.pdf, {
    kind: "pdf",
    backend: "pdfium-render",
    explicitLoadRequired: true,
    nativeSurfaceAllocated: false,
    supported: true
  });
  validateSurface(report.office, {
    kind: "office",
    backend: "quick-xml+zip",
    explicitLoadRequired: true,
    nativeSurfaceAllocated: false,
    supported: true
  });
  exactKeys(
    report.privacy,
    [
      "terminalOutputStoredInDiagnostics",
      "urlStoredInWebDiagnostics",
      "pdfContentStoredInDiagnostics",
      "officeContentStoredInDiagnostics"
    ],
    "privacy"
  );
  for (const [key, value] of Object.entries(report.privacy)) {
    assert(value === false, `${key} leaked content into diagnostics`);
  }
}

const temp = mkdtempSync(join(tmpdir(), "vibex-native-content-"));
try {
  const output = join(temp, "native-content-contract.json");
  run("cargo", [
    "run",
    "-p",
    "vibex-desktop",
    "--locked",
    "--",
    "--native-content-contract",
    output
  ]);
  validate(JSON.parse(readFileSync(output, "utf8")));
  console.log("GPUI native content contract verified");
} finally {
  rmSync(temp, { recursive: true, force: true });
}

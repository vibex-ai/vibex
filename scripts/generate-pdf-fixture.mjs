import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import console from "node:console";
import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const FIXTURE_PATH = resolve(ROOT, "docs/platform/fixtures/pdf-feasibility.pdf");
const EXPECTED_FIXTURE_SHA256 = "5eee17047b63701cff30656d9f044cd6d205c175275d02429177294012ccde89";

function fail(message) {
  throw new Error(message);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

if (process.argv.includes("--write")) {
  fail(
    "The deterministic PDF fixture is retained as an immutable reviewed artifact; " +
      "Vibex no longer copies the upstream gpui-component font source needed to regenerate it."
  );
}

assert(existsSync(FIXTURE_PATH), "PDF fixture is missing");
const fixture = readFileSync(FIXTURE_PATH);
assert(sha256(fixture) === EXPECTED_FIXTURE_SHA256, "PDF fixture identity drifted");
assert(fixture.includes(Buffer.from("NotoSansSC")), "PDF fixture lost its embedded font marker");
assert(fixture.includes(Buffer.from("Skia/PDF m149")), "PDF fixture producer drifted");
console.log(`GPUI PDF fixture verified: ${fixture.length} bytes, ${EXPECTED_FIXTURE_SHA256}`);

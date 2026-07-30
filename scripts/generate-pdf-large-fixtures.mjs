import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import console from "node:console";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import {
  EXTREME_PAGE_HEIGHT_POINTS,
  EXTREME_PAGE_PDF_FIXTURE_PATH,
  EXTREME_PAGE_WIDTH_POINTS,
  TOO_MANY_PAGES_PDF_FIXTURE_PATH,
  TOO_MANY_PAGES_PDF_PAGE_COUNT
} from "./pdf-large-fixture-contract.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const TOO_MANY_PATH = resolve(ROOT, TOO_MANY_PAGES_PDF_FIXTURE_PATH);
const EXTREME_PATH = resolve(ROOT, EXTREME_PAGE_PDF_FIXTURE_PATH);
const EXPECTED_TOO_MANY_SHA256 = "dd534ec87c455deff158b2e3ccbbddbe59608627588c2a0b309ae271de7953cb";
const EXPECTED_EXTREME_SHA256 = "4a18789abdaf8bef7ed3b7d2e35d848f3227a8ff94b2f2e812398663d26ecada";

function fail(message) {
  throw new Error(message);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function buildPdf(objects, trailerFields = "") {
  const chunks = [Buffer.from("%PDF-1.4\n%\xe2\xe3\xcf\xd3\n", "latin1")];
  const offsets = [0];
  let length = chunks[0].length;
  for (const [index, object] of objects.entries()) {
    offsets.push(length);
    const chunk = Buffer.from(`${index + 1} 0 obj\n${object}\nendobj\n`, "ascii");
    chunks.push(chunk);
    length += chunk.length;
  }
  const xrefOffset = length;
  const xref = ["xref", `0 ${objects.length + 1}`, "0000000000 65535 f "];
  for (const offset of offsets.slice(1)) {
    xref.push(`${String(offset).padStart(10, "0")} 00000 n `);
  }
  chunks.push(
    Buffer.from(
      `${xref.join("\n")}\ntrailer\n` +
        `<< /Size ${objects.length + 1} /Root 1 0 R ${trailerFields}>>\n` +
        `startxref\n${xrefOffset}\n%%EOF\n`,
      "ascii"
    )
  );
  return Buffer.concat(chunks);
}

function buildTooManyPagesFixture() {
  const firstPageObject = 3;
  const kids = Array.from(
    { length: TOO_MANY_PAGES_PDF_PAGE_COUNT },
    (_, index) => `${firstPageObject + index} 0 R`
  ).join(" ");
  const objects = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    `<< /Type /Pages /Kids [${kids}] /Count ${TOO_MANY_PAGES_PDF_PAGE_COUNT} >>`
  ];
  for (let index = 0; index < TOO_MANY_PAGES_PDF_PAGE_COUNT; index += 1) {
    objects.push("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>");
  }
  return buildPdf(objects);
}

function buildExtremePageFixture() {
  return buildPdf([
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 ${EXTREME_PAGE_WIDTH_POINTS} ${EXTREME_PAGE_HEIGHT_POINTS}] >>`
  ]);
}

const fixtures = [
  {
    label: "too-many-pages",
    path: TOO_MANY_PATH,
    bytes: buildTooManyPagesFixture(),
    expectedSha256: EXPECTED_TOO_MANY_SHA256
  },
  {
    label: "extreme-page",
    path: EXTREME_PATH,
    bytes: buildExtremePageFixture(),
    expectedSha256: EXPECTED_EXTREME_SHA256
  }
];

if (process.argv.includes("--write")) {
  for (const fixture of fixtures) {
    mkdirSync(dirname(fixture.path), { recursive: true });
    writeFileSync(fixture.path, fixture.bytes);
    console.log(
      `GPUI ${fixture.label} PDF fixture written: ${fixture.bytes.length} bytes, ${sha256(fixture.bytes)}`
    );
  }
  process.exit(0);
}

for (const fixture of fixtures) {
  assert(existsSync(fixture.path), `${fixture.label} PDF fixture is missing`);
  const actual = readFileSync(fixture.path);
  assert(actual.equals(fixture.bytes), `${fixture.label} PDF fixture bytes drifted`);
  assert(sha256(actual) === fixture.expectedSha256, `${fixture.label} PDF fixture identity drifted`);
  console.log(
    `GPUI ${fixture.label} PDF fixture verified: ${actual.length} bytes, ${fixture.expectedSha256}`
  );
}

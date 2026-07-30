import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import console from "node:console";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import {
  ENCRYPTED_PDF_FIXTURE_OWNER_PASSWORD,
  ENCRYPTED_PDF_FIXTURE_PASSWORD,
  ENCRYPTED_PDF_FIXTURE_PATH
} from "./pdf-encrypted-fixture-contract.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const FIXTURE_PATH = resolve(ROOT, ENCRYPTED_PDF_FIXTURE_PATH);
const EXPECTED_FIXTURE_SHA256 = "2b3361f59f2534ffb1ed263de53c24e62ae671e813fa97ea790c2024f9c7d2b6";
const PASSWORD_PADDING = Buffer.from(
  "28bf4e5e4e758a4164004e56fffa01082e2e00b6d0683e802f0ca9fe6453697a",
  "hex"
);
const PERMISSIONS = -4;

function fail(message) {
  throw new Error(message);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function digest(algorithm, ...buffers) {
  const hash = createHash(algorithm);
  for (const buffer of buffers) hash.update(buffer);
  return hash.digest();
}

function sha256(content) {
  return digest("sha256", content).toString("hex");
}

function padPassword(password) {
  const bytes = Buffer.from(password, "latin1").subarray(0, 32);
  return Buffer.concat([bytes, PASSWORD_PADDING.subarray(0, 32 - bytes.length)]);
}

function rc4(key, input) {
  const state = Uint8Array.from({ length: 256 }, (_, index) => index);
  let j = 0;
  for (let i = 0; i < 256; i += 1) {
    j = (j + state[i] + key[i % key.length]) & 0xff;
    [state[i], state[j]] = [state[j], state[i]];
  }
  const output = Buffer.alloc(input.length);
  let i = 0;
  j = 0;
  for (let offset = 0; offset < input.length; offset += 1) {
    i = (i + 1) & 0xff;
    j = (j + state[i]) & 0xff;
    [state[i], state[j]] = [state[j], state[i]];
    output[offset] = input[offset] ^ state[(state[i] + state[j]) & 0xff];
  }
  return output;
}

function objectKey(encryptionKey, objectNumber, generation = 0) {
  const suffix = Buffer.from([
    objectNumber & 0xff,
    (objectNumber >> 8) & 0xff,
    (objectNumber >> 16) & 0xff,
    generation & 0xff,
    (generation >> 8) & 0xff
  ]);
  return digest("md5", encryptionKey, suffix).subarray(
    0,
    Math.min(encryptionKey.length + 5, 16)
  );
}

function buildFixture() {
  const fileId = digest("md5", Buffer.from("vibex-encrypted-pdf-fixture-v1"));
  const ownerKey = digest(
    "md5",
    padPassword(ENCRYPTED_PDF_FIXTURE_OWNER_PASSWORD)
  ).subarray(0, 5);
  const ownerEntry = rc4(ownerKey, padPassword(ENCRYPTED_PDF_FIXTURE_PASSWORD));
  const permissions = Buffer.alloc(4);
  permissions.writeInt32LE(PERMISSIONS);
  const encryptionKey = digest(
    "md5",
    padPassword(ENCRYPTED_PDF_FIXTURE_PASSWORD),
    ownerEntry,
    permissions,
    fileId
  ).subarray(0, 5);
  const userEntry = rc4(encryptionKey, PASSWORD_PADDING);
  const content = Buffer.from(
    "BT\n/F1 24 Tf\n72 720 Td\n(Vibex encrypted PDF fixture) Tj\nET\n",
    "ascii"
  );
  const encryptedContent = rc4(objectKey(encryptionKey, 5), content);
  const objects = [
    Buffer.from("<< /Type /Catalog /Pages 2 0 R >>", "ascii"),
    Buffer.from("<< /Type /Pages /Kids [3 0 R] /Count 1 >>", "ascii"),
    Buffer.from(
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] " +
        "/Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
      "ascii"
    ),
    Buffer.from("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>", "ascii"),
    Buffer.concat([
      Buffer.from(`<< /Length ${encryptedContent.length} >>\nstream\n`, "ascii"),
      encryptedContent,
      Buffer.from("\nendstream", "ascii")
    ]),
    Buffer.from(
      `<< /Filter /Standard /V 1 /R 2 /O <${ownerEntry.toString("hex")}> ` +
        `/U <${userEntry.toString("hex")}> /P ${PERMISSIONS} /Length 40 >>`,
      "ascii"
    )
  ];
  const chunks = [Buffer.from("%PDF-1.4\n%\xe2\xe3\xcf\xd3\n", "latin1")];
  const offsets = [0];
  let length = chunks[0].length;
  for (const [index, object] of objects.entries()) {
    offsets.push(length);
    const chunk = Buffer.concat([
      Buffer.from(`${index + 1} 0 obj\n`, "ascii"),
      object,
      Buffer.from("\nendobj\n", "ascii")
    ]);
    chunks.push(chunk);
    length += chunk.length;
  }
  const xrefOffset = length;
  const xref = ["xref", `0 ${objects.length + 1}`, "0000000000 65535 f "];
  for (const offset of offsets.slice(1)) {
    xref.push(`${String(offset).padStart(10, "0")} 00000 n `);
  }
  const idHex = fileId.toString("hex");
  chunks.push(
    Buffer.from(
      `${xref.join("\n")}\ntrailer\n` +
        `<< /Size ${objects.length + 1} /Root 1 0 R /Encrypt 6 0 R ` +
        `/ID [<${idHex}><${idHex}>] >>\n` +
        `startxref\n${xrefOffset}\n%%EOF\n`,
      "ascii"
    )
  );
  return Buffer.concat(chunks);
}

const expected = buildFixture();
if (process.argv.includes("--write")) {
  mkdirSync(dirname(FIXTURE_PATH), { recursive: true });
  writeFileSync(FIXTURE_PATH, expected);
  console.log(
    `GPUI encrypted PDF fixture written: ${expected.length} bytes, ${sha256(expected)}`
  );
  process.exit(0);
}

assert(existsSync(FIXTURE_PATH), "encrypted PDF fixture is missing");
const fixture = readFileSync(FIXTURE_PATH);
assert(fixture.equals(expected), "encrypted PDF fixture bytes drifted from the generator");
assert(sha256(fixture) === EXPECTED_FIXTURE_SHA256, "encrypted PDF fixture identity drifted");
assert(fixture.includes(Buffer.from("/Encrypt 6 0 R", "ascii")), "PDF encryption dictionary is missing");
assert(!fixture.includes(Buffer.from("Vibex encrypted PDF fixture", "ascii")), "PDF plaintext leaked into the encrypted stream");
assert(!fixture.includes(Buffer.from(ENCRYPTED_PDF_FIXTURE_PASSWORD, "ascii")), "PDF fixture contains its user password");
console.log(
  `GPUI encrypted PDF fixture verified: ${fixture.length} bytes, ${EXPECTED_FIXTURE_SHA256}`
);

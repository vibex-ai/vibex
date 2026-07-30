import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import console from "node:console";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve, sep } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const REVIEW_PATH = "docs/licenses/pdfium-7881-review.json";
const RELEASE_URL =
  "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F7881";
const OUTPUT_ROOT = resolve(ROOT, "target/native/pdfium");

function fail(message) {
  throw new Error(message);
}

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function targetFromHost() {
  const key = `${process.platform}-${process.arch}`;
  const targets = {
    "linux-x64": "linux-x86_64",
    "darwin-x64": "macos-x86_64",
    "darwin-arm64": "macos-aarch64",
    "win32-x64": "windows-x86_64"
  };
  return targets[key] ?? fail(`unsupported PDFium host ${key}`);
}

function parseArguments() {
  const args = process.argv.slice(2);
  let target = targetFromHost();
  let offline = false;
  let allowDeferred = false;
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] === "--target") {
      target = args[++index] ?? fail("--target requires a value");
    } else if (args[index] === "--offline") {
      offline = true;
    } else if (args[index] === "--allow-deferred") {
      allowDeferred = true;
    } else {
      fail(`unknown argument ${args[index]}`);
    }
  }
  return { target, offline, allowDeferred };
}

function repositoryPath(path) {
  const absolute = resolve(path);
  if (absolute !== ROOT && !absolute.startsWith(`${ROOT}${sep}`)) {
    fail(`path escapes repository: ${path}`);
  }
  return relative(ROOT, absolute).split(sep).join("/");
}

function verifyFile(path, bytes, digest, label) {
  if (!existsSync(path) || !statSync(path).isFile()) fail(`${label} is missing`);
  const content = readFileSync(path);
  if (content.length !== bytes) fail(`${label} byte count changed`);
  if (sha256(content) !== digest) fail(`${label} SHA-256 changed`);
  return content;
}

function manifestFor(review, archive, output) {
  return {
    schemaVersion: "vibex-pdfium-runtime.v1",
    engineVersion: review.engine.version,
    build: review.engine.build,
    target: archive.target,
    source: `${RELEASE_URL}/${archive.asset}`,
    archive: {
      bytes: archive.archiveBytes,
      sha256: archive.archiveSha256
    },
    library: {
      path: archive.libraryPath,
      bytes: archive.libraryBytes,
      sha256: archive.librarySha256
    },
    licenses: review.licenseFiles.map(({ path, sha256: digest, expression }) => ({
      path,
      sha256: digest,
      expression
    })),
    output: repositoryPath(output),
    distributionApproved: review.review.registeredTargets.includes(archive.target)
  };
}

function verifyPrepared(output, manifest) {
  const manifestPath = join(output, "manifest.json");
  if (!existsSync(manifestPath)) return false;
  let actual;
  try {
    actual = JSON.parse(readFileSync(manifestPath, "utf8"));
  } catch {
    return false;
  }
  if (JSON.stringify(actual) !== JSON.stringify(manifest)) return false;
  verifyFile(
    join(output, manifest.library.path),
    manifest.library.bytes,
    manifest.library.sha256,
    "prepared PDFium library"
  );
  for (const license of manifest.licenses) {
    const content = readFileSync(join(output, license.path));
    if (sha256(content) !== license.sha256) return false;
  }
  return existsSync(join(output, "PDFIUM-NOTICE.md"));
}

async function download(url, destination, expectedBytes) {
  const response = await fetch(url, {
    redirect: "follow",
    signal: AbortSignal.timeout(180_000)
  });
  if (!response.ok) fail(`PDFium download returned HTTP ${response.status}`);
  const content = Buffer.from(await response.arrayBuffer());
  if (content.length > Math.max(expectedBytes, 16 * 1024 * 1024)) {
    fail("PDFium download exceeded the bounded archive size");
  }
  writeFileSync(destination, content);
}

function extractArchive(archive, output) {
  const result = spawnSync("tar", ["-xzf", archive, "-C", output], {
    cwd: ROOT,
    encoding: "utf8",
    timeout: 60_000
  });
  if (result.error) fail(`tar failed to start: ${result.error.message}`);
  if (result.status !== 0) fail(`tar extraction failed: ${result.stderr ?? "unknown error"}`);
}

function writeNotice(output, review) {
  const notice = `# PDFium Runtime Notices

Vibex distributes the pinned PDFium ${review.engine.version} runtime from
${review.engine.binaryRepository}, release ${review.engine.binaryRelease}.

Portions of this software are copyright The FreeType Project
(https://www.freetype.org). All rights reserved.

This software is based in part on the work of the Independent JPEG Group.

The complete, unmodified license texts are included in this directory under
\`LICENSE\` and \`licenses/\`. Names of upstream projects or contributors may not be
used to endorse Vibex. PDFium and its bundled components are provided without warranty.
`;
  writeFileSync(join(output, "PDFIUM-NOTICE.md"), notice);
}

async function main() {
  const { target, offline, allowDeferred } = parseArguments();
  const review = JSON.parse(readFileSync(resolve(ROOT, REVIEW_PATH), "utf8"));
  if (review.schemaVersion !== "vibex-pdfium-native-review.v1") fail("invalid PDFium review");
  if (review.review.status !== "approved_for_linux_distribution") {
    fail("PDFium distribution is not approved");
  }
  const archive = review.archives.find((candidate) => candidate.target === target);
  if (!archive) fail(`PDFium target ${target} is not registered`);
  const approved = review.review.registeredTargets.includes(target);
  if (!approved && !allowDeferred) {
    fail(`PDFium target ${target} is deferred until native validation`);
  }

  const output = resolve(OUTPUT_ROOT, target);
  const manifest = manifestFor(review, archive, output);
  if (verifyPrepared(output, manifest)) {
    console.log(`PDFium runtime verified at ${repositoryPath(output)}`);
    return;
  }
  if (offline) fail(`prepared PDFium runtime is stale or missing at ${repositoryPath(output)}`);

  const temporary = mkdtempSync(join(tmpdir(), "vibex-pdfium-runtime-"));
  try {
    const archivePath = join(temporary, archive.asset);
    const extracted = join(temporary, "extracted");
    mkdirSync(extracted, { recursive: true });
    await download(`${RELEASE_URL}/${archive.asset}`, archivePath, archive.archiveBytes);
    verifyFile(
      archivePath,
      archive.archiveBytes,
      archive.archiveSha256,
      "downloaded PDFium archive"
    );
    extractArchive(archivePath, extracted);
    verifyFile(
      join(extracted, archive.libraryPath),
      archive.libraryBytes,
      archive.librarySha256,
      "extracted PDFium library"
    );
    for (const license of review.licenseFiles) {
      const content = readFileSync(join(extracted, license.path));
      if (sha256(content) !== license.sha256) fail(`PDFium license changed: ${license.path}`);
    }

    rmSync(output, { recursive: true, force: true });
    mkdirSync(output, { recursive: true });
    mkdirSync(join(output, dirname(archive.libraryPath)), { recursive: true });
    copyFileSync(join(extracted, archive.libraryPath), join(output, archive.libraryPath));
    for (const license of review.licenseFiles) {
      const destination = join(output, license.path);
      mkdirSync(dirname(destination), { recursive: true });
      copyFileSync(join(extracted, license.path), destination);
    }
    writeNotice(output, review);
    writeFileSync(join(output, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
    if (!verifyPrepared(output, manifest)) fail("prepared PDFium runtime failed verification");
    console.log(`PDFium runtime prepared at ${repositoryPath(output)}`);
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

await main().catch((error) => {
  console.error(error.message);
  process.exit(1);
});

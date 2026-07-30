import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import console from "node:console";
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const REVIEW = JSON.parse(
  readFileSync(resolve(ROOT, "docs/licenses/pdfium-7881-review.json"), "utf8")
);
const DEB = resolve(
  ROOT,
  "target/hosted-packages/vibex-desktop_0.1.0-rc.1_amd64.deb"
);
const APPIMAGE = resolve(
  ROOT,
  "target/hosted-packages/vibex-desktop_0.1.0-rc.1_x86_64.AppImage"
);
const PREPARED = resolve(ROOT, "target/native/pdfium/linux-x86_64");
const PACKAGE_RESOURCE = "usr/lib/vibex-desktop/pdfium";
const WEB_SOURCE = resolve(ROOT, "apps/web/dist");
const WEB_PACKAGE_RESOURCE = "usr/lib/vibex-desktop/web";
const REQUIRED_WEB_ASSETS = [
  "index.html",
  "offline.html",
  "styles.css",
  "host.js",
  "host-services.js",
  "platform-compat.js",
  "manifest.webmanifest",
  "icon.svg",
  "service-worker.js",
  "build.json",
  "pkg/vibex_web.js",
  "pkg/vibex_web_bg.wasm"
];

function fail(message) {
  throw new Error(message);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? ROOT,
    encoding: "utf8",
    env: { ...process.env, LC_ALL: "C", ...(options.env ?? {}) },
    maxBuffer: 128 * 1024 * 1024,
    timeout: options.timeout ?? 120_000
  });
  if (result.error) fail(`${command} failed to start: ${result.error.message}`);
  if (result.status !== 0) {
    process.stderr.write(result.stderr ?? "");
    process.stdout.write(result.stdout ?? "");
    fail(`${command} ${args.join(" ")} failed with status ${result.status ?? "unknown"}`);
  }
  return result.stdout.trim();
}

function fileIdentity(path) {
  assert(existsSync(path) && statSync(path).isFile(), `missing package file ${path}`);
  return { bytes: statSync(path).size, sha256: sha256(path) };
}

function elfIdentity(path) {
  const notes = run("readelf", ["-n", path]);
  const dynamic = run("readelf", ["-d", path]);
  const buildId = notes.match(/Build ID: ([0-9a-f]+)/)?.[1] ?? fail(`missing Build ID: ${path}`);
  const needed = [...dynamic.matchAll(/Shared library: \[([^\]]+)\]/g)]
    .map((match) => match[1])
    .sort();
  const runpath = dynamic.match(/Library runpath: \[([^\]]+)\]/)?.[1] ?? null;
  return { buildId, needed, runpath };
}

function verifyLicenseBundle(root, label) {
  for (const license of REVIEW.licenseFiles) {
    const path = join(root, license.path);
    assert(existsSync(path), `${label} license is missing: ${license.path}`);
    assert(sha256(path) === license.sha256, `${label} license hash drifted: ${license.path}`);
  }
  assert(existsSync(join(root, "PDFIUM-NOTICE.md")), `${label} PDFium notice is missing`);
  assert(existsSync(join(root, "manifest.json")), `${label} PDFium manifest is missing`);
}

function verifyWebBuild(root, label) {
  const assets = {};
  for (const relativePath of REQUIRED_WEB_ASSETS) {
    assets[relativePath] = fileIdentity(join(root, relativePath));
  }
  const build = JSON.parse(readFileSync(join(root, "build.json"), "utf8"));
  assert(build.schemaVersion === "vibex-web-build.v1", `${label} Web build schema drifted`);
  assert(build.profile === "release", `${label} Web build is not release`);
  assert(/^[0-9a-f]{24}$/.test(build.buildId ?? ""), `${label} Web build id is invalid`);
  assert(/^[0-9a-f]{40}$/.test(build.gitCommit ?? ""), `${label} Web source revision is invalid`);
  return { build, assets };
}

function assertWebBuildMatches(source, packaged, label) {
  assert(JSON.stringify(packaged.build) === JSON.stringify(source.build), `${label} Web build identity drifted`);
  for (const relativePath of REQUIRED_WEB_ASSETS) {
    assert(
      JSON.stringify(packaged.assets[relativePath]) === JSON.stringify(source.assets[relativePath]),
      `${label} Web asset drifted: ${relativePath}`
    );
  }
}

function verifyNativeWebBinding(binary, build, label) {
  const bytes = readFileSync(binary);
  assert(bytes.includes(build.buildId), `${label} binary is not bound to the packaged Web build id`);
  assert(bytes.includes(build.gitCommit), `${label} binary is not bound to the packaged Web source revision`);
}

function probe(binary, args = []) {
  return JSON.parse(run(binary, [...args, "--probe"]));
}

function validate() {
  assert(REVIEW.review.status === "approved_for_linux_distribution", "PDFium is not approved");
  const archive = REVIEW.archives.find((candidate) => candidate.target === "linux-x86_64");
  assert(archive, "Linux PDFium archive review is missing");
  run("node", ["scripts/prepare-pdfium-runtime.mjs", "--offline"]);
  assert(fileIdentity(join(PREPARED, archive.libraryPath)).sha256 === archive.librarySha256,
    "prepared PDFium source hash drifted");

  assert(existsSync(DEB), "native-content .deb is missing");
  assert(existsSync(APPIMAGE), "native-content AppImage is missing");
  const sourceWebBuild = verifyWebBuild(WEB_SOURCE, "source");
  const temporary = mkdtempSync(join(tmpdir(), "vibex-native-content-package-"));
  try {
    const debRoot = join(temporary, "deb");
    run("dpkg-deb", ["-x", DEB, debRoot]);
    run(APPIMAGE, ["--appimage-extract"], { cwd: temporary });
    const appImageRoot = join(temporary, "squashfs-root");
    const debPdfium = join(debRoot, PACKAGE_RESOURCE);
    const appImagePdfium = join(appImageRoot, PACKAGE_RESOURCE);
    const debWeb = verifyWebBuild(join(debRoot, WEB_PACKAGE_RESOURCE), ".deb");
    const appImageWeb = verifyWebBuild(join(appImageRoot, WEB_PACKAGE_RESOURCE), "AppImage");
    assertWebBuildMatches(sourceWebBuild, debWeb, ".deb");
    assertWebBuildMatches(sourceWebBuild, appImageWeb, "AppImage");
    verifyNativeWebBinding(
      join(debRoot, "usr/bin/vibex-desktop"),
      sourceWebBuild.build,
      ".deb"
    );
    verifyNativeWebBinding(
      join(appImageRoot, "usr/bin/vibex-desktop"),
      sourceWebBuild.build,
      "AppImage"
    );
    const debLibrary = join(debPdfium, archive.libraryPath);
    const appImageLibrary = join(appImagePdfium, archive.libraryPath);

    const debIdentity = fileIdentity(debLibrary);
    assert(debIdentity.bytes === archive.libraryBytes, ".deb PDFium byte count drifted");
    assert(debIdentity.sha256 === archive.librarySha256, ".deb PDFium hash drifted");
    verifyLicenseBundle(debPdfium, ".deb");
    verifyLicenseBundle(appImagePdfium, "AppImage");

    const sourceElf = elfIdentity(join(PREPARED, archive.libraryPath));
    const appImageElf = elfIdentity(appImageLibrary);
    assert(appImageElf.buildId === sourceElf.buildId, "AppImage PDFium Build ID drifted");
    assert(JSON.stringify(appImageElf.needed) === JSON.stringify(sourceElf.needed),
      "AppImage PDFium NEEDED set drifted");
    assert(appImageElf.runpath === "$ORIGIN", "AppImage PDFium RUNPATH is not the bounded transform");

    const debProbe = probe(join(debRoot, "usr/bin/vibex-desktop"));
    const appImageProbe = probe(APPIMAGE, ["--appimage-extract-and-run"]);
    assert(JSON.stringify(debProbe) === JSON.stringify(appImageProbe),
      ".deb and AppImage probes differ");

    console.log(JSON.stringify({
      status: "passed",
      deb: fileIdentity(DEB),
      appImage: fileIdentity(APPIMAGE),
      pdfium: {
        sourceSha256: archive.librarySha256,
        appImageSha256: sha256(appImageLibrary),
        buildId: sourceElf.buildId,
        needed: sourceElf.needed,
        appImageRunpath: appImageElf.runpath,
        licenseFiles: REVIEW.licenseFiles.length
      },
      webBuild: {
        buildId: sourceWebBuild.build.buildId,
        gitCommit: sourceWebBuild.build.gitCommit,
        assetCount: REQUIRED_WEB_ASSETS.length
      }
    }, null, 2));
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

try {
  validate();
} catch (error) {
  console.error(error.message);
  process.exit(1);
}

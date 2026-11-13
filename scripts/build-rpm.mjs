#!/usr/bin/env node
// Build a Linux .rpm from the .deb that cargo-packager produced.
//
// cargo-packager 0.11.8 has no RPM bundler, so the reviewed deb payload is the
// single source of truth: the script extracts `data.tar.*` from the ar archive,
// stages the installed tree, and drives `rpmbuild` with a generated spec. The
// installed files, their modes, and the PDFium resources therefore stay byte
// identical to the .deb; only the surrounding package metadata changes.
//
// `rpmbuild` reads ELF NEEDED entries to synthesize the library requirements,
// which works for a cross-built aarch64 tree as well as a native x86_64 one.
// The brp strip scripts are neutralized because they would rewrite the already
// reviewed binaries and cannot strip a foreign architecture.

import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import * as nodeZlib from "node:zlib";

import { parseAr } from "./compress-deb.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DEFAULT_LICENSE = "AGPL-3.0-or-later";
const DEFAULT_URL = "https://github.com/vibex-ai/vibex";
const HOST_TRIPLES = {
  x64: "x86_64-unknown-linux-gnu",
  arm64: "aarch64-unknown-linux-gnu"
};

function fail(message) {
  throw new Error(message);
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? ROOT,
    input: options.input,
    encoding: options.encoding ?? "utf8",
    maxBuffer: 1024 * 1024 * 1024,
    env: options.env ?? process.env,
    stdio: options.stdio ?? (options.input ? ["pipe", "pipe", "inherit"] : "inherit"),
    windowsHide: true
  });
  if (result.error) fail(`${command} failed to start: ${result.error.message}`);
  if (result.status !== 0) fail(`${command} ${args.join(" ")} exited with status ${result.status ?? "unknown"}`);
  return result.stdout ?? "";
}

function fieldValue(source, field) {
  const match = source.match(new RegExp(`^${field}\\s*=\\s*"((?:[^"\\\\]|\\\\.)*)"$`, "m"));
  return match ? match[1].replace(/\\"/g, '"').replace(/\\\\/g, "\\") : null;
}

// The RPM version cannot carry the `-` that separates a prerelease suffix, so
// the first hyphen becomes a `~`, which RPM orders below the final release. Any
// other character rpmbuild rejects is surfaced instead of silently shipped.
function rpmVersion(version) {
  const normalized = version.replace(/^v/u, "").replace("-", "~");
  if (!/^[0-9A-Za-z._~+]+$/u.test(normalized)) {
    fail(`cannot express version ${version} as an RPM version`);
  }
  return normalized;
}

function rpmArchitecture(triple) {
  const arch = triple.split("-")[0];
  if (arch !== "x86_64" && arch !== "aarch64") {
    fail(`unsupported RPM architecture from target triple ${triple}`);
  }
  return arch;
}

function readPackagerMetadata(configPath) {
  const source = readFileSync(resolve(ROOT, configPath), "utf8");
  const name = fieldValue(source, "name") ?? fail(`${configPath} is missing name`);
  const version = fieldValue(source, "version") ?? fail(`${configPath} is missing version`);
  const description = fieldValue(source, "description") ?? fieldValue(source, "productName") ?? name;
  return {
    name,
    version,
    description,
    publisher: fieldValue(source, "publisher") ?? "Vibex"
  };
}

function decompressTar(member) {
  if (member.name.endsWith(".tar")) return member.data;
  if (member.name.endsWith(".tar.gz")) return nodeZlib.gunzipSync(member.data);
  if (member.name.endsWith(".tar.xz")) {
    return run("xz", ["--decompress", "--stdout"], { input: member.data, encoding: "buffer" });
  }
  if (member.name.endsWith(".tar.zst")) {
    // Node 22.15+ can inflate zstd in-process; older runtimes fall back to the
    // host tool. cargo-packager gzips the deb and compress-deb.mjs repacks it to
    // xz, so this branch only serves a hand-built or foreign .deb.
    if (typeof nodeZlib.zstdDecompressSync === "function") {
      return nodeZlib.zstdDecompressSync(member.data);
    }
    return run("zstd", ["--decompress", "--stdout", "--quiet"], { input: member.data, encoding: "buffer" });
  }
  fail(`unsupported deb payload compression: ${member.name}`);
}

function extractDebPayload(debPath, stagingDirectory) {
  const members = parseAr(readFileSync(debPath));
  const dataTar = members.find((member) => member.name.startsWith("data.tar"));
  if (!dataTar) fail(`${debPath} has no data.tar member`);
  const tar = decompressTar(dataTar);
  run("tar", [
    "--extract",
    "--preserve-permissions",
    "--no-same-owner",
    "--file",
    "-",
    "--directory",
    stagingDirectory
  ], { input: tar, stdio: ["pipe", "inherit", "inherit"] });
}

// Every staged path as an absolute entry. Directories are implicit once a file
// below them is packaged, so only directories with no packaged descendant are
// listed explicitly with `%dir`; that keeps /usr/bin and the hicolor icon tree
// owned by the packages that already provide them.
function collectFiles(root) {
  const files = [];
  const directories = [];
  const walk = (current, relativePath) => {
    const entries = readdirSync(current, { withFileTypes: true });
    let containsFile = false;
    for (const entry of entries) {
      const absolute = join(current, entry.name);
      const entryPath = `${relativePath}/${entry.name}`;
      if (entry.isDirectory()) {
        if (walk(absolute, entryPath)) containsFile = true;
      } else {
        files.push(entryPath);
        containsFile = true;
      }
    }
    if (!containsFile && relativePath !== "") directories.push(relativePath);
    return containsFile;
  };
  walk(root, "");
  files.sort();
  directories.sort();
  return { files, directories };
}

function specEscape(value) {
  return value.replace(/%/gu, "%%").replace(/\r?\n/gu, " ").trim();
}

function renderSpec({ name, version, release, summary, license, url, vendor, description }) {
  return [
    "# Generated by scripts/build-rpm.mjs; the installed tree comes from the reviewed deb.",
    "%global __strip /bin/true",
    "%global __objcopy /bin/true",
    "",
    `Name:           ${name}`,
    `Version:        ${version}`,
    `Release:        ${release}`,
    `Summary:        ${specEscape(summary)}`,
    `License:        ${license}`,
    `URL:            ${url}`,
    `Vendor:         ${specEscape(vendor)}`,
    "Requires:       /bin/sh",
    "AutoReqProv:    yes",
    "",
    "%description",
    specEscape(description),
    "",
    "%prep",
    "",
    "%build",
    "",
    "%install",
    "rm -rf %{buildroot}",
    "mkdir -p %{buildroot}",
    "cp -a %{vibex_stage}/. %{buildroot}/",
    "",
    "%files -f %{vibex_files}",
    ""
  ].join("\n");
}

function writeFileList(path, files, directories) {
  const lines = [
    ...files,
    ...directories.map((directory) => `%dir ${directory}`)
  ];
  writeFileSync(path, `${lines.join("\n")}\n`);
}

function findSingleDeb(directory) {
  const entries = readdirSync(directory).filter((entry) => entry.endsWith(".deb"));
  if (entries.length !== 1) {
    fail(`expected exactly one .deb in ${directory}, found ${entries.length}`);
  }
  return join(directory, entries[0]);
}

function findBuiltRpm(rpmsDirectory) {
  const found = [];
  const walk = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (entry.isFile() && entry.name.endsWith(".rpm")) found.push(path);
    }
  };
  walk(rpmsDirectory);
  if (found.length !== 1) fail(`expected exactly one built rpm under ${rpmsDirectory}, found ${found.length}`);
  return found[0];
}

/**
 * Build one `.rpm` from a cargo-packager `.deb`. Returns the written path.
 */
export function buildRpmPackage({
  deb,
  outDir,
  configPath,
  target,
  license = DEFAULT_LICENSE
}) {
  const debPath = resolve(ROOT, deb);
  if (!existsSync(debPath)) fail(`no deb package to convert: ${debPath}`);
  if (!existsSync(resolve(ROOT, configPath))) fail(`packager configuration is missing: ${configPath}`);
  const triple = target ?? HOST_TRIPLES[process.arch];
  if (!triple) fail(`unsupported host architecture: ${process.arch}`);
  const arch = rpmArchitecture(triple);
  const metadata = readPackagerMetadata(configPath);
  const outputDirectory = resolve(ROOT, outDir);
  mkdirSync(outputDirectory, { recursive: true });

  const temporary = mkdtempSync(join(tmpdir(), "vibex-rpm-"));
  try {
    const staging = join(temporary, "stage");
    mkdirSync(staging, { recursive: true });
    extractDebPayload(debPath, staging);
    const { files, directories } = collectFiles(staging);
    if (files.length === 0) fail(`${debPath} carries an empty data payload`);

    const topdir = join(temporary, "rpmbuild");
    for (const name of ["BUILD", "BUILDROOT", "RPMS", "SOURCES", "SPECS", "SRPMS"]) {
      mkdirSync(join(topdir, name), { recursive: true });
    }
    const version = rpmVersion(metadata.version);
    const specPath = join(topdir, "SPECS", `${metadata.name}.spec`);
    const fileListPath = join(topdir, "SPECS", `${metadata.name}.files`);
    writeFileSync(
      specPath,
      renderSpec({
        name: metadata.name,
        version,
        release: "1",
        summary: metadata.description,
        license,
        url: DEFAULT_URL,
        vendor: metadata.publisher,
        description: metadata.description
      })
    );
    writeFileList(fileListPath, files, directories);
    run("rpmbuild", [
      "-bb",
      specPath,
      "--target",
      triple,
      "--define",
      `_topdir ${topdir}`,
      "--define",
      `vibex_stage ${staging}`,
      "--define",
      `vibex_files ${fileListPath}`,
      "--define",
      "_binary_payload w9.gzdio"
    ]);
    const built = findBuiltRpm(join(topdir, "RPMS"));
    const destination = join(outputDirectory, `${metadata.name}-${version}-1.${arch}.rpm`);
    // rpmbuild's temporary topdir and the package directory can live on
    // different filesystems, where rename(2) returns EXDEV.
    copyFileSync(built, destination);
    rmSync(built, { force: true });
    console.log(`Built ${destination}`);
    return destination;
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

function parseArguments(argv) {
  const values = {};
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    if (!flag.startsWith("--")) fail(`unexpected argument ${flag}`);
    const value = argv[++index];
    if (!value || value.startsWith("--")) fail(`${flag} requires a value`);
    values[flag.slice(2)] = value;
  }
  if (!values.config) fail("--config is required");
  if (!values["out-dir"]) fail("--out-dir is required");
  return values;
}

function main() {
  const values = parseArguments(process.argv.slice(2));
  const outDir = resolve(ROOT, values["out-dir"]);
  if (!values.deb && !existsSync(outDir)) {
    fail(`the package output directory does not exist: ${values["out-dir"]}`);
  }
  const deb = values.deb ?? findSingleDeb(outDir);
  buildRpmPackage({
    deb,
    outDir: values["out-dir"],
    configPath: values.config,
    target: values.target,
    license: values.license ?? DEFAULT_LICENSE
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    main();
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}

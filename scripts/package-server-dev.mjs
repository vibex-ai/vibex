#!/usr/bin/env node
// Package a local `vibex-server` for the development Remote-instances flow.
//
// The signed release manifest is the only artifact source a release build has,
// and it needs `VIBEX_UPDATE_PUBLIC_KEY` at compile time. A debug build has no
// such key, so `apps/desktop` accepts a locally built tarball through
// `VIBEX_DEV_SERVER_ARTIFACT` instead (see
// `apps/desktop/src/ssh_connection/dev_artifact.rs`). This script builds that
// tarball and prints the environment it wants.
//
// The output is the same shape the release ships — a gzipped tar whose only
// top-level entry is `vibex-server` — because the remote install script and the
// upgrade transaction verify exactly that.
//
// Usage:
//   node scripts/package-server-dev.mjs                 # debug build, linux
//   node scripts/package-server-dev.mjs --release       # release build
//   node scripts/package-server-dev.mjs --target aarch64-unknown-linux-gnu
//
// Then, in the shell that runs `pnpm dev:desktop`:
//   export VIBEX_DEV_SERVER_ARTIFACT=target/dev-server-artifacts/vibex-server-<version>-linux-<arch>.tar.gz

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  readFileSync,
  rmSync,
  statSync
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const OUTPUT = join(ROOT, "target", "dev-server-artifacts");

const TARGET_ARCH = {
  "x86_64-unknown-linux-gnu": "x86_64",
  "aarch64-unknown-linux-gnu": "aarch64",
  "x86_64-apple-darwin": "x86_64",
  "aarch64-apple-darwin": "aarch64"
};

function fail(message) {
  console.error(`package-server-dev: ${message}`);
  process.exit(1);
}

function parseArguments(argv) {
  const values = { release: false, target: undefined };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    if (flag === "--release") {
      values.release = true;
      continue;
    }
    if (flag === "--target") {
      const value = argv[index + 1];
      if (!value || value.startsWith("--")) fail("--target requires a value");
      values.target = value;
      index += 1;
      continue;
    }
    fail(`unknown argument ${flag}`);
  }
  return values;
}

const { release, target } = parseArguments(process.argv.slice(2));

// The workspace version is the version the desktop reports, and it is what the
// installer puts under `server/<version>/`; reading it from Cargo keeps the two
// from drifting.
function workspaceVersion() {
  const metadata = JSON.parse(
    execFileSync("cargo", ["metadata", "--locked", "--no-deps", "--format-version", "1"], {
      cwd: ROOT,
      encoding: "utf8"
    })
  );
  const desktop = metadata.packages.find((pkg) => pkg.name === "vibex-desktop");
  if (!desktop) fail("cargo metadata does not list vibex-desktop");
  return desktop.version;
}

const version = workspaceVersion();
const arch = target
  ? TARGET_ARCH[target]
  : TARGET_ARCH[`${process.arch === "arm64" ? "aarch64" : "x86_64"}-${process.platform === "darwin" ? "apple-darwin" : "unknown-linux-gnu"}`];
if (!arch) fail(`unsupported target ${target ?? `${process.platform}/${process.arch}`}`);
if (process.platform !== "linux" && process.platform !== "darwin") {
  fail("the headless runtime only builds for linux and macOS");
}
const system = process.platform === "darwin" ? "macos" : "linux";

const cargoArgs = [
  "build",
  "-p",
  "vibex-server",
  "--locked",
  ...(release ? ["--release"] : []),
  ...(target ? ["--target", target] : [])
];
console.log(`package-server-dev: cargo ${cargoArgs.join(" ")}`);
execFileSync("cargo", cargoArgs, { cwd: ROOT, stdio: "inherit" });

const profileDirectory = release ? "release" : "debug";
const binary = join(
  ROOT,
  "target",
  ...(target ? [target] : []),
  profileDirectory,
  "vibex-server"
);
let binaryStat;
try {
  binaryStat = statSync(binary);
} catch {
  fail(`the build did not produce ${binary}`);
}
if (!binaryStat.isFile() || binaryStat.size === 0) fail(`${binary} is not a non-empty file`);

mkdirSync(OUTPUT, { recursive: true });
const name = `vibex-server-${version}-${system}-${arch}.tar.gz`;
const tarball = join(OUTPUT, name);
const stage = join(OUTPUT, `.stage-${process.pid}`);
rmSync(stage, { recursive: true, force: true });
mkdirSync(stage, { recursive: true });
try {
  const staged = join(stage, "vibex-server");
  copyFileSync(binary, staged);
  chmodSync(staged, 0o755);
  // One top-level file, normalised ownership: the remote install script
  // compares `tar -tzf` against exactly `vibex-server`.
  execFileSync(
    "tar",
    ["--sort=name", "--mtime=@0", "--owner=0", "--group=0", "--numeric-owner", "-czf", tarball, "-C", stage, "vibex-server"],
    { stdio: ["ignore", "ignore", "pipe"] }
  );
} finally {
  rmSync(stage, { recursive: true, force: true });
}

const sha256 = createHash("sha256").update(readFileSync(tarball)).digest("hex");
console.log("");
console.log("packaged the development server artifact:");
console.log(`  ${tarball}`);
console.log(`  version ${version}  ${system}/${arch}  ${statSync(tarball).size} bytes  sha256 ${sha256}`);
console.log("");
console.log("point the development desktop at it before `pnpm dev:desktop`:");
console.log(`  export VIBEX_DEV_SERVER_ARTIFACT=${tarball}`);
console.log("");
console.log("this override only exists in debug builds; a packaged desktop keeps using the signed manifest.");

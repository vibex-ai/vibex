#!/usr/bin/env node
// Package one checked headless server binary as a release asset.
//
// Output:
//   vibex-server-<version>-<os>-<arch>.tar.gz
//   vibex-server-<version>-<os>-<arch>.tar.gz.sha256
//
// The tarball contains exactly one top-level file named `vibex-server`, so the
// installer never has to guess a path. The archive is written with normalised
// ownership and timestamps to keep rebuilds comparable.
//
// Usage:
//   node scripts/package-server-release.mjs \
//     --version 0.2.0 --arch x86_64 --os linux \
//     --binary target/x86_64-unknown-linux-gnu/release/vibex-server \
//     --output target/server-artifacts

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync
} from "node:fs";
import { basename, join, resolve } from "node:path";

const ARCHES = new Set(["x86_64", "aarch64"]);
const SYSTEMS = new Set(["linux", "macos"]);

function fail(message) {
  console.error(`package-server-release: ${message}`);
  process.exit(1);
}

function parseArguments(argv) {
  const values = { os: "linux" };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag.startsWith("--") || value === undefined) {
      fail(`invalid argument ${flag ?? ""}`.trim());
    }
    index += 1;
    const key = {
      "--version": "version",
      "--arch": "arch",
      "--os": "os",
      "--binary": "binary",
      "--output": "output"
    }[flag];
    if (!key) fail(`unknown argument ${flag}`);
    values[key] = value;
  }
  for (const required of ["version", "arch", "binary", "output"]) {
    if (!values[required]) fail(`--${required} is required`);
  }
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(values.version)) {
    fail("--version is not a release version");
  }
  if (!ARCHES.has(values.arch)) {
    fail("--arch must be x86_64 or aarch64");
  }
  if (!SYSTEMS.has(values.os)) {
    fail("--os must be linux or macos");
  }
  return values;
}

const { version, arch, os, binary, output } = parseArguments(process.argv.slice(2));
const binaryPath = resolve(binary);
const outputDir = resolve(output);
let binaryStat;
try {
  binaryStat = statSync(binaryPath);
} catch {
  fail(`binary does not exist: ${binaryPath}`);
}
if (!binaryStat.isFile() || binaryStat.size === 0) {
  fail(`binary is not a non-empty file: ${binaryPath}`);
}

mkdirSync(outputDir, { recursive: true });
const name = `vibex-server-${version}-${os}-${arch}.tar.gz`;
const tarball = join(outputDir, name);
const stage = join(outputDir, `.stage-server-${arch}-${process.pid}`);
rmSync(stage, { recursive: true, force: true });
mkdirSync(stage, { recursive: true });

try {
  const stagedBinary = join(stage, "vibex-server");
  copyFileSync(binaryPath, stagedBinary);
  chmodSync(stagedBinary, 0o755);
  const tarArgs =
    os === "linux"
      ? [
          "--sort=name",
          "--mtime=@0",
          "--owner=0",
          "--group=0",
          "--numeric-owner",
          "-czf",
          tarball,
          "-C",
          stage,
          "vibex-server"
        ]
      : ["-czf", tarball, "-C", stage, "vibex-server"];
  execFileSync("tar", tarArgs, { stdio: ["ignore", "ignore", "pipe"] });
} finally {
  rmSync(stage, { recursive: true, force: true });
}

const digest = createHash("sha256").update(readFileSync(tarball)).digest("hex");
writeFileSync(join(outputDir, `${name}.sha256`), `${digest}  ${basename(tarball)}\n`);
console.log(
  JSON.stringify(
    {
      artifact: tarball,
      sha256: digest,
      size: statSync(tarball).size
    },
    null,
    2
  )
);

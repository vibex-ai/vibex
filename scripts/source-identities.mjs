import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join, resolve, sep } from "node:path";

export const GPUI_DEPENDENCY_SOURCE_POLICY = "fork_submodule_root_cargo_lock";
export const ZED_REPOSITORY = "https://github.com/vibex-ai/zed.git";
export const ZED_SUBMODULE_PATH = "vendor/zed";
export const UPSTREAM_ZED_REPOSITORY = "https://github.com/zed-industries/zed";

const REGISTRY_SOURCE = "registry+https://github.com/rust-lang/crates.io-index";
// gpui-kit 0.6.0 ships gpui-component from crates.io instead of a Git fork, so
// the component identity is derived from the locked registry packages instead
// of a commit. The list is ordered; the digest is over this exact order.
const GPUI_COMPONENT_REGISTRY_PACKAGES = [
  "gpui-component",
  "gpui-component-macros",
  "gpui-kit-assets",
];

function git(root, args, label) {
  const result = spawnSync("git", args, { cwd: root, encoding: "utf8" });
  if (result.error) throw new Error(`${label} failed to start: ${result.error.message}`);
  if (result.status !== 0) {
    throw new Error(result.stderr || `${label} exited ${result.status ?? 1}`);
  }
  return result.stdout.trim();
}

function metadata(root) {
  const result = spawnSync(
    "cargo",
    ["metadata", "--locked", "--format-version", "1"],
    { cwd: root, encoding: "utf8", maxBuffer: 128 * 1024 * 1024 }
  );
  if (result.error) throw new Error(`cargo metadata failed to start: ${result.error.message}`);
  if (result.status !== 0) {
    throw new Error(result.stderr || `cargo metadata exited ${result.status ?? 1}`);
  }
  return JSON.parse(result.stdout);
}

function singlePackage(packages, name) {
  const matches = packages.filter((pkg) => pkg.name === name);
  if (matches.length !== 1) {
    throw new Error(`expected one ${name} package, found ${matches.length}`);
  }
  return matches[0];
}

function packageIsInSubmodule(pkg, root) {
  const submoduleRoot = resolve(root, ZED_SUBMODULE_PATH);
  const manifestPath = resolve(pkg.manifest_path);
  return pkg.source === null && manifestPath.startsWith(`${submoduleRoot}${sep}`);
}

function lockedRegistryPackages(root, names) {
  const wanted = new Set(names);
  const found = new Map();
  const lock = readFileSync(join(root, "Cargo.lock"), "utf8");
  for (const block of lock.split("[[package]]").slice(1)) {
    const field = (key) => block.match(new RegExp(`^${key} = "(.*)"$`, "m"))?.[1];
    const name = field("name");
    if (name === undefined || !wanted.has(name)) continue;
    found.set(name, { version: field("version"), checksum: field("checksum") });
  }
  return found;
}

function registryIdentity(root, packages, names) {
  const locked = lockedRegistryPackages(root, names);
  const lines = [];
  for (const name of names) {
    const pkg = singlePackage(packages, name);
    if (pkg.source !== REGISTRY_SOURCE) {
      throw new Error(`${name} resolved from unexpected source: ${pkg.source ?? "path"}`);
    }
    const entry = locked.get(name);
    if (entry?.version !== pkg.version || !/^[a-f0-9]{64}$/.test(entry?.checksum ?? "")) {
      throw new Error(`${name} is not locked from ${REGISTRY_SOURCE} with a checksum`);
    }
    lines.push(`${name}@${entry.version}:${entry.checksum}\n`);
  }
  // The identity fills the historical 40-hex revision slot, so downstream
  // evidence fields keep their format; it is a fingerprint, not a Git commit.
  return createHash("sha1").update(lines.join("")).digest("hex");
}

export function resolveZedSubmoduleRevision(root) {
  const revision = git(
    root,
    ["-C", ZED_SUBMODULE_PATH, "rev-parse", "--verify", "HEAD"],
    "Zed submodule revision lookup"
  );
  if (!/^[a-f0-9]{40}$/.test(revision)) {
    throw new Error(`Zed submodule HEAD is not a full Git revision: ${revision}`);
  }
  return revision;
}

// The gpui-component layer identity shared between evidence classification and
// graph verification, derivable from an already-resolved cargo metadata graph.
export function gpuiComponentIdentity(root, packages) {
  return registryIdentity(root, packages, GPUI_COMPONENT_REGISTRY_PACKAGES);
}

export function resolveGpuiSourceIdentities(root) {
  const graph = metadata(root);
  for (const name of ["gpui-pre", "gpui_platform", "gpui_tokio"]) {
    const pkg = singlePackage(graph.packages, name);
    if (!packageIsInSubmodule(pkg, root)) {
      throw new Error(`${name} resolved outside ${ZED_SUBMODULE_PATH}: ${pkg.source ?? pkg.manifest_path}`);
    }
  }
  const zedRevision = resolveZedSubmoduleRevision(root);
  const gpuiComponentRevision = gpuiComponentIdentity(root, graph.packages);
  return {
    dependencySourcePolicy: GPUI_DEPENDENCY_SOURCE_POLICY,
    zedRevision,
    gpuiComponentRevision
  };
}

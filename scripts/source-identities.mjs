import { spawnSync } from "node:child_process";
import { resolve, sep } from "node:path";

export const GPUI_DEPENDENCY_SOURCE_POLICY = "fork_submodule_root_cargo_lock";
export const ZED_REPOSITORY = "https://github.com/vibex-ai/zed.git";
export const ZED_SUBMODULE_PATH = "vendor/zed";
export const UPSTREAM_ZED_REPOSITORY = "https://github.com/zed-industries/zed";
export const GPUI_COMPONENT_REPOSITORY = "https://github.com/longbridge/gpui-component";

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

function commitFromSource(source, repository, packageName) {
  const expectedPrefix = `git+${repository}#`;
  if (!source?.startsWith(expectedPrefix)) {
    throw new Error(`${packageName} resolved from unexpected source: ${source ?? "path"}`);
  }
  const commit = source.slice(expectedPrefix.length);
  if (!/^[a-f0-9]{40}$/.test(commit)) {
    throw new Error(`${packageName} source is not an unqualified locked Git commit: ${source}`);
  }
  return commit;
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

export function resolveGpuiSourceIdentities(root) {
  const graph = metadata(root);
  for (const name of ["gpui", "gpui_platform", "gpui_tokio"]) {
    const pkg = singlePackage(graph.packages, name);
    if (!packageIsInSubmodule(pkg, root)) {
      throw new Error(`${name} resolved outside ${ZED_SUBMODULE_PATH}: ${pkg.source ?? pkg.manifest_path}`);
    }
  }
  const zedRevision = resolveZedSubmoduleRevision(root);
  const gpuiComponentRevision = commitFromSource(
    singlePackage(graph.packages, "gpui-component").source,
    GPUI_COMPONENT_REPOSITORY,
    "gpui-component"
  );
  return {
    dependencySourcePolicy: GPUI_DEPENDENCY_SOURCE_POLICY,
    zedRevision,
    gpuiComponentRevision
  };
}

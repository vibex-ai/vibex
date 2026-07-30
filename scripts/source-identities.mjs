import { spawnSync } from "node:child_process";

export const GPUI_DEPENDENCY_SOURCE_POLICY = "upstream_git_root_cargo_lock";

const ZED_REPOSITORY = "https://github.com/zed-industries/zed";
const COMPONENT_REPOSITORY = "https://github.com/longbridge/gpui-component";

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

export function resolveGpuiSourceIdentities(root) {
  const graph = metadata(root);
  const zedPackages = graph.packages.filter((pkg) =>
    pkg.source?.startsWith(`git+${ZED_REPOSITORY}`)
  );
  const zedCommits = new Set(
    zedPackages.map((pkg) => commitFromSource(pkg.source, ZED_REPOSITORY, pkg.name))
  );
  if (zedCommits.size !== 1) {
    throw new Error(`Zed packages resolve from multiple commits: ${[...zedCommits].join(", ")}`);
  }
  const zedRevision = [...zedCommits][0];
  const gpuiComponentRevision = commitFromSource(
    singlePackage(graph.packages, "gpui-component").source,
    COMPONENT_REPOSITORY,
    "gpui-component"
  );
  return {
    dependencySourcePolicy: GPUI_DEPENDENCY_SOURCE_POLICY,
    zedRevision,
    gpuiComponentRevision
  };
}

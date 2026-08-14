import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import {
  GPUI_COMPONENT_REPOSITORY,
  GPUI_DEPENDENCY_SOURCE_POLICY,
  UPSTREAM_ZED_REPOSITORY,
  ZED_REPOSITORY,
  ZED_SUBMODULE_PATH,
  resolveZedSubmoduleRevision
} from "./source-identities.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const ZED_ROOT = join(ROOT, ZED_SUBMODULE_PATH);
const SHARED_UI_ROOT = join(ROOT, "crates", "vibex-ui");
const SHARED_BACKEND_ROOT = join(ROOT, "crates", "vibex-backend");
const SHARED_TOKEN_SOURCE = join(SHARED_UI_ROOT, "theme", "tokens.json");
const SHARED_GENERATED_TOKENS = join(SHARED_UI_ROOT, "src", "generated_tokens.rs");
const LEGACY_DESKTOP_TOKENS = join(ROOT, "apps", "desktop", "src", "generated_tokens.rs");

function fail(message) {
  console.error(message);
  process.exit(1);
}

function metadata(target = null) {
  const args = ["metadata", "--locked", "--format-version", "1"];
  if (target) args.push("--filter-platform", target);
  const result = spawnSync(
    "cargo",
    args,
    { cwd: ROOT, encoding: "utf8", maxBuffer: 128 * 1024 * 1024 }
  );
  if (result.status !== 0) {
    process.stderr.write(result.stderr);
    process.exit(result.status ?? 1);
  }
  return JSON.parse(result.stdout);
}

function findLockfiles(path = ROOT) {
  const locks = [];
  for (const entry of readdirSync(path)) {
    if ([".git", ".trellis", "node_modules", "target"].includes(entry)) continue;
    const child = join(path, entry);
    if (resolve(child) === ZED_ROOT) continue;
    if (statSync(child).isDirectory()) {
      locks.push(...findLockfiles(child));
    } else if (entry === "Cargo.lock") {
      locks.push(relative(ROOT, child));
    }
  }
  return locks.sort();
}

function findSourceFiles(path) {
  const files = [];
  for (const entry of readdirSync(path)) {
    const child = join(path, entry);
    if (statSync(child).isDirectory()) {
      files.push(...findSourceFiles(child));
    } else if (/\.(?:json|rs|toml)$/.test(entry)) {
      files.push(child);
    }
  }
  return files;
}

function assertSharedUiIsolation() {
  if (!existsSync(SHARED_TOKEN_SOURCE) || !existsSync(SHARED_GENERATED_TOKENS)) {
    fail("vibex-ui must own both the structured token source and generated Rust values");
  }
  if (existsSync(LEGACY_DESKTOP_TOKENS)) {
    fail("desktop must not retain a private generated_tokens.rs module");
  }
  const forbiddenReferences = [
    "apps/web",
    "apps/mobile-wasm",
    "packages/ui",
    "@vibex/ui",
    "react",
    "tailwind",
    "shadcn",
    "apps/desktop/src/styles.css"
  ];
  for (const file of findSourceFiles(SHARED_UI_ROOT)) {
    const source = readFileSync(file, "utf8").toLowerCase();
    for (const reference of forbiddenReferences) {
      if (source.includes(reference.toLowerCase())) {
        fail(`${relative(ROOT, file)} references frozen UI input ${reference}`);
      }
    }
  }
  const manifest = readFileSync(join(SHARED_UI_ROOT, "Cargo.toml"), "utf8");
  for (const dependency of [
    "vibex-desktop-runtime",
    "tauri",
    "wry",
    "tokio",
    "rusqlite",
    "gtk",
    "webkit2gtk"
  ]) {
    if (new RegExp(`^${dependency.replaceAll("-", "\\-")}\\s*=`, "m").test(manifest)) {
      fail(`vibex-ui must stay platform-neutral; found ${dependency}`);
    }
  }
}

function assertSharedBackendIsolation() {
  const manifestPath = join(SHARED_BACKEND_ROOT, "Cargo.toml");
  const libraryPath = join(SHARED_BACKEND_ROOT, "src", "lib.rs");
  const nativePath = join(SHARED_BACKEND_ROOT, "src", "native.rs");
  if (!existsSync(manifestPath) || !existsSync(libraryPath) || !existsSync(nativePath)) {
    fail("vibex-backend must provide its manifest, shared contracts, and native adapter");
  }

  const manifest = readFileSync(manifestPath, "utf8");
  const sharedDependencies = manifest.match(/\[dependencies\]([\s\S]*?)(?=\n\[|$)/)?.[1];
  if (!sharedDependencies) fail("vibex-backend is missing shared dependencies");
  for (const dependency of [
    "vibex-desktop-runtime",
    "tokio",
    "rusqlite",
    "pdfium-render",
    "tauri",
    "wry",
    "gtk",
    "webkit2gtk"
  ]) {
    if (new RegExp(`^${dependency.replaceAll("-", "\\-")}\\s*=`, "m").test(sharedDependencies)) {
      fail(`vibex-backend default graph must stay platform-neutral; found ${dependency}`);
    }
  }
  if (!manifest.includes("[target.'cfg(not(target_family = \"wasm\"))'.dependencies]")) {
    fail("vibex-backend must scope native runtime dependencies outside wasm targets");
  }
  if (!/native\s*=\s*\["dep:tokio",\s*"dep:vibex-desktop-runtime"\]/.test(manifest)) {
    fail("vibex-backend native feature must opt into Tokio and DesktopRuntime explicitly");
  }
  for (const dependency of ["tokio", "vibex-desktop-runtime"]) {
    const declaration = manifest.match(
      new RegExp(`^${dependency.replaceAll("-", "\\-")}\\s*=\\s*\\{[^\\n]+\\}$`, "m")
    )?.[0];
    if (!declaration?.includes("optional = true")) {
      fail(`vibex-backend native dependency ${dependency} must be optional`);
    }
  }

  const source = readFileSync(libraryPath, "utf8");
  if (!source.includes("#[cfg(all(feature = \"native\", not(target_family = \"wasm\")))]\nmod native;")) {
    fail("vibex-backend must cfg-gate NativeBackend outside wasm builds");
  }
}

function reachablePackageNames(graph, rootPackageId) {
  const packageById = new Map(graph.packages.map((pkg) => [pkg.id, pkg]));
  const nodesById = new Map((graph.resolve?.nodes ?? []).map((node) => [node.id, node]));
  const visited = new Set();
  const pending = [rootPackageId];
  while (pending.length) {
    const id = pending.pop();
    if (visited.has(id)) continue;
    visited.add(id);
    for (const dependency of nodesById.get(id)?.deps ?? []) {
      // Runtime graph checks must not treat test-only helpers (for example
      // tokio used by controller unit tests) as shipped WASM dependencies.
      const kinds = dependency.dep_kinds ?? [];
      if (kinds.length === 0 || kinds.some((kind) => kind.kind !== "dev")) {
        pending.push(dependency.pkg);
      }
    }
  }
  return new Set([...visited].map((id) => packageById.get(id)?.name).filter(Boolean));
}

function assertUnqualifiedGitDependency(manifest, dependency, repository) {
  const escaped = dependency.replaceAll("-", "\\-");
  const declaration = manifest.match(new RegExp(`^${escaped} = \\{[^\\n]+\\}$`, "m"))?.[0];
  if (!declaration) fail(`missing workspace dependency ${dependency}`);
  if (!declaration.includes(`git = "${repository}"`)) {
    fail(`${dependency} must use upstream Git ${repository}: ${declaration}`);
  }
  if (/\b(?:rev|tag|branch)\s*=/.test(declaration)) {
    fail(`${dependency} must not pin rev, tag, or branch: ${declaration}`);
  }
}

function manifestSection(manifest, header) {
  const marker = `${header}\n`;
  const start = manifest.indexOf(marker);
  if (start === -1) return "";
  const remainder = manifest.slice(start + marker.length);
  const nextHeader = remainder.search(/^\[/m);
  return nextHeader === -1 ? remainder : remainder.slice(0, nextHeader);
}

function assertPathDependency(section, dependency, path) {
  const escaped = dependency.replaceAll("-", "\\-");
  const declaration = section.match(new RegExp(`^${escaped} = \\{[^\\n]+\\}$`, "m"))?.[0];
  if (!declaration?.includes(`path = "${path}"`)) {
    fail(`${dependency} must resolve from ${path}: ${declaration ?? "missing"}`);
  }
  if (/\b(?:git|rev|tag|branch)\s*=/.test(declaration)) {
    fail(`${dependency} path dependency contains a Git selector: ${declaration}`);
  }
}

function packageIsInZedSubmodule(pkg) {
  const manifestPath = resolve(pkg.manifest_path);
  return pkg.source === null && manifestPath.startsWith(`${ZED_ROOT}${sep}`);
}

function assertSinglePackage(packages, name, expectedVersion = null) {
  const matches = packages.filter((pkg) => pkg.name === name);
  if (matches.length !== 1) fail(`expected one ${name} package, found ${matches.length}`);
  if (expectedVersion && matches[0].version !== expectedVersion) {
    fail(`expected ${name} ${expectedVersion}, found ${matches[0].version}`);
  }
  return matches[0];
}

function gitCommit(source, repository, packageName) {
  const escaped = repository.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = source?.match(new RegExp(`^git\\+${escaped}#([a-f0-9]{40})$`));
  if (!match) {
    fail(`${packageName} must resolve from unqualified Git ${repository}: ${source ?? "path"}`);
  }
  return match[1];
}

const rootManifest = readFileSync(join(ROOT, "Cargo.toml"), "utf8");
const workspaceDependencies = manifestSection(rootManifest, "[workspace.dependencies]");
for (const dependency of ["gpui", "gpui_platform", "gpui_tokio"]) {
  assertPathDependency(workspaceDependencies, dependency, `${ZED_SUBMODULE_PATH}/crates/${dependency}`);
}
for (const dependency of ["gpui-component", "gpui-component-assets"]) {
  assertUnqualifiedGitDependency(rootManifest, dependency, GPUI_COMPONENT_REPOSITORY);
}
if (!/^exclude = \["vendor\/zed"\]$/m.test(rootManifest)) {
  fail("the Zed submodule must be excluded from the Vibex workspace");
}
const zedPatch = manifestSection(rootManifest, `[patch."${UPSTREAM_ZED_REPOSITORY}"]`);
for (const dependency of ["gpui", "gpui_macros"]) {
  assertPathDependency(zedPatch, dependency, `${ZED_SUBMODULE_PATH}/crates/${dependency}`);
}

const gitmodules = readFileSync(join(ROOT, ".gitmodules"), "utf8");
for (const expected of [
  `[submodule "${ZED_SUBMODULE_PATH}"]`,
  `path = ${ZED_SUBMODULE_PATH}`,
  `url = ${ZED_REPOSITORY}`,
  "branch = main",
  "shallow = true"
]) {
  if (!gitmodules.includes(expected)) fail(`Zed submodule config is missing: ${expected}`);
}
const gitlink = spawnSync("git", ["ls-files", "--stage", "--", ZED_SUBMODULE_PATH], {
  cwd: ROOT,
  encoding: "utf8"
});
if (gitlink.status !== 0 || !/^160000 [a-f0-9]{40} 0\tvendor\/zed\s*$/.test(gitlink.stdout)) {
  fail(`${ZED_SUBMODULE_PATH} must be tracked as a Git submodule`);
}

const desktopManifest = readFileSync(join(ROOT, "apps", "desktop", "Cargo.toml"), "utf8");
for (const dependency of ["gpui-component", "gpui-component-assets"]) {
  if (!new RegExp(`^${dependency}\\.workspace = true$`, "m").test(desktopManifest)) {
    fail(`${dependency} must use the root workspace dependency`);
  }
}
if (/vendor\//.test(desktopManifest)) {
  fail("desktop Cargo.toml must not reference vendor paths");
}
if (!/^vibex-ui = \{ path = "\.\.\/\.\.\/crates\/vibex-ui" \}$/m.test(desktopManifest)) {
  fail("desktop must consume the shared vibex-ui crate");
}
assertSharedUiIsolation();
assertSharedBackendIsolation();

const graph = metadata();
const workspaceNames = graph.packages
  .filter((pkg) => graph.workspace_members.includes(pkg.id))
  .map((pkg) => pkg.name);
if (!workspaceNames.includes("vibex-desktop")) {
  fail("vibex-desktop is missing from workspace metadata");
}
if (!workspaceNames.includes("vibex-ui")) {
  fail("vibex-ui is missing from workspace metadata");
}
if (!workspaceNames.includes("vibex-backend")) {
  fail("vibex-backend is missing from workspace metadata");
}
const desktopPackage = assertSinglePackage(graph.packages, "vibex-desktop", "0.1.0-rc.1");
const sharedUiPackage = assertSinglePackage(graph.packages, "vibex-ui", "0.1.0-rc.1");
const sharedBackendPackage = assertSinglePackage(graph.packages, "vibex-backend", "0.1.0-rc.1");
if (!desktopPackage.dependencies.some((dependency) => dependency.name === sharedUiPackage.name)) {
  fail("desktop metadata does not depend on vibex-ui");
}
if (!desktopPackage.dependencies.some((dependency) =>
  dependency.name === sharedBackendPackage.name && dependency.features.includes("native")
)) {
  fail("desktop must compose vibex-backend with its native adapter feature");
}
if (!sharedUiPackage.dependencies.some((dependency) => dependency.name === sharedBackendPackage.name)) {
  fail("vibex-ui metadata does not depend on vibex-backend contracts");
}

const wasmGraph = metadata("wasm32-unknown-unknown");
const wasmSharedUiPackage = assertSinglePackage(wasmGraph.packages, "vibex-ui", "0.1.0-rc.1");
const wasmReachable = reachablePackageNames(wasmGraph, wasmSharedUiPackage.id);
for (const dependency of [
  "vibex-desktop-runtime",
  "vibex-terminal",
  "tokio",
  "rusqlite",
  "pdfium-render",
  "tauri",
  "wry"
]) {
  if (wasmReachable.has(dependency)) {
    fail(`vibex-ui wasm dependency graph must not reach native package ${dependency}`);
  }
}

const leakedZedGitPackages = graph.packages.filter(
  (pkg) =>
    pkg.source?.startsWith(`git+${UPSTREAM_ZED_REPOSITORY}`) ||
    pkg.source?.startsWith(`git+${ZED_REPOSITORY}`)
);
if (leakedZedGitPackages.length) {
  fail(`Zed packages escaped the submodule: ${leakedZedGitPackages.map((pkg) => pkg.name).join(", ")}`);
}
const zedPackages = graph.packages.filter(packageIsInZedSubmodule);
if (!zedPackages.length) fail(`Cargo metadata contains no packages from ${ZED_SUBMODULE_PATH}`);
const zedCommit = resolveZedSubmoduleRevision(ROOT);

for (const name of ["gpui", "gpui_platform", "gpui_tokio"]) {
  const pkg = assertSinglePackage(graph.packages, name);
  if (!packageIsInZedSubmodule(pkg)) {
    fail(`${name} does not resolve from ${ZED_SUBMODULE_PATH}`);
  }
}

const duplicateZedPackages = Object.entries(
  zedPackages.reduce((counts, pkg) => ({ ...counts, [pkg.name]: (counts[pkg.name] ?? 0) + 1 }), {})
).filter(([, count]) => count > 1);
if (duplicateZedPackages.length) {
  fail(`duplicate Zed packages: ${duplicateZedPackages.map(([name]) => name).join(", ")}`);
}

const componentPackages = [
  assertSinglePackage(graph.packages, "gpui-component", "0.5.2"),
  assertSinglePackage(graph.packages, "gpui-component-assets", "0.5.1"),
  assertSinglePackage(graph.packages, "gpui-component-macros", "0.5.1")
];
const componentCommits = new Set(
  componentPackages.map((pkg) => gitCommit(pkg.source, GPUI_COMPONENT_REPOSITORY, pkg.name))
);
if (componentCommits.size !== 1) {
  fail(`gpui-component packages resolve from multiple commits: ${[...componentCommits].join(", ")}`);
}
const componentCommit = [...componentCommits][0];

const tokenSource = JSON.parse(readFileSync(SHARED_TOKEN_SOURCE, "utf8"));
if (
  tokenSource.schemaVersion !== "vibex-design-tokens.v1" ||
  tokenSource.productVisualSource !== "apps/desktop" ||
  tokenSource.dependencySource?.policy !== GPUI_DEPENDENCY_SOURCE_POLICY ||
  tokenSource.dependencySource?.gpuiRevision !== zedCommit ||
  tokenSource.dependencySource?.gpuiComponentRevision !== componentCommit
) {
  fail("shared GPUI token source is stale against the resolved upstream graph");
}

const procMacroError = assertSinglePackage(graph.packages, "proc-macro-error2", "2.0.1");
if (!procMacroError.source?.startsWith("registry+https://github.com/rust-lang/crates.io-index")) {
  fail(`proc-macro-error2 must resolve from crates.io: ${procMacroError.source ?? "path"}`);
}

for (const name of ["ztracing", "ztracing_macro", "zlog"]) {
  const pkg = assertSinglePackage(graph.packages, name, "0.1.0");
  if (!packageIsInZedSubmodule(pkg)) {
    fail(`${name} does not resolve from ${ZED_SUBMODULE_PATH}`);
  }
}

const lockfiles = findLockfiles();
if (JSON.stringify(lockfiles) !== JSON.stringify(["Cargo.lock"])) {
  fail(`expected exactly one root Cargo.lock, found: ${lockfiles.join(", ")}`);
}
const vendorEntries = readdirSync(join(ROOT, "vendor")).sort();
if (JSON.stringify(vendorEntries) !== JSON.stringify(["zed"])) {
  fail(`vendor must contain only the Zed submodule, found: ${vendorEntries.join(", ")}`);
}

console.log(
  `GPUI graph verified: ${zedPackages.length} fork-submodule packages at ${zedCommit.slice(0, 12)}, ` +
    `gpui-component at ${componentCommit.slice(0, 12)}, shared UI isolated, ` +
    `shared backend wasm-isolated, crates.io proc-macro-error2, and forked Zed GPL tracing`
);

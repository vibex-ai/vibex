import { createHash } from "node:crypto";
import console from "node:console";
import {
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync
} from "node:fs";
import { dirname, extname, join, relative, resolve, sep } from "node:path";
import { spawnSync } from "node:child_process";
import process from "node:process";
import { fileURLToPath } from "node:url";
import parseSpdx from "spdx-expression-parse";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const POLICY_PATH = "docs/licenses/desktop-policy.json";
const SBOM_PATH = "docs/licenses/desktop.cdx.json";
const NOTICES_PATH = "docs/licenses/desktop-third-party-notices.md";
const ASSET_EXTENSIONS = new Set([
  ".gif",
  ".icns",
  ".ico",
  ".jpeg",
  ".jpg",
  ".otf",
  ".png",
  ".svg",
  ".ttf",
  ".webp",
  ".woff",
  ".woff2"
]);
const GENERATED_ASSET_ROOTS = [resolve(ROOT, "apps/mobile-wasm/dist")];

function fail(message) {
  console.error(message);
  process.exit(1);
}

function read(path) {
  return readFileSync(join(ROOT, path), "utf8");
}

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function metadata() {
  const result = spawnSync(
    "cargo",
    ["metadata", "--locked", "--format-version", "1"],
    { cwd: ROOT, encoding: "utf8", maxBuffer: 128 * 1024 * 1024 }
  );
  if (result.error) fail(`cargo metadata failed to start: ${result.error.message}`);
  if (result.status !== 0) {
    process.stderr.write(result.stderr ?? "");
    process.exit(result.status ?? 1);
  }
  return JSON.parse(result.stdout);
}

function posixPath(path) {
  return path.split(sep).join("/");
}

function repositoryPath(absolutePath) {
  const resolvedPath = resolve(absolutePath);
  if (resolvedPath !== ROOT && !resolvedPath.startsWith(`${ROOT}${sep}`)) {
    fail(`path escapes the repository: ${absolutePath}`);
  }
  return posixPath(relative(ROOT, resolvedPath)) || ".";
}

function packageSource(pkg) {
  if (pkg.source) return pkg.source;
  return `path:${repositoryPath(dirname(pkg.manifest_path))}`;
}

function packageRef(pkg) {
  const sourceHash = sha256(packageSource(pkg)).slice(0, 16);
  return `cargo:${pkg.name}@${pkg.version}:${sourceHash}`;
}

function compareText(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

function runtimeGraph(graph, rootPackageName) {
  const rootPackage = graph.packages.find((pkg) => pkg.name === rootPackageName);
  if (!rootPackage) fail(`Cargo package ${rootPackageName} was not found`);

  const nodes = new Map(graph.resolve.nodes.map((node) => [node.id, node]));
  const reachable = new Set();
  const pending = [rootPackage.id];
  while (pending.length) {
    const id = pending.pop();
    if (reachable.has(id)) continue;
    reachable.add(id);
    const node = nodes.get(id);
    if (!node) fail(`Cargo resolve node is missing for ${id}`);
    for (const dependency of node.deps) {
      if (dependency.dep_kinds.length === 0 || dependency.dep_kinds.some((kind) => kind.kind !== "dev")) {
        pending.push(dependency.pkg);
      }
    }
  }

  const packages = graph.packages
    .filter((pkg) => reachable.has(pkg.id))
    .sort((left, right) => {
      return (
        compareText(left.name, right.name) ||
        compareText(left.version, right.version) ||
        compareText(packageSource(left), packageSource(right))
      );
    });
  const packageById = new Map(packages.map((pkg) => [pkg.id, pkg]));
  const dependencies = packages.map((pkg) => {
    const node = nodes.get(pkg.id);
    const dependsOn = node.deps
      .filter((dependency) => {
        return (
          reachable.has(dependency.pkg) &&
          (dependency.dep_kinds.length === 0 ||
            dependency.dep_kinds.some((kind) => kind.kind !== "dev"))
        );
      })
      .map((dependency) => packageRef(packageById.get(dependency.pkg)))
      .filter((ref, index, refs) => refs.indexOf(ref) === index)
      .sort();
    return { ref: packageRef(pkg), dependsOn };
  });
  return { rootPackage, packages, dependencies };
}

function loadPolicy() {
  const policy = JSON.parse(read(POLICY_PATH));
  if (policy.schemaVersion !== "vibex-license-policy.v1") {
    fail(`${POLICY_PATH} has an unsupported schemaVersion`);
  }
  if (policy.graphScope !== "all-target-normal-and-build") {
    fail(`${POLICY_PATH} must audit all target-specific normal and build dependencies`);
  }
  for (const key of [
    "allowedLicenses",
    "allowedExceptions",
    "packageLicenseOverrides",
    "assetInputs",
    "fontInputs",
    "nativeInputs",
    "installerInputs",
    "systemRuntimePrerequisites"
  ]) {
    if (!Array.isArray(policy[key])) fail(`${POLICY_PATH} is missing array ${key}`);
  }
  const allowedLicenses = new Set();
  for (const entry of policy.allowedLicenses) {
    if (!entry.id?.trim() || !entry.rationale?.trim()) {
      fail(`${POLICY_PATH} contains an allowed license without id or rationale`);
    }
    if (allowedLicenses.has(entry.id)) fail(`duplicate allowed license ${entry.id}`);
    allowedLicenses.add(entry.id);
  }
  return {
    policy,
    allowedLicenses,
    allowedExceptions: new Set(policy.allowedExceptions),
    unapprovedSelections: []
  };
}

function normalizeExpression(expression, policy) {
  const trimmed = expression.trim();
  return policy.expressionNormalizations[trimmed] ?? trimmed;
}

function selectApprovedLicense(ast, allowedLicenses, allowedExceptions) {
  if (ast.license) {
    if (!allowedLicenses.has(ast.license)) return null;
    if (ast.exception && !allowedExceptions.has(ast.exception)) return null;
    return ast.exception ? `${ast.license} WITH ${ast.exception}` : ast.license;
  }
  const left = selectApprovedLicense(ast.left, allowedLicenses, allowedExceptions);
  const right = selectApprovedLicense(ast.right, allowedLicenses, allowedExceptions);
  if (ast.conjunction === "or") return left ?? right;
  if (ast.conjunction === "and" && left && right) return `(${left} AND ${right})`;
  return null;
}

function auditExpression(expression, context, policyState) {
  const normalized = normalizeExpression(expression, policyState.policy);
  let ast;
  try {
    ast = parseSpdx(normalized);
  } catch (error) {
    fail(`${context} has an invalid SPDX expression ${JSON.stringify(normalized)}: ${error.message}`);
  }
  const selected = selectApprovedLicense(
    ast,
    policyState.allowedLicenses,
    policyState.allowedExceptions
  );
  if (!selected) {
    policyState.unapprovedSelections.push({ context, declared: normalized });
    return { declared: normalized, selected: null, approved: false };
  }
  return { declared: normalized, selected, approved: true };
}

function policySelection(license) {
  return license.selected ?? "UNAPPROVED";
}

function packageLicenses(packages, policyState) {
  const overrideUses = policyState.policy.packageLicenseOverrides.map((override) => ({
    override,
    used: false
  }));

  const audited = new Map();
  for (const pkg of packages) {
    let expression = pkg.license?.trim();
    let evidence = "cargo-manifest";
    const matchingOverrides = overrideUses.filter(({ override }) => {
      if (override.name !== pkg.name || override.version !== pkg.version) return false;
      if (override.source) return override.source === (pkg.source ?? "");
      if (override.sourceRepository) {
        return pkg.source?.startsWith(`git+${override.sourceRepository}#`) ?? false;
      }
      return false;
    });
    if (matchingOverrides.length > 1) {
      fail(`multiple license overrides match ${pkg.name} ${pkg.version}`);
    }
    const overrideUse = matchingOverrides[0];
    if (expression && overrideUse) {
      fail(`stale license override for ${pkg.name} ${pkg.version}; its manifest now declares ${expression}`);
    }
    if (!expression) {
      if (!overrideUse) fail(`missing license metadata for ${pkg.name} ${pkg.version}`);
      const packageRoot = dirname(pkg.manifest_path);
      const evidencePath = resolve(packageRoot, overrideUse.override.evidencePath);
      if (evidencePath !== packageRoot && !evidencePath.startsWith(`${packageRoot}${sep}`)) {
        fail(`license evidence escapes package root for ${pkg.name} ${pkg.version}`);
      }
      if (!existsSync(evidencePath)) fail(`missing license evidence for ${pkg.name}: ${evidencePath}`);
      const evidenceHash = sha256(readFileSync(evidencePath));
      if (evidenceHash !== overrideUse.override.evidenceSha256) {
        fail(`license evidence hash changed for ${pkg.name} ${pkg.version}`);
      }
      expression = overrideUse.override.licenseExpression;
      evidence = `${overrideUse.override.upstreamPath}#sha256=${evidenceHash}`;
      overrideUse.used = true;
    }
    audited.set(
      pkg.id,
      {
        ...auditExpression(expression, `${pkg.name} ${pkg.version}`, policyState),
        evidence
      }
    );
  }
  for (const { override, used } of overrideUses) {
    if (!used) fail(`unused package license override for ${override.name} ${override.version}`);
  }
  return audited;
}

function walkFiles(root) {
  const files = [];
  function walk(directory) {
    for (const entry of readdirSync(directory).sort()) {
      const absolute = join(directory, entry);
      const stat = lstatSync(absolute);
      if (stat.isSymbolicLink()) fail(`provenance input must not be a symlink: ${absolute}`);
      if (stat.isDirectory()) walk(absolute);
      else if (stat.isFile()) files.push(absolute);
    }
  }
  walk(root);
  return files;
}

function treeDigest(root, files) {
  const hash = createHash("sha256");
  for (const file of files) {
    hash.update(posixPath(relative(root, file)));
    hash.update("\0");
    hash.update(readFileSync(file));
    hash.update("\0");
  }
  return hash.digest("hex");
}

function auditInputs(policyState, runtime) {
  const fileInputs = [];
  const distributedInputs = [];
  const registeredFiles = new Set();
  const inputIds = new Set();
  for (const collectionName of ["assetInputs", "fontInputs"]) {
    for (const input of policyState.policy[collectionName]) {
      if (
        !input.id?.trim() ||
        !input.name?.trim() ||
        !input.version?.trim() ||
        (!input.root?.trim() && !input.packageRelativeRoot?.trim())
      ) {
        fail(`${collectionName} entry is missing id, name, version, or input root`);
      }
      if (inputIds.has(input.id)) fail(`duplicate provenance input id ${input.id}`);
      inputIds.add(input.id);
      let root;
      if (input.root) {
        root = resolve(ROOT, input.root);
        repositoryPath(root);
      } else {
        const owner = runtime.packages.find((pkg) => pkg.name === input.ownerPackage);
        if (!owner) fail(`asset owner package was not found: ${input.ownerPackage}`);
        const packageRoot = dirname(owner.manifest_path);
        root = resolve(packageRoot, input.packageRelativeRoot);
        if (root !== packageRoot && !root.startsWith(`${packageRoot}${sep}`)) {
          fail(`${input.id} packageRelativeRoot escapes ${input.ownerPackage}`);
        }
      }
      if (!existsSync(root) || !lstatSync(root).isDirectory()) {
        fail(`${collectionName} root does not exist: ${input.root}`);
      }
      let files;
      if (input.files !== undefined) {
        if (
          !Array.isArray(input.files) ||
          input.files.length === 0 ||
          JSON.stringify(input.files) !== JSON.stringify([...new Set(input.files)].sort())
        ) {
          fail(`${input.id} files must be a non-empty unique list in stable order`);
        }
        files = input.files.map((path) => {
          const file = resolve(root, path);
          if (file === root || !file.startsWith(`${root}${sep}`)) {
            fail(`${input.id} file escapes its provenance root: ${path}`);
          }
          if (!existsSync(file) || !lstatSync(file).isFile() || lstatSync(file).isSymbolicLink()) {
            fail(`${input.id} file is missing, not regular, or a symlink: ${path}`);
          }
          return file;
        });
      } else {
        files = walkFiles(root);
      }
      const allowedExtensions = new Set(input.extensions);
      const unexpected = files.filter((file) => !allowedExtensions.has(extname(file).toLowerCase()));
      if (unexpected.length) {
        fail(`${input.id} contains unreviewed file types: ${unexpected.map(repositoryPath).join(", ")}`);
      }
      const digest = treeDigest(root, files);
      if (files.length !== input.expectedFileCount || digest !== input.treeSha256) {
        fail(
          `${input.id} provenance changed: expected ${input.expectedFileCount}/${input.treeSha256}, ` +
            `found ${files.length}/${digest}`
        );
      }
      for (const file of files) registeredFiles.add(resolve(file));
      fileInputs.push({
        input,
        kind: collectionName === "fontInputs" ? "font" : "asset",
        files: input.root
          ? files.map(repositoryPath)
          : files.map((file) => posixPath(relative(root, file))),
        licenses: auditExpression(input.licenseExpression, input.id, policyState)
      });
    }
  }

  for (const collectionName of ["nativeInputs", "installerInputs"]) {
    for (const input of policyState.policy[collectionName]) {
      if (
        !input.id?.trim() ||
        !input.name?.trim() ||
        !input.version?.trim() ||
        !input.source?.trim() ||
        !input.licenseExpression?.trim() ||
        !input.distribution?.trim() ||
        !Array.isArray(input.platforms) ||
        input.platforms.length === 0 ||
        !/^[a-f0-9]{64}$/.test(input.sha256 ?? "")
      ) {
        fail(`${collectionName} entry is missing versioned source, SHA-256, license, platform, or distribution metadata`);
      }
      if (inputIds.has(input.id)) fail(`duplicate provenance input id ${input.id}`);
      inputIds.add(input.id);
      distributedInputs.push({
        input,
        kind: collectionName === "installerInputs" ? "installer" : "native",
        licenses: auditExpression(input.licenseExpression, input.id, policyState)
      });
    }
  }
  for (const input of policyState.policy.systemRuntimePrerequisites) {
    if (!input.id?.trim() || !input.name?.trim() || !input.licenseExpression?.trim()) {
      fail("systemRuntimePrerequisites entry is missing id, name, or licenseExpression");
    }
    if (inputIds.has(input.id)) fail(`duplicate provenance input id ${input.id}`);
    inputIds.add(input.id);
    auditExpression(input.licenseExpression, input.id, policyState);
  }

  const scanRoots = [resolve(ROOT, "apps/desktop"), resolve(ROOT, "apps/mobile-wasm")];
  const unregistered = scanRoots
    .filter(existsSync)
    .flatMap(walkFiles)
    .filter(
      (file) =>
        !GENERATED_ASSET_ROOTS.some(
          (generatedRoot) => file === generatedRoot || file.startsWith(`${generatedRoot}${sep}`)
        )
    )
    .filter((file) => ASSET_EXTENSIONS.has(extname(file).toLowerCase()))
    .filter((file) => !registeredFiles.has(resolve(file)));
  if (unregistered.length) {
    fail(`unregistered GPUI assets/fonts: ${unregistered.map(repositoryPath).join(", ")}`);
  }
  return { fileInputs, distributedInputs };
}

function externalReferences(pkg) {
  const references = [];
  if (pkg.repository?.startsWith("http")) {
    references.push({ type: "vcs", url: pkg.repository });
  } else if (pkg.source?.startsWith("git+")) {
    references.push({ type: "vcs", url: pkg.source.slice(4) });
  } else if (pkg.source?.startsWith("registry+")) {
    references.push({
      type: "distribution",
      url: `https://crates.io/crates/${encodeURIComponent(pkg.name)}/${encodeURIComponent(pkg.version)}`
    });
  }
  return references;
}

function packageComponent(pkg, license) {
  const component = {
    type: pkg.name === "vibex-desktop" ? "application" : "library",
    "bom-ref": packageRef(pkg),
    name: pkg.name,
    version: pkg.version,
    licenses: [{ expression: license.declared }],
    purl: `pkg:cargo/${encodeURIComponent(pkg.name)}@${encodeURIComponent(pkg.version)}`,
    properties: [
      { name: "vibex:cargo-source", value: packageSource(pkg) },
      { name: "vibex:selected-license", value: policySelection(license) },
      { name: "vibex:license-policy-status", value: license.approved ? "approved" : "unapproved" },
      { name: "vibex:license-evidence", value: license.evidence }
    ]
  };
  const references = externalReferences(pkg);
  if (references.length) component.externalReferences = references;
  return component;
}

function inputComponent(auditedInput) {
  const { input, kind, licenses } = auditedInput;
  return {
    type: "file",
    "bom-ref": `${kind}:${input.id}:${input.treeSha256.slice(0, 16)}`,
    name: input.name,
    version: input.version,
    licenses: [{ expression: licenses.declared }],
    hashes: [{ alg: "SHA-256", content: input.treeSha256 }],
    externalReferences: [{ type: "vcs", url: input.source }],
    properties: [
      { name: "vibex:input-kind", value: kind },
      {
        name: "vibex:asset-root",
        value: input.root ?? `${input.ownerPackage}:${input.packageRelativeRoot}`
      },
      { name: "vibex:file-count", value: String(input.expectedFileCount) },
      { name: "vibex:selected-license", value: policySelection(licenses) }
    ]
  };
}

function distributedInputComponent(auditedInput) {
  const { input, kind, licenses } = auditedInput;
  const properties = [
    { name: "vibex:input-kind", value: kind },
    { name: "vibex:platforms", value: input.platforms.join(",") },
    { name: "vibex:selected-license", value: policySelection(licenses) },
    { name: "vibex:distribution-role", value: input.distribution }
  ];
  if (input.licenseEvidence) {
    properties.push({ name: "vibex:license-evidence", value: input.licenseEvidence });
  }
  return {
    type: "application",
    "bom-ref": `${kind}:${input.id}:${input.sha256.slice(0, 16)}`,
    name: input.name,
    version: input.version,
    licenses: [{ expression: licenses.declared }],
    hashes: [{ alg: "SHA-256", content: input.sha256 }],
    externalReferences: [{ type: "distribution", url: input.source }],
    properties
  };
}

function generateSbom(runtime, licenses, auditedFileInputs, auditedDistributedInputs, policy) {
  const rootComponent = packageComponent(runtime.rootPackage, licenses.get(runtime.rootPackage.id));
  const inputComponents = auditedFileInputs.map(inputComponent);
  const distributedComponents = auditedDistributedInputs.map(distributedInputComponent);
  const dependencies = runtime.dependencies.map((dependency) => ({
    ref: dependency.ref,
    dependsOn: [...dependency.dependsOn]
  }));
  for (const auditedInput of auditedFileInputs) {
    const owner = runtime.packages.find((pkg) => pkg.name === auditedInput.input.ownerPackage);
    if (!owner) fail(`asset owner package was not found: ${auditedInput.input.ownerPackage}`);
    const ownerDependency = dependencies.find((dependency) => dependency.ref === packageRef(owner));
    ownerDependency.dependsOn.push(
      `${auditedInput.kind}:${auditedInput.input.id}:${auditedInput.input.treeSha256.slice(0, 16)}`
    );
    ownerDependency.dependsOn.sort();
  }
  for (const component of inputComponents) dependencies.push({ ref: component["bom-ref"], dependsOn: [] });
  const rootDependency = dependencies.find((dependency) => dependency.ref === packageRef(runtime.rootPackage));
  if (!rootDependency) fail("SBOM root dependency entry is missing");
  for (const component of distributedComponents) {
    rootDependency.dependsOn.push(component["bom-ref"]);
    dependencies.push({ ref: component["bom-ref"], dependsOn: [] });
  }
  rootDependency.dependsOn.sort();
  dependencies.sort((left, right) => compareText(left.ref, right.ref));

  return {
    bomFormat: "CycloneDX",
    specVersion: "1.6",
    version: 1,
    metadata: {
      component: rootComponent,
      properties: [
        { name: "vibex:graph-scope", value: policy.graphScope },
        { name: "vibex:license-policy", value: POLICY_PATH }
      ]
    },
    components: [
      ...runtime.packages
        .filter((pkg) => pkg.id !== runtime.rootPackage.id)
        .map((pkg) => packageComponent(pkg, licenses.get(pkg.id))),
      ...inputComponents,
      ...distributedComponents
    ],
    dependencies
  };
}

function escapeCell(value) {
  return String(value).replaceAll("|", "\\|").replaceAll("\n", " ");
}

function generateNotices(runtime, licenses, auditedInputs, policy, unapprovedSelections) {
  const copyleftSelections = [...new Set(
    [...licenses.values()]
      .map((license) => license.selected)
      .filter((selected) => selected && /\b(?:A?GPL|LGPL)-/i.test(selected))
  )].sort();
  const policySummary = unapprovedSelections.length
    ? `${unapprovedSelections.length} unapproved selection(s) are marked UNAPPROVED; this graph is not distribution-approved.`
    : copyleftSelections.length
      ? `Intentional copyleft selections: ${copyleftSelections.join(", ")}. Alternative license expressions use the policy selection shown below.`
      : "License expressions with alternatives use the policy selection shown below.";
  const lines = [
    "# Desktop GPUI Third-Party Notices",
    "",
    "Generated by `node scripts/check-licenses.mjs --write`. Manual edits are rejected.",
    "",
    `Scope: ${policy.graphScope}; Cargo dev dependencies are excluded and target-specific normal/build dependencies for all platforms are included.`,
    "",
    `Rust components: ${runtime.packages.length - 1} third-party packages plus the Vibex application root.`,
    `Registered asset/font bundles: ${auditedInputs.length}.`,
    policySummary,
    "",
    "This inventory records source and license provenance. The corresponding license texts remain in each Cargo source package or registered distribution input.",
    "",
    "## Rust Dependencies",
    "",
    "| Package | Version | Source | Declared license | Policy selection | Evidence |",
    "| --- | --- | --- | --- | --- | --- |"
  ];
  for (const pkg of runtime.packages.filter((pkg) => pkg.id !== runtime.rootPackage.id)) {
    const license = licenses.get(pkg.id);
    lines.push(
      `| ${escapeCell(pkg.name)} | ${escapeCell(pkg.version)} | ${escapeCell(packageSource(pkg))} | ` +
        `${escapeCell(license.declared)} | ${escapeCell(policySelection(license))} | ${escapeCell(license.evidence)} |`
    );
  }
  lines.push("", "## Assets And Fonts", "");
  if (!auditedInputs.length) {
    lines.push("No bundled asset or font inputs are registered.");
  } else {
    lines.push("| Input | Files | Source | Declared license | Policy selection |", "| --- | ---: | --- | --- | --- |");
    for (const { input, licenses: inputLicenses } of auditedInputs) {
      lines.push(
        `| ${escapeCell(input.name)} | ${input.expectedFileCount} | ${escapeCell(input.source)} | ` +
          `${escapeCell(inputLicenses.declared)} | ${escapeCell(policySelection(inputLicenses))} |`
      );
    }
  }
  lines.push(
    "",
    "## Native And Installer Inputs",
    "",
    `Redistributed native inputs: ${policy.nativeInputs.length}.`,
    `Redistributed installer inputs: ${policy.installerInputs.length}.`,
    `System-provided runtime prerequisites (not redistributed): ${policy.systemRuntimePrerequisites.length}.`,
    ""
  );
  const nativeAndInstallerInputs = [
    ...policy.nativeInputs.map((input) => ({ kind: "native", input })),
    ...policy.installerInputs.map((input) => ({ kind: "installer", input }))
  ];
  if (nativeAndInstallerInputs.length) {
    lines.push(
      "| Kind | Input | Version | Platforms | Source | Declared license | Distribution |",
      "| --- | --- | --- | --- | --- | --- | --- |"
    );
    for (const { kind, input } of nativeAndInstallerInputs) {
      lines.push(
        `| ${kind} | ${escapeCell(input.name)} | ${escapeCell(input.version ?? "unversioned")} | ` +
          `${escapeCell((input.platforms ?? []).join(", ") || "all")} | ` +
          `${escapeCell(input.source ?? "repository policy")} | ${escapeCell(input.licenseExpression)} | ` +
          `${escapeCell(input.distribution ?? "redistributed")} |`
      );
    }
    lines.push("");
  }
  return `${lines.join("\n").trimEnd()}\n`;
}

function verifyOrWrite(path, content, write) {
  const absolute = join(ROOT, path);
  if (write) {
    mkdirSync(dirname(absolute), { recursive: true });
    writeFileSync(absolute, content);
    return;
  }
  if (!existsSync(absolute)) fail(`${path} is missing; run the generator with --write`);
  if (readFileSync(absolute, "utf8") !== content) {
    fail(`${path} is stale; review the graph and run node scripts/check-licenses.mjs --write`);
  }
}

const args = process.argv.slice(2);
if (args.some((arg) => arg !== "--write") || args.filter((arg) => arg === "--write").length > 1) {
  fail("usage: node scripts/check-licenses.mjs [--write]");
}
const write = args.includes("--write");
const policyState = loadPolicy();
const graph = metadata();
const runtime = runtimeGraph(graph, policyState.policy.rootPackage);
const licenses = packageLicenses(runtime.packages, policyState);
const auditedInputs = auditInputs(policyState, runtime);
const sbom = `${JSON.stringify(
  generateSbom(
    runtime,
    licenses,
    auditedInputs.fileInputs,
    auditedInputs.distributedInputs,
    policyState.policy
  ),
  null,
  2
)}\n`;
const notices = generateNotices(
  runtime,
  licenses,
  auditedInputs.fileInputs,
  policyState.policy,
  policyState.unapprovedSelections
);
verifyOrWrite(SBOM_PATH, sbom, write);
verifyOrWrite(NOTICES_PATH, notices, write);

if (policyState.unapprovedSelections.length) {
  const detail = policyState.unapprovedSelections
    .map((selection) => `${selection.context}: ${selection.declared}`)
    .join(", ");
  fail(
    `GPUI license inventory contains ${policyState.unapprovedSelections.length} unapproved selection(s): ${detail}`
  );
}

console.log(
  `GPUI license graph verified: ${runtime.packages.length} Cargo packages, ` +
    `${auditedInputs.fileInputs.length} asset/font bundles, ` +
    `${auditedInputs.distributedInputs.length} native/installer inputs, all license selections approved`
);

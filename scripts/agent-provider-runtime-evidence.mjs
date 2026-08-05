import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  existsSync,
  readFileSync,
  readdirSync,
  statSync
} from "node:fs";
import { dirname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

export const EVIDENCE_PATH = "docs/platform/evidence/agent-provider-runtime.json";
export const EVIDENCE_SCHEMA = "agent-provider-runtime-evidence.v1";
export const MANIFEST_SCHEMA = "agent-provider-runtime-manifest.v1";
export const AGENT_COUNT = 38;
export const BUILTIN_IDS = ["claude", "codex", "opencode"];

const MANIFEST_ENTRY_KEYS = [
  "agentId",
  "catalogVersion",
  "adapterId",
  "descriptorId",
  "descriptorVersion",
  "versionPolicy",
  "capabilityMode",
  "runtimeHomeStrategy",
  "switchBehavior",
  "credentialKinds",
  "modelInterfaces",
  "evidenceState",
  "sourceEvidenceReference",
  "smokeId"
];
const MANIFEST_KEYS = ["schemaVersion", "entries"];
const SMOKE_KEYS = [
  "agentId",
  "smokeId",
  "catalogVersion",
  "descriptorId",
  "descriptorVersion",
  "versionPolicy",
  "capabilityMode",
  "switchBehavior",
  "status",
  "execution",
  "diagnosticCode",
  "detectedIdentity",
  "descriptorMatch",
  "projectionFingerprint",
  "facts",
  "redactionPassed",
  "sourceSurvivedPrepareFailure",
  "providerVerified",
  "liveSwitchVerified",
  "binaryOnlyVerified",
  "sourceEvidenceReference",
  "recordedAt"
];
const FACT_KEYS = [
  "binaryIdentity",
  "acpHandshake",
  "authentication",
  "session",
  "modelCatalog",
  "modelSelection",
  "providerProjection",
  "sessionResume",
  "switchCompatibility",
  "redaction"
];
const FACT_STATUSES = new Set(["passed", "failed", "unsupported", "blocked", "skipped"]);
const PROBE_STAGES = [
  "resolving_identity",
  "planning_projection",
  "starting_process",
  "initializing_acp",
  "authenticating",
  "creating_or_loading_session",
  "discovering_models",
  "applying_model_and_config",
  "optional_minimal_prompt",
  "confirming_effective_provider",
  "cleaning_up"
];
const MATRIX_SCENARIOS = [
  "reserve_precedes_external_effect",
  "prepare_failure_preserves_source",
  "auth_failure_preserves_source",
  "timeout_preserves_source",
  "cancel_preserves_source",
  "startup_recovery_reconciles",
  "target_cleanup_bounded",
  "active_work_generation_fence"
];

// Keep this list explicit.  It is the release-gate binding between the
// runtime implementation and the evidence, so a new boundary cannot be
// silently omitted from the source identity.
export const SOURCE_BINDINGS = Object.freeze({
  catalog: ["crates/core/src/acp_catalog.rs"],
  manifest: [
    "crates/core/src/ids.rs",
    "crates/core/src/lib.rs",
    "crates/core/src/agent_provider_runtime.rs",
    "crates/core/src/provider_projection.rs",
    "crates/core/examples/agent_provider_runtime_manifest.rs"
  ],
  probe: [
    "crates/agent-acp/src/lib.rs",
    "crates/agent-acp/src/runtime.rs",
    "crates/agent-acp/src/runtime_probe.rs"
  ],
  database: [
    "crates/db/Cargo.toml",
    "crates/db/src/lib.rs",
    "crates/db/src/agent_provider_probe.rs",
    "crates/db/src/provider_projection.rs"
  ],
  remote: [
    "crates/core/src/remote.rs",
    "crates/vibex-backend/src/capability.rs",
    "crates/vibex-backend/src/disconnected.rs",
    "crates/vibex-backend/src/management.rs",
    "crates/vibex-backend/src/native.rs",
    "crates/remote/src/lib.rs",
    "crates/vibex-remote-client/src/backend.rs"
  ],
  ui: [
    "apps/desktop/src/management.rs",
    "crates/desktop-runtime/src/lib.rs",
    "crates/desktop-runtime/src/management.rs",
    "crates/vibex-ui/src/management.rs"
  ],
  switch: [
    "crates/agent/src/runtime_route.rs",
    "crates/agent/src/runtime_selection.rs",
    "crates/agent/src/runtime_switch.rs",
    "crates/agent/src/test_support.rs"
  ],
  scripts: [
    "scripts/agent-provider-runtime-evidence.mjs",
    "scripts/capture-agent-provider-runtime.mjs",
    "scripts/check-agent-provider-runtime.mjs"
  ],
  packageMetadata: [
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "package.json",
    "crates/core/Cargo.toml",
    "crates/agent-acp/Cargo.toml",
    "crates/agent/Cargo.toml",
    "crates/vibex-backend/Cargo.toml",
    "crates/remote/Cargo.toml",
    "crates/vibex-remote-client/Cargo.toml",
    "crates/desktop-runtime/Cargo.toml",
    "crates/vibex-ui/Cargo.toml",
    "apps/desktop/Cargo.toml"
  ]
});

export const SOURCE_INPUT_ROOTS = Object.freeze(
  [...new Set(Object.values(SOURCE_BINDINGS).flat())]
);

function fail(message) {
  throw new Error(message);
}

export function assert(condition, message) {
  if (!condition) fail(message);
}

function rootPath(path) {
  const absolute = resolve(ROOT, path);
  assert(
    absolute !== ROOT && absolute.startsWith(`${ROOT}${sep}`),
    `path escapes repository: ${path}`
  );
  return absolute;
}

export function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function canonicalize(value) {
  if (Array.isArray(value)) return value.map(canonicalize);
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, canonicalize(value[key])])
    );
  }
  return value;
}

export function canonicalJson(value) {
  return JSON.stringify(canonicalize(value));
}

function sourceFilesFor(path) {
  const absolute = rootPath(path);
  assert(existsSync(absolute), `source input is missing: ${path}`);
  if (statSync(absolute).isFile()) return [path.replaceAll("\\", "/")];
  return readdirSync(absolute, { withFileTypes: true })
    .sort((left, right) => left.name.localeCompare(right.name))
    .flatMap((entry) => sourceFilesFor(`${path}/${entry.name}`));
}

function hashPaths(paths) {
  const hash = createHash("sha256");
  for (const path of [...new Set(paths)].flatMap(sourceFilesFor).sort()) {
    hash.update(path.replaceAll("\\", "/"));
    hash.update("\0");
    hash.update(readFileSync(rootPath(path)));
    hash.update("\0");
  }
  return hash.digest("hex");
}

function readText(path) {
  return readFileSync(rootPath(path), "utf8");
}

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
    timeout: 4 * 60 * 1000,
    env: { ...process.env, CARGO_TERM_COLOR: "never" }
  });
  if (result.error) fail(`${command} failed to start: ${result.error.message}`);
  if (result.status !== 0) {
    fail(
      `${command} ${args.join(" ")} failed with status ${result.status ?? "unknown"}:\n${
        result.stderr || result.stdout || ""
      }`
    );
  }
  return result.stdout ?? "";
}

export function loadFreshManifest() {
  const output = run("cargo", [
    "run",
    "-q",
    "-p",
    "vibex-core",
    "--example",
    "agent_provider_runtime_manifest",
    "--locked"
  ]);
  let manifest;
  try {
    manifest = JSON.parse(output);
  } catch (error) {
    fail(`Rust rollout manifest was not valid JSON: ${error.message}`);
  }
  validateManifestShape(manifest);
  return manifest;
}

function exactKeys(value, keys, label) {
  assert(value && typeof value === "object" && !Array.isArray(value), `${label} must be an object`);
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  assert(JSON.stringify(actual) === JSON.stringify(expected), `${label} keys drifted`);
}

function safeIdentity(value, label) {
  assert(
    typeof value === "string" &&
      value.length > 0 &&
      value.length <= 192 &&
      !value.includes("..") &&
      !/[\\\s\u0000-\u001f]/.test(value) &&
      /^[A-Za-z0-9_./:+@-]+$/.test(value),
    `${label} is not a bounded identity`
  );
}

function safeHash(value, label) {
  assert(typeof value === "string" && /^[a-f0-9]{64}$/.test(value), `${label} is not a SHA-256`);
}

function safeFingerprint(value, label) {
  assert(
    typeof value === "string" && /^sha256:[a-f0-9]{16,192}$/.test(value),
    `${label} is not a bounded projection fingerprint`
  );
}

function validateManifestEntry(entry, label) {
  exactKeys(entry, MANIFEST_ENTRY_KEYS, label);
  for (const key of ["agentId", "catalogVersion", "adapterId", "descriptorId", "descriptorVersion", "capabilityMode", "runtimeHomeStrategy", "switchBehavior", "evidenceState", "sourceEvidenceReference", "smokeId"]) {
    safeIdentity(entry[key], `${label}.${key}`);
  }
  exactKeys(entry.versionPolicy, ["kind"], `${label}.versionPolicy`);
  assert(["exact", "detected_manual"].includes(entry.versionPolicy.kind), `${label}.versionPolicy is invalid`);
  if (entry.versionPolicy.kind === "detected_manual") {
    assert(entry.catalogVersion === "manual", `${label} manual policy must use manual catalog version`);
  } else {
    assert(entry.catalogVersion !== "manual", `${label} exact policy cannot use manual catalog version`);
  }
  assert(Array.isArray(entry.credentialKinds), `${label}.credentialKinds must be an array`);
  assert(Array.isArray(entry.modelInterfaces), `${label}.modelInterfaces must be an array`);
  for (const [index, model] of entry.modelInterfaces.entries()) {
    assert(model && typeof model === "object", `${label}.modelInterfaces[${index}] is invalid`);
    assert(typeof model.wireProtocolId === "string" && model.wireProtocolId.length > 0, `${label}.modelInterfaces[${index}] has no wire protocol`);
  }
}

export function validateManifestShape(manifest) {
  exactKeys(manifest, MANIFEST_KEYS, "rollout manifest");
  assert(manifest.schemaVersion === MANIFEST_SCHEMA, "rollout manifest schema drifted");
  assert(Array.isArray(manifest.entries), "rollout manifest entries are missing");
  assert(manifest.entries.length === AGENT_COUNT, "rollout manifest must contain 38 entries");
  const ids = manifest.entries.map((entry) => entry.agentId);
  assert(new Set(ids).size === ids.length, "rollout manifest contains duplicate Agent ids");
  for (const [index, entry] of manifest.entries.entries()) validateManifestEntry(entry, `manifest.entries[${index}]`);
  const builtinIds = ids.filter((id) => BUILTIN_IDS.includes(id)).sort();
  assert(JSON.stringify(builtinIds) === JSON.stringify([...BUILTIN_IDS].sort()), "builtin coverage drifted");
  assert(ids.filter((id) => !BUILTIN_IDS.includes(id)).length === 35, "catalog coverage must contain 35 entries");
  return manifest;
}

function expectedSourceIdentity(manifest) {
  const bindings = Object.fromEntries(
    Object.entries(SOURCE_BINDINGS).map(([name, paths]) => [name, hashPaths(paths)])
  );
  return {
    captureParentCommit: run("git", ["rev-parse", "HEAD"]).trim(),
    rustToolchain: run("rustc", ["--version"]).trim().split(/\s+/).slice(0, 2).join("-"),
    sourceInputRoots: [...SOURCE_INPUT_ROOTS],
    sourceInputTreeSha256: hashPaths(SOURCE_INPUT_ROOTS),
    bindings,
    cargoLockSha256: sha256(readFileSync(rootPath("Cargo.lock"))),
    packageJsonSha256: sha256(readFileSync(rootPath("package.json"))),
    rolloutManifestSha256: sha256(canonicalJson(manifest))
  };
}

export function buildSourceIdentity(manifest) {
  return expectedSourceIdentity(manifest);
}

function validateSourceIdentity(source, manifest) {
  exactKeys(
    source,
    [
      "captureParentCommit",
      "rustToolchain",
      "sourceInputRoots",
      "sourceInputTreeSha256",
      "bindings",
      "cargoLockSha256",
      "packageJsonSha256",
      "rolloutManifestSha256"
    ],
    "evidence.source"
  );
  assert(/^[a-f0-9]{40}$/.test(source.captureParentCommit), "capture parent commit is invalid");
  safeIdentity(source.rustToolchain, "evidence.source.rustToolchain");
  assert(JSON.stringify(source.sourceInputRoots) === JSON.stringify(SOURCE_INPUT_ROOTS), "source input roots drifted");
  const expected = expectedSourceIdentity(manifest);
  assert(source.sourceInputTreeSha256 === expected.sourceInputTreeSha256, "source input identity is stale");
  assert(source.cargoLockSha256 === expected.cargoLockSha256, "Cargo.lock identity is stale");
  assert(source.packageJsonSha256 === expected.packageJsonSha256, "package metadata identity is stale");
  assert(source.rolloutManifestSha256 === expected.rolloutManifestSha256, "rollout manifest identity is stale");
  exactKeys(source.bindings, Object.keys(SOURCE_BINDINGS), "evidence.source.bindings");
  for (const name of Object.keys(SOURCE_BINDINGS)) {
    assert(source.bindings[name] === expected.bindings[name], `${name} source identity is stale`);
  }
}

function validateProbeContract(contract) {
  exactKeys(
    contract,
    [
      "schemaVersion",
      "stageOrder",
      "independentFacts",
      "externalEffectsAfterDurableIntent",
      "diagnosticsBounded",
      "processCleanupRequired",
      "nativeHomeWrite",
      "sensitivePayloadPersistence"
    ],
    "probeContract"
  );
  assert(contract.schemaVersion === "agent-runtime-probe-contract.v1", "probe contract schema drifted");
  assert(JSON.stringify(contract.stageOrder) === JSON.stringify(PROBE_STAGES), "probe stage order drifted");
  for (const key of ["independentFacts", "externalEffectsAfterDurableIntent", "diagnosticsBounded", "processCleanupRequired", "nativeHomeWrite", "sensitivePayloadPersistence"]) {
    assert(typeof contract[key] === "boolean", `probeContract.${key} must be boolean`);
  }
  assert(contract.independentFacts && contract.externalEffectsAfterDurableIntent && contract.diagnosticsBounded && contract.processCleanupRequired, "probe contract safety flags are incomplete");
  assert(contract.nativeHomeWrite === false && contract.sensitivePayloadPersistence === false, "probe contract permits unsafe persistence");
}

function validateRedaction(redaction) {
  exactKeys(
    redaction,
    ["status", "credentialMaterialPersisted", "nativeIdentityPersisted", "runtimePayloadPersisted", "absolutePathPersisted"],
    "redaction"
  );
  assert(redaction.status === "passed", "evidence redaction scan did not pass");
  for (const key of ["credentialMaterialPersisted", "nativeIdentityPersisted", "runtimePayloadPersisted", "absolutePathPersisted"]) {
    assert(redaction[key] === false, `redaction.${key} must be false`);
  }
}

function validateFailureMatrix(matrix) {
  exactKeys(matrix, ["schemaVersion", "status", "execution", "scenarios"], "failure retention matrix");
  assert(matrix.schemaVersion === "agent-provider-switch-failure-matrix.v1", "failure matrix schema drifted");
  assert(matrix.status === "passed" && matrix.execution === "fixture_contract", "failure matrix is not a passed fixture contract");
  assert(Array.isArray(matrix.scenarios), "failure matrix scenarios are missing");
  assert(JSON.stringify(matrix.scenarios.map((scenario) => scenario.id)) === JSON.stringify(MATRIX_SCENARIOS), "failure matrix scenario order drifted");
  for (const [index, scenario] of matrix.scenarios.entries()) {
    exactKeys(scenario, ["id", "status", "basis"], `failure matrix scenario ${index}`);
    assert(scenario.status === "passed", `failure matrix scenario ${scenario.id} did not pass`);
    safeIdentity(scenario.id, `failure matrix scenario ${index}.id`);
    safeIdentity(scenario.basis, `failure matrix scenario ${index}.basis`);
  }
}

function validateDetectedIdentity(identity, label) {
  if (identity === null) return;
  exactKeys(identity, ["agentVersion", "adapterId", "adapterVersion"], label);
  safeIdentity(identity.agentVersion, `${label}.agentVersion`);
  safeIdentity(identity.adapterId, `${label}.adapterId`);
  if (identity.adapterVersion !== null) safeIdentity(identity.adapterVersion, `${label}.adapterVersion`);
}

function providerProjectionVerified(smoke) {
  const requiredFacts = [
    "binaryIdentity",
    "acpHandshake",
    "authentication",
    "session",
    "modelSelection",
    "providerProjection"
  ];
  return (
    smoke.descriptorMatch === "exact" &&
    smoke.projectionFingerprint !== null &&
    smoke.redactionPassed === true &&
    requiredFacts.every((fact) => smoke.facts[fact] === "passed")
  );
}

function validateSmoke(smoke, entry, matrix) {
  exactKeys(smoke, SMOKE_KEYS, `smoke ${entry.agentId}`);
  for (const key of ["agentId", "smokeId", "catalogVersion", "descriptorId", "descriptorVersion", "capabilityMode", "switchBehavior", "sourceEvidenceReference"]) {
    safeIdentity(smoke[key], `smoke ${entry.agentId}.${key}`);
  }
  assert(smoke.agentId === entry.agentId, `smoke Agent mismatch for ${entry.agentId}`);
  assert(smoke.smokeId === entry.smokeId, `smoke id mismatch for ${entry.agentId}`);
  assert(smoke.catalogVersion === entry.catalogVersion, `smoke catalog version mismatch for ${entry.agentId}`);
  assert(smoke.descriptorId === entry.descriptorId && smoke.descriptorVersion === entry.descriptorVersion, `smoke descriptor mismatch for ${entry.agentId}`);
  assert(smoke.capabilityMode === entry.capabilityMode && smoke.switchBehavior === entry.switchBehavior, `smoke capability mismatch for ${entry.agentId}`);
  assert(smoke.sourceEvidenceReference === entry.sourceEvidenceReference, `smoke source reference mismatch for ${entry.agentId}`);
  assert(smoke.versionPolicy === entry.versionPolicy.kind, `smoke version policy mismatch for ${entry.agentId}`);
  assert(["blocked", "passed"].includes(smoke.status), `smoke status is invalid for ${entry.agentId}`);
  assert(["not_run", "real_runtime"].includes(smoke.execution), `smoke execution is invalid for ${entry.agentId}`);
  if (smoke.status === "blocked") {
    assert(smoke.execution === "not_run", `blocked smoke ${entry.agentId} must be not_run`);
    assert(typeof smoke.diagnosticCode === "string" && smoke.diagnosticCode.length > 0, `blocked smoke ${entry.agentId} needs a diagnostic code`);
  } else {
    assert(smoke.execution === "real_runtime" && smoke.diagnosticCode === null, `passed smoke ${entry.agentId} has an invalid execution state`);
  }
  validateDetectedIdentity(smoke.detectedIdentity, `smoke ${entry.agentId}.detectedIdentity`);
  assert(["not_checked", "conservative", "exact"].includes(smoke.descriptorMatch), `smoke descriptor match is invalid for ${entry.agentId}`);
  if (smoke.projectionFingerprint !== null) safeFingerprint(smoke.projectionFingerprint, `smoke ${entry.agentId}.projectionFingerprint`);
  exactKeys(smoke.facts, FACT_KEYS, `smoke ${entry.agentId}.facts`);
  for (const key of FACT_KEYS) assert(FACT_STATUSES.has(smoke.facts[key]), `smoke ${entry.agentId}.facts.${key} is invalid`);
  for (const key of ["redactionPassed", "sourceSurvivedPrepareFailure", "providerVerified", "liveSwitchVerified", "binaryOnlyVerified"]) {
    assert(typeof smoke[key] === "boolean", `smoke ${entry.agentId}.${key} must be boolean`);
  }
  assert(smoke.recordedAt === null || typeof smoke.recordedAt === "string", `smoke ${entry.agentId}.recordedAt is invalid`);
  if (smoke.status === "blocked") {
    assert(Object.values(smoke.facts).every((status) => status === "blocked"), `blocked smoke ${entry.agentId} contains a partial success claim`);
    assert(smoke.detectedIdentity === null && smoke.descriptorMatch === "not_checked" && smoke.projectionFingerprint === null, `blocked smoke ${entry.agentId} retained runtime identity`);
    assert(!smoke.redactionPassed && !smoke.sourceSurvivedPrepareFailure, `blocked smoke ${entry.agentId} retained runtime proof`);
  }
  const derivedProviderVerified = providerProjectionVerified(smoke);
  assert(smoke.providerVerified === derivedProviderVerified, `provider verification claim is inconsistent for ${entry.agentId}`);
  assert(smoke.binaryOnlyVerified === false, `binary-only verification is forbidden for ${entry.agentId}`);
  const derivedLive =
    derivedProviderVerified &&
    smoke.switchBehavior === "live_session_config" &&
    smoke.sourceSurvivedPrepareFailure &&
    smoke.facts.switchCompatibility === "passed" &&
    matrix.status === "passed";
  assert(smoke.liveSwitchVerified === derivedLive, `live switch claim is inconsistent for ${entry.agentId}`);
  if (smoke.status === "passed" && ["replaceable_provider", "cloud_credential"].includes(entry.capabilityMode)) {
    assert(smoke.providerVerified, `provider-capable smoke ${entry.agentId} passed without provider verification`);
  }
}

function forbiddenKey(key) {
  const normalized = key
    .replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`)
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_|_$/g, "");
  return [
    /(^|_)command($|_)/,
    /(^|_)overlay($|_)/,
    /(^|_)env(ironment)?($|_)/,
    /(^|_)(secret|token|password|cookie)($|_)/,
    /(^|_)(private_key|api_key|access_key|authorization)($|_)/,
    /(^|_)prompt($|_)/,
    /(^|_)(stdout|stderr|raw_payload|raw_output)($|_)/,
    /(^|_)native_session(_id)?($|_)/,
    /(^|_)(workspace_root|user_home|temp_path)($|_)/
  ].some((pattern) => pattern.test(normalized));
}

function forbiddenValue(value) {
  return [
    /-----BEGIN [^-]*PRIVATE KEY-----/i,
    /(?:^|[\s"'=])(sk|pk|ghp|github_pat|xai)-[A-Za-z0-9_-]{12,}/,
    /\bBearer\s+[A-Za-z0-9._~+/=-]{12,}/i,
    /(?:^|["'\s])\/(?:home|tmp|Users|private\/tmp|var\/folders)\/[^\s"'`}]+/,
    /(?:^|[A-Za-z]:\\)Users\\/i,
    /native[_-]?session[_-]?id/i,
    /(?:api[_-]?key|access[_-]?token|refresh[_-]?token)\s*[:=]\s*[^\s,}]{8,}/i
  ].some((pattern) => pattern.test(value));
}

function scanForSensitiveFields(value, path = "evidence") {
  if (Array.isArray(value)) {
    value.forEach((item, index) => scanForSensitiveFields(item, `${path}[${index}]`));
    return;
  }
  if (value && typeof value === "object") {
    for (const [key, child] of Object.entries(value)) {
      assert(!forbiddenKey(key), `${path}.${key} is a forbidden runtime field`);
      scanForSensitiveFields(child, `${path}.${key}`);
    }
    return;
  }
  if (typeof value === "string") assert(!forbiddenValue(value), `${path} contains sensitive runtime data`);
}

export function validateEvidence(evidence, manifest = loadFreshManifest()) {
  validateManifestShape(manifest);
  exactKeys(
    evidence,
    ["schemaVersion", "status", "claimScope", "realRuntimeVerification", "capturedAt", "source", "manifest", "probeContract", "smokes", "switch", "redaction", "summary", "limitations"],
    "agent provider runtime evidence"
  );
  assert(evidence.schemaVersion === EVIDENCE_SCHEMA, "agent provider runtime evidence schema drifted");
  assert(evidence.status === "passed" && evidence.claimScope === "manifest_and_contract", "evidence status scope is invalid");
  assert(["blocked", "partial", "passed"].includes(evidence.realRuntimeVerification), "runtime verification state is invalid");
  assert(typeof evidence.capturedAt === "string" && !Number.isNaN(Date.parse(evidence.capturedAt)), "capture timestamp is invalid");
  assert(canonicalJson(evidence.manifest.entries) === canonicalJson(manifest.entries), "evidence manifest does not match Rust manifest");
  exactKeys(evidence.manifest, ["schemaVersion", "totalAgents", "builtinCount", "catalogCount", "entries"], "evidence.manifest");
  assert(evidence.manifest.schemaVersion === MANIFEST_SCHEMA && evidence.manifest.totalAgents === AGENT_COUNT && evidence.manifest.builtinCount === 3 && evidence.manifest.catalogCount === 35, "evidence manifest counts are invalid");
  validateSourceIdentity(evidence.source, manifest);
  validateProbeContract(evidence.probeContract);
  validateFailureMatrix(evidence.switch.failureRetentionMatrix);
  exactKeys(evidence.switch, ["failureRetentionMatrix", "liveSwitchPolicy", "liveSwitchVerifiedAgentIds"], "evidence.switch");
  assert(evidence.switch.liveSwitchPolicy === "provider_verification_and_failure_retention_matrix", "live switch policy drifted");
  assert(Array.isArray(evidence.switch.liveSwitchVerifiedAgentIds), "live switch ids are missing");
  assert(evidence.switch.liveSwitchVerifiedAgentIds.length === 0, "live switch evidence requires a real provider smoke");
  validateRedaction(evidence.redaction);
  assert(Array.isArray(evidence.smokes) && evidence.smokes.length === AGENT_COUNT, "evidence smoke coverage is incomplete");
  const entriesById = new Map(manifest.entries.map((entry) => [entry.agentId, entry]));
  const seen = new Set();
  for (const smoke of evidence.smokes) {
    assert(!seen.has(smoke.agentId), `duplicate smoke Agent ${smoke.agentId}`);
    seen.add(smoke.agentId);
    const entry = entriesById.get(smoke.agentId);
    assert(entry, `smoke Agent ${smoke.agentId} is not in the manifest`);
    validateSmoke(smoke, entry, evidence.switch.failureRetentionMatrix);
  }
  assert(seen.size === AGENT_COUNT, "smoke coverage does not contain all Agents");
  exactKeys(evidence.summary, ["totalAgents", "blockedRealSmokes", "passedRealSmokes", "providerVerifiedAgents", "liveSwitchVerifiedAgents", "binaryOnlyVerifiedAgents", "conservativeRollout"], "evidence.summary");
  const blocked = evidence.smokes.filter((smoke) => smoke.status === "blocked").length;
  const passed = evidence.smokes.filter((smoke) => smoke.status === "passed").length;
  const providerVerified = evidence.smokes.filter((smoke) => smoke.providerVerified).length;
  const liveVerified = evidence.smokes.filter((smoke) => smoke.liveSwitchVerified).length;
  const binaryOnly = evidence.smokes.filter((smoke) => smoke.binaryOnlyVerified).length;
  assert(evidence.summary.totalAgents === AGENT_COUNT && evidence.summary.blockedRealSmokes === blocked && evidence.summary.passedRealSmokes === passed && evidence.summary.providerVerifiedAgents === providerVerified && evidence.summary.liveSwitchVerifiedAgents === liveVerified && evidence.summary.binaryOnlyVerifiedAgents === binaryOnly, "evidence summary is inconsistent");
  assert(binaryOnly === 0 && liveVerified === 0, "evidence contains an invalid verification claim");
  const expectedRuntimeState = passed === 0 ? "blocked" : passed === AGENT_COUNT ? "passed" : "partial";
  assert(evidence.realRuntimeVerification === expectedRuntimeState, "runtime verification summary is inconsistent");
  assert(evidence.summary.conservativeRollout === (providerVerified === 0 && liveVerified === 0), "conservative rollout flag is inconsistent");
  assert(Array.isArray(evidence.limitations) && evidence.limitations.length > 0 && evidence.limitations.every((item) => typeof item === "string" && item.length > 0), "evidence limitations are incomplete");
  scanForSensitiveFields(evidence);
  return evidence;
}

export function buildConservativeEvidence(manifest) {
  validateManifestShape(manifest);
  const source = buildSourceIdentity(manifest);
  const smokes = manifest.entries.map((entry) => ({
    agentId: entry.agentId,
    smokeId: entry.smokeId,
    catalogVersion: entry.catalogVersion,
    descriptorId: entry.descriptorId,
    descriptorVersion: entry.descriptorVersion,
    versionPolicy: entry.versionPolicy.kind,
    capabilityMode: entry.capabilityMode,
    switchBehavior: entry.switchBehavior,
    status: "blocked",
    execution: "not_run",
    diagnosticCode: "operator_prerequisites_unavailable",
    detectedIdentity: null,
    descriptorMatch: "not_checked",
    projectionFingerprint: null,
    facts: Object.fromEntries(FACT_KEYS.map((key) => [key, "blocked"])),
    redactionPassed: false,
    sourceSurvivedPrepareFailure: false,
    providerVerified: false,
    liveSwitchVerified: false,
    binaryOnlyVerified: false,
    sourceEvidenceReference: entry.sourceEvidenceReference,
    recordedAt: null
  }));
  const evidence = {
    schemaVersion: EVIDENCE_SCHEMA,
    status: "passed",
    claimScope: "manifest_and_contract",
    realRuntimeVerification: "blocked",
    capturedAt: new Date().toISOString(),
    source,
    manifest: {
      schemaVersion: manifest.schemaVersion,
      totalAgents: AGENT_COUNT,
      builtinCount: BUILTIN_IDS.length,
      catalogCount: AGENT_COUNT - BUILTIN_IDS.length,
      entries: manifest.entries
    },
    probeContract: {
      schemaVersion: "agent-runtime-probe-contract.v1",
      stageOrder: PROBE_STAGES,
      independentFacts: true,
      externalEffectsAfterDurableIntent: true,
      diagnosticsBounded: true,
      processCleanupRequired: true,
      nativeHomeWrite: false,
      sensitivePayloadPersistence: false
    },
    smokes,
    switch: {
      failureRetentionMatrix: {
        schemaVersion: "agent-provider-switch-failure-matrix.v1",
        status: "passed",
        execution: "fixture_contract",
        scenarios: MATRIX_SCENARIOS.map((id) => ({ id, status: "passed", basis: "runtime_switch_fixture" }))
      },
      liveSwitchPolicy: "provider_verification_and_failure_retention_matrix",
      liveSwitchVerifiedAgentIds: []
    },
    redaction: {
      status: "passed",
      credentialMaterialPersisted: false,
      nativeIdentityPersisted: false,
      runtimePayloadPersisted: false,
      absolutePathPersisted: false
    },
    summary: {
      totalAgents: AGENT_COUNT,
      blockedRealSmokes: AGENT_COUNT,
      passedRealSmokes: 0,
      providerVerifiedAgents: 0,
      liveSwitchVerifiedAgents: 0,
      binaryOnlyVerifiedAgents: 0,
      conservativeRollout: true
    },
    limitations: [
      "Operator credentials, commercial licenses, cloud projects, and local model assets were unavailable in this capture environment.",
      "Every real Agent smoke is therefore recorded as blocked/not_run; no binary or ACP handshake result is promoted to provider verification.",
      "The passed switch matrix is a source-bound fixture contract. Live switching remains disabled until a provider-specific runtime smoke and failure-retention run are captured.",
      "The evidence records bounded identities and capability states only; prompts, credentials, native session identities, raw runtime payloads, and user paths are never persisted."
    ]
  };
  validateEvidence(evidence, manifest);
  return evidence;
}

export function readEvidence() {
  return JSON.parse(readText(EVIDENCE_PATH));
}

export function repositoryRoot() {
  return ROOT;
}

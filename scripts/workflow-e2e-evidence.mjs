import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { resolveGpuiSourceIdentities } from "./source-identities.mjs";
import {
  WORKFLOW_SOURCE_INPUTS,
  MOBILE_SOURCE_INPUTS,
  WEB_SOURCE_INPUTS,
  sourceTreeSha256
} from "./wasm-source-tree.mjs";

export const WORKFLOW_EVIDENCE_SCHEMA_VERSION = "vibex-workflow-e2e.v1";
export const WORKFLOW_EVIDENCE_PATH = "docs/platform/evidence/workflow-e2e.json";
export const WORKFLOW_IDS = ["agent", "files", "git", "terminal", "management"];
export const TARGET_IDS = [
  ["web_browser", "direct"],
  ["web_browser", "relay"],
  ["android_physical", "direct"],
  ["android_physical", "relay"]
];

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function fileSha256(path) {
  return sha256(readFileSync(path));
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function currentCommit(root) {
  return execFileSync("git", ["rev-parse", "HEAD"], {
    cwd: root,
    encoding: "utf8"
  }).trim();
}

export function sourceCommitIsAncestor(root, ancestor, descendant = currentCommit(root)) {
  try {
    execFileSync("git", ["merge-base", "--is-ancestor", ancestor, descendant], {
      cwd: root,
      stdio: "ignore"
    });
    return true;
  } catch {
    return false;
  }
}

function androidBuildIsCurrent(androidBuild, source, webBuild) {
  if (androidBuild?.schemaVersion !== "vibex-android-build.v1") return false;
  const buildSource = androidBuild.source;
  if (
    buildSource?.webSourceTreeSha256 !== source.sourceTreeSha256 ||
    buildSource?.mobileShellTreeSha256 !== source.mobileShellTreeSha256 ||
    buildSource?.cargoLockfileSha256 !== source.cargoLockSha256 ||
    buildSource?.pnpmLockfileSha256 !== source.pnpmLockSha256 ||
    buildSource?.zedRevision !== source.zedRevision ||
    buildSource?.gpuiComponentRevision !== source.gpuiComponentRevision
  ) {
    return false;
  }
  return ["buildId", "profile", "wasmBytes", "wasmSha256", "glueSha256", "staticSha256"].every(
    (field) => androidBuild.webBuild?.[field] === webBuild[field]
  );
}

function normalizedApk(root, androidBuild, source, webBuild) {
  if (!androidBuildIsCurrent(androidBuild, source, webBuild)) return null;
  const artifact = androidBuild?.artifact;
  if (!artifact?.path) return null;
  const absolute = join(root, artifact.path);
  if (!existsSync(absolute)) return null;
  const bytes = readFileSync(absolute);
  if (bytes.length !== artifact.bytes || sha256(bytes) !== artifact.sha256) return null;
  return {
    path: artifact.path,
    bytes: artifact.bytes,
    sha256: artifact.sha256,
    applicationId: artifact.applicationId
  };
}

function digestInput(identity) {
  return {
    // sourceCommit is provenance, not content identity. Evidence is normally
    // committed after capture, so validation checks ancestry separately while
    // these hashes keep the candidate stable across an evidence-only commit.
    sourceTreeSha256: identity.source.sourceTreeSha256,
    workflowSourceTreeSha256: identity.source.workflowSourceTreeSha256,
    mobileShellTreeSha256: identity.source.mobileShellTreeSha256,
    cargoLockSha256: identity.source.cargoLockSha256,
    pnpmLockSha256: identity.source.pnpmLockSha256,
    zedRevision: identity.source.zedRevision,
    gpuiComponentRevision: identity.source.gpuiComponentRevision,
    webBuild: identity.webBuild,
    androidApk: identity.androidApk
  };
}

export function workflowCandidateDigest(identity) {
  return sha256(JSON.stringify(digestInput(identity)));
}

export function resolveWorkflowCandidateIdentity(root) {
  const webBuildPath = join(root, "apps/web/dist/build.json");
  if (!existsSync(webBuildPath)) throw new Error("apps/web/dist/build.json is missing");
  const webBuild = readJson(webBuildPath);
  if (webBuild.schemaVersion !== "vibex-web-build.v1") {
    throw new Error("Web build identity has an invalid schema");
  }
  const hostPath = join(root, "apps/web/dist/host.js");
  const wasmPath = join(root, "apps/web/dist/pkg/vibex_web_bg.wasm");
  if (!existsSync(hostPath) || !existsSync(wasmPath)) {
    throw new Error("Web dist is incomplete");
  }
  if (fileSha256(wasmPath) !== webBuild.wasmSha256) {
    throw new Error("Web WASM does not match build.json");
  }
  const androidBuildPath = join(root, "docs/platform/evidence/wasm-android-build.json");
  const androidBuild = existsSync(androidBuildPath) ? readJson(androidBuildPath) : null;
  const dependency = resolveGpuiSourceIdentities(root);
  const source = {
    sourceCommit: currentCommit(root),
    sourceTreeSha256: sourceTreeSha256(root, WEB_SOURCE_INPUTS),
    workflowSourceTreeSha256: sourceTreeSha256(root, WORKFLOW_SOURCE_INPUTS),
    mobileShellTreeSha256: sourceTreeSha256(root, MOBILE_SOURCE_INPUTS),
    cargoLockSha256: fileSha256(join(root, "Cargo.lock")),
    pnpmLockSha256: fileSha256(join(root, "pnpm-lock.yaml")),
    zedRevision: dependency.zedRevision,
    gpuiComponentRevision: dependency.gpuiComponentRevision
  };
  const normalizedWebBuild = {
    buildId: webBuild.buildId,
    profile: webBuild.profile,
    wasmBytes: webBuild.wasmBytes,
    wasmSha256: webBuild.wasmSha256,
    glueSha256: webBuild.glueSha256,
    staticSha256: webBuild.staticSha256,
    buildJsonSha256: fileSha256(webBuildPath),
    hostJsSha256: fileSha256(hostPath)
  };
  const identity = {
    source,
    webBuild: normalizedWebBuild,
    androidApk: normalizedApk(root, androidBuild, source, normalizedWebBuild)
  };
  return {
    ...identity,
    candidateDigest: workflowCandidateDigest(identity)
  };
}

export function emptyWorkflowEvidence(identity) {
  return {
    schemaVersion: WORKFLOW_EVIDENCE_SCHEMA_VERSION,
    updatedAt: new Date().toISOString(),
    candidate: identity,
    targets: {
      web_browser: { direct: null, relay: null },
      android_physical: { direct: null, relay: null }
    },
    redaction: {
      bounded: true,
      containsSecrets: false,
      containsUserContent: false,
      containsRawLogs: false,
      containsPairingMaterial: false
    }
  };
}

export function mergeWorkflowTarget(existing, identity, targetKind, transport, target) {
  const evidence =
    existing?.schemaVersion === WORKFLOW_EVIDENCE_SCHEMA_VERSION &&
    existing?.candidate?.candidateDigest === identity.candidateDigest
      ? structuredClone(existing)
      : emptyWorkflowEvidence(identity);
  evidence.updatedAt = new Date().toISOString();
  evidence.candidate = identity;
  evidence.targets[targetKind][transport] = target;
  return evidence;
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function exactKeys(value, expected, label) {
  assert(value && typeof value === "object" && !Array.isArray(value), `${label} is missing`);
  assert(
    JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...expected].sort()),
    `${label} has unexpected or missing fields`
  );
}

function validateHash(value, label) {
  assert(/^[0-9a-f]{64}$/.test(value ?? ""), `${label} is not a SHA-256 digest`);
}

function validateCandidate(candidate) {
  exactKeys(candidate, ["source", "webBuild", "androidApk", "candidateDigest"], "candidate");
  exactKeys(
    candidate.source,
    [
      "sourceCommit",
      "sourceTreeSha256",
      "workflowSourceTreeSha256",
      "mobileShellTreeSha256",
      "cargoLockSha256",
      "pnpmLockSha256",
      "zedRevision",
      "gpuiComponentRevision"
    ],
    "candidate.source"
  );
  assert(/^[0-9a-f]{40}$/.test(candidate.source.sourceCommit ?? ""), "source commit is invalid");
  for (const field of [
    "sourceTreeSha256",
    "workflowSourceTreeSha256",
    "mobileShellTreeSha256",
    "cargoLockSha256",
    "pnpmLockSha256"
  ]) {
    validateHash(candidate.source[field], `candidate.source.${field}`);
  }
  for (const field of ["zedRevision", "gpuiComponentRevision"]) {
    assert(/^[0-9a-f]{40}$/.test(candidate.source[field] ?? ""), `${field} is invalid`);
  }
  exactKeys(
    candidate.webBuild,
    [
      "buildId",
      "profile",
      "wasmBytes",
      "wasmSha256",
      "glueSha256",
      "staticSha256",
      "buildJsonSha256",
      "hostJsSha256"
    ],
    "candidate.webBuild"
  );
  assert(/^[0-9a-f]{24}$/.test(candidate.webBuild.buildId ?? ""), "Web build id is invalid");
  assert(candidate.webBuild.profile === "release", "workflow evidence requires a release Web build");
  assert(Number.isSafeInteger(candidate.webBuild.wasmBytes) && candidate.webBuild.wasmBytes > 0, "WASM size is invalid");
  for (const field of [
    "wasmSha256",
    "glueSha256",
    "staticSha256",
    "buildJsonSha256",
    "hostJsSha256"
  ]) {
    validateHash(candidate.webBuild[field], `candidate.webBuild.${field}`);
  }
  validateHash(candidate.candidateDigest, "candidate digest");
  assert(
    candidate.candidateDigest === workflowCandidateDigest(candidate),
    "candidate digest does not match the candidate identity"
  );
  if (candidate.androidApk !== null) {
    exactKeys(candidate.androidApk, ["path", "bytes", "sha256", "applicationId"], "candidate.androidApk");
    assert(candidate.androidApk.path.startsWith("apps/mobile/artifacts/"), "APK path is outside the artifact directory");
    assert(Number.isSafeInteger(candidate.androidApk.bytes) && candidate.androidApk.bytes > 0, "APK size is invalid");
    validateHash(candidate.androidApk.sha256, "APK hash");
    assert(candidate.androidApk.applicationId === "dev.vibex.remote", "APK application id is invalid");
  }
}

function validateWorkflowResult(result, label) {
  exactKeys(
    result,
    ["status", "permission", "businessResult", "liveEvent", "reconnectRecovery", "errorCode"],
    label
  );
  assert(result.status === "passed", `${label} did not pass`);
  for (const field of ["permission", "businessResult", "liveEvent", "reconnectRecovery"]) {
    assert(result[field] === "passed", `${label}.${field} did not pass`);
  }
  assert(result.errorCode === null, `${label} contains an error code`);
}

function validateTarget(target, targetKind, transport, candidateDigest, androidApk) {
  assert(target !== null, `${targetKind}/${transport} is missing`);
  exactKeys(
    target,
    [
      "target",
      "transport",
      "status",
      "capturedAt",
      "candidateDigest",
      "environment",
      "topology",
      "workflows",
      "permissionContractSha256",
      "redactionScan"
    ],
    `${targetKind}/${transport}`
  );
  assert(target.target === targetKind, `${targetKind}/${transport} target is mismatched`);
  assert(target.transport === transport, `${targetKind}/${transport} transport is mismatched`);
  assert(target.status === "passed", `${targetKind}/${transport} did not pass`);
  assert(!Number.isNaN(Date.parse(target.capturedAt)), `${targetKind}/${transport} timestamp is invalid`);
  assert(target.candidateDigest === candidateDigest, `${targetKind}/${transport} is stale`);
  validateHash(target.permissionContractSha256, `${targetKind}/${transport} permission contract`);
  assert(target.redactionScan === "passed", `${targetKind}/${transport} redaction scan failed`);
  exactKeys(target.workflows, WORKFLOW_IDS, `${targetKind}/${transport}.workflows`);
  for (const workflow of WORKFLOW_IDS) {
    validateWorkflowResult(target.workflows[workflow], `${targetKind}/${transport}/${workflow}`);
  }
  exactKeys(
    target.topology,
    ["kind", "endpointSha256", "selfHosted", "e2ee", "officialService"],
    `${targetKind}/${transport}.topology`
  );
  validateHash(target.topology.endpointSha256, `${targetKind}/${transport} endpoint`);
  assert(target.topology.officialService === false, "official Relay evidence is forbidden");
  if (transport === "direct") {
    assert(target.topology.kind === "lan_or_private_network", "Direct topology is invalid");
    assert(target.topology.selfHosted === true, "Direct authority must be user-owned");
  } else {
    assert(target.topology.kind === "self_hosted_relay", "Relay topology is invalid");
    assert(target.topology.selfHosted === true, "Relay must be self-hosted");
    assert(target.topology.e2ee === true, "Relay evidence does not prove E2EE");
  }
  if (targetKind === "web_browser") {
    exactKeys(
      target.environment,
      ["kind", "browserName", "browserVersion", "platformSha256"],
      `${targetKind}/${transport}.environment`
    );
    assert(target.environment.kind === "browser", "Web target environment is invalid");
    validateHash(target.environment.platformSha256, "browser platform hash");
  } else {
    assert(androidApk !== null, "Android workflow evidence has no current APK");
    exactKeys(
      target.environment,
      [
        "kind",
        "physical",
        "deviceIdentitySha256",
        "model",
        "osVersion",
        "webViewVersion",
        "apkSha256"
      ],
      `${targetKind}/${transport}.environment`
    );
    assert(target.environment.kind === "android", "Android target environment is invalid");
    assert(target.environment.physical === true, "Android target is not a physical device");
    validateHash(target.environment.deviceIdentitySha256, "Android device identity");
    assert(target.environment.apkSha256 === androidApk.sha256, "Android target APK is stale");
  }
}

function scanRedaction(value, path = "evidence") {
  if (typeof value === "string") {
    assert(!/(?:https?|wss?):\/\//i.test(value), `${path} contains a raw endpoint`);
    assert(!/\/(?:home|Users)\//.test(value), `${path} contains a private absolute path`);
    assert(!/\bBearer\s+/i.test(value), `${path} contains an authorization header`);
    assert(!/vibex:\/\/(?:pair|open)/i.test(value), `${path} contains a deep-link or pairing fragment`);
    return;
  }
  if (Array.isArray(value)) {
    value.forEach((item, index) => scanRedaction(item, `${path}[${index}]`));
    return;
  }
  if (!value || typeof value !== "object") return;
  for (const [key, child] of Object.entries(value)) {
    const allowedHashKey = [
      "deviceIdentitySha256",
      "endpointSha256",
      "permissionContractSha256",
      "pnpmLockSha256"
    ].includes(key);
    const allowedRedactionKey = [
      "containsSecrets",
      "containsUserContent",
      "containsRawLogs",
      "containsPairingMaterial"
    ].includes(key);
    const allowedArtifactPath = path === "evidence.candidate.androidApk" && key === "path";
    assert(
      allowedHashKey ||
        allowedRedactionKey ||
        allowedArtifactPath ||
        !/(prompt|content|diff|terminalBytes|secret|token|pairingCode|pairingFragment|rawLog|serial)/i.test(key),
      `${path}.${key} is not a redacted evidence field`
    );
    scanRedaction(child, `${path}.${key}`);
  }
}

export function validateWorkflowEvidence(evidence, currentIdentity = null, repositoryRoot = null) {
  exactKeys(evidence, ["schemaVersion", "updatedAt", "candidate", "targets", "redaction"], "evidence");
  assert(evidence.schemaVersion === WORKFLOW_EVIDENCE_SCHEMA_VERSION, "workflow evidence schema is invalid");
  assert(!Number.isNaN(Date.parse(evidence.updatedAt)), "workflow evidence timestamp is invalid");
  validateCandidate(evidence.candidate);
  if (currentIdentity) {
    assert(
      evidence.candidate.candidateDigest === currentIdentity.candidateDigest,
      "workflow evidence candidate is stale"
    );
    const evidenceCommit = evidence.candidate.source.sourceCommit;
    const currentSourceCommit = currentIdentity.source.sourceCommit;
    assert(
      evidenceCommit === currentSourceCommit ||
        (repositoryRoot &&
          sourceCommitIsAncestor(repositoryRoot, evidenceCommit, currentSourceCommit)),
      "workflow evidence source commit is not current or an ancestor"
    );
  }
  exactKeys(evidence.targets, ["web_browser", "android_physical"], "targets");
  const permissionContracts = new Set();
  for (const [targetKind, transport] of TARGET_IDS) {
    const target = evidence.targets[targetKind]?.[transport];
    validateTarget(
      target,
      targetKind,
      transport,
      evidence.candidate.candidateDigest,
      evidence.candidate.androidApk
    );
    permissionContracts.add(target.permissionContractSha256);
  }
  assert(permissionContracts.size === 1, "Direct/Relay permission contracts are inconsistent");
  exactKeys(
    evidence.redaction,
    ["bounded", "containsSecrets", "containsUserContent", "containsRawLogs", "containsPairingMaterial"],
    "redaction"
  );
  assert(evidence.redaction.bounded === true, "workflow evidence is unbounded");
  for (const field of [
    "containsSecrets",
    "containsUserContent",
    "containsRawLogs",
    "containsPairingMaterial"
  ]) {
    assert(evidence.redaction[field] === false, `workflow evidence ${field}`);
  }
  scanRedaction(evidence);
  return evidence;
}

export function workflowEvidenceStatus(evidence, currentIdentity, repositoryRoot = null) {
  try {
    validateWorkflowEvidence(evidence, currentIdentity, repositoryRoot);
    return { passed: true, reason: null };
  } catch (error) {
    return {
      passed: false,
      reason: error instanceof Error ? error.message : "workflow evidence is invalid"
    };
  }
}

export function permissionContractSha256(remoteState) {
  const capabilities = remoteState?.capabilities ?? {};
  const projection = Object.fromEntries(
    ["agent", "file", "git", "terminal", "management", "device"].map((domain) => [
      domain,
      {
        availability: capabilities[domain]?.availability ?? "missing",
        operations: [...(capabilities[domain]?.operations ?? [])].sort()
      }
    ])
  );
  return sha256(JSON.stringify(projection));
}

export function endpointSha256(endpoint) {
  return sha256(String(endpoint));
}

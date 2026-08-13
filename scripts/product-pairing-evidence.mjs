import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { sourceTreeSha256 } from "./wasm-source-tree.mjs";

export const PRODUCT_PAIRING_EVIDENCE_SCHEMA_VERSION =
  "vibex-product-pairing-e2e.v1";
export const PRODUCT_PAIRING_EVIDENCE_PATH =
  "docs/platform/evidence/product-pairing-e2e.json";
export const PRODUCT_PAIRING_MODES = [
  "tailscale",
  "direct",
  "relay",
  "relay-no-tailscale",
  "direct-relay-fallback"
];
export const PRODUCT_PAIRING_PERMISSIONS = [
  "read_only",
  "approve_only",
  "full_control"
];
export const PRODUCT_PAIRING_PHYSICAL_TRANSPORTS = ["tailscale", "relay"];
export const PRODUCT_PAIRING_PHYSICAL_UNAVAILABLE_REASONS = [
  "product_pairing_android_device_unavailable",
  "product_pairing_android_runner_unavailable"
];
export const PRODUCT_PAIRING_WORKFLOWS = [
  "agent",
  "files",
  "git",
  "terminal",
  "management"
];
export const PRODUCT_PAIRING_CHECKS = [
  "desktop_product_offer",
  "copy_link_broker",
  "visible_runtime_confirm",
  "fresh_mobile_viewport",
  "app_link_intake",
  "scanner_intake",
  "scanner_denied",
  "scanner_canceled",
  "scanner_invalid",
  "scanner_oversized",
  "permission_matrix",
  "refresh_restore",
  "development_host_reopen",
  "desktop_restart",
  "method_recovery",
  "global_disable_reenable",
  "device_revoke_repair",
  "local_clear_repair",
  "offer_expired",
  "offer_canceled",
  "offer_tampered",
  "offer_replayed",
  "wrong_server_identity",
  "storage_failure",
  "protocol_incompatible",
  "orphan_device_check",
  "relay_transport_only",
  "secret_scan"
];

const PRODUCT_SOURCE_INPUTS = [
  "Cargo.lock",
  "package.json",
  "pnpm-lock.yaml",
  "apps/desktop/Cargo.toml",
  "apps/desktop/src/remote_access_pairing.rs",
  "apps/desktop/tests/product_pairing_harness.rs",
  "apps/mobile/native-shell-contract.json",
  "apps/mobile/scripts",
  "apps/relay-server/src",
  "apps/mobile-wasm/src",
  "apps/mobile-wasm/host",
  "crates/core/src/remote_v2.rs",
  "crates/desktop-runtime/src/remote_connectivity.rs",
  "crates/desktop-runtime/src/relay.rs",
  "crates/remote/src",
  "crates/vibex-remote-client/src",
  "crates/vibex-ui/src/workbench.rs",
  "scripts/check-product-pairing.mjs",
  "scripts/e2e-desktop-pairing.mjs",
  "scripts/e2e-product-pairing-android.mjs",
  "scripts/e2e-workflows.mjs",
  "scripts/e2e-local-env/run-product-pairing.mjs",
  "scripts/product-pairing-evidence.mjs"
];

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function currentCommit(root) {
  return execFileSync("git", ["rev-parse", "HEAD"], {
    cwd: root,
    encoding: "utf8"
  }).trim();
}

function sourceCommitIsAncestor(root, ancestor, descendant) {
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

function candidateDigest(candidate) {
  return sha256(
    JSON.stringify({
      productSourceTreeSha256: candidate.productSourceTreeSha256,
      cargoLockSha256: candidate.cargoLockSha256,
      pnpmLockSha256: candidate.pnpmLockSha256,
      mobileRuntimeBuildId: candidate.mobileRuntimeBuildId,
      mobileRuntimeGitCommit: candidate.mobileRuntimeGitCommit,
      mobileRuntimeBuildSha256: candidate.mobileRuntimeBuildSha256
    })
  );
}

export function resolveProductPairingCandidate(root) {
  const mobileRuntimeBuildBytes = readFileSync(join(root, "apps/mobile-wasm/dist/build.json"));
  const mobileRuntimeBuild = JSON.parse(mobileRuntimeBuildBytes.toString("utf8"));
  if (
    mobileRuntimeBuild.schemaVersion !== "vibex-mobile-wasm-build.v1" ||
    mobileRuntimeBuild.profile !== "release" ||
    !/^[0-9a-f]{24}$/.test(mobileRuntimeBuild.buildId ?? "") ||
    !/^[0-9a-f]{40}$/.test(mobileRuntimeBuild.gitCommit ?? "")
  ) {
    throw new Error("product_pairing_mobile_runtime_build_invalid");
  }
  const candidate = {
    sourceCommit: currentCommit(root),
    productSourceTreeSha256: sourceTreeSha256(root, PRODUCT_SOURCE_INPUTS),
    cargoLockSha256: sha256(readFileSync(join(root, "Cargo.lock"))),
    pnpmLockSha256: sha256(readFileSync(join(root, "pnpm-lock.yaml"))),
    mobileRuntimeBuildId: mobileRuntimeBuild.buildId,
    mobileRuntimeGitCommit: mobileRuntimeBuild.gitCommit,
    mobileRuntimeBuildSha256: sha256(mobileRuntimeBuildBytes)
  };
  return { ...candidate, candidateDigest: candidateDigest(candidate) };
}

export function emptyProductPairingEvidence(candidate) {
  return {
    schemaVersion: PRODUCT_PAIRING_EVIDENCE_SCHEMA_VERSION,
    updatedAt: new Date().toISOString(),
    candidate,
    automated: Object.fromEntries(PRODUCT_PAIRING_MODES.map((mode) => [mode, null])),
    physical: {
      releaseGateSatisfied: false,
      tailscale: null,
      relay: null
    },
    redaction: {
      bounded: true,
      containsEndpoints: false,
      containsNetworkIdentifiers: false,
      containsPairingMaterial: false,
      containsCredentials: false,
      containsRawLogs: false
    }
  };
}

export function mergeProductPairingMode(existing, candidate, mode, result) {
  const evidence =
    existing?.schemaVersion === PRODUCT_PAIRING_EVIDENCE_SCHEMA_VERSION &&
    existing?.candidate?.candidateDigest === candidate.candidateDigest
      ? structuredClone(existing)
      : emptyProductPairingEvidence(candidate);
  evidence.updatedAt = new Date().toISOString();
  evidence.candidate = candidate;
  evidence.automated[mode] = result;
  return evidence;
}

export function mergeProductPairingPhysical(existing, candidate, transport, result) {
  const evidence =
    existing?.schemaVersion === PRODUCT_PAIRING_EVIDENCE_SCHEMA_VERSION &&
    existing?.candidate?.candidateDigest === candidate.candidateDigest
      ? structuredClone(existing)
      : emptyProductPairingEvidence(candidate);
  evidence.updatedAt = new Date().toISOString();
  evidence.candidate = candidate;
  evidence.physical[transport] = result;
  evidence.physical.releaseGateSatisfied = PRODUCT_PAIRING_PHYSICAL_TRANSPORTS.every(
    (target) => evidence.physical[target]?.status === "passed"
  );
  return evidence;
}

export function mergeProductPairingPhysicalUnavailable(
  existing,
  candidate,
  reasonCode
) {
  assert(
    PRODUCT_PAIRING_PHYSICAL_UNAVAILABLE_REASONS.includes(reasonCode),
    "product_pairing_physical_unavailable_reason_invalid"
  );
  const evidence =
    existing?.schemaVersion === PRODUCT_PAIRING_EVIDENCE_SCHEMA_VERSION &&
    existing?.candidate?.candidateDigest === candidate.candidateDigest
      ? structuredClone(existing)
      : emptyProductPairingEvidence(candidate);
  const recordedAt = new Date().toISOString();
  evidence.updatedAt = recordedAt;
  evidence.candidate = candidate;
  for (const transport of PRODUCT_PAIRING_PHYSICAL_TRANSPORTS) {
    if (evidence.physical[transport]?.status === "passed") continue;
    evidence.physical[transport] = {
      transport,
      status: "unavailable",
      recordedAt,
      candidateDigest: candidate.candidateDigest,
      reasonCode,
      releaseRisk: "open"
    };
  }
  evidence.physical.releaseGateSatisfied = PRODUCT_PAIRING_PHYSICAL_TRANSPORTS.every(
    (target) => evidence.physical[target]?.status === "passed"
  );
  return evidence;
}

function assert(condition, code) {
  if (!condition) throw new Error(code);
}

function exactKeys(value, keys, code) {
  assert(value && typeof value === "object" && !Array.isArray(value), code);
  assert(
    JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...keys].sort()),
    code
  );
}

function hash(value, code) {
  assert(/^[0-9a-f]{64}$/.test(value ?? ""), code);
}

function passedMap(value, keys, code) {
  exactKeys(value, keys, code);
  for (const key of keys) assert(value[key] === "passed", code);
}

function validateCandidate(candidate) {
  exactKeys(
    candidate,
    [
      "sourceCommit",
      "productSourceTreeSha256",
      "cargoLockSha256",
      "pnpmLockSha256",
      "mobileRuntimeBuildId",
      "mobileRuntimeGitCommit",
      "mobileRuntimeBuildSha256",
      "candidateDigest"
    ],
    "product_pairing_candidate_shape_invalid"
  );
  assert(/^[0-9a-f]{40}$/.test(candidate.sourceCommit), "product_pairing_commit_invalid");
  assert(/^[0-9a-f]{24}$/.test(candidate.mobileRuntimeBuildId), "product_pairing_build_id_invalid");
  assert(
    /^[0-9a-f]{40}$/.test(candidate.mobileRuntimeGitCommit),
    "product_pairing_mobile_runtime_commit_invalid"
  );
  for (const field of [
    "productSourceTreeSha256",
    "cargoLockSha256",
    "pnpmLockSha256",
    "mobileRuntimeBuildSha256",
    "candidateDigest"
  ]) {
    hash(candidate[field], `product_pairing_${field}_invalid`);
  }
  assert(candidate.candidateDigest === candidateDigest(candidate), "product_pairing_digest_invalid");
}

function validateAutomated(result, mode, digest) {
  exactKeys(
    result,
    [
      "mode",
      "status",
      "capturedAt",
      "candidateDigest",
      "desktopArtifactSha256",
      "relayArtifactSha256",
      "serverIdentitySha256",
      "routeSetSha256",
      "transport",
      "tailscaleConfigured",
      "permissions",
      "entries",
      "checks",
      "workflows",
      "redactionScan"
    ],
    `product_pairing_${mode}_shape_invalid`
  );
  assert(result.mode === mode && result.status === "passed", `product_pairing_${mode}_failed`);
  assert(!Number.isNaN(Date.parse(result.capturedAt)), `product_pairing_${mode}_time_invalid`);
  assert(result.candidateDigest === digest, `product_pairing_${mode}_stale`);
  hash(result.desktopArtifactSha256, `product_pairing_${mode}_desktop_invalid`);
  if (["relay", "relay-no-tailscale", "direct-relay-fallback"].includes(mode)) {
    hash(result.relayArtifactSha256, `product_pairing_${mode}_relay_invalid`);
  } else {
    assert(result.relayArtifactSha256 === null, `product_pairing_${mode}_relay_unexpected`);
  }
  hash(result.serverIdentitySha256, `product_pairing_${mode}_identity_invalid`);
  hash(result.routeSetSha256, `product_pairing_${mode}_route_invalid`);
  const transport = {
    tailscale: "tailnet",
    direct: "direct",
    relay: "self_hosted_relay",
    "relay-no-tailscale": "self_hosted_relay",
    "direct-relay-fallback": "direct_with_relay_fallback"
  }[mode];
  assert(result.transport === transport, `product_pairing_${mode}_transport_invalid`);
  assert(
    result.tailscaleConfigured === (mode === "tailscale"),
    `product_pairing_${mode}_tailscale_invalid`
  );
  exactKeys(result.permissions, PRODUCT_PAIRING_PERMISSIONS, `product_pairing_${mode}_permissions_invalid`);
  for (const permission of PRODUCT_PAIRING_PERMISSIONS) {
    hash(result.permissions[permission], `product_pairing_${mode}_${permission}_invalid`);
  }
  passedMap(
    result.entries,
    ["development_host", "app_link", "in_app_scanner"],
    `product_pairing_${mode}_entries_invalid`
  );
  passedMap(result.checks, PRODUCT_PAIRING_CHECKS, `product_pairing_${mode}_checks_invalid`);
  passedMap(result.workflows, PRODUCT_PAIRING_WORKFLOWS, `product_pairing_${mode}_workflows_invalid`);
  assert(result.redactionScan === "passed", `product_pairing_${mode}_redaction_failed`);
}

function validatePhysical(result, transport, digest) {
  if (result?.status === "unavailable") {
    exactKeys(
      result,
      [
        "transport",
        "status",
        "recordedAt",
        "candidateDigest",
        "reasonCode",
        "releaseRisk"
      ],
      `product_pairing_physical_${transport}_shape_invalid`
    );
    assert(result.transport === transport, `product_pairing_physical_${transport}_transport_invalid`);
    assert(
      !Number.isNaN(Date.parse(result.recordedAt)),
      `product_pairing_physical_${transport}_time_invalid`
    );
    assert(result.candidateDigest === digest, `product_pairing_physical_${transport}_stale`);
    assert(
      PRODUCT_PAIRING_PHYSICAL_UNAVAILABLE_REASONS.includes(result.reasonCode),
      `product_pairing_physical_${transport}_reason_invalid`
    );
    assert(result.releaseRisk === "open", `product_pairing_physical_${transport}_risk_invalid`);
    return;
  }
  exactKeys(
    result,
    [
      "transport",
      "status",
      "capturedAt",
      "candidateDigest",
      "deviceIdentitySha256",
      "apkSha256",
      "serverIdentitySha256",
      "mobileViewport",
      "productPairing",
      "workflowSmoke",
      "redactionScan"
    ],
    `product_pairing_physical_${transport}_shape_invalid`
  );
  assert(
    result.transport === transport && result.status === "passed",
    `product_pairing_physical_${transport}_failed`
  );
  assert(!Number.isNaN(Date.parse(result.capturedAt)), `product_pairing_physical_${transport}_time_invalid`);
  assert(result.candidateDigest === digest, `product_pairing_physical_${transport}_stale`);
  for (const field of ["deviceIdentitySha256", "apkSha256", "serverIdentitySha256"]) {
    hash(result[field], `product_pairing_physical_${transport}_${field}_invalid`);
  }
  for (const field of ["mobileViewport", "productPairing", "workflowSmoke", "redactionScan"]) {
    assert(result[field] === "passed", `product_pairing_physical_${transport}_${field}_failed`);
  }
}

function scanRedaction(value, path = "evidence") {
  if (typeof value === "string") {
    assert(!/(?:https?|wss?):\/\//i.test(value), `${path}_contains_endpoint`);
    assert(!/#\/pair\//i.test(value), `${path}_contains_pairing_fragment`);
    assert(!/\b(?:100\.(?:6[4-9]|[78]\d|9[0-9]|1[01]\d|12[0-7])|10|127|169\.254|172\.(?:1[6-9]|2\d|3[01])|192\.168)\./.test(value), `${path}_contains_private_ip`);
    assert(!/\.ts\.net\b/i.test(value), `${path}_contains_tailnet_name`);
    assert(!/\/(?:home|Users)\//.test(value), `${path}_contains_private_path`);
    return;
  }
  if (Array.isArray(value)) {
    value.forEach((item, index) => scanRedaction(item, `${path}_${index}`));
    return;
  }
  if (!value || typeof value !== "object") return;
  for (const [key, child] of Object.entries(value)) {
    const classificationKey = path.endsWith("_entries") && key === "development_host";
    if (!classificationKey) {
      assert(
        !/(?:url|origin|room|peer|grant|challenge|fragment|privateKey|rawLog|serial|dnsName)$/i.test(key),
        `${path}_${key}_is_forbidden`
      );
    }
    scanRedaction(child, `${path}_${key}`);
  }
}

export function validateProductPairingEvidence(evidence, currentCandidate = null, root = null) {
  exactKeys(
    evidence,
    ["schemaVersion", "updatedAt", "candidate", "automated", "physical", "redaction"],
    "product_pairing_evidence_shape_invalid"
  );
  assert(
    evidence.schemaVersion === PRODUCT_PAIRING_EVIDENCE_SCHEMA_VERSION,
    "product_pairing_schema_invalid"
  );
  assert(!Number.isNaN(Date.parse(evidence.updatedAt)), "product_pairing_updated_at_invalid");
  validateCandidate(evidence.candidate);
  if (currentCandidate) {
    assert(
      evidence.candidate.candidateDigest === currentCandidate.candidateDigest,
      "product_pairing_evidence_stale"
    );
    assert(
      evidence.candidate.sourceCommit === currentCandidate.sourceCommit ||
        (root &&
          sourceCommitIsAncestor(
            root,
            evidence.candidate.sourceCommit,
            currentCandidate.sourceCommit
          )),
      "product_pairing_source_commit_invalid"
    );
  }
  exactKeys(evidence.automated, PRODUCT_PAIRING_MODES, "product_pairing_modes_invalid");
  for (const mode of PRODUCT_PAIRING_MODES) {
    assert(evidence.automated[mode] !== null, `product_pairing_${mode}_missing`);
    validateAutomated(evidence.automated[mode], mode, evidence.candidate.candidateDigest);
  }
  exactKeys(
    evidence.physical,
    ["releaseGateSatisfied", ...PRODUCT_PAIRING_PHYSICAL_TRANSPORTS],
    "product_pairing_physical_invalid"
  );
  for (const transport of PRODUCT_PAIRING_PHYSICAL_TRANSPORTS) {
    assert(evidence.physical[transport] !== null, `product_pairing_physical_${transport}_missing`);
    validatePhysical(evidence.physical[transport], transport, evidence.candidate.candidateDigest);
  }
  assert(
    evidence.physical.releaseGateSatisfied ===
      PRODUCT_PAIRING_PHYSICAL_TRANSPORTS.every(
        (transport) => evidence.physical[transport].status === "passed"
      ),
    "product_pairing_physical_release_gate_invalid"
  );
  exactKeys(
    evidence.redaction,
    [
      "bounded",
      "containsEndpoints",
      "containsNetworkIdentifiers",
      "containsPairingMaterial",
      "containsCredentials",
      "containsRawLogs"
    ],
    "product_pairing_redaction_shape_invalid"
  );
  assert(evidence.redaction.bounded === true, "product_pairing_evidence_unbounded");
  for (const field of [
    "containsEndpoints",
    "containsNetworkIdentifiers",
    "containsPairingMaterial",
    "containsCredentials",
    "containsRawLogs"
  ]) {
    assert(evidence.redaction[field] === false, `product_pairing_redaction_${field}`);
  }
  scanRedaction(evidence);
  return evidence;
}

export function sha256Classification(value) {
  return sha256(String(value));
}

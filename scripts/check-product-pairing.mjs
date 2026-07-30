import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  PRODUCT_PAIRING_CHECKS,
  PRODUCT_PAIRING_EVIDENCE_PATH,
  PRODUCT_PAIRING_MODES,
  PRODUCT_PAIRING_PHYSICAL_TRANSPORTS,
  PRODUCT_PAIRING_PERMISSIONS,
  PRODUCT_PAIRING_WORKFLOWS,
  emptyProductPairingEvidence,
  resolveProductPairingCandidate,
  validateProductPairingEvidence
} from "./product-pairing-evidence.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function assert(condition, code) {
  if (!condition) throw new Error(code);
}

function hash(character) {
  return character.repeat(64);
}

function candidate() {
  return {
    sourceCommit: "a".repeat(40),
    productSourceTreeSha256: hash("1"),
    cargoLockSha256: hash("2"),
    pnpmLockSha256: hash("3"),
    webBuildId: "4".repeat(24),
    webGitCommit: "b".repeat(40),
    webBuildSha256: hash("5"),
    candidateDigest: ""
  };
}

function passedMap(keys) {
  return Object.fromEntries(keys.map((key) => [key, "passed"]));
}

function syntheticEvidence() {
  const value = candidate();
  const base = emptyProductPairingEvidence(value);
  const digestSource = JSON.stringify({
    productSourceTreeSha256: value.productSourceTreeSha256,
    cargoLockSha256: value.cargoLockSha256,
    pnpmLockSha256: value.pnpmLockSha256,
    webBuildId: value.webBuildId,
    webGitCommit: value.webGitCommit,
    webBuildSha256: value.webBuildSha256
  });
  value.candidateDigest = createHash("sha256").update(digestSource).digest("hex");
  base.candidate = value;
  for (const mode of PRODUCT_PAIRING_MODES) {
    base.automated[mode] = {
      mode,
      status: "passed",
      capturedAt: "2026-07-28T00:00:00.000Z",
      candidateDigest: value.candidateDigest,
      desktopArtifactSha256: hash("6"),
      relayArtifactSha256: ["relay", "relay-no-tailscale", "direct-relay-fallback"].includes(mode)
        ? hash("7")
        : null,
      serverIdentitySha256: hash("8"),
      routeSetSha256: hash("9"),
      transport: {
        tailscale: "tailnet",
        direct: "direct",
        relay: "self_hosted_relay",
        "relay-no-tailscale": "self_hosted_relay",
        "direct-relay-fallback": "direct_with_relay_fallback"
      }[mode],
      tailscaleConfigured: mode === "tailscale",
      permissions: Object.fromEntries(
        PRODUCT_PAIRING_PERMISSIONS.map((permission) => [permission, hash("a")])
      ),
      entries: passedMap(["browser_url", "app_link", "in_app_scanner"]),
      checks: passedMap(PRODUCT_PAIRING_CHECKS),
      workflows: passedMap(PRODUCT_PAIRING_WORKFLOWS),
      redactionScan: "passed"
    };
  }
  for (const transport of PRODUCT_PAIRING_PHYSICAL_TRANSPORTS) {
    base.physical[transport] = {
      transport,
      status: "passed",
      capturedAt: "2026-07-28T00:00:00.000Z",
      candidateDigest: value.candidateDigest,
      deviceIdentitySha256: hash("b"),
      apkSha256: hash("c"),
      serverIdentitySha256: hash("d"),
      mobileViewport: "passed",
      productPairing: "passed",
      workflowSmoke: "passed",
      redactionScan: "passed"
    };
  }
  base.physical.releaseGateSatisfied = true;
  return base;
}

function rejected(evidence, label) {
  let didReject = false;
  try {
    validateProductPairingEvidence(evidence);
  } catch {
    didReject = true;
  }
  assert(didReject, `product_pairing_self_test_accepted_${label}`);
}

async function selfTest() {
  const complete = syntheticEvidence();
  validateProductPairingEvidence(complete);

  const missingMode = structuredClone(complete);
  missingMode.automated.relay = null;
  rejected(missingMode, "missing_mode");

  const missingPhysical = structuredClone(complete);
  missingPhysical.physical.tailscale = null;
  rejected(missingPhysical, "missing_physical");

  const unavailablePhysical = structuredClone(complete);
  for (const transport of PRODUCT_PAIRING_PHYSICAL_TRANSPORTS) {
    unavailablePhysical.physical[transport] = {
      transport,
      status: "unavailable",
      recordedAt: "2026-07-28T00:00:00.000Z",
      candidateDigest: complete.candidate.candidateDigest,
      reasonCode: "product_pairing_android_device_unavailable",
      releaseRisk: "open"
    };
  }
  unavailablePhysical.physical.releaseGateSatisfied = false;
  validateProductPairingEvidence(unavailablePhysical);

  const hiddenPhysicalRisk = structuredClone(unavailablePhysical);
  hiddenPhysicalRisk.physical.releaseGateSatisfied = true;
  rejected(hiddenPhysicalRisk, "hidden_physical_risk");

  const closedPhysicalRisk = structuredClone(unavailablePhysical);
  closedPhysicalRisk.physical.relay.releaseRisk = "closed";
  rejected(closedPhysicalRisk, "closed_physical_risk");

  const stale = structuredClone(complete);
  stale.automated.direct.candidateDigest = hash("0");
  rejected(stale, "stale_mode");

  const leaked = structuredClone(complete);
  leaked.automated.direct.transport = "https://192.168.1.2";
  rejected(leaked, "raw_endpoint");

  const missingCheck = structuredClone(complete);
  delete missingCheck.automated.direct.checks.offer_replayed;
  rejected(missingCheck, "missing_check");

  console.log("GPUI product pairing evidence fail-closed self-test passed");
}

if (process.argv.includes("--self-test")) {
  await selfTest();
} else {
  try {
    const evidencePath = join(ROOT, PRODUCT_PAIRING_EVIDENCE_PATH);
    assert(existsSync(evidencePath), "product_pairing_evidence_missing");
    const evidence = JSON.parse(readFileSync(evidencePath, "utf8"));
    const current = resolveProductPairingCandidate(ROOT);
    validateProductPairingEvidence(evidence, current, ROOT);
    const physical = evidence.physical.releaseGateSatisfied
      ? "physical-passed"
      : "physical-release-risk-open";
    console.log(`GPUI product pairing evidence verified: ${current.candidateDigest}; ${physical}`);
  } catch (error) {
    console.error(
      `GPUI product pairing blocked: ${
        error instanceof Error && /^[a-z0-9_]+$/.test(error.message)
          ? error.message
          : "product_pairing_evidence_invalid"
      }`
    );
    process.exitCode = 1;
  }
}

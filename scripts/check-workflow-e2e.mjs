import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  TARGET_IDS,
  WORKFLOW_EVIDENCE_PATH,
  WORKFLOW_IDS,
  emptyWorkflowEvidence,
  resolveWorkflowCandidateIdentity,
  validateWorkflowEvidence,
  workflowCandidateDigest
} from "./workflow-e2e-evidence.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const selfTest = process.argv.includes("--self-test");

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function syntheticIdentity() {
  const hash = (character) => character.repeat(64);
  const identity = {
    source: {
      sourceCommit: "a".repeat(40),
      sourceTreeSha256: hash("1"),
      workflowSourceTreeSha256: hash("0"),
      mobileShellTreeSha256: hash("2"),
      cargoLockSha256: hash("3"),
      pnpmLockSha256: hash("4"),
      zedRevision: "b".repeat(40),
      gpuiComponentRevision: "c".repeat(40)
    },
    webBuild: {
      buildId: "d".repeat(24),
      profile: "release",
      wasmBytes: 1024,
      wasmSha256: hash("5"),
      glueSha256: hash("6"),
      staticSha256: hash("7"),
      buildJsonSha256: hash("8"),
      hostJsSha256: hash("9")
    },
    androidApk: {
      path: "apps/mobile/artifacts/vibex-gate-debug.apk",
      bytes: 2048,
      sha256: hash("a"),
      applicationId: "dev.vibex.remote"
    }
  };
  return { ...identity, candidateDigest: workflowCandidateDigest(identity) };
}

function passedWorkflow() {
  return {
    status: "passed",
    permission: "passed",
    businessResult: "passed",
    liveEvent: "passed",
    reconnectRecovery: "passed",
    errorCode: null
  };
}

function passedTarget(identity, target, transport) {
  const android = target === "android_physical";
  return {
    target,
    transport,
    status: "passed",
    capturedAt: "2026-07-26T00:00:00.000Z",
    candidateDigest: identity.candidateDigest,
    environment: android
      ? {
          kind: "android",
          physical: true,
          deviceIdentitySha256: "c".repeat(64),
          model: "physical-device",
          osVersion: "16",
          webViewVersion: "149.0.0.0",
          apkSha256: identity.androidApk.sha256
        }
      : {
          kind: "browser",
          browserName: "chromium",
          browserVersion: "149.0.0.0",
          platformSha256: "d".repeat(64)
        },
    topology: {
      kind: transport === "direct" ? "lan_or_private_network" : "self_hosted_relay",
      endpointSha256: "e".repeat(64),
      selfHosted: true,
      e2ee: transport === "relay",
      officialService: false
    },
    workflows: Object.fromEntries(WORKFLOW_IDS.map((workflow) => [workflow, passedWorkflow()])),
    permissionContractSha256: "f".repeat(64),
    redactionScan: "passed"
  };
}

function completeSyntheticEvidence() {
  const identity = syntheticIdentity();
  const evidence = emptyWorkflowEvidence(identity);
  evidence.updatedAt = "2026-07-26T00:00:00.000Z";
  for (const [target, transport] of TARGET_IDS) {
    evidence.targets[target][transport] = passedTarget(identity, target, transport);
  }
  return { evidence, identity };
}

function rejects(evidence, identity, label) {
  let rejected = false;
  try {
    validateWorkflowEvidence(evidence, identity);
  } catch {
    rejected = true;
  }
  assert(rejected, `self-test accepted ${label}`);
}

function runSelfTest() {
  const { evidence, identity } = completeSyntheticEvidence();
  validateWorkflowEvidence(evidence, identity);

  const missing = structuredClone(evidence);
  missing.targets.android_physical.relay = null;
  rejects(missing, identity, "a partial target matrix");

  const stale = structuredClone(evidence);
  stale.targets.web_browser.direct.candidateDigest = "0".repeat(64);
  rejects(stale, identity, "stale target evidence");

  const forgedDigest = structuredClone(evidence);
  forgedDigest.candidate.webBuild.wasmBytes += 1;
  rejects(forgedDigest, identity, "a forged candidate digest");

  const forgedCommit = structuredClone(evidence);
  forgedCommit.candidate.source.sourceCommit = "0".repeat(40);
  rejects(forgedCommit, identity, "a forged source commit");

  const evidenceOnlyCommit = structuredClone(identity);
  evidenceOnlyCommit.source.sourceCommit = "d".repeat(40);
  evidenceOnlyCommit.candidateDigest = workflowCandidateDigest(evidenceOnlyCommit);
  assert(
    evidenceOnlyCommit.candidateDigest === identity.candidateDigest,
    "an evidence-only commit changed the content-bound candidate digest"
  );

  const inconsistentPermissions = structuredClone(evidence);
  inconsistentPermissions.targets.web_browser.relay.permissionContractSha256 = "0".repeat(64);
  rejects(inconsistentPermissions, identity, "Direct/Relay permission drift");

  const leaked = structuredClone(evidence);
  leaked.targets.web_browser.direct.topology.endpointSha256 = "wss://relay.example/room";
  rejects(leaked, identity, "a raw Relay endpoint");

  const virtualDevice = structuredClone(evidence);
  virtualDevice.targets.android_physical.direct.environment.physical = false;
  rejects(virtualDevice, identity, "an Android virtual device");

  console.log("GPUI workflow E2E fail-closed self-test passed");
}

if (selfTest) {
  runSelfTest();
} else {
  try {
    const evidencePath = join(ROOT, WORKFLOW_EVIDENCE_PATH);
    assert(existsSync(evidencePath), `${WORKFLOW_EVIDENCE_PATH} is missing`);
    const evidence = JSON.parse(readFileSync(evidencePath, "utf8"));
    const identity = resolveWorkflowCandidateIdentity(ROOT);
    validateWorkflowEvidence(evidence, identity, ROOT);
    console.log(`GPUI workflow E2E evidence verified: ${identity.candidateDigest}`);
  } catch (error) {
    console.error(
      `GPUI workflow E2E blocked: ${error instanceof Error ? error.message : "evidence is invalid"}`
    );
    process.exitCode = 1;
  }
}

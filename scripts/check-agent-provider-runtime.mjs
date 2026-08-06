import console from "node:console";
import {
  EVIDENCE_PATH,
  loadFreshManifest,
  readEvidence,
  validateEvidence
} from "./agent-provider-runtime-evidence.mjs";

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function verify() {
  const manifest = loadFreshManifest();
  const evidence = readEvidence();
  validateEvidence(evidence, manifest);
  console.log(
    `Agent runtime rollout gate passed: ${evidence.manifest.totalAgents} Agents, ` +
      `${evidence.summary.blockedRealSmokes} blocked/not_run, ` +
      `source=${evidence.source.sourceInputTreeSha256}`
  );
}

function expectRejected(evidence, manifest, label) {
  let rejected = false;
  try {
    validateEvidence(evidence, manifest);
  } catch {
    rejected = true;
  }
  assert(rejected, `self-test accepted ${label}`);
}

function selfTest() {
  const manifest = loadFreshManifest();
  const baseline = readEvidence();
  validateEvidence(baseline, manifest);
  const mutations = [
    ["removed Agent", (copy) => copy.manifest.entries.pop()],
    ["duplicate Agent", (copy) => (copy.smokes[1].agentId = copy.smokes[0].agentId)],
    ["false provider verification", (copy) => (copy.smokes[0].providerVerified = true)],
    ["binary-only verification", (copy) => {
      copy.smokes[0].status = "passed";
      copy.smokes[0].execution = "real_runtime";
      copy.smokes[0].diagnosticCode = null;
      copy.smokes[0].facts.binaryIdentity = "passed";
      copy.smokes[0].facts.acpHandshake = "passed";
    }],
    ["false live switching", (copy) => copy.switch.liveSwitchVerifiedAgentIds.push("claude")],
    ["source drift", (copy) => (copy.source.bindings.probe = "0".repeat(64))],
    ["leaked overlay", (copy) => (copy.smokes[0].overlay = "credential-material")],
    ["leaked user path", (copy) => (copy.limitations[0] = "/home/operator/.config/agent")],
    ["stale manifest version", (copy) => (copy.manifest.entries[0].catalogVersion = "9.9.9")],
    ["invalid semantic-version policy", (copy) => {
      const entry = copy.manifest.entries.find((candidate) => candidate.versionPolicy.kind === "detected_semver");
      entry.versionPolicy.requirement = null;
    }],
    ["missing conservative diagnostic", (copy) => {
      const entry = copy.manifest.entries.find((candidate) => candidate.switchBehavior === "unverified");
      entry.capabilityDiagnosticCode = null;
    }]
  ];
  for (const [label, mutate] of mutations) {
    const copy = structuredClone(baseline);
    mutate(copy);
    expectRejected(copy, manifest, label);
  }
  console.log("Agent runtime rollout negative self-test passed");
}

try {
  if (process.argv.includes("--self-test")) selfTest();
  else if (process.argv.length === 2) verify();
  else throw new Error("usage: node scripts/check-agent-provider-runtime.mjs [--self-test]");
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}

export { verify };

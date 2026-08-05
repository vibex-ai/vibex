import console from "node:console";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import {
  EVIDENCE_PATH,
  buildConservativeEvidence,
  loadFreshManifest,
  readEvidence,
  repositoryRoot,
  validateEvidence
} from "./agent-provider-runtime-evidence.mjs";

function fail(message) {
  throw new Error(message);
}

function capture() {
  const manifest = loadFreshManifest();
  const evidence = buildConservativeEvidence(manifest);
  const path = `${repositoryRoot()}/${EVIDENCE_PATH}`;
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, `${JSON.stringify(evidence, null, 2)}\n`);
  validateEvidence(readEvidence(), manifest);
  console.log(
    `Agent runtime rollout evidence captured: ${evidence.summary.totalAgents} Agents, ` +
      `${evidence.summary.blockedRealSmokes} blocked/not_run, providerVerified=0`
  );
}

const mode = process.argv[2];
try {
  if (mode === "--write") capture();
  else if (mode === undefined) {
    const manifest = loadFreshManifest();
    const evidence = readEvidence();
    validateEvidence(evidence, manifest);
    console.log(`Agent runtime rollout evidence is valid: ${evidence.realRuntimeVerification}`);
  } else {
    fail("usage: node scripts/capture-agent-provider-runtime.mjs [--write]");
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { resolveGpuiSourceIdentities } from "./source-identities.mjs";

const DISPOSITION_PATH = "docs/platform/evidence/upstream-dependency-revalidation.json";

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

export function classifyGpuiEvidence(root, evidencePath, evidenceSource) {
  const current = resolveGpuiSourceIdentities(root);
  const currentLockSha256 = sha256(readFileSync(join(root, "Cargo.lock")));
  const sourceLockSha256 = evidenceSource.lockfileSha256 ?? evidenceSource.cargoLockSha256;
  const revisionsMatch =
    (!evidenceSource.zedRevision || evidenceSource.zedRevision === current.zedRevision) &&
    (!evidenceSource.gpuiComponentRevision ||
      evidenceSource.gpuiComponentRevision === current.gpuiComponentRevision);
  if (revisionsMatch && sourceLockSha256 === currentLockSha256) return "current";

  const disposition = JSON.parse(readFileSync(join(root, DISPOSITION_PATH), "utf8"));
  assert(
    disposition.schemaVersion === "upstream-dependency-revalidation.v1" &&
      disposition.status === "pending_physical_recapture",
    "GPUI dependency revalidation disposition is missing or invalid"
  );
  assert(
    disposition.currentSource?.dependencySourcePolicy === current.dependencySourcePolicy &&
      disposition.currentSource.zedRevision === current.zedRevision &&
      disposition.currentSource.gpuiComponentRevision === current.gpuiComponentRevision &&
      disposition.currentSource.cargoLockSha256 === currentLockSha256,
    "GPUI dependency revalidation disposition is stale"
  );

  const historicalSources = Array.isArray(disposition.historicalSources)
    ? disposition.historicalSources
    : [
        {
          source: disposition.historicalSource,
          evidence: disposition.historicalEvidence
        }
      ];
  const matches = historicalSources.filter(
    (candidate) =>
      Array.isArray(candidate?.evidence) && candidate.evidence.includes(evidencePath)
  );
  assert(
    matches.length === 1 && matches[0]?.source,
    `${evidencePath} is stale without one exact reviewed revalidation disposition`
  );
  const [historical] = matches;
  if (evidenceSource.zedRevision) {
    assert(
      evidenceSource.zedRevision === historical.source.zedRevision,
      `${evidencePath} has an unrecognized historical Zed revision`
    );
  }
  if (evidenceSource.gpuiComponentRevision) {
    assert(
      evidenceSource.gpuiComponentRevision === historical.source.gpuiComponentRevision,
      `${evidencePath} has an unrecognized historical gpui-component revision`
    );
  }
  assert(
    sourceLockSha256 === historical.source.cargoLockSha256,
    `${evidencePath} has an unrecognized historical lockfile identity`
  );
  const expectedSourceTree = historical.sourceInputTreeSha256ByEvidence?.[evidencePath];
  if (expectedSourceTree) {
    assert(
      evidenceSource.sourceInputTreeSha256 === expectedSourceTree,
      `${evidencePath} has an unrecognized historical source-tree identity`
    );
  }
  return "historical_pending_recapture";
}

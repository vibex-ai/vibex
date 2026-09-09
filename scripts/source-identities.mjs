import { spawnSync } from "node:child_process";

export const ZED_REPOSITORY = "https://github.com/vibex-ai/zed.git";
export const ZED_SUBMODULE_PATH = "vendor/zed";

function git(root, args, label) {
  const result = spawnSync("git", args, { cwd: root, encoding: "utf8" });
  if (result.error) throw new Error(`${label} failed to start: ${result.error.message}`);
  if (result.status !== 0) {
    throw new Error(result.stderr || `${label} exited ${result.status ?? 1}`);
  }
  return result.stdout.trim();
}

export function resolveZedSubmoduleRevision(root) {
  const revision = git(
    root,
    ["-C", ZED_SUBMODULE_PATH, "rev-parse", "--verify", "HEAD"],
    "Zed submodule revision lookup"
  );
  if (!/^[a-f0-9]{40}$/.test(revision)) {
    throw new Error(`Zed submodule HEAD is not a full Git revision: ${revision}`);
  }
  return revision;
}

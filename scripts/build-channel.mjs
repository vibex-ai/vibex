import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const channel = process.argv[2]?.trim().toLowerCase();

if (!new Set(["preview", "rc", "stable"]).has(channel)) {
  console.error("usage: node scripts/build-channel.mjs <preview|rc|stable>");
  process.exit(2);
}

const nativeBuildResult = spawnSync(
  process.env.CARGO || "cargo",
  ["build", "-p", "vibex-desktop", "--release", "--locked"],
  {
    cwd: ROOT,
    stdio: "inherit",
    env: { ...process.env, VIBEX_CHANNEL: channel }
  }
);

if (nativeBuildResult.error) {
  console.error(`failed to build GPUI ${channel} channel: ${nativeBuildResult.error.message}`);
  process.exit(1);
}
process.exit(nativeBuildResult.status ?? 1);

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const channel = process.argv[2]?.trim().toLowerCase();

if (!new Set(["preview", "rc", "stable"]).has(channel)) {
  console.error("usage: node scripts/build-channel.mjs <preview|rc|stable>");
  process.exit(2);
}

const webBuildResult = spawnSync(
  process.env.PNPM || "pnpm",
  ["--filter", "@vibex/web", "build:release"],
  { cwd: ROOT, stdio: "inherit", env: process.env }
);
if (webBuildResult.error) {
  console.error(`failed to build the WebUI: ${webBuildResult.error.message}`);
  process.exit(1);
}
if (webBuildResult.status !== 0) process.exit(webBuildResult.status ?? 1);

const webBuild = JSON.parse(
  readFileSync(resolve(ROOT, "apps/web/dist/build.json"), "utf8")
);
if (
  webBuild.schemaVersion !== "vibex-web-build.v1" ||
  webBuild.profile !== "release" ||
  !/^[0-9a-f]{24}$/.test(webBuild.buildId ?? "") ||
  !/^[0-9a-f]{40}$/.test(webBuild.gitCommit ?? "")
) {
  console.error("WebUI release build identity is invalid");
  process.exit(1);
}

const nativeBuildResult = spawnSync(
  process.env.CARGO || "cargo",
  ["build", "-p", "vibex-desktop", "--release", "--locked"],
  {
    cwd: ROOT,
    stdio: "inherit",
    env: {
      ...process.env,
      VIBEX_CHANNEL: channel,
      VIBEX_WEB_BUILD_ID: webBuild.buildId,
      VIBEX_WEB_GIT_COMMIT: webBuild.gitCommit
    }
  }
);

if (nativeBuildResult.error) {
  console.error(`failed to build GPUI ${channel} channel: ${nativeBuildResult.error.message}`);
  process.exit(1);
}
process.exit(nativeBuildResult.status ?? 1);

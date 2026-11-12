import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const channel = process.argv[2]?.trim().toLowerCase();
const targetIndex = process.argv.indexOf("--target");
const target = targetIndex >= 0 ? process.argv[targetIndex + 1]?.trim() : undefined;

if (!new Set(["preview", "rc", "stable"]).has(channel)) {
  console.error("usage: node scripts/build-channel.mjs <preview|rc|stable> [--target <triple>]");
  process.exit(2);
}
if (targetIndex >= 0 && (!target || target.startsWith("--"))) {
  console.error("--target requires a value");
  process.exit(2);
}

// Identical code folding deletes the duplicate monomorphised bodies that make
// the GPUI binaries large, but only lld implements `--icf`; the GNU bfd linker
// rustc reaches through `cc` rejects it. Scope both flags to the Linux x86_64
// target through target rustflags so macOS and Windows links are untouched and
// an outer RUSTFLAGS is never overwritten.
const LINUX_X86_64_LINK_FLAGS =
  '["-C", "link-arg=-fuse-ld=lld", "-C", "link-arg=-Wl,--icf=all"]';

// Both binaries the Packager configs declare must be built here: the desktop
// GUI, and the standalone `vibex` character-grid client from `vibex-client`.
// `vibex-desktop` only depends on `vibex-client` as a library, so building the
// desktop package alone never produces `target/release/vibex` and cargo
// packager fails to copy it into the package.
//
// The Windows NSIS template delegates that package's PATH edits to
// `vibex-installer-env`, so it is built for Windows targets too. Linux and
// macOS have no use for a Windows registry helper and keep their existing
// package contents.
const needsInstallerEnv = target?.includes("windows") ?? process.platform === "win32";
const packages = [
  "vibex-desktop",
  "vibex-client",
  ...(needsInstallerEnv ? ["vibex-installer-env"] : [])
];
const nativeBuildResult = spawnSync(
  process.env.CARGO || "cargo",
  [
    "build",
    ...packages.flatMap((name) => ["-p", name]),
    "--release",
    "--locked",
    ...(target ? ["--target", target] : []),
    ...(process.platform === "linux"
      ? ["--config", `target.x86_64-unknown-linux-gnu.rustflags=${LINUX_X86_64_LINK_FLAGS}`]
      : [])
  ],
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

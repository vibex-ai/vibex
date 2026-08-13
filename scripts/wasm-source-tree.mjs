import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";

export const MOBILE_RUNTIME_SOURCE_INPUTS = [
  "Cargo.toml",
  "Cargo.lock",
  "pnpm-lock.yaml",
  "apps/mobile-wasm/Cargo.toml",
  "apps/mobile-wasm/assets",
  "apps/mobile-wasm/package.json",
  "apps/mobile-wasm/rust-toolchain.toml",
  "apps/mobile-wasm/scripts",
  "apps/mobile-wasm/src",
  "apps/mobile-wasm/host",
  "crates/core/Cargo.toml",
  "crates/core/src",
  "crates/desktop-model/Cargo.toml",
  "crates/desktop-model/src",
  "crates/relay/Cargo.toml",
  "crates/relay/src",
  "crates/vibex-backend/Cargo.toml",
  "crates/vibex-backend/src",
  "crates/vibex-remote-client/Cargo.toml",
  "crates/vibex-remote-client/src",
  "crates/vibex-markdown/Cargo.toml",
  "crates/vibex-markdown/src",
  "crates/vibex-terminal-ui/Cargo.toml",
  "crates/vibex-terminal-ui/src",
  "crates/vibex-ui/Cargo.toml",
  "crates/vibex-ui/src",
  "crates/vibex-ui/theme/tokens.json",
  "scripts/capture-mobile-wasm-host.mjs",
  "scripts/check-workflow-e2e.mjs",
  "scripts/e2e-workflows.mjs",
  "scripts/wasm-source-tree.mjs",
  "scripts/mobile-wasm-test-server.mjs",
  "scripts/workflow-e2e-evidence.mjs",
  "docs/platform/workflow-e2e.schema.json"
];

export const MOBILE_SOURCE_INPUTS = [
  "pnpm-lock.yaml",
  "apps/mobile/README.md",
  "apps/mobile/capacitor.config.ts",
  "apps/mobile/native-shell-contract.json",
  "apps/mobile/package.json",
  "apps/mobile/scripts/build-android-debug.mjs",
  "apps/mobile/scripts/build-ios.mjs",
  "apps/mobile/scripts/configure-native-links.mjs",
  "apps/mobile/tsconfig.json",
  "scripts/capture-wasm-mobile.mjs",
  "scripts/wasm-source-tree.mjs"
];

export const WORKFLOW_SOURCE_INPUTS = [
  ...MOBILE_RUNTIME_SOURCE_INPUTS,
  "crates/remote/Cargo.toml",
  "crates/remote/src"
];

function sourceFiles(root, path) {
  const absolute = join(root, path);
  if (!existsSync(absolute)) throw new Error(`source input is missing: ${path}`);
  if (statSync(absolute).isFile()) return [path];
  return readdirSync(absolute, { withFileTypes: true })
    .sort((left, right) => left.name.localeCompare(right.name))
    .flatMap((entry) => sourceFiles(root, `${path}/${entry.name}`));
}

export function sourceTreeSha256(root, inputs) {
  const hash = createHash("sha256");
  for (const path of inputs.flatMap((input) => sourceFiles(root, input))) {
    hash.update(path);
    hash.update("\0");
    hash.update(readFileSync(join(root, path)));
    hash.update("\0");
  }
  return hash.digest("hex");
}

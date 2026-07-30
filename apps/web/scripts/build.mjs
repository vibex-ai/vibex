import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  accessSync,
  constants,
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync
} from "node:fs";
import { delimiter, dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const APP = join(ROOT, "apps/web");
const DIST = join(APP, "dist");
const release = process.argv.includes("--release");
const profile = release ? "release" : "debug";
const CACHE_ASSETS = [
  "./",
  "./index.html",
  "./offline.html",
  "./styles.css",
  "./host.js",
  "./host-services.js",
  "./platform-compat.js",
  "./manifest.webmanifest",
  "./icon.svg",
  "./build.json",
  "./pkg/vibex_web.js",
  "./pkg/vibex_web_bg.wasm"
];
const STATIC_IDENTITY_ASSETS = [
  "index.html",
  "offline.html",
  "styles.css",
  "host.js",
  "host-services.js",
  "platform-compat.js",
  "manifest.webmanifest",
  "icon.svg",
  "service-worker.js"
];

function fail(message) {
  throw new Error(message);
}

function executable(path) {
  if (!path) return false;
  try {
    accessSync(path, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

function commandOnPath(command) {
  for (const path of (process.env.PATH ?? "").split(delimiter)) {
    const candidate = join(path, command);
    if (executable(candidate)) return candidate;
  }
  return null;
}

function resolveWasmBindgen() {
  const candidates = [
    process.env.WASM_BINDGEN,
    commandOnPath("wasm-bindgen"),
    process.env.CARGO_HOME ? join(process.env.CARGO_HOME, "bin/wasm-bindgen") : null,
    join(process.env.HOME ?? "", ".cargo/bin/wasm-bindgen"),
    join(process.env.HOME ?? "", ".local/share/cargo/bin/wasm-bindgen")
  ];
  const binary = candidates.find(executable);
  if (!binary) {
    fail("wasm-bindgen-cli is missing; install the version pinned in Cargo.lock");
  }
  return binary;
}

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: "utf8",
    stdio: "inherit",
    env: process.env
  });
  if (result.error) fail(`${command} failed to start: ${result.error.message}`);
  if (result.status !== 0) fail(`${command} ${args.join(" ")} exited with ${result.status}`);
}

function output(command, args) {
  const result = spawnSync(command, args, { cwd: ROOT, encoding: "utf8" });
  if (result.error || result.status !== 0) {
    fail(`${command} ${args.join(" ")} failed: ${result.stderr ?? result.error?.message}`);
  }
  return result.stdout.trim();
}

function sourceGitCommit() {
  const configured = process.env.VIBEX_SOURCE_GIT_COMMIT?.trim();
  if (configured) return validateGitCommit(configured, "VIBEX_SOURCE_GIT_COMMIT");

  const result = spawnSync("git", ["rev-parse", "HEAD"], {
    cwd: ROOT,
    encoding: "utf8"
  });
  if (!result.error && result.status === 0) {
    return validateGitCommit(result.stdout.trim(), "git rev-parse HEAD");
  }

  const dotGit = join(ROOT, ".git");
  if (!existsSync(dotGit)) return "uncommitted-worktree";
  let gitDirectory = dotGit;
  if (existsSync(dotGit) && statSync(dotGit).isFile()) {
    const pointer = readFileSync(dotGit, "utf8").trim();
    if (!pointer.startsWith("gitdir:")) fail("unsupported .git worktree pointer");
    gitDirectory = resolve(ROOT, pointer.slice("gitdir:".length).trim());
  }
  const head = readFileSync(join(gitDirectory, "HEAD"), "utf8").trim();
  if (!head.startsWith("ref:")) return validateGitCommit(head, ".git/HEAD");
  const reference = head.slice("ref:".length).trim();
  const looseReference = join(gitDirectory, reference);
  if (existsSync(looseReference)) {
    return validateGitCommit(readFileSync(looseReference, "utf8").trim(), reference);
  }
  const packedRefs = join(gitDirectory, "packed-refs");
  if (existsSync(packedRefs)) {
    const match = readFileSync(packedRefs, "utf8")
      .split(/\r?\n/)
      .find((line) => line.endsWith(` ${reference}`));
    if (match) return validateGitCommit(match.split(" ", 1)[0], reference);
  }
  return "uncommitted-worktree";
}

function validateGitCommit(value, source) {
  if (!/^[0-9a-f]{40}$/.test(value)) fail(`${source} did not resolve a full lowercase Git commit`);
  return value;
}

function lockedWasmBindgenVersion() {
  const lock = readFileSync(join(ROOT, "Cargo.lock"), "utf8");
  const match = lock.match(/name = "wasm-bindgen"\nversion = "([^"]+)"/);
  if (!match) fail("Cargo.lock does not contain wasm-bindgen");
  return match[1];
}

const wasmBindgen = resolveWasmBindgen();
const cliVersion = output(wasmBindgen, ["--version"]).match(/(\d+\.\d+\.\d+)/)?.[1];
const lockedVersion = lockedWasmBindgenVersion();
if (cliVersion !== lockedVersion) {
  fail(`wasm-bindgen-cli ${cliVersion ?? "unknown"} does not match Cargo.lock ${lockedVersion}`);
}

const cargoArgs = [
  "+nightly-2026-07-24",
  "build",
  "-p",
  "vibex-web",
  "--target",
  "wasm32-unknown-unknown",
  "--locked"
];
if (release) cargoArgs.push("--release");
run("cargo", cargoArgs);

rmSync(DIST, { recursive: true, force: true });
mkdirSync(join(DIST, "pkg"), { recursive: true });
cpSync(join(APP, "web"), DIST, { recursive: true });

const wasmInput = join(
  ROOT,
  "target/wasm32-unknown-unknown",
  profile,
  "vibex_web.wasm"
);
if (!existsSync(wasmInput)) fail(`compiled WASM is missing: ${wasmInput}`);
run(wasmBindgen, [
  wasmInput,
  "--target",
  "web",
  "--out-dir",
  join(DIST, "pkg"),
  "--out-name",
  "vibex_web",
  "--no-typescript"
]);

const wasmOutput = join(DIST, "pkg/vibex_web_bg.wasm");
const bytes = readFileSync(wasmOutput);
const wasmSha256 = createHash("sha256").update(bytes).digest("hex");
const packageVersion = JSON.parse(readFileSync(join(APP, "package.json"), "utf8")).version;
const gitCommit = sourceGitCommit();
const serviceWorkerPath = join(DIST, "service-worker.js");
const serviceWorkerTemplate = readFileSync(serviceWorkerPath, "utf8");
if (
  !serviceWorkerTemplate.includes("__VIBEX_BUILD_ID__") ||
  !serviceWorkerTemplate.includes("__VIBEX_CACHE_ASSETS__")
) {
  fail("service worker build placeholders are missing");
}
writeFileSync(
  serviceWorkerPath,
  serviceWorkerTemplate.replace(
    "__VIBEX_CACHE_ASSETS__",
    JSON.stringify(CACHE_ASSETS)
  )
);
const staticHash = createHash("sha256");
for (const path of STATIC_IDENTITY_ASSETS) {
  staticHash.update(path);
  staticHash.update("\0");
  staticHash.update(readFileSync(join(DIST, path)));
  staticHash.update("\0");
}
const staticSha256 = staticHash.digest("hex");
const glueSha256 = createHash("sha256")
  .update(readFileSync(join(DIST, "pkg/vibex_web.js")))
  .digest("hex");
const buildId = createHash("sha256")
  .update(JSON.stringify({
    packageVersion,
    profile,
    gitCommit,
    wasmSha256,
    glueSha256,
    staticSha256
  }))
  .digest("hex")
  .slice(0, 24);
writeFileSync(
  join(DIST, "build.json"),
  `${JSON.stringify(
    {
      schemaVersion: "vibex-web-build.v1",
      buildId,
      packageVersion,
      profile,
      gitCommit,
      wasmBindgenVersion: cliVersion,
      wasmBytes: bytes.length,
      wasmSha256,
      glueSha256,
      staticSha256,
      cacheName: `vibex-static-${buildId}`,
      cacheAssets: CACHE_ASSETS
    },
    null,
    2
  )}\n`
);

const serviceWorker = readFileSync(serviceWorkerPath, "utf8");
if (!serviceWorker.includes("__VIBEX_BUILD_ID__")) {
  fail("service worker build id placeholder is missing");
}
writeFileSync(
  serviceWorkerPath,
  serviceWorker.replace("__VIBEX_BUILD_ID__", buildId)
);

console.log(`Built apps/web/dist (${profile}, ${buildId}, ${bytes.length} WASM bytes)`);

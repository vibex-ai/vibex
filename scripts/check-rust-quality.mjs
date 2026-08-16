import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const RUST_VERSION = "1.97.0";
const FUTURE_INCOMPAT_ALLOWLIST_PATH =
  "docs/development/rust-future-incompat-allowlist.json";

function fail(message) {
  console.error(message);
  process.exit(1);
}

function read(path) {
  return readFileSync(join(ROOT, path), "utf8");
}

function assertToolchainPin() {
  const cargoManifest = read("Cargo.toml");
  const toolchain = read("rust-toolchain.toml");
  const workflow = read(".github/workflows/ci.yml");
  if (!cargoManifest.includes(`rust-version = "${RUST_VERSION}"`)) {
    fail(`Cargo.toml must pin workspace rust-version to ${RUST_VERSION}`);
  }
  if (!toolchain.includes(`channel = "${RUST_VERSION}"`)) {
    fail(`rust-toolchain.toml must pin channel to ${RUST_VERSION}`);
  }
  if (!workflow.includes(`dtolnay/rust-toolchain@${RUST_VERSION}`)) {
    fail(`CI must pin dtolnay/rust-toolchain to ${RUST_VERSION}`);
  }
}

function loadFutureIncompatAllowlist() {
  const parsed = JSON.parse(read(FUTURE_INCOMPAT_ALLOWLIST_PATH));
  if (parsed.schemaVersion !== "vibex-rust-future-incompat-allowlist.v1") {
    fail(`${FUTURE_INCOMPAT_ALLOWLIST_PATH} has an unsupported schemaVersion`);
  }
  if (!Array.isArray(parsed.packages)) {
    fail(`${FUTURE_INCOMPAT_ALLOWLIST_PATH} packages must be an array`);
  }
  const allowed = new Set();
  for (const entry of parsed.packages) {
    if (
      !entry.name?.trim() ||
      !entry.version?.trim() ||
      !entry.owner?.trim() ||
      !entry.rationale?.trim() ||
      !entry.upstreamIssue?.trim() ||
      !entry.removalGate?.trim()
    ) {
      fail(`future-incompatibility entry ${entry.name ?? "unknown"} is incomplete`);
    }
    const identity = `${entry.name} v${entry.version}`;
    if (allowed.has(identity)) fail(`duplicate future-incompatibility entry ${identity}`);
    allowed.add(identity);
  }
  return allowed;
}

function run(command, args) {
  console.log(`> ${command} ${args.join(" ")}`);
  const result = spawnSync(command, args, { cwd: ROOT, stdio: "inherit" });
  if (result.error) fail(`${command} failed to start: ${result.error.message}`);
  if (result.status !== 0) process.exit(result.status ?? 1);
}

function workspacePackageNames() {
  const result = spawnSync(
    "cargo",
    ["metadata", "--locked", "--no-deps", "--format-version", "1"],
    { cwd: ROOT, encoding: "utf8", maxBuffer: 16 * 1024 * 1024 }
  );
  if (result.error) fail(`cargo metadata failed to start: ${result.error.message}`);
  if (result.status !== 0) {
    process.stderr.write(result.stderr ?? "");
    process.exit(result.status ?? 1);
  }
  const graph = JSON.parse(result.stdout);
  const members = new Set(graph.workspace_members);
  return graph.packages
    .filter((pkg) => members.has(pkg.id))
    .map((pkg) => pkg.name)
    .sort();
}

function runFutureCompatibleCheck(allowedPackages) {
  const args = ["check", "--workspace", "--all-targets", "--locked", "--future-incompat-report"];
  console.log(`> cargo ${args.join(" ")}`);
  const result = spawnSync("cargo", args, {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024
  });
  process.stdout.write(result.stdout ?? "");
  process.stderr.write(result.stderr ?? "");
  if (result.error) fail(`cargo failed to start: ${result.error.message}`);
  if (result.status !== 0) process.exit(result.status ?? 1);
  const output = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  const reportMatches = [
    ...output.matchAll(
      /packages contain code that will be rejected by a future version of Rust: ([^\n]+)/g
    )
  ];
  const reportedPackages = new Set(
    reportMatches.flatMap((match) => match[1].split(",").map((value) => value.trim()))
  );
  const hasReport =
    output.includes("code that will be rejected by a future version of Rust") ||
    output.includes("cargo report future-incompatibilities --id");
  if (hasReport && reportedPackages.size === 0) {
    fail("Rust future-incompatibility report could not be parsed");
  }
  const unexpected = [...reportedPackages].filter((pkg) => !allowedPackages.has(pkg));
  const stale = [...allowedPackages].filter((pkg) => !reportedPackages.has(pkg));
  if (unexpected.length) {
    fail(`unapproved Rust future-incompatible packages: ${unexpected.join(", ")}`);
  }
  if (stale.length) {
    fail(`stale Rust future-incompatibility exceptions: ${stale.join(", ")}`);
  }
}

assertToolchainPin();
const allowedFutureIncompatibilities = loadFutureIncompatAllowlist();
const workspacePackages = workspacePackageNames();

run("cargo", [
  "fmt",
  ...workspacePackages.flatMap((name) => ["--package", name]),
  "--",
  "--check"
]);
runFutureCompatibleCheck(allowedFutureIncompatibilities);
run("cargo", [
  "clippy",
  "--workspace",
  "--all-targets",
  "--locked",
  "--",
  "-D",
  "warnings"
]);
run("cargo", ["test", "--workspace", "--locked"]);

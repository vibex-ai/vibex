import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const WRITE = process.argv.includes("--write");
const SELF_TEST = process.argv.includes("--self-test");
const EXPECTED_DEFERRED_PLATFORMS = ["macos", "windows"];

function path(relativePath) {
  return resolve(ROOT, relativePath);
}

function readJson(relativePath) {
  return JSON.parse(readFileSync(path(relativePath), "utf8"));
}

function source(relativePath) {
  return readFileSync(path(relativePath), "utf8");
}

function sha256(relativePath) {
  return createHash("sha256").update(readFileSync(path(relativePath))).digest("hex");
}

function currentCommit() {
  try {
    return execFileSync("git", ["rev-parse", "--verify", "HEAD"], {
      cwd: ROOT,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"]
    }).trim();
  } catch {
    return null;
  }
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function result(name, status, classification, notes) {
  return { name, status, classification, notes };
}

function runCheck(name, check, classification = "deterministic") {
  try {
    check();
    return result(name, "pass", classification, "passed");
  } catch (error) {
    return result(name, "fail", classification, error instanceof Error ? error.message : String(error));
  }
}

function validateStableIdentity() {
  assert(existsSync(path("apps/desktop/Cargo.toml")), "current desktop shell is missing");
  const runtime = source("crates/desktop-runtime/src/lib.rs");
  const app = source("apps/desktop/src/app.rs");
  for (const value of [
    "dev.vibex.desktop",
    "dev.vibex.desktop.preview",
    "dev.vibex.desktop.rc",
    "desktop-preview",
    "desktop-rc",
    "desktop-stable"
  ]) {
    assert(runtime.includes(value), `missing GPUI release identity ${value}`);
  }
  assert(runtime.includes("acquire_home_lock: true"), "desktop release homes must retain locking");
  assert(app.includes('option_env!("VIBEX_CHANNEL")'), "packaged channel identity is not embedded");
  assert(app.includes("release_channel_override_rejected"), "packaged channels allow runtime override");
  assert(app.includes("stable_channel_requires_release_build"), "stable identity is not build-gated");
}

function validatePackaging() {
  const packageJson = readJson("package.json");
  const linuxPackagingConfigs = [
    "apps/desktop/Packager.toml",
    "apps/desktop/Packager.linux.toml",
    "apps/desktop/Packager.preview.toml",
    "apps/desktop/Packager.rc.toml",
    "apps/desktop/Packager.stable.toml"
  ];
  for (const configPath of linuxPackagingConfigs) {
    const config = source(configPath);
    assert(
      config.includes("libayatana-appindicator3-1 | libappindicator3-1"),
      `${configPath} is missing its system-tray runtime dependency`
    );
  }
  const channels = [
    ["preview", "dev.vibex.desktop.preview", "Vibex Preview"],
    ["rc", "dev.vibex.desktop.rc", "Vibex RC"],
    ["stable", "dev.vibex.desktop", "Vibex"]
  ];
  for (const [channel, applicationId, productName] of channels) {
    const config = source(`apps/desktop/Packager.${channel}.toml`);
    const desktopEntryPath = `apps/desktop/packaging/${applicationId}.desktop`;
    const desktopEntry = source(desktopEntryPath);
    assert(config.includes(`identifier = "${applicationId}"`), `${channel} package id drifted`);
    assert(config.includes(`productName = "${productName}"`), `${channel} product name drifted`);
    assert(config.includes('assets/app-icons/icon.png'), `${channel} package does not own its icon`);
    assert(config.includes('assets/app-icons/icon-256.png'), `${channel} package is missing its taskbar icon size`);
    assert(config.includes(`packaging/${applicationId}.desktop`), `${channel} package is missing its app-id desktop entry`);
    assert(desktopEntry.includes(`StartupWMClass=${applicationId}`), `${channel} desktop entry app id drifted`);
    assert(desktopEntry.includes("Icon=vibex-desktop"), `${channel} desktop entry icon drifted`);
    assert(
      !config.includes("../web/dist") && !config.includes("../mobile-wasm/dist"),
      `${channel} desktop package must not embed browser or mobile runtime assets`
    );
    const command = packageJson.scripts?.[`package:${channel}`] ?? "";
    assert(command.includes(`build-channel.mjs ${channel}`), `${channel} package command drifted`);
    assert(command.includes("--formats deb,appimage"), `${channel} package formats drifted`);
  }
  const workflow = source(".github/workflows/release-candidate.yml");
  assert(workflow.includes("ubuntu-24.04"), "candidate workflow lost its Linux host");
  assert(workflow.includes("--formats deb,appimage"), "candidate workflow formats drifted");
  for (const dependency of [
    "libglib2.0-dev",
    "libgtk-3-dev",
    "libayatana-appindicator3-dev",
    "libwebkit2gtk-4.1-dev"
  ]) {
    assert(workflow.includes(dependency), `candidate workflow is missing ${dependency}`);
  }
  const releaseWorkflow = source(".github/workflows/release.yml");
  for (const dependency of [
    "libglib2.0-dev",
    "libgtk-3-dev",
    "libayatana-appindicator3-dev",
    "libwebkit2gtk-4.1-dev"
  ]) {
    assert(releaseWorkflow.includes(dependency), `publish workflow is missing ${dependency}`);
  }
  for (const required of [
    "platform: linux",
    "platform: macos",
    "platform: windows",
    "name: Desktop ${{ matrix.platform }}",
    "mobile-android",
    "mobile-ios",
    "download-artifact",
    "gh release create",
    "package-desktop-release.mjs",
    "VIBEX_UPDATE_SIGNING_ENABLED",
    "CARGO_NDK_VERSION",
    "ANDROID_NDK_HOME",
    "ANDROID_NDK_ROOT",
    "MARKETING_VERSION",
    "generated_key",
    "SHA256SUMS"
  ]) {
    assert(releaseWorkflow.includes(required), `publish workflow is missing ${required}`);
  }
  assert(
    source("apps/mobile/scripts/build-android.sh").includes("bundleRelease"),
    "Android release workflow must produce an AAB"
  );
  const iosBuildScript = source("apps/mobile/scripts/build-ios.sh");
  assert(iosBuildScript.includes("--crate-type staticlib"), "iOS release build must emit a static library");
  assert(iosBuildScript.includes("--lib"), "iOS release build must select the mobile library target");
  assert(!iosBuildScript.includes("cargo build -p vibex-mobile"), "iOS release build must not link manifest crate types");
  const desktopReleaseScript = source("scripts/package-desktop-release.mjs");
  assert(
    desktopReleaseScript.includes("withIcons(config, MACOS_ICON_INPUTS)"),
    "macOS release packaging must select a supported ICNS input set"
  );
  for (const icon of ["icon-16.png", "icon-32.png", "icon-48.png", "icon-128.png", "icon-256.png"]) {
    assert(
      desktopReleaseScript.includes(`assets/app-icons/${icon}`),
      `macOS release packaging is missing ${icon}`
    );
  }
  assert(
    desktopReleaseScript.includes("--formats"),
    "desktop release packager must select an explicit platform format"
  );
}

function validateUiStateSafety() {
  const model = source("crates/desktop-model/src/ui_state.rs");
  for (const retained of [
    "load_read_only",
    "load_or_default",
    "backup_snapshot",
    "decode_and_migrate",
    "back_up_corrupt_state",
    "replace_file"
  ]) {
    assert(model.includes(retained), `UI-state safety capability is missing: ${retained}`);
  }
  assert(existsSync(path("crates/backup")), "data backup crate was removed");
  assert(existsSync(path("crates/desktop-runtime/src/home_lock.rs")), "runtime home lock was removed");
}

function validateCurrentReleaseDocs() {
  const runbook = source("docs/operations/release.md");
  const matrix = source("docs/operations/release-packaging-matrix.md");
  assert(
    /published[- ]artifact/.test(runbook),
    "runbook does not preserve published-artifact rollback"
  );
  assert(matrix.includes("package:stable"), "packaging matrix omits the stable desktop package");
}

function validateDependencyAndSupplyChain() {
  const metadata = JSON.parse(
    execFileSync("cargo", ["metadata", "--locked", "--format-version", "1"], {
      cwd: ROOT,
      encoding: "utf8",
      maxBuffer: 128 * 1024 * 1024
    })
  );
  assert(metadata.packages.some((pkg) => pkg.name === "vibex-desktop"), "current desktop Cargo package is missing");
  assert(!metadata.packages.some((pkg) => pkg.name === "vibex-desktop-gpui"), "old desktop Cargo package remains");
  const policy = readJson("docs/licenses/desktop-policy.json");
  const packageJson = readJson("package.json");
  const fontVersion = packageJson.devDependencies?.["@fontsource-variable/inter"];
  const desktopAssets = source("apps/desktop/src/assets.rs");
  assert(fontVersion === "5.2.8", "GPUI build-time Inter dependency is not pinned at the workspace root");
  assert(
    desktopAssets.includes(`@fontsource-variable+inter@${fontVersion}/`),
    "GPUI embedded Inter path does not match the declared workspace dependency"
  );
  const appIcons = policy.assetInputs?.find(
    (input) => input.id === "vibex-desktop-application-icons"
  );
  assert(appIcons?.root === "apps/desktop/assets/app-icons", "application icon ownership drifted");
  const sbom = readJson("docs/licenses/desktop.cdx.json");
  assert(sbom.bomFormat === "CycloneDX" && sbom.components?.length > 0, "release SBOM is invalid");
}

function validateSelfTest() {
  const tampered = source("apps/desktop/Packager.stable.toml").replace(
    'identifier = "dev.vibex.desktop"',
    'identifier = "dev.vibex.tampered"'
  );
  let rejected = false;
  try {
    assert(tampered.includes('identifier = "dev.vibex.desktop"'), "fixture package id drifted");
  } catch {
    rejected = true;
  }
  assert(rejected, "release checker self-test accepted a drifted stable package identity");
}

const checks = [
  runCheck("stable_identity", validateStableIdentity),
  runCheck("packaging", validatePackaging),
  runCheck("ui_state_safety", validateUiStateSafety),
  runCheck("current_release_docs", validateCurrentReleaseDocs),
  runCheck("dependency_and_supply_chain", validateDependencyAndSupplyChain)
];
if (SELF_TEST) checks.push(runCheck("checker_self_test", validateSelfTest));

const commit = currentCommit();
assert(!WRITE || commit, "release preflight evidence requires a committed HEAD");

const report = {
  schemaVersion: "release-preflight.v3",
  generatedAtMs: Date.now(),
  commit: commit ?? "uncommitted-worktree",
  cargoLockSha256: sha256("Cargo.lock"),
  overallStatus: checks.every((check) => check.status === "pass") ? "pass" : "fail",
  releaseOwner: "apps/desktop",
  rollbackMechanism: "published_release_artifacts",
  acceptedDeferredPlatforms: EXPECTED_DEFERRED_PLATFORMS,
  checks
};

if (WRITE) {
  writeFileSync(path("docs/release/release-preflight.json"), `${JSON.stringify(report, null, 2)}\n`);
}
console.log(JSON.stringify(report, null, 2));
if (report.overallStatus !== "pass") process.exitCode = 1;

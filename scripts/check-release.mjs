import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const WRITE = process.argv.includes("--write");
const SELF_TEST = process.argv.includes("--self-test");
// PDFium distribution is approved for linux-x86_64 only; every other desktop
// packaging target ships without the embedded PDFium runtime until its native
// runtime review completes.
const EXPECTED_PDFIUM_APPROVED_ARCHITECTURES = { linux: ["x86_64"] };
const REQUIRED_APPIMAGE_EXCLUDED_LIBS = 'excludedLibs = ["libpdfium.so", "libwayland*"]';

function path(relativePath) {
  return resolve(ROOT, relativePath);
}

function readJson(relativePath) {
  return JSON.parse(readFileSync(path(relativePath), "utf8"));
}

function source(relativePath) {
  return readFileSync(path(relativePath), "utf8");
}

// Windows runners check text sources out with CRLF, so multi-line contract
// assertions must match against normalized line endings.
function workflowSource(relativePath) {
  return source(relativePath).split("\r\n").join("\n");
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

function validateAppImageGraphicsBoundary(configPath, config) {
  assert(
    config.includes(REQUIRED_APPIMAGE_EXCLUDED_LIBS),
    `${configPath} must leave Wayland libraries to the host graphics stack`
  );
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
  const appImagePackagingConfigs = ["linux", "preview", "rc", "stable"].map(
    (name) => `apps/desktop/Packager.${name}.toml`
  );
  const linuxPackagingConfigs = [
    "apps/desktop/Packager.toml",
    ...appImagePackagingConfigs
  ];
  for (const configPath of linuxPackagingConfigs) {
    const config = source(configPath);
    assert(
      config.includes("libgtk-3-0"),
      `${configPath} is missing its GTK 3 runtime dependency`
    );
    assert(
      !config.includes("appindicator"),
      `${configPath} still asks for a tray runtime the ksni backend does not use`
    );
  }
  for (const configPath of appImagePackagingConfigs) {
    validateAppImageGraphicsBoundary(configPath, source(configPath));
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
      /^Categories=\S+;$/mu.test(desktopEntry),
      `${channel} desktop entry needs one main Categories value; appimagetool refuses an empty or missing key`
    );
    assert(
      config.includes('category = "Developer Tool"'),
      `${channel} Packager config must categorize the generated entry too`
    );
    assert(
      !config.includes("../web/dist") && !config.includes("../mobile-wasm/dist"),
      `${channel} desktop package must not embed browser or mobile runtime assets`
    );
    const command = packageJson.scripts?.[`package:${channel}`] ?? "";
    assert(command.includes(`build-channel.mjs ${channel}`), `${channel} package command drifted`);
    assert(command.includes("--formats deb,appimage"), `${channel} package formats drifted`);
    assert(command.includes("compress-deb.mjs"), `${channel} package does not xz-compress its deb`);
  }
  const workflow = workflowSource(".github/workflows/release-candidate.yml");
  assert(workflow.includes("ubuntu-24.04"), "candidate workflow lost its Linux host");
  assert(workflow.includes("--formats deb,appimage"), "candidate workflow formats drifted");
  assert(workflow.includes("compress-deb.mjs"), "candidate workflow does not xz-compress its deb");
  for (const dependency of [
    "libglib2.0-dev",
    "libgtk-3-dev",
    "libwebkit2gtk-4.1-dev"
  ]) {
    assert(workflow.includes(dependency), `candidate workflow is missing ${dependency}`);
  }
  assert(
    !workflow.includes("appindicator"),
    "candidate workflow still installs a dev package the ksni tray does not build against"
  );
  const releaseWorkflow = workflowSource(".github/workflows/release.yml");
  for (const dependency of [
    "libglib2.0-dev",
    "libgtk-3-dev",
    "libwebkit2gtk-4.1-dev"
  ]) {
    assert(releaseWorkflow.includes(dependency), `publish workflow is missing ${dependency}`);
  }
  assert(
    !releaseWorkflow.includes("appindicator"),
    "publish workflow still installs a dev package the ksni tray does not build against"
  );
  const releaseMatrix = [
    ["linux", "x86_64", "ubuntu-24.04", "x86_64-unknown-linux-gnu"],
    ["linux", "aarch64", "ubuntu-24.04", "aarch64-unknown-linux-gnu"],
    ["macos", "x86_64", "macos-14", "x86_64-apple-darwin"],
    ["macos", "aarch64", "macos-14", "aarch64-apple-darwin"],
    ["windows", "x86_64", "windows-2022", "x86_64-pc-windows-msvc"],
    ["windows", "aarch64", "windows-2022", "aarch64-pc-windows-msvc"]
  ];
  for (const [platform, arch, runner, target] of releaseMatrix) {
    const matrixEntry = `platform: ${platform}\n            runner: ${runner}\n            arch: ${arch}\n            target: ${target}`;
    assert(
      releaseWorkflow.includes(matrixEntry),
      `publish workflow is missing the ${platform}-${arch} release matrix entry`
    );
  }
  assert(
    releaseWorkflow.includes("--arch ${{ matrix.arch }}"),
    "publish workflow does not pass the release architecture to the packaging script"
  );
  for (const required of [
    "gcc-aarch64-linux-gnu",
    "g++-aarch64-linux-gnu",
    "libwebkit2gtk-4.1-dev:arm64",
    "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER"
  ]) {
    assert(
      releaseWorkflow.includes(required),
      `publish workflow is missing the Linux arm64 cross-build contract ${required}`
    );
  }
  assert(
    releaseWorkflow.includes("Multi-Arch: same packages must resolve to one version") &&
      releaseWorkflow.includes("--allow-downgrades"),
    "publish workflow lost the Linux arm64 Multi-Arch version alignment"
  );
  assert(
    releaseWorkflow.includes("name: desktop-${{ matrix.platform }}-${{ matrix.arch }}-${{ github.sha }}"),
    "publish workflow artifact names lost their architecture segment"
  );
  assert(
    releaseWorkflow.includes("manifest_arch"),
    "publish workflow updater manifest does not normalize artifact architectures"
  );
  assert(
    releaseWorkflow.includes("for attempt in 1 2 3") && releaseWorkflow.includes("gh release delete"),
    "publish workflow lost its recovery from a rolled-back release upload"
  );
  for (const required of [
    "platform: linux",
    "platform: macos",
    "platform: windows",
    "name: Desktop ${{ matrix.platform }} ${{ matrix.arch }}",
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
  // The desktop About page resolves the changelog from a per-release notes
  // asset, so the publish workflow must stage the tagged document under the
  // exact name the updater requests.
  assert(
    releaseWorkflow.includes('notes="docs/operations/release-notes-v${version}.md"'),
    "publish workflow does not read the release notes for the published version"
  );
  assert(
    releaseWorkflow.includes("target/publish/vibex-release-notes.md"),
    "publish workflow does not stage vibex-release-notes.md for upload"
  );
  assert(
    releaseWorkflow.includes("has no English section"),
    "publish workflow does not require an English release-notes section"
  );
  // Container images publish from their own tag/branch-triggered workflow.
  // Keep the registry, the build inputs, the architecture coverage, and the
  // "latest only points at a stable release" guarantee under contract.
  const containerWorkflow = workflowSource(".github/workflows/publish-container-images.yml");
  for (const required of [
    "ghcr.io",
    "deploy/server/Dockerfile",
    "deploy/relay/Dockerfile",
    "apps/server/Cargo.toml",
    "apps/relay-server/Cargo.toml",
    "linux/amd64",
    "linux/arm64",
    "ubuntu-24.04-arm",
    "push-by-digest=true",
    "type=raw,value=rc,enable=",
    "type=raw,value=latest,enable=",
    "latest=false"
  ]) {
    assert(
      containerWorkflow.includes(required),
      `container image workflow is missing ${required}`
    );
  }
  const latestTagRule =
    containerWorkflow.split("\n").find((line) => line.includes("value=latest,enable=")) ?? "";
  assert(
    latestTagRule.includes("refs/tags/") && latestTagRule.includes("contains(github.ref_name, '-')"),
    "container image workflow must not publish latest for a prerelease tag"
  );
  // Docker Hub is a mirror of the GHCR release, so it must stay a copy rather
  // than a second build, must never carry a prerelease, and must not be able to
  // withhold the images operators already pull from GHCR.
  for (const required of [
    "scripts/mirror-registry.mjs",
    "DOCKERHUB_NAMESPACE: ${{ vars.DOCKERHUB_NAMESPACE || 'vibexai' }}",
    "DOCKERHUB_TOKEN",
    "docker.io",
    "continue-on-error: true"
  ]) {
    assert(containerWorkflow.includes(required), `Docker Hub mirror is missing ${required}`);
  }
  // The Docker Hub repository name must match the GHCR one, so an operator
  // swaps the registry prefix and nothing else. A rename here would silently
  // ship an image no document mentions.
  const mirrorScript = source("scripts/mirror-registry.mjs");
  assert(
    mirrorScript.includes("/vibex-${service}`"),
    "Docker Hub mirror must keep the published image names"
  );
  const dockerHubJob = containerWorkflow.split("\n  dockerhub:")[1] ?? "";
  assert(
    dockerHubJob.includes("contains(github.ref_name, '-')"),
    "Docker Hub mirror must not run for a prerelease tag"
  );
  assert(
    !dockerHubJob.includes("docker/build-push-action"),
    "Docker Hub mirror must copy the published manifest list, not rebuild it"
  );
  assert(
    mirrorScript.includes("imagetools") && mirrorScript.includes("create"),
    "registry mirrors must copy manifest lists with buildx imagetools"
  );

  // CNB is the China-reachable mirror, so it carries the same contract as
  // Docker Hub: a copy of the GHCR index, stable only, and never able to
  // withhold the GHCR images.
  for (const required of [
    "docker.cnb.cool",
    "CNB_NAMESPACE: ${{ vars.CNB_NAMESPACE || 'vibex-ai/vibex' }}",
    "CNB_TOKEN",
    "username: cnb"
  ]) {
    assert(containerWorkflow.includes(required), `CNB mirror is missing ${required}`);
  }
  const cnbJob = containerWorkflow.split("\n  cnb:")[1] ?? "";
  assert(
    cnbJob.includes("contains(github.ref_name, '-')"),
    "CNB mirror must not run for a prerelease tag"
  );
  assert(
    cnbJob.includes("continue-on-error: true"),
    "CNB mirror must not be able to withhold the GHCR images"
  );
  assert(
    !cnbJob.includes("docker/build-push-action"),
    "CNB mirror must copy the published manifest list, not rebuild it"
  );

  // The AUR package republishes the release .deb. AUR carries exactly one
  // version per package, so a prerelease pushed there downgrades every Arch
  // user; keep the stable-only gate, the checksum recording, and the pinned
  // host keys under contract.
  const aurWorkflow = workflowSource(".github/workflows/publish-aur.yml");
  for (const required of [
    "types: [published]",
    "packaging/aur/vibex-bin/PKGBUILD",
    "scripts/publish-aur.mjs",
    "makepkg",
    "namcap",
    "AUR_SSH_PRIVATE_KEY",
    "aur.archlinux.org ssh-ed25519",
    "RELEASE_PRERELEASE",
    // `gh` downloads the release payloads; it is not part of base-devel.
    "github-cli",
    "gh release download",
    // makepkg refuses to run as root, and the container job is root.
    "runuser -u builder",
    "useradd"
  ]) {
    assert(aurWorkflow.includes(required), `AUR workflow is missing ${required}`);
  }
  assert(
    aurWorkflow.includes("is not a stable x.y.z release"),
    "AUR workflow does not refuse a prerelease tag"
  );
  const aurPackage = source("packaging/aur/vibex-bin/PKGBUILD");
  assert(
    /^pkgname=vibex-bin$/mu.test(aurPackage),
    "AUR package name drifted"
  );
  assert(
    aurPackage.includes("license=('AGPL-3.0-or-later')"),
    "AUR package lost its license declaration"
  );
  assert(
    aurPackage.includes("StartupWMClass=dev.vibex.desktop"),
    "AUR desktop entry must keep the stable window class"
  );
  assert(
    !/^NoDisplay=true$/mu.test(aurPackage),
    "AUR desktop entry must stay visible in application menus"
  );

  for (const required of [
    "sign-android-release.sh",
    "VIBEX_ANDROID_KEYSTORE_BASE64",
    "VIBEX_ANDROID_KEYSTORE_PASSWORD"
  ]) {
    assert(releaseWorkflow.includes(required), `publish workflow is missing Android signing contract ${required}`);
  }
  const androidSigningScript = source("apps/mobile/scripts/sign-android-release.sh");
  for (const required of ["apksigner", "zipalign", "jarsigner", "VIBEX_RELEASE_CHANNEL"]) {
    assert(
      androidSigningScript.includes(required),
      `Android signing script is missing ${required}`
    );
  }
  assert(
    !releaseWorkflow.includes("app-release-unsigned.apk \"${output}/vibex-"),
    "publish workflow must not publish an unsigned Android APK"
  );
  assert(
    source("apps/mobile/scripts/build-android.sh").includes("bundleRelease"),
    "Android release workflow must produce an AAB"
  );
  const iosBuildScript = source("apps/mobile/scripts/build-ios.sh");
  assert(iosBuildScript.includes("--crate-type staticlib"), "iOS release build must emit a static library");
  assert(iosBuildScript.includes("--lib"), "iOS release build must select the mobile library target");
  assert(!iosBuildScript.includes("cargo build -p vibex-mobile"), "iOS release build must not link manifest crate types");
  // The unsigned device IPA is how an iOS build ships without an Apple account.
  // Keep it unsigned by contract: a signed bundle would need credentials the
  // release pipeline deliberately does not hold, and the artifact name would
  // stop describing its contents.
  const iosIpaScript = source("apps/mobile/scripts/package-ios-unsigned-ipa.sh");
  for (const required of [
    "-sdk iphoneos",
    "CODE_SIGNING_ALLOWED=NO",
    "CODE_SIGNING_REQUIRED=NO",
    "Payload/Vibex.app",
    "refusing to publish a signed bundle as an unsigned IPA"
  ]) {
    assert(
      iosIpaScript.includes(required),
      `iOS unsigned packaging script is missing ${required}`
    );
  }
  assert(
    iosIpaScript.includes("vibex-${VERSION}-ios-unsigned.ipa"),
    "iOS unsigned packaging script does not name the device IPA"
  );
  assert(
    releaseWorkflow.includes("package-ios-unsigned-ipa.sh"),
    "publish workflow does not package the unsigned iOS device IPA"
  );
  assert(
    releaseWorkflow.includes('"${output}"/*.ipa'),
    "publish workflow does not checksum the unsigned iOS device IPA"
  );
  const desktopReleaseScript = source("scripts/package-desktop-release.mjs");
  assert(
    desktopReleaseScript.includes("withIcons(config, MACOS_ICON_INPUTS)"),
    "macOS release packaging must select a supported ICNS input set"
  );
  // macOS draws an app icon exactly as provided, so the bundle must carry the
  // icon-grid renditions (824x824 body on a 1024x1024 canvas) instead of the
  // full-bleed Linux/Windows artwork, which renders about a quarter larger than
  // neighbouring Dock icons.
  for (const icon of [
    "icon-macos-16.png",
    "icon-macos-32.png",
    "icon-macos-48.png",
    "icon-macos-128.png",
    "icon-macos-256.png"
  ]) {
    assert(
      desktopReleaseScript.includes(`assets/app-icons/${icon}`),
      `macOS release packaging is missing ${icon}`
    );
    assert(
      existsSync(path(`apps/desktop/assets/app-icons/${icon}`)),
      `macOS release packaging references a missing icon: ${icon}`
    );
  }
  assert(
    !desktopReleaseScript.includes("assets/app-icons/icon-16.png"),
    "macOS release packaging must not fall back to the full-bleed icon renditions"
  );
  assert(
    desktopReleaseScript.includes("--formats"),
    "desktop release packager must select an explicit platform format"
  );
  assert(
    desktopReleaseScript.includes("const ARCHITECTURES = new Set([\"x86_64\", \"aarch64\"])"),
    "desktop release packager lost its cross-architecture set"
  );
  assert(
    desktopReleaseScript.includes("TARGET_TRIPLES"),
    "desktop release packager lost its target triple map"
  );
  assert(
    desktopReleaseScript.includes('"--target", triple'),
    "cross-architecture packaging must pass --target to cargo packager"
  );
  assert(
    desktopReleaseScript.includes('withField(config, "binariesDir"') &&
      desktopReleaseScript.includes("binariesDir"),
    "cross-architecture packaging must repoint binariesDir at the target triple build directory"
  );
  assert(
    desktopReleaseScript.includes("withoutResources"),
    "deferred-architecture packaging must drop the Linux-only PDFium resources"
  );
  assert(
    desktopReleaseScript.includes('platform === "linux" && arch === "x86_64"'),
    "desktop release packager must gate the approved PDFium runtime to linux-x86_64"
  );
  for (const triple of [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc"
  ]) {
    assert(
      desktopReleaseScript.includes(triple),
      `desktop release packager is missing the ${triple} release target`
    );
  }
  const buildChannelScript = source("scripts/build-channel.mjs");
  assert(
    buildChannelScript.includes('"--target", target'),
    "channel build script must forward --target to cargo build"
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
  assert(
    runbook.includes("release-notes-v<version>.md") &&
      runbook.includes("vibex-release-notes.md"),
    "runbook does not document the localized release-notes asset"
  );
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

function validateServerRelease() {
  const dockerfile = source("deploy/server/Dockerfile.build");
  assert(
    dockerfile.includes("FROM debian:bookworm-slim@sha256:"),
    "server builder must pin the Debian 12 base image by digest"
  );
  assert(
    dockerfile.includes("--default-toolchain 1.97.0"),
    "server builder must pin the workspace Rust toolchain"
  );
  for (const required of [
    "gcc-aarch64-linux-gnu",
    "g++-aarch64-linux-gnu",
    "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER"
  ]) {
    assert(dockerfile.includes(required), `server builder is missing ${required}`);
  }
  for (const required of [
    "scripts/check-server-artifact.mjs",
    "scripts/package-server-release.mjs"
  ]) {
    assert(existsSync(path(required)), `server release script is missing: ${required}`);
  }

  const workflow = workflowSource(".github/workflows/release.yml");
  for (const required of [
    "name: Server Linux",
    "deploy/server/Dockerfile.build",
    "cargo build -p vibex-server --release --locked",
    "for arch in x86_64 aarch64",
    "--max-glibc 2.35",
    "scripts/check-server-artifact.mjs",
    "scripts/package-server-release.mjs",
    "--server-artifact",
    "require_artifact_class 'vibex-server-*-linux-x86_64.tar.gz'",
    "require_artifact_class 'vibex-server-*-linux-aarch64.tar.gz'",
    "name: server-linux-${{ github.sha }}"
  ]) {
    assert(workflow.includes(required), `publish workflow is missing server release contract ${required}`);
  }
  assert(
    workflow.includes("docker run --rm"),
    "server artifact must be linked inside the pinned builder, not on the runner"
  );

  const manifest = source("crates/app-update/src/manifest.rs");
  assert(manifest.includes("pub struct ServerArtifact"), "update manifest has no headless server artifact type");
  assert(manifest.includes("server_artifacts"), "update manifest has no server artifact field");
  const generator = source("crates/app-update/src/bin/vibex-update-manifest.rs");
  assert(generator.includes("--server-artifact"), "manifest generator cannot add a server artifact");
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

  const configPath = "apps/desktop/Packager.stable.toml";
  const missingWaylandExclusion = source(configPath).replace(
    REQUIRED_APPIMAGE_EXCLUDED_LIBS,
    'excludedLibs = ["libpdfium.so"]'
  );
  rejected = false;
  try {
    validateAppImageGraphicsBoundary(configPath, missingWaylandExclusion);
  } catch {
    rejected = true;
  }
  assert(rejected, "release checker self-test accepted bundled Wayland libraries");
}

const checks = [
  runCheck("stable_identity", validateStableIdentity),
  runCheck("packaging", validatePackaging),
  runCheck("server_release", validateServerRelease),
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
  acceptedPdfiumApprovedArchitectures: EXPECTED_PDFIUM_APPROVED_ARCHITECTURES,
  checks
};

if (WRITE) {
  const evidencePath = path("docs/release/release-preflight.json");
  // The evidence directory is not tracked, so a fresh checkout does not have
  // it; create it instead of failing the documented `pnpm release:preflight`.
  mkdirSync(dirname(evidencePath), { recursive: true });
  writeFileSync(evidencePath, `${JSON.stringify(report, null, 2)}\n`);
}
console.log(JSON.stringify(report, null, 2));
if (report.overallStatus !== "pass") process.exitCode = 1;

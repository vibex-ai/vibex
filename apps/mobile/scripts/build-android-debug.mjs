import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { resolveGpuiSourceIdentities } from "../../../scripts/source-identities.mjs";
import {
  MOBILE_SOURCE_INPUTS,
  WEB_SOURCE_INPUTS,
  sourceTreeSha256
} from "../../../scripts/wasm-source-tree.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const APP = join(ROOT, "apps/mobile");
const ANDROID = join(APP, "android");
const ARTIFACTS = join(APP, "artifacts");
const release = process.argv.includes("--release");
const variant = release ? "release" : "debug";
const APK_SOURCE = join(
  ANDROID,
  release
    ? "app/build/outputs/apk/release/app-release-unsigned.apk"
    : "app/build/outputs/apk/debug/app-debug.apk"
);
const artifactName = release ? "vibex-release-unsigned.apk" : "vibex-gate-debug.apk";
const APK_OUTPUT = join(ARTIFACTS, artifactName);
const LOCAL_EVIDENCE = join(ARTIFACTS, `android-${variant}-build.json`);
const REPOSITORY_EVIDENCE = join(ROOT, "docs/platform/evidence/wasm-android-build.json");
const MOBILE_EVIDENCE_CHECKER = join(ROOT, "scripts/capture-wasm-mobile.mjs");
const NATIVE_SHELL_CONTRACT = join(APP, "native-shell-contract.json");
const writeEvidence = process.argv.includes("--write-evidence");

function fail(message) {
  throw new Error(message);
}

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function output(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? ROOT,
    encoding: "utf8",
    env: options.env ?? process.env,
    maxBuffer: 16 * 1024 * 1024
  });
  if (result.error || result.status !== 0) {
    fail(`${command} ${args.join(" ")} failed:\n${result.stderr ?? result.error?.message}`);
  }
  return result.stdout.trim() || result.stderr.trim();
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? ROOT,
    env: options.env ?? process.env,
    stdio: "inherit"
  });
  if (result.error || result.status !== 0) {
    fail(`${command} ${args.join(" ")} exited with ${result.status ?? result.error?.message}`);
  }
}

function firstDirectory(path, prefix) {
  if (!existsSync(path)) return null;
  return readdirSync(path)
    .filter((entry) => entry.startsWith(prefix) && statSync(join(path, entry)).isDirectory())
    .sort()
    .at(-1);
}

function resolveJavaHome() {
  const candidates = [
    process.env.JAVA_HOME,
    firstDirectory(join(process.env.HOME ?? "", ".local/share/jdks"), "temurin-21")
      ? join(
          process.env.HOME ?? "",
          ".local/share/jdks",
          firstDirectory(join(process.env.HOME ?? "", ".local/share/jdks"), "temurin-21")
        )
      : null
  ];
  const home = candidates.find((candidate) => candidate && existsSync(join(candidate, "bin/java")));
  if (!home) fail("JDK 21 is required; set JAVA_HOME to a JDK 21 installation");
  const version = output(join(home, "bin/java"), ["-version"]);
  const versionLine = version.split("\n").find((line) => /(?:openjdk|java) version/.test(line));
  if (!versionLine || !/version "21\./.test(versionLine)) {
    fail(`JDK 21 is required, found: ${version}`);
  }
  return { home, version: versionLine };
}

function resolveAndroidHome() {
  const candidates = [
    process.env.ANDROID_HOME,
    process.env.ANDROID_SDK_ROOT,
    join(process.env.HOME ?? "", ".local/share/android-sdk"),
    join(process.env.HOME ?? "", "Android/Sdk")
  ];
  const home = candidates.find(
    (candidate) => candidate && existsSync(join(candidate, "platform-tools")) && existsSync(join(candidate, "platforms"))
  );
  if (!home) fail("Android SDK is required; set ANDROID_HOME or ANDROID_SDK_ROOT");
  return home;
}

function androidPackageRevision(path, packageName) {
  if (!existsSync(path)) fail(`${packageName} source.properties is missing: ${path}`);
  const properties = new Map(
    readFileSync(path, "utf8")
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => line && !line.startsWith("#") && line.includes("="))
      .map((line) => {
        const separator = line.indexOf("=");
        return [line.slice(0, separator).trim(), line.slice(separator + 1).trim()];
      })
  );
  const revision = properties.get("Pkg.Revision");
  if (!revision) fail(`${packageName} revision is missing from source.properties`);
  return `${packageName} ${revision}`;
}

function archiveContainsGpui(apk, unzip, expectedPlugins) {
  const listing = output(unzip, ["-l", apk]);
  if (
    !listing.includes("assets/public/host.js") ||
    !listing.includes("assets/public/host-services.js") ||
    !listing.includes("assets/public/platform-compat.js") ||
    !listing.includes("assets/public/manifest.webmanifest") ||
    !listing.includes("assets/public/service-worker.js") ||
    !listing.includes("assets/public/pkg/vibex_web_bg.wasm")
  ) {
    fail("APK does not contain the Web host and WASM assets");
  }
  if (!listing.includes("assets/public/build.json")) fail("APK does not contain the Web build identity");
  if (!listing.includes("assets/capacitor.plugins.json")) fail("APK does not contain the Capacitor plugin manifest");
  if (listing.includes("assets/public/assets/")) {
    fail("APK contains a legacy bundler assets directory instead of the scoped Web dist");
  }
  const packagedPlugins = JSON.parse(output(unzip, ["-p", apk, "assets/capacitor.plugins.json"]));
  for (const expected of expectedPlugins) {
    if (
      !packagedPlugins.some(
        (entry) => entry.pkg === expected.package && entry.classpath === expected.classpath
      )
    ) {
      fail(`APK is missing Capacitor plugin ${expected.package}`);
    }
  }
}

function validateNativeShell(contract) {
  if (contract.schemaVersion !== "vibex-native-shell.v1") {
    fail("native shell contract schema is invalid");
  }
  const manifestPath = join(ANDROID, "app/src/main/assets/capacitor.plugins.json");
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  const appGradle = readFileSync(join(ANDROID, "app/build.gradle"), "utf8");
  const gradle = readFileSync(join(ANDROID, "app/capacitor.build.gradle"), "utf8");
  const androidManifest = readFileSync(join(ANDROID, "app/src/main/AndroidManifest.xml"), "utf8");
  const strings = readFileSync(join(ANDROID, "app/src/main/res/values/strings.xml"), "utf8");
  const variables = readFileSync(join(ANDROID, "variables.gradle"), "utf8");
  if (!appGradle.includes(`namespace = "${contract.applicationId}"`) || !appGradle.includes(`applicationId "${contract.applicationId}"`)) {
    fail("generated Android Gradle identity does not match the native shell contract");
  }
  if (!strings.includes(`<string name="app_name">${contract.applicationName}</string>`) || !strings.includes(`<string name="package_name">${contract.applicationId}</string>`)) {
    fail("generated Android string identity does not match the native shell contract");
  }
  if (!(contract.deepLink?.customSchemes ?? []).every((scheme) => androidManifest.includes(`android:scheme="${scheme}"`))) {
    fail("generated Android manifest does not contain every native shell scheme");
  }
  const configuredMinSdk = Number(variables.match(/minSdkVersion\s*=\s*(\d+)/)?.[1]);
  if (configuredMinSdk !== contract.platform?.android?.minSdk) {
    fail(`generated Android minSdk ${configuredMinSdk} does not match the native shell contract`);
  }
  for (const permission of contract.platform?.android?.permissions ?? []) {
    if (!androidManifest.includes(`android:name="${permission}"`)) {
      fail(`generated Android manifest is missing ${permission}`);
    }
  }
  for (const expected of contract.plugins) {
    const packageManifest = JSON.parse(
      readFileSync(join(APP, "node_modules", expected.package, "package.json"), "utf8")
    );
    if (packageManifest.version !== expected.version) {
      fail(
        `${expected.package} resolved to ${packageManifest.version}; expected ${expected.version}`
      );
    }
    if (
      !manifest.some(
        (entry) => entry.pkg === expected.package && entry.classpath === expected.classpath
      )
    ) {
      fail(`generated plugin manifest is missing ${expected.package}`);
    }
    if (!gradle.includes(`implementation project(':${expected.gradleProject}')`)) {
      fail(`generated Gradle dependencies are missing ${expected.gradleProject}`);
    }
  }
  const generatedFiles = Object.fromEntries(
    contract.generatedFiles.map((path) => {
      const absolute = join(APP, path);
      if (!existsSync(absolute)) fail(`generated native shell input is missing: ${path}`);
      const content = readFileSync(absolute);
      return [path, { bytes: content.length, sha256: sha256(content) }];
    })
  );
  return {
    contractSchemaVersion: contract.schemaVersion,
    plugins: contract.plugins.map(({ package: packageName, version, classpath }) => ({
      package: packageName,
      version,
      classpath
    })),
    android: {
      minSdk: configuredMinSdk,
      permissions: [...(contract.platform?.android?.permissions ?? [])]
    },
    generatedFiles
  };
}

const java = resolveJavaHome();
const androidHome = resolveAndroidHome();
const androidBuildTools = firstDirectory(join(androidHome, "build-tools"), "");
if (!androidBuildTools) fail("Android SDK build-tools are required");
const environment = {
  ...process.env,
  JAVA_HOME: java.home,
  ANDROID_HOME: androidHome,
  ANDROID_SDK_ROOT: androidHome,
  PATH: `${join(java.home, "bin")}:${join(androidHome, "platform-tools")}:${process.env.PATH ?? ""}`
};

run("pnpm", ["--filter", "@vibex/web", "build:release"], { env: environment });
if (!existsSync(ANDROID)) {
  run("pnpm", ["exec", "cap", "add", "android"], { cwd: APP, env: environment });
}
run("pnpm", ["exec", "cap", "sync", "android"], { cwd: APP, env: environment });
run("node", ["scripts/configure-native-links.mjs"], { cwd: APP, env: environment });
const nativeShellContract = JSON.parse(readFileSync(NATIVE_SHELL_CONTRACT, "utf8"));
const nativeShell = validateNativeShell(nativeShellContract);
rmSync(APK_SOURCE, { force: true });
run("./gradlew", ["--no-daemon", release ? "assembleRelease" : "assembleDebug"], { cwd: ANDROID, env: environment });

if (!existsSync(APK_SOURCE)) fail(`Gradle did not create ${APK_SOURCE}`);
mkdirSync(ARTIFACTS, { recursive: true });
copyFileSync(APK_SOURCE, APK_OUTPUT);

const apkanalyzer = join(androidHome, "cmdline-tools/latest/bin/apkanalyzer");
const unzip = "/usr/bin/unzip";
if (!existsSync(apkanalyzer)) fail(`apkanalyzer is missing: ${apkanalyzer}`);
if (!existsSync(unzip)) fail("unzip is required to validate packaged Web assets");
archiveContainsGpui(APK_OUTPUT, unzip, nativeShellContract.plugins);

const apk = readFileSync(APK_OUTPUT);
const packagedMinSdk = Number(output(apkanalyzer, ["manifest", "min-sdk", APK_OUTPUT]));
if (packagedMinSdk !== nativeShellContract.platform.android.minSdk) {
  fail(`packaged Android minSdk ${packagedMinSdk} does not match the native shell contract`);
}
const packagedApplicationId = output(apkanalyzer, ["manifest", "application-id", APK_OUTPUT]);
if (packagedApplicationId !== nativeShellContract.applicationId) {
  fail(
    `packaged Android application id ${packagedApplicationId} does not match the native shell contract`
  );
}
const webBuild = JSON.parse(readFileSync(join(ROOT, "apps/web/dist/build.json"), "utf8"));
const evidence = {
  schemaVersion: "vibex-android-build.v1",
  capturedAt: new Date().toISOString(),
  status: "packaged_not_device_validated",
  source: {
    ...resolveGpuiSourceIdentities(ROOT),
    webSourceTreeSha256: sourceTreeSha256(ROOT, WEB_SOURCE_INPUTS),
    mobileShellTreeSha256: sourceTreeSha256(ROOT, MOBILE_SOURCE_INPUTS),
    cargoLockfileSha256: sha256(readFileSync(join(ROOT, "Cargo.lock"))),
    pnpmLockfileSha256: sha256(readFileSync(join(ROOT, "pnpm-lock.yaml")))
  },
  artifact: {
    path: `apps/mobile/artifacts/${artifactName}`,
    bytes: apk.length,
    sha256: sha256(apk),
    applicationId: packagedApplicationId,
    minSdk: packagedMinSdk,
    targetSdk: Number(output(apkanalyzer, ["manifest", "target-sdk", APK_OUTPUT]))
  },
  webBuild,
  toolchain: {
    java: java.version,
    androidSdk: {
      buildTools: androidBuildTools,
      platformTools: androidPackageRevision(
        join(androidHome, "platform-tools/source.properties"),
        "Android SDK Platform-Tools"
      )
    },
    gradle: output("./gradlew", ["--version"], { cwd: ANDROID, env: environment })
      .split("\n")
      .find((line) => line.startsWith("Gradle ")),
    capacitor: output("pnpm", ["exec", "cap", "--version"], { cwd: APP, env: environment })
  },
  packageContract: {
    webDir: "../web/dist",
    containsGpuiHost: true,
    containsPlatformCompatibilityAdapter: true,
    containsGpuiWasm: true,
    containsLegacyReactDist: false,
    plugins: nativeShell.plugins.map((plugin) => `${plugin.package}@${plugin.version}`)
  },
  nativeShell,
  physicalValidation: {
    android: "pending",
    ios: "pending",
    installVerified: false
  }
};

writeFileSync(LOCAL_EVIDENCE, `${JSON.stringify(evidence, null, 2)}\n`);
if (writeEvidence && !release) {
  mkdirSync(dirname(REPOSITORY_EVIDENCE), { recursive: true });
  writeFileSync(REPOSITORY_EVIDENCE, `${JSON.stringify(evidence, null, 2)}\n`);
  run(process.execPath, [MOBILE_EVIDENCE_CHECKER, "--sync-current-build", "--write"], {
    env: environment
  });
}
console.log(`Android ${variant} APK: ${APK_OUTPUT}`);
console.log(`SHA-256: ${evidence.artifact.sha256}`);

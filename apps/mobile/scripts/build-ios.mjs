import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const APP = join(ROOT, "apps/mobile");
const IOS = join(APP, "ios");
const ARTIFACTS = join(APP, "artifacts");
const release = process.argv.includes("--release");
const configuration = release ? "Release" : "Debug";
const derivedData = join(ARTIFACTS, `ios-derived-${configuration.toLowerCase()}`);
const NATIVE_SHELL_CONTRACT = join(APP, "native-shell-contract.json");

function fail(message) {
  throw new Error(message);
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? ROOT,
    env: process.env,
    encoding: "utf8",
    stdio: options.capture ? "pipe" : "inherit"
  });
  if (result.error || result.status !== 0) {
    fail(`${command} ${args.join(" ")} failed: ${result.stderr ?? result.error?.message}`);
  }
  return result.stdout?.trim() ?? "";
}

if (process.platform !== "darwin") {
  fail("iOS GPUI shell builds require macOS with Xcode");
}
run("xcodebuild", ["-version"], { capture: true });
run("pnpm", ["--filter", "@vibex/web", "build:release"]);
if (!existsSync(IOS)) run("pnpm", ["exec", "cap", "add", "ios"], { cwd: APP });
run("pnpm", ["exec", "cap", "sync", "ios"], { cwd: APP });
run("node", ["scripts/configure-native-links.mjs"], { cwd: APP });
const nativeShell = JSON.parse(readFileSync(NATIVE_SHELL_CONTRACT, "utf8"));
const plist = readFileSync(join(IOS, "App/App/Info.plist"), "utf8");
const project = readFileSync(join(IOS, "App/App.xcodeproj/project.pbxproj"), "utf8");
if (!plist.includes(`<key>CFBundleDisplayName</key>`) || !plist.includes(`<string>${nativeShell.applicationName}</string>`)) {
  fail("generated iOS display name does not match the native shell contract");
}
if (!project.includes(`PRODUCT_BUNDLE_IDENTIFIER = ${nativeShell.applicationId};`)) {
  fail("generated iOS bundle id does not match the native shell contract");
}
for (const [key, value] of Object.entries(nativeShell.platform?.ios?.usageDescriptions ?? {})) {
  if (!plist.includes(`<key>${key}</key>`) || !plist.includes(`<string>${value}</string>`)) {
    fail(`generated iOS Info.plist is missing ${key}`);
  }
}

const workspace = join(IOS, "App/App.xcworkspace");
if (!existsSync(workspace)) fail(`Capacitor iOS workspace is missing: ${workspace}`);
mkdirSync(derivedData, { recursive: true });
run("xcodebuild", [
  "-workspace", workspace,
  "-scheme", "App",
  "-configuration", configuration,
  "-sdk", "iphonesimulator",
  "-destination", "generic/platform=iOS Simulator",
  "-derivedDataPath", derivedData,
  "CODE_SIGNING_ALLOWED=NO",
  "build"
]);

const appBundle = join(
  derivedData,
  `Build/Products/${configuration}-iphonesimulator/App.app`
);
if (!existsSync(appBundle)) fail(`Xcode did not create ${appBundle}`);
const webBuild = JSON.parse(readFileSync(join(ROOT, "apps/web/dist/build.json"), "utf8"));
const evidence = {
  schemaVersion: "vibex-ios-build.v1",
  status: "simulator_shell_built_not_device_validated",
  configuration,
  appBundle: `apps/mobile/artifacts/ios-derived-${configuration.toLowerCase()}/Build/Products/${configuration}-iphonesimulator/App.app`,
  webBuild,
  physicalValidation: {
    ios: "pending",
    signedDeviceBuild: false
  }
};
mkdirSync(ARTIFACTS, { recursive: true });
writeFileSync(
  join(ARTIFACTS, `ios-${configuration.toLowerCase()}-build.json`),
  `${JSON.stringify(evidence, null, 2)}\n`
);
console.log(`iOS ${configuration} shell: ${appBundle}`);

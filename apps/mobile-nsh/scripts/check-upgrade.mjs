// Guards the one thing a phone user cannot fix: an update that installs
// beside the old app instead of replacing it, or that installs over it but
// cannot see the data it left.
//
// The nsh client takes the GPUI client's place on both platforms, so the two
// have to agree on:
//
// - Android: `applicationId` (a different one is a second app, and Android
//   answers with "uninstall the existing app first"), and the `versionCode`
//   derivation (an update only installs when the integer is at least the
//   installed one, and the two clients compute it the same way).
// - iOS: `CFBundleIdentifier` of the app and of its share extension (iOS
//   installs a different bundle id side by side).
//
// The data side is checked in Rust: `apps/mobile-nsh/src/migration.rs` reads
// the files the GPUI client wrote and its tests pin both platforms' layouts.
//
// Run directly or through `pnpm check:mobile-native`.

import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const SELF_TEST = process.argv.includes("--self-test");

const LEGACY_ANDROID_BUILD = "apps/mobile/android/app/build.gradle";
const NEXT_ANDROID_BUILD = "apps/mobile-nsh/android/app/build.gradle";
const LEGACY_IOS_PROJECT = "apps/mobile/ios/project.yml";
const NEXT_IOS_PROJECT = "apps/mobile-nsh/ios/project.yml";
const NEXT_APP_MANIFEST = "apps/mobile-nsh/nsh.json";
const NEXT_MIGRATION = "apps/mobile-nsh/src/migration.rs";
const NEXT_IOS_BUILD = "apps/mobile-nsh/scripts/build-ios-host.sh";

function source(path) {
  return readFileSync(join(ROOT, path), "utf8");
}

function assert(condition, code) {
  if (!condition) throw new Error(code);
}

/** The first `name 'value'` / `name: value` at the top level of a Gradle file. */
function gradleValue(text, name) {
  const match = text.match(new RegExp(`^\\s*${name}\\s+['"]([^'"]+)['"]`, "mu"));
  return match?.[1] ?? null;
}

/** The `versionCode …` line, in the shape both builds derive it. */
function versionCodeExpression(text, label) {
  const match = text.match(/^\s*versionCode\s+([A-Za-z_][\w.]*)\s*$/mu);
  assert(match, `${label}: build.gradle has no derived versionCode`);
  return match[1];
}

/** The derivation block both builds have to share, whitespace-normalized. */
function versionCodeDerivation(text, label) {
  const match = text.match(/def vibexVersionCode = \{[\s\S]*?\n\}\(\)/u);
  assert(match, `${label}: build.gradle lost its versionCode derivation`);
  return match[0].replace(/\s+/gu, " ").trim();
}

function yamlValue(text, key, label) {
  const match = text.match(new RegExp(`^\\s*${key}:\\s*(\\S+)\\s*$`, "mu"));
  assert(match, `${label}: no ${key}`);
  return match[1];
}

/** The block of a named target in an XcodeGen spec (up to the next sibling). */
function targetBlock(text, target, label) {
  const lines = text.split("\n");
  const start = lines.findIndex((line) => line === `  ${target}:`);
  assert(start >= 0, `${label}: no ${target} target`);
  let end = lines.length;
  for (let index = start + 1; index < lines.length; index += 1) {
    // A sibling target, or the next top-level key (`schemes:`), ends it.
    if (/^  \S/u.test(lines[index]) || /^\S/u.test(lines[index])) {
      end = index;
      break;
    }
  }
  return lines.slice(start, end).join("\n");
}

/** The bundle identifier of a named target in an XcodeGen spec. */
function bundleId(text, target, label) {
  return yamlValue(
    targetBlock(text, target, label),
    "PRODUCT_BUNDLE_IDENTIFIER",
    `${label}:${target}`
  );
}

export function validateUpgradeContract(read = source) {
  const legacyAndroid = read(LEGACY_ANDROID_BUILD);
  const nextAndroid = read(NEXT_ANDROID_BUILD);

  const legacyId = gradleValue(legacyAndroid, "applicationId");
  const nextId = gradleValue(nextAndroid, "applicationId");
  // The nsh build may name it through a constant, which is the point: the
  // comment there explains why the id is the GPUI client's.
  const nextIdResolved =
    nextId ?? nextAndroid.match(/legacyApplicationId = '([^']+)'/u)?.[1] ?? null;
  assert(nextIdResolved, "the nsh Android build has no applicationId");
  assert(
    legacyId === nextIdResolved,
    `android_application_id_changed: the GPUI client shipped ${legacyId}, the nsh build declares ${nextIdResolved}; Android would install it as a second app`
  );

  assert(
    versionCodeExpression(legacyAndroid, "gpui") ===
      versionCodeExpression(nextAndroid, "nsh"),
    "android_version_code_source_changed: the two builds read different variables"
  );
  assert(
    versionCodeDerivation(legacyAndroid, "gpui") ===
      versionCodeDerivation(nextAndroid, "nsh"),
    "android_version_code_scheme_changed: the nsh build derives a different versionCode from the same version name"
  );
  // The release workflow stamps the tag's version; a build that fell back to a
  // stale literal would compute a versionCode the installed app outranks.
  assert(
    /^\s*versionName\s+vibexVersionName\s*$/mu.test(nextAndroid),
    "android_version_name_missing: the nsh build must stamp the version it derives the versionCode from"
  );

  const legacyIos = read(LEGACY_IOS_PROJECT);
  const nextIos = read(NEXT_IOS_PROJECT);
  const nextAppId = bundleId(nextIos, "Vibex", "nsh");
  assert(
    bundleId(legacyIos, "Vibex", "gpui") === nextAppId,
    `ios_bundle_id_changed: the GPUI client shipped ${bundleId(legacyIos, "Vibex", "gpui")}, the nsh build declares ${nextAppId}; iOS would install it side by side`
  );
  // The GPUI client had no share extension. The new one's id only has to live
  // under the app's, so both targets re-sign with a single team profile and
  // the extension is replaced with its app.
  const nextExtensionId = bundleId(nextIos, "VibexShare", "nsh");
  assert(
    nextExtensionId.startsWith(`${nextAppId}.`),
    `ios_extension_bundle_id_off: ${nextExtensionId} is not under the app's bundle id ${nextAppId}`
  );
  assert(
    !nextIos.includes(".nsh"),
    "ios_bundle_id_changed: an nsh-suffixed bundle id cannot replace the GPUI app"
  );
  // A managed entitlement cannot be signed with a free Apple ID, which is how
  // this app is installed, and adding one to the upgrade would strand the
  // install path the GPUI client used.
  assert(
    !nextIos.includes("CODE_SIGN_ENTITLEMENTS"),
    "ios_entitlement_added: the install path re-signs with a free Apple ID, which cannot sign managed capabilities"
  );
  // iOS decides whether a bundle may replace the installed one partly on
  // `CFBundleVersion`, and the GPUI client shipped build 1 for every release.
  // The packaging script derives an increasing one from the version.
  const iosHostScript = read(NEXT_IOS_BUILD);
  assert(
    iosHostScript.includes("CURRENT_PROJECT_VERSION=${build_number}") &&
      iosHostScript.includes("MARKETING_VERSION=${VIBEX_MOBILE_VERSION}"),
    "ios_build_number_not_derived: an install would keep the GPUI client's build 1"
  );

  const appJson = JSON.parse(read(NEXT_APP_MANIFEST));
  assert(
    appJson.id === nextIdResolved,
    `nsh_json_id_mismatch: nsh.json names ${appJson.id}, the app is ${nextIdResolved}`
  );

  const migration = read(NEXT_MIGRATION);
  for (const required of [
    "remote-hosts.json",
    "remote-credentials.json",
    "client-identity.json",
    "app-settings.json",
    "composer-state.json",
  ]) {
    assert(
      migration.includes(required),
      `migration_lost_file:${required}: the upgrade would drop what the GPUI client saved`
    );
  }
}

function runSelfTest() {
  const files = new Map(
    [
      LEGACY_ANDROID_BUILD,
      NEXT_ANDROID_BUILD,
      LEGACY_IOS_PROJECT,
      NEXT_IOS_PROJECT,
      NEXT_APP_MANIFEST,
      NEXT_MIGRATION,
      NEXT_IOS_BUILD,
    ].map((path) => [path, source(path)])
  );
  const read = (path) => files.get(path);

  function expectRejected(path, from, to, code) {
    const original = files.get(path);
    assert(original.includes(from), `self-test marker missing in ${path}: ${from}`);
    expectRejectedWith(path, original.replace(from, to), code);
  }

  function expectRejectedWith(path, mutated, code) {
    const original = files.get(path);
    files.set(path, mutated);
    let rejected = false;
    try {
      validateUpgradeContract(read);
    } catch {
      rejected = true;
    } finally {
      files.set(path, original);
    }
    assert(rejected, code);
  }

  expectRejected(
    NEXT_ANDROID_BUILD,
    "legacyApplicationId = 'ai.vibex.mobile'",
    "legacyApplicationId = 'ai.vibex.mobile.nsh'",
    "self_test_accepted_a_changed_android_application_id"
  );
  expectRejected(
    NEXT_ANDROID_BUILD,
    "return major * 10000000 + minor * 100000 + patch * 1000 + preRank",
    "return major * 100000 + minor * 1000 + patch",
    "self_test_accepted_a_changed_version_code_scheme"
  );
  expectRejected(
    NEXT_IOS_PROJECT,
    "PRODUCT_BUNDLE_IDENTIFIER: ai.vibex.mobile\n        PRODUCT_NAME: Vibex",
    "PRODUCT_BUNDLE_IDENTIFIER: ai.vibex.mobile.nsh\n        PRODUCT_NAME: Vibex",
    "self_test_accepted_a_changed_ios_bundle_id"
  );
  // Any entitlement at all breaks the free-account install path, so the
  // checker must reject one being added, not only a stale reference.
  expectRejectedWith(
    NEXT_IOS_PROJECT,
    `${files.get(NEXT_IOS_PROJECT)}\n        CODE_SIGN_ENTITLEMENTS: Vibex.entitlements\n`,
    "self_test_accepted_an_entitlement"
  );
  expectRejected(
    NEXT_APP_MANIFEST,
    '"id": "ai.vibex.mobile"',
    '"id": "ai.vibex.mobile.nsh"',
    "self_test_accepted_a_changed_nsh_json_id"
  );
  expectRejected(
    NEXT_MIGRATION,
    "composer-state.json",
    "composer-state-v2.json",
    "self_test_accepted_a_dropped_legacy_file"
  );
  expectRejected(
    NEXT_IOS_BUILD,
    'CURRENT_PROJECT_VERSION=${build_number}',
    'CURRENT_PROJECT_VERSION=1',
    "self_test_accepted_a_fixed_ios_build_number"
  );
}

try {
  if (SELF_TEST) runSelfTest();
  else validateUpgradeContract();
  console.log(
    SELF_TEST
      ? "Mobile upgrade checker self-test passed"
      : "Mobile upgrade contract verified (application id, versionCode, bundle id, legacy files)"
  );
} catch (error) {
  console.error(error instanceof Error ? error.message : "mobile_upgrade_check_failed");
  process.exitCode = 1;
}

// Guards the one thing a phone user cannot fix: an update that installs
// beside the app already on the phone instead of replacing it, or that
// replaces it but cannot see the data it left.
//
// The GPUI client that used to ship on these phones is gone from the tree, but
// its builds are still installed. This client has to keep accepting them as
// "the same app" forever, so the values that decide that are pinned here
// rather than read from files that no longer exist:
//
// - Android `applicationId` (a different one is a second app, and Android
//   answers with "uninstall the existing app first") and the `versionCode`
//   derivation (an update only installs when the integer is at least the
//   installed one).
// - iOS `CFBundleIdentifier` of the app and of its share extension (iOS
//   installs a different bundle id side by side), and the build number, which
//   iOS also compares.
// - The files the old clients wrote, listed in `src/migration.rs`, which is
//   still what carries the runtimes and drafts over.
//
// The released values these pin, from `0.1.0-rc.10` (versionCode 100510) and
// `0.1.9` (109999):
//
//   major*10000000 + minor*100000 + patch*1000 + rank
//   rank: 999 release, 500+seq rc, 100+seq preview
//
// Run directly or through `pnpm check:mobile-native`.

import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const SELF_TEST = process.argv.includes("--self-test");

const ANDROID_BUILD = "apps/mobile-nsh/android/app/build.gradle";
const IOS_PROJECT = "apps/mobile-nsh/ios/project.yml";
const IOS_BUILD = "apps/mobile-nsh/scripts/build-ios-host.sh";
const APP_MANIFEST = "apps/mobile-nsh/nsh.json";
const MIGRATION = "apps/mobile-nsh/src/migration.rs";
const APP_ROOT = "apps/mobile-nsh/src/lib.rs";

/** The id every released build used, on both platforms. */
const RELEASED_APP_ID = "ai.vibex.mobile";
const RELEASED_EXTENSION_ID = "ai.vibex.mobile.share";

/** Version names this app already shipped and the versionCode of each. */
const RELEASED_VERSION_CODES = new Map([
  ["0.1.0-rc.10", 100510],
  ["0.1.9", 109999]
]);

/** The legacy files the upgrade has to keep reading. */
const LEGACY_FILES = [
  "remote-hosts.json",
  "remote-credentials.json",
  "client-identity.json",
  "app-settings.json",
  "composer-state.json"
];

function read(path) {
  return readFileSync(join(ROOT, path), "utf8");
}

function exists(path) {
  return existsSync(join(ROOT, path));
}

function assert(condition, code) {
  if (!condition) throw new Error(code);
}

/**
 * The `versionCode` derivation of a Gradle file, evaluated in JS.
 *
 * The equations are pinned separately, so this only has to prove that the
 * build file still derives the number rather than hard-coding one.
 */
function gradleVersionCode(text) {
  const match = text.match(
    /def preRank = channel == null \? 999 : \(channel == 'rc' \? 500 : 100\) \+ sequence\s*\n\s*return major \* 10000000 \+ minor \* 100000 \+ patch \* 1000 \+ preRank/u
  );
  return match !== null;
}

/** The same arithmetic, as the checker's own reference implementation. */
export function referenceVersionCode(version) {
  const match = version.match(/^(\d+)\.(\d+)\.(\d+)(?:-(preview|rc)\.(\d+))?$/u);
  if (!match) throw new Error(`cannot derive a versionCode from '${version}'`);
  const [, major, minor, patch, channel, sequence] = match;
  const rank = channel === undefined ? 999 : (channel === "rc" ? 500 : 100) + Number(sequence ?? 0);
  return Number(major) * 10000000 + Number(minor) * 100000 + Number(patch) * 1000 + rank;
}

/** The first `name = 'value'` (or `name 'value'`) at a Gradle file's top level. */
function gradleValue(text, name) {
  return (
    text.match(new RegExp(`^\\s*${name}\\s*=?\\s*['"]([^'"]+)['"]`, "mu"))?.[1] ?? null
  );
}

/**
 * The `applicationId` a Gradle file declares, following a `def` constant.
 *
 * The build names it through a constant so the comment above it can explain
 * why the value may never change; the constant is what has to be checked.
 */
function gradleApplicationId(text) {
  const direct = text.match(/^\s*applicationId\s+['"]([^'"]+)['"]/mu)?.[1];
  if (direct) return direct;
  const reference = text.match(/^\s*applicationId\s+([A-Za-z_][\w.]*)\s*$/mu)?.[1];
  return reference ? gradleValue(text, `def ${reference}`) ?? null : null;
}

/** The block of a named target in an XcodeGen spec (up to the next sibling). */
function targetBlock(text, target) {
  const lines = text.split("\n");
  const start = lines.findIndex((line) => line === `  ${target}:`);
  if (start < 0) return null;
  let end = lines.length;
  for (let index = start + 1; index < lines.length; index += 1) {
    // A sibling target, or the next top-level key (`schemes:`), ends it.
    if (/^ {2}\S/u.test(lines[index]) || /^\S/u.test(lines[index])) {
      end = index;
      break;
    }
  }
  return lines.slice(start, end).join("\n");
}

function bundleId(text, target) {
  const block = targetBlock(text, target);
  if (block === null) return null;
  return block.match(/^\s*PRODUCT_BUNDLE_IDENTIFIER:\s*(\S+)\s*$/mu)?.[1] ?? null;
}

export function validateUpgradeContract(readFile = read, existsFile = exists) {
  const android = readFile(ANDROID_BUILD);
  const applicationId = gradleApplicationId(android);
  assert(
    applicationId === RELEASED_APP_ID,
    `android_application_id_changed: the build declares ${applicationId}, not ${RELEASED_APP_ID}; an installed app would be joined by a second one instead of being updated`
  );
  assert(
    gradleVersionCode(android),
    "android_version_code_scheme_changed: the versionCode is no longer derived with the released arithmetic, so a new build can rank below the one on the phone"
  );
  // The version the build stamps has to be the one the release tag names, or
  // the derived versionCode describes a different release.
  assert(
    /^\s*versionName\s+vibexVersionName\s*$/mu.test(android),
    "android_version_name_not_stamped: the APK would not carry the version its versionCode was derived from"
  );

  const ios = readFile(IOS_PROJECT);
  assert(
    bundleId(ios, "Vibex") === RELEASED_APP_ID,
    `ios_bundle_id_changed: the app is ${bundleId(ios, "Vibex")}, not ${RELEASED_APP_ID}; iOS would install it beside the one on the phone`
  );
  assert(
    bundleId(ios, "VibexShare") === RELEASED_EXTENSION_ID,
    `ios_extension_bundle_id_changed: the share extension is ${bundleId(ios, "VibexShare")}, not ${RELEASED_EXTENSION_ID}`
  );
  // A managed entitlement cannot be signed with a free Apple ID, which is how
  // this app is installed, so asking for one would strand the install path.
  assert(
    !ios.includes("CODE_SIGN_ENTITLEMENTS"),
    "ios_entitlement_added: the install path re-signs with a free Apple ID, which cannot sign managed capabilities"
  );
  for (const entitlements of [
    "apps/mobile-nsh/ios/Vibex.entitlements",
    "apps/mobile-nsh/ios/VibexShare.entitlements"
  ]) {
    assert(
      !existsFile(entitlements),
      `ios_entitlement_file_present:${entitlements}: a managed entitlement breaks free re-signing and the upgrade`
    );
  }
  // iOS compares the build number as well as the version: the released build
  // was always 1, so a fixed literal would make every future install seal the
  // phone to the first one.
  const iosBuild = readFile(IOS_BUILD);
  assert(
    iosBuild.includes("CURRENT_PROJECT_VERSION=${build_number}") &&
      iosBuild.includes("MARKETING_VERSION=${VIBEX_MOBILE_VERSION}"),
    "ios_build_number_not_derived: an install would keep the released build 1 and never move forward"
  );

  const manifest = JSON.parse(readFile(APP_MANIFEST));
  assert(
    manifest.id === RELEASED_APP_ID,
    `nsh_json_id_mismatch: nsh.json names ${manifest.id}, the app is ${RELEASED_APP_ID}`
  );
  // The nsh store lives in `<support>/<app id>`; the id the app mounts has to
  // be the one the packaged manifest names, or the app reads an empty store.
  assert(
    readFile(APP_ROOT).includes(`const APP_ID: &str = "${RELEASED_APP_ID}";`),
    "app_id_not_mounted: the phone root mounts a different app id than nsh.json names"
  );

  const migration = readFile(MIGRATION);
  for (const file of LEGACY_FILES) {
    assert(
      migration.includes(file),
      `migration_lost_file:${file}: the upgrade would drop what the installed app saved`
    );
  }
}

function runSelfTest() {
  // The unmutated tree has to pass first: without this, a checker that rejects
  // everything would still pass every negative case below.
  validateUpgradeContract();

  // The reference arithmetic has to reproduce what was actually published.
  for (const [version, expected] of RELEASED_VERSION_CODES) {
    const actual = referenceVersionCode(version);
    assert(
      actual === expected,
      `self_test_version_code_drift: ${version} derives ${actual}, published as ${expected}`
    );
  }

  const files = new Map(
    [ANDROID_BUILD, IOS_PROJECT, IOS_BUILD, APP_MANIFEST, MIGRATION, APP_ROOT].map((path) => [
      path,
      read(path)
    ])
  );
  const readFile = (path) => files.get(path);
  // The entitlement files being gone is part of the contract, so the stand-in
  // reports them absent and everything else from disk.
  const existsFile = (path) =>
    path.endsWith(".entitlements") ? false : exists(path);

  function expectRejected(path, from, to, code) {
    const original = files.get(path);
    assert(original.includes(from), `self-test marker missing in ${path}: ${from}`);
    files.set(path, original.replace(from, to));
    let rejected = false;
    try {
      validateUpgradeContract(readFile, existsFile);
    } catch {
      rejected = true;
    } finally {
      files.set(path, original);
    }
    assert(rejected, code);
  }

  function expectRejectedWith(path, mutated, code) {
    const original = files.get(path);
    files.set(path, mutated);
    let rejected = false;
    try {
      validateUpgradeContract(readFile, existsFile);
    } catch {
      rejected = true;
    } finally {
      files.set(path, original);
    }
    assert(rejected, code);
  }

  expectRejected(
    ANDROID_BUILD,
    "def releasedApplicationId = 'ai.vibex.mobile'",
    "def releasedApplicationId = 'ai.vibex.mobile.nsh'",
    "self_test_accepted_a_changed_android_application_id"
  );
  expectRejected(
    ANDROID_BUILD,
    "return major * 10000000 + minor * 100000 + patch * 1000 + preRank",
    "return major * 100000 + minor * 1000 + patch",
    "self_test_accepted_a_changed_version_code_scheme"
  );
  expectRejected(
    IOS_PROJECT,
    "PRODUCT_BUNDLE_IDENTIFIER: ai.vibex.mobile\n        PRODUCT_NAME: Vibex",
    "PRODUCT_BUNDLE_IDENTIFIER: ai.vibex.mobile.nsh\n        PRODUCT_NAME: Vibex",
    "self_test_accepted_a_changed_ios_bundle_id"
  );
  expectRejected(
    IOS_PROJECT,
    "PRODUCT_BUNDLE_IDENTIFIER: ai.vibex.mobile.share",
    "PRODUCT_BUNDLE_IDENTIFIER: ai.vibex.mobile.nsh.share",
    "self_test_accepted_a_changed_ios_extension_id"
  );
  // Any entitlement at all breaks the free-account install path, so the
  // checker must reject one being added, not only a stale reference.
  expectRejectedWith(
    IOS_PROJECT,
    `${files.get(IOS_PROJECT)}\n        CODE_SIGN_ENTITLEMENTS: Vibex.entitlements\n`,
    "self_test_accepted_an_entitlement"
  );
  expectRejected(
    IOS_BUILD,
    "CURRENT_PROJECT_VERSION=${build_number}",
    "CURRENT_PROJECT_VERSION=1",
    "self_test_accepted_a_fixed_ios_build_number"
  );
  expectRejected(
    APP_MANIFEST,
    '"id": "ai.vibex.mobile"',
    '"id": "ai.vibex.mobile.nsh"',
    "self_test_accepted_a_changed_nsh_json_id"
  );
  expectRejected(
    APP_ROOT,
    'const APP_ID: &str = "ai.vibex.mobile";',
    'const APP_ID: &str = "ai.vibex.mobile.nsh";',
    "self_test_accepted_a_changed_mounted_app_id"
  );
  expectRejected(
    MIGRATION,
    "composer-state.json",
    "composer-state-v2.json",
    "self_test_accepted_a_dropped_legacy_file"
  );
}

try {
  if (SELF_TEST) runSelfTest();
  else validateUpgradeContract();
  console.log(
    SELF_TEST
      ? "Mobile upgrade checker self-test passed"
      : "Mobile upgrade contract verified (application id, versionCode, bundle ids, build number, legacy files)"
  );
} catch (error) {
  console.error(error instanceof Error ? error.message : "mobile_upgrade_check_failed");
  process.exitCode = 1;
}

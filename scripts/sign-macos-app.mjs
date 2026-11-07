// Ad-hoc sign a macOS app bundle, and verify the signature a dmg ships with.
//
// Apple Silicon runs only signed code, so the linker gives every arm64
// executable an ad-hoc signature of its own. That signature covers the
// executable alone: it binds no Info.plist and seals no resources. Gatekeeper
// reports a downloaded bundle that carries nothing more as "damaged", and that
// verdict offers no way to open the app. A bundle signature seals the bundle,
// which turns the verdict into the ordinary unidentified-developer prompt that
// a user can allow once.
//
// The signature stays ad hoc: no Developer ID, no notarization, no hardened
// runtime, and no secure timestamp. Those arrive together with a Developer ID
// certificate, and the hardened runtime needs a run on a real Mac first.
//
// The release packager runs this as cargo-packager's beforeEachPackageCommand,
// which fires ahead of every format. The bundle exists once the app format has
// run, so it is signed ahead of the dmg format, which is assembled from it.

import { spawnSync } from "node:child_process";
import { closeSync, existsSync, mkdtempSync, openSync, readdirSync, readSync, rmdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, relative, resolve } from "node:path";
import { pathToFileURL } from "node:url";

// The Mach-O magics cargo-packager scans a bundle for: thin 32- and 64-bit
// images in either byte order, and universal binaries.
const MACH_O_MAGICS = new Set([0xfeedface, 0xfeedfacf, 0xcafebabe, 0xcefaedfe, 0xcffaedfe]);
// Nested bundles are signed as bundles, inside out. Vibex ships none, so meeting
// one means this script has to learn how before it signs anything.
const NESTED_BUNDLE_EXTENSIONS = [".app", ".appex", ".bundle", ".framework", ".plugin", ".xpc"];

function fail(message) {
  throw new Error(message);
}

function exitDescription(result) {
  return result.signal ? `signal ${result.signal}` : `status ${result.status}`;
}

function run(command, args) {
  console.log(`> ${command} ${args.join(" ")}`);
  const result = spawnSync(command, args, { stdio: "inherit" });
  if (result.error) fail(`${command} failed to start: ${result.error.message}`);
  if (result.status !== 0) fail(`${command} exited with ${exitDescription(result)}`);
}

function capture(command, args) {
  const result = spawnSync(command, args, { encoding: "utf8" });
  if (result.error) fail(`${command} failed to start: ${result.error.message}`);
  const output = `${result.stdout ?? ""}${result.stderr ?? ""}`;
  if (result.status !== 0) {
    fail(`${command} ${args.join(" ")} exited with ${exitDescription(result)}:\n${output}`);
  }
  return output;
}

function infoPlistValue(app, key) {
  const plist = join(app, "Contents", "Info.plist");
  return capture("plutil", ["-extract", key, "raw", "-o", "-", plist]).trim();
}

function isMachO(path) {
  const descriptor = openSync(path, "r");
  try {
    const header = Buffer.alloc(4);
    return readSync(descriptor, header, 0, 4, 0) === 4 && MACH_O_MAGICS.has(header.readUInt32BE(0));
  } finally {
    closeSync(descriptor);
  }
}

function nestedCode(app, directory, mainExecutable) {
  const found = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      if (NESTED_BUNDLE_EXTENSIONS.some((extension) => entry.name.endsWith(extension))) {
        fail(`${relative(app, path)} is a nested bundle, which this script cannot sign`);
      }
      found.push(...nestedCode(app, path, mainExecutable));
    } else if (entry.isFile() && path !== mainExecutable && isMachO(path)) {
      found.push(path);
    }
  }
  return found;
}

export function signMacosApp(appPath) {
  const app = resolve(appPath);
  const identifier = infoPlistValue(app, "CFBundleIdentifier");
  const mainExecutable = join(app, "Contents", "MacOS", infoPlistValue(app, "CFBundleExecutable"));
  if (!existsSync(mainExecutable)) fail(`${app} has no main executable at ${mainExecutable}`);
  // codesign refuses a bundle whose files carry extended attributes such as
  // Finder information.
  run("xattr", ["-cr", app]);
  // Inside out: the bundle signature records the signatures of its nested code.
  for (const path of nestedCode(app, join(app, "Contents"), mainExecutable)) {
    run("codesign", ["--force", "--sign", "-", "--identifier", `${identifier}.${basename(path)}`, path]);
  }
  run("codesign", ["--force", "--sign", "-", app]);
  verifyMacosApp(app);
}

export function verifyMacosApp(appPath) {
  const app = resolve(appPath);
  const identifier = infoPlistValue(app, "CFBundleIdentifier");
  capture("codesign", ["--verify", "--deep", "--strict", "--verbose=2", app]);
  // The linker's signature fails each of these: it names the executable rather
  // than the bundle, binds no Info.plist, and seals no resources.
  const details = capture("codesign", ["--display", "--verbose=4", app]).split("\n");
  if (!details.includes(`Identifier=${identifier}`)) {
    fail(`${app} is not signed as ${identifier}:\n${details.join("\n")}`);
  }
  if (!details.some((line) => /^Info\.plist entries=\d+/.test(line))) {
    fail(`${app} signature does not bind its Info.plist:\n${details.join("\n")}`);
  }
  if (!details.some((line) => /^Sealed Resources version=\d+/.test(line))) {
    fail(`${app} signature does not seal its resources:\n${details.join("\n")}`);
  }
  console.log(`${app}: the signature seals the bundle as ${identifier}`);
}

export function verifyMacosDmg(dmgPath, bundleName) {
  const mountpoint = mkdtempSync(join(tmpdir(), "vibex-dmg-"));
  run("hdiutil", ["attach", "-nobrowse", "-noautoopen", "-readonly", "-mountpoint", mountpoint, dmgPath]);
  try {
    verifyMacosApp(join(mountpoint, bundleName));
  } finally {
    const detached = spawnSync("hdiutil", ["detach", mountpoint], { stdio: "inherit" });
    if (detached.status !== 0) spawnSync("hdiutil", ["detach", "-force", mountpoint], { stdio: "inherit" });
    try {
      rmdirSync(mountpoint);
    } catch {
      // A volume that would not detach keeps its mountpoint; the runner is
      // discarded after the job either way.
    }
  }
}

function main() {
  const values = process.argv.slice(2);
  if (values.length !== 1) fail("usage: sign-macos-app.mjs <path/to/Name.app>");
  const format = process.env.CARGO_PACKAGER_FORMAT;
  if (format && format !== "dmg") {
    console.log(`sign-macos-app: the bundle is signed ahead of the dmg format, not ${format}`);
    return;
  }
  signMacosApp(values[0]);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    main();
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}

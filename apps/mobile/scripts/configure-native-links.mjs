import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const ROOT = resolve(fileURLToPath(new URL("../../..", import.meta.url)));
const APP = join(ROOT, "apps/mobile");
const ANDROID_MANIFEST = join(APP, "android/app/src/main/AndroidManifest.xml");
const ANDROID_VARIABLES = join(APP, "android/variables.gradle");
const ANDROID_JAVA = join(APP, "android/app/src/main/java");
const ANDROID_BUILD_GRADLE = join(APP, "android/app/build.gradle");
const ANDROID_STRINGS = join(APP, "android/app/src/main/res/values/strings.xml");
const IOS_PLIST = join(APP, "ios/App/App/Info.plist");
const IOS_ENTITLEMENTS = join(APP, "ios/App/App/App.entitlements");
const IOS_PROJECT = join(APP, "ios/App/App.xcodeproj/project.pbxproj");
const NATIVE_SHELL_CONTRACT = join(APP, "native-shell-contract.json");
const MARKER = "VIBEX_PAIRING_LINKS_V1";
const nativeShellContract = JSON.parse(readFileSync(NATIVE_SHELL_CONTRACT, "utf8"));
const applicationId = nativeShellContract.applicationId;
const applicationName = nativeShellContract.applicationName;
const customSchemes = nativeShellContract.deepLink?.customSchemes ?? [];
const androidMinSdk = nativeShellContract.platform?.android?.minSdk;
const androidPermissions = nativeShellContract.platform?.android?.permissions ?? [];
const iosUsageDescriptions = nativeShellContract.platform?.ios?.usageDescriptions ?? {};

function fail(message) {
  throw new Error(message);
}

function xmlEscape(value) {
  return value.replace(/[&<>"']/g, (character) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&apos;"
  })[character]);
}

function optionalUniversalHost() {
  const value = process.env.VIBEX_APP_LINK_HOST?.trim() ?? "";
  if (!value) return null;
  if (!/^[A-Za-z0-9.-]{1,253}$/.test(value) || value.startsWith(".") || value.endsWith(".")) {
    fail("VIBEX_APP_LINK_HOST must be a plain DNS host name");
  }
  return value;
}

function findFiles(root, name) {
  if (!existsSync(root)) return [];
  const matches = [];
  for (const entry of readdirSync(root)) {
    const path = join(root, entry);
    if (statSync(path).isDirectory()) matches.push(...findFiles(path, name));
    else if (entry === name) matches.push(path);
  }
  return matches;
}

function matchingXmlElementEnd(source, start, tag) {
  const tokens = new RegExp(`<\\/?${tag}(?:\\s[^>]*)?>`, "g");
  tokens.lastIndex = start;
  let depth = 0;
  for (let match = tokens.exec(source); match; match = tokens.exec(source)) {
    depth += match[0].startsWith("</") ? -1 : 1;
    if (depth === 0) return tokens.lastIndex;
  }
  return -1;
}

function configureAndroidApplicationId() {
  if (!existsSync(ANDROID_JAVA)) return;
  if (typeof applicationId !== "string" || !/^[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)+$/.test(applicationId)) {
    fail("native shell applicationId is not a valid Java package");
  }
  if (typeof applicationName !== "string" || !applicationName.trim() || applicationName.length > 80) {
    fail("native shell applicationName is invalid");
  }
  const target = join(ANDROID_JAVA, ...applicationId.split("."), "MainActivity.java");
  const existing = findFiles(ANDROID_JAVA, "MainActivity.java");
  if (!existsSync(target)) {
    if (existing.length !== 1) {
      fail(`expected one generated Android MainActivity, found ${existing.length}`);
    }
    mkdirSync(dirname(target), { recursive: true });
    renameSync(existing[0], target);
  }
  for (const stale of existing) {
    if (stale !== target && existsSync(stale)) rmSync(stale, { force: true });
  }
  const source = readFileSync(target, "utf8");
  if (!/^package\s+[A-Za-z0-9_.]+;/m.test(source)) {
    fail("Android MainActivity package declaration is missing");
  }
  writeFileSync(target, source.replace(/^package\s+[A-Za-z0-9_.]+;/m, `package ${applicationId};`));

  if (!existsSync(ANDROID_BUILD_GRADLE)) fail("Android app/build.gradle was not generated");
  let buildGradle = readFileSync(ANDROID_BUILD_GRADLE, "utf8");
  if (!/namespace\s*=\s*["'][^"']+["']/.test(buildGradle) || !/applicationId\s+["'][^"']+["']/.test(buildGradle)) {
    fail("Android Gradle application identity was not found");
  }
  buildGradle = buildGradle
    .replace(/namespace\s*=\s*["'][^"']+["']/, `namespace = "${applicationId}"`)
    .replace(/applicationId\s+["'][^"']+["']/, `applicationId "${applicationId}"`);
  writeFileSync(ANDROID_BUILD_GRADLE, buildGradle);

  if (!existsSync(ANDROID_STRINGS)) fail("Android string resources were not generated");
  let strings = readFileSync(ANDROID_STRINGS, "utf8");
  const resources = {
    app_name: applicationName,
    title_activity_main: applicationName,
    package_name: applicationId,
    custom_url_scheme: applicationId
  };
  for (const [name, value] of Object.entries(resources)) {
    const pattern = new RegExp(`(<string name=["']${name}["']>)[^<]*(</string>)`);
    if (!pattern.test(strings)) fail(`Android string resource ${name} was not found`);
    strings = strings.replace(pattern, `$1${xmlEscape(value)}$2`);
  }
  writeFileSync(ANDROID_STRINGS, strings);
}

function configureAndroid(host) {
  if (!existsSync(ANDROID_MANIFEST)) return false;
  configureAndroidApplicationId();
  let manifest = readFileSync(ANDROID_MANIFEST, "utf8");
  for (const permission of androidPermissions) {
    if (!manifest.includes(`android:name="${permission}"`)) {
      const application = manifest.indexOf("    <application");
      if (application < 0) fail("Android application element was not found");
      manifest = `${manifest.slice(0, application)}    <uses-permission android:name="${xmlEscape(permission)}" />\n${manifest.slice(application)}`;
    }
  }
  const data = customSchemes
    .map((scheme) => `                <data android:scheme="${scheme}" android:host="open" />`)
    .join("\n");
  const https = host
    ? `\n                <data android:scheme="https" android:host="${xmlEscape(host)}" android:pathPrefix="/open" />`
    : "";
  const filter = `
            <!-- ${MARKER} -->
            <intent-filter android:autoVerify="${host ? "true" : "false"}">
                <action android:name="android.intent.action.VIEW" />
                <category android:name="android.intent.category.DEFAULT" />
                <category android:name="android.intent.category.BROWSABLE" />
${data}${https}
            </intent-filter>
`;
  const marker = manifest.indexOf(`<!-- ${MARKER} -->`);
  if (marker >= 0) {
    const start = manifest.lastIndexOf("\n", marker) + 1;
    const closing = manifest.indexOf("</intent-filter>", marker);
    if (closing < 0) fail("Android pairing intent filter is malformed");
    const end = closing + "</intent-filter>".length;
    manifest = `${manifest.slice(0, start)}${filter.slice(1)}${manifest.slice(end)}`;
  } else {
    const closing = manifest.indexOf("        </activity>");
    if (closing < 0) fail("Android MainActivity was not found while configuring pairing links");
    manifest = `${manifest.slice(0, closing)}${filter}${manifest.slice(closing)}`;
  }
  writeFileSync(ANDROID_MANIFEST, manifest);
  if (!existsSync(ANDROID_VARIABLES)) fail("Android variables.gradle was not generated");
  let variables = readFileSync(ANDROID_VARIABLES, "utf8");
  if (!/minSdkVersion\s*=\s*\d+/.test(variables)) {
    fail("Android minSdkVersion was not found");
  }
  if (!Number.isInteger(androidMinSdk)) fail("Android minSdk is missing from the native shell contract");
  variables = variables.replace(/minSdkVersion\s*=\s*\d+/, `minSdkVersion = ${androidMinSdk}`);
  writeFileSync(ANDROID_VARIABLES, variables);
  return true;
}

function configureIos(host) {
  if (!existsSync(IOS_PLIST)) return false;
  let plist = readFileSync(IOS_PLIST, "utf8");
  const displayName = /(<key>CFBundleDisplayName<\/key>\s*<string>)[^<]*(<\/string>)/;
  if (!displayName.test(plist)) fail("iOS CFBundleDisplayName was not found");
  plist = plist.replace(displayName, `$1${xmlEscape(applicationName)}$2`);
  for (const [key, value] of Object.entries(iosUsageDescriptions)) {
    if (!plist.includes(`<key>${key}</key>`)) {
      const closing = plist.lastIndexOf("</dict>");
      if (closing < 0) fail("iOS Info.plist has no root dict");
      const entry = `    <key>${xmlEscape(key)}</key>\n    <string>${xmlEscape(value)}</string>\n`;
      plist = `${plist.slice(0, closing)}${entry}${plist.slice(closing)}`;
    }
  }
  const schemes = customSchemes
    .map((scheme) => `            <dict>\n                <key>CFBundleURLSchemes</key>\n                <array><string>${xmlEscape(scheme)}</string></array>\n            </dict>`)
    .join("\n");
  const urls = `
    <!-- ${MARKER} -->
    <key>CFBundleURLTypes</key>
    <array>
${schemes}
    </array>
`;
  const marker = plist.indexOf(`<!-- ${MARKER} -->`);
  if (marker >= 0) {
    const start = plist.lastIndexOf("\n", marker) + 1;
    const array = plist.indexOf("<array>", marker);
    if (array < 0) fail("iOS pairing URL types are malformed");
    const end = matchingXmlElementEnd(plist, array, "array");
    if (end < 0) fail("iOS pairing URL types are malformed");
    plist = `${plist.slice(0, start)}${urls.slice(1)}${plist.slice(end)}`;
  } else {
    const closing = plist.lastIndexOf("</dict>");
    if (closing < 0) fail("iOS Info.plist has no root dict");
    plist = `${plist.slice(0, closing)}${urls}${plist.slice(closing)}`;
  }
  writeFileSync(IOS_PLIST, plist);
  if (!existsSync(IOS_PROJECT)) fail("iOS Xcode project was not generated");
  let project = readFileSync(IOS_PROJECT, "utf8");
  if (!/PRODUCT_BUNDLE_IDENTIFIER\s*=\s*[^;]+;/.test(project)) {
    fail("iOS PRODUCT_BUNDLE_IDENTIFIER was not found");
  }
  project = project.replace(
    /PRODUCT_BUNDLE_IDENTIFIER\s*=\s*[^;]+;/g,
    `PRODUCT_BUNDLE_IDENTIFIER = ${applicationId};`
  );
  writeFileSync(IOS_PROJECT, project);
  if (host && existsSync(join(APP, "ios/App/App.xcodeproj"))) {
    const domains = `<array><string>applinks:${xmlEscape(host)}</string></array>`;
    if (!existsSync(IOS_ENTITLEMENTS)) {
      writeFileSync(
        IOS_ENTITLEMENTS,
        `<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0"><dict><key>com.apple.developer.associated-domains</key>${domains}</dict></plist>\n`
      );
    }
  }
  return true;
}

const host = optionalUniversalHost();
const android = configureAndroid(host);
const ios = configureIos(host);
console.log(JSON.stringify({
  schemaVersion: "vibex-native-links.v1",
  applicationId,
  applicationName,
  customSchemes,
  universalLinkHost: host,
  androidConfigured: android,
  iosConfigured: ios,
  androidMinSdk,
  iosUsageDescriptions,
  fragmentRoute: "#/pair/"
}));

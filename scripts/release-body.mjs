#!/usr/bin/env node
// Compose the GitHub Release body for a Vibex tag.
//
// One release page carries three parts, in this order:
//
//   1. the tagged bilingual notes, one level-two section per language;
//   2. a Downloads table rendered from the assets this run is about to upload;
//   3. the contributor credit and the comparison link that close the page.
//
// English is the release's primary language. The notes carry one section per
// language, while the Downloads table and the closing sections are English only
// and follow both note sections, so a reader picks a language once and the
// shared release data stays in one place.
//
// The page deliberately carries no per-change list: a range of several hundred
// commits makes one, and the bilingual notes above already say what changed in
// the terms a reader cares about. Only the two facts a change list adds survive
// here — who contributed for the first time, and where the full comparison is.
//
// The contributor credit comes from the notes GitHub generates for the release
// range, which names each new contributor's first pull request. A range built
// from direct commits generates no entries, so the composer falls back to the
// commit log and credits the author of a first commit in the range.
//
// Usage:
//   node scripts/release-body.mjs \
//     --version 0.1.0-rc.10 --tag v0.1.0-rc.10 \
//     --notes docs/operations/release-notes-v0.1.0-rc.10.md \
//     --assets target/publish \
//     --changelog target/release-body/generated-changelog.md \
//     --previous-tag v0.1.0-rc.9 \
//     --output target/release-body/vibex-release-body.md
//
// Every path is resolved against the repository root. `--output` is optional
// and prints the body instead of writing it; `--assets` and `--changelog` are
// optional so a local draft can be composed before any build exists.

import { execFileSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  statSync,
  writeFileSync
} from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DEFAULT_REPOSITORY = "vibex-ai/vibex";
const VERSION_PATTERN = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/u;
const REPOSITORY_PATTERN = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/u;

// Desktop platforms in the order the release page lists them. Each row pairs
// the desktop package with the headless server tarball for the same operating
// system and architecture, because both are downloaded from the same release.
const DESKTOP_PLATFORMS = [
  { os: "windows", arch: "x86_64", label: "Windows x64" },
  { os: "windows", arch: "aarch64", label: "Windows ARM64" },
  { os: "macos", arch: "aarch64", label: "macOS Apple Silicon" },
  { os: "macos", arch: "x86_64", label: "macOS Intel" },
  { os: "linux", arch: "x86_64", label: "Linux x64" },
  { os: "linux", arch: "aarch64", label: "Linux ARM64" }
];

const MOBILE_PLATFORMS = ["Android arm64", "iOS"];

const USAGE = `usage: node scripts/release-body.mjs --version <version> --notes <file> [options]

  --version <version>       Release version, without the leading v
  --tag <tag>               Release tag (default: v<version>)
  --repository <owner/name> GitHub repository (default: $GITHUB_REPOSITORY or ${DEFAULT_REPOSITORY})
  --notes <file>            Tagged release notes document with one section per language
  --assets <directory>      Published asset directory used to render the Downloads table
  --changelog <file>        GitHub generated release notes; its contributor credit is reused
  --previous-tag <tag>      Tag the page compares against (default: git describe of the tag)
  --output <file>           Write the body here instead of printing it
`;

function fail(message) {
  throw new Error(message);
}

function git(argumentsList, cwd = ROOT) {
  return execFileSync("git", argumentsList, {
    cwd,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"]
  }).trim();
}

function parseArguments(argv) {
  const values = {};
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    if (flag === "--help" || flag === "-h") return { help: true };
    const key = {
      "--version": "version",
      "--tag": "tag",
      "--repository": "repository",
      "--notes": "notes",
      "--assets": "assets",
      "--changelog": "changelog",
      "--previous-tag": "previousTag",
      "--output": "output"
    }[flag];
    if (!key) fail(`unknown argument ${flag}`);
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) fail(`${flag} requires a value`);
    index += 1;
    values[key] = value;
  }
  if (!values.version) fail("--version is required");
  if (!values.notes) fail("--notes is required");
  return values;
}

// Level-two headings and their bodies, ignoring headings inside fenced code
// blocks. The notes document uses one section per language, and a diff block
// can otherwise look like a heading in the middle of a migration snippet.
export function parseNotesSections(document) {
  const lines = document.replace(/\r\n?/gu, "\n").split("\n");
  const sections = [];
  let current = null;
  let fence = null;
  for (const line of lines) {
    const marker = /^\s*(`{3,}|~{3,})/u.exec(line);
    if (marker) {
      if (fence === null) fence = marker[1][0];
      else if (fence === marker[1][0]) fence = null;
    }
    if (fence === null) {
      const heading = /^##[ \t]+(.+?)[ \t]*$/u.exec(line);
      if (heading) {
        if (current) sections.push(current);
        current = { heading: heading[1], lines: [] };
        continue;
      }
    }
    if (current) current.lines.push(line);
  }
  if (current) sections.push(current);
  return sections.map((section) => ({
    heading: section.heading,
    body: trimSection(section.lines.join("\n"))
  }));
}

// Drop the horizontal rules a document uses to separate its sections, and the
// trailing blank lines that would otherwise stack before the next separator.
function trimSection(content) {
  const lines = content.replace(/\r\n?/gu, "\n").split("\n");
  while (lines.length > 0 && (lines[0].trim() === "" || /^(?:---|\*\*\*|___)$/u.test(lines[0].trim()))) {
    lines.shift();
  }
  while (lines.length > 0) {
    const last = lines[lines.length - 1].trim();
    if (last !== "" && !/^(?:---|\*\*\*|___)$/u.test(last)) break;
    lines.pop();
  }
  return lines.join("\n").trimEnd();
}

function findSection(sections, headings) {
  for (const heading of headings) {
    const section = sections.find(
      (candidate) => candidate.heading.toLocaleLowerCase("en-US") === heading.toLocaleLowerCase("en-US") && candidate.body !== ""
    );
    if (section) return section;
  }
  return null;
}

// Mirror the resolution the desktop About page uses: English is required, and
// the first Chinese heading a document carries wins, most specific first.
function pickNotesSections(document) {
  const sections = parseNotesSections(document);
  return {
    english: findSection(sections, ["English"]),
    chinese: findSection(sections, ["简体中文", "簡體中文", "中文", "繁體中文", "繁体中文"])
  };
}

function desktopArtifacts(version, { os, arch }) {
  if (os === "windows") {
    return [{ name: `vibex-${version}-windows-${arch}-nsis.exe`, label: "Installer" }];
  }
  if (os === "macos") {
    return [{ name: `vibex-${version}-macos-${arch}-app.dmg`, label: "Disk image" }];
  }
  return [
    { name: `vibex-${version}-linux-${arch}-appimage.AppImage`, label: "AppImage" },
    { name: `vibex-${version}-linux-${arch}-deb.deb`, label: "deb" }
  ];
}

function mobileArtifacts(version) {
  return {
    "Android arm64": [
      { name: `vibex-${version}-android-arm64.apk`, label: "APK" },
      { name: `vibex-${version}-android-arm64.aab`, label: "AAB" }
    ],
    iOS: [
      { name: `vibex-${version}-ios-unsigned.ipa`, label: "Unsigned IPA" },
      { name: `vibex-${version}-ios-simulator.app.zip`, label: "Simulator app" },
      { name: `vibex-${version}-ios-xcframework.zip`, label: "XCFramework" }
    ]
  };
}

// Render the Downloads table from the exact file names the release publishes.
// A platform whose job produced no artifact keeps its row only when the other
// column has something to offer, and an absent artifact reads as a dash rather
// than a link that 404s.
export function renderDownloads({ version, repository, tag, assetNames }) {
  const names = new Set(assetNames);
  const link = (name) => `https://github.com/${repository}/releases/download/${tag}/${name}`;
  const rows = [];
  for (const platform of DESKTOP_PLATFORMS) {
    const desktop = desktopArtifacts(version, platform).filter((artifact) => names.has(artifact.name));
    const serverName = `vibex-server-${version}-${platform.os}-${platform.arch}.tar.gz`;
    const hasServer = names.has(serverName);
    if (desktop.length === 0 && !hasServer) continue;
    rows.push({
      platform: platform.label,
      desktop: desktop.length > 0 ? desktop.map((artifact) => `[${artifact.label}](${link(artifact.name)})`).join(" · ") : "—",
      server: hasServer ? `[Server tarball](${link(serverName)})` : "—"
    });
  }

  const mobile = mobileArtifacts(version);
  const mobileRows = [];
  for (const platform of MOBILE_PLATFORMS) {
    const artifacts = mobile[platform].filter((artifact) => names.has(artifact.name));
    if (artifacts.length === 0) continue;
    mobileRows.push(`| ${platform} | ${artifacts.map((artifact) => `[${artifact.label}](${link(artifact.name)})`).join(" · ")} |`);
  }

  if (rows.length === 0 && mobileRows.length === 0) return "";

  const lines = ["## Downloads", ""];
  if (rows.length > 0) {
    lines.push("| Platform | Desktop client | Server package |", "| --- | --- | --- |");
    for (const row of rows) lines.push(`| ${row.platform} | ${row.desktop} | ${row.server} |`);
    lines.push("");
  }
  if (mobileRows.length > 0) {
    lines.push("### Mobile", "", "| Platform | Package |", "| --- | --- |", ...mobileRows, "");
  }
  if (names.has("SHA256SUMS")) {
    lines.push(`Every artifact publishes a SHA-256 sidecar; [SHA256SUMS](${link("SHA256SUMS")}) covers the whole release.`);
  }
  return lines.join("\n").trimEnd();
}

// Credits and the comparison link: the part of a generated changelog the notes
// cannot state themselves. `contributors` holds ready-to-render list lines, so
// a GitHub-generated credit keeps the pull request it points at.
export function renderClosing({ repository, from, to, contributors = [], fullChangelog = "" }) {
  const lines = [];
  if (contributors.length > 0) {
    lines.push("## New Contributors", "", ...contributors, "");
  }
  const comparison =
    fullChangelog.trim() ||
    (from
      ? `**Full Changelog**: https://github.com/${repository}/compare/${from}...${to}`
      : `**Full Changelog**: https://github.com/${repository}/releases/tag/${to}`);
  lines.push(comparison);
  return lines.join("\n").trimEnd();
}

// The two parts of GitHub's generated notes this page keeps. Its "What's
// Changed" list is dropped on purpose: several hundred commits per release make
// it longer than everything a reader came for, and the notes already describe
// the release in the terms a reader cares about.
export function parseGeneratedNotes(body) {
  const lines = body.replace(/\r\n?/gu, "\n").split("\n");
  const contributors = [];
  let inContributors = false;
  for (const line of lines) {
    if (/^##[ \t]+New Contributors[ \t]*$/u.test(line)) {
      inContributors = true;
      continue;
    }
    if (!inContributors) continue;
    if (/^##[ \t]/u.test(line)) break;
    if (/^\*[ \t]+\S/u.test(line)) contributors.push(line.trimEnd());
  }
  const comparison = lines.find((line) => /^\*\*Full Changelog\*\*:/u.test(line.trim()));
  return { contributors, fullChangelog: comparison?.trim() ?? "" };
}

// Authors that never appear before the previous tag contributed for the first
// time. Email is the identity compared here, because a contributor keeps it
// across author-name changes and every commit records one.
function readGitRange({ from, to }) {
  const range = from ? `${from}..${to}` : to;
  // A tag that has not been created yet — a local draft before the tag push —
  // leaves no range to read. The page then carries the comparison link alone
  // rather than failing the release over a courtesy credit.
  let raw = "";
  let history = "";
  try {
    raw = git(["log", "--no-merges", "--format=%an%x00%ae", range]);
    history = from ? git(["log", "--format=%ae", from]) : "";
  } catch {
    return [];
  }
  const commits = raw
    .split("\n")
    .filter((line) => line.length > 0)
    .map((line) => {
      const [name, email] = line.split("\u0000");
      return { name, email };
    })
    .reverse();

  const known = new Set();
  for (const email of history.split("\n")) {
    if (email !== "") known.add(email);
  }
  const seen = new Set();
  const newContributors = [];
  for (const commit of commits) {
    if (known.has(commit.email) || seen.has(commit.email)) continue;
    seen.add(commit.email);
    newContributors.push(`* ${commit.name} made their first contribution`);
  }
  return newContributors;
}

function previousTagFor(to) {
  try {
    return git(["describe", "--tags", "--abbrev=0", `${to}^`]);
  } catch {
    return "";
  }
}

function revisionFor(tag) {
  try {
    git(["rev-parse", "--verify", "--quiet", `${tag}^{commit}`]);
    return tag;
  } catch {
    return "HEAD";
  }
}

export function buildReleaseBody({
  version,
  repository,
  tag,
  notesDocument,
  assetNames = [],
  changelog = "",
  from = ""
}) {
  const notes = pickNotesSections(notesDocument);
  if (!notes.english) {
    fail("release notes carry no English section; add a level-two `## English` heading");
  }
  const parts = [`## English\n\n${notes.english.body}`];
  if (notes.chinese) parts.push(`## ${notes.chinese.heading}\n\n${notes.chinese.body}`);
  const downloads = renderDownloads({ version, repository, tag, assetNames });
  if (downloads !== "") parts.push(downloads);

  const generated = parseGeneratedNotes(changelog);
  const contributors =
    generated.contributors.length > 0 ? generated.contributors : from ? readGitRange({ from, to: tag }) : [];
  const closing = renderClosing({
    repository,
    from,
    to: tag,
    contributors,
    fullChangelog: generated.fullChangelog
  });
  if (closing !== "") parts.push(closing);
  return `${parts.join("\n\n---\n\n")}\n`;
}

function readAssetNames(directory) {
  if (!existsSync(directory)) fail(`asset directory does not exist: ${directory}`);
  return readdirSync(directory).filter((name) => {
    const path = resolve(directory, name);
    return statSync(path).isFile();
  });
}

function compose(options) {
  const version = options.version;
  if (!VERSION_PATTERN.test(version)) fail(`--version is not a release version: ${version}`);
  const tag = options.tag ?? `v${version}`;
  const repository = options.repository ?? process.env.GITHUB_REPOSITORY ?? DEFAULT_REPOSITORY;
  if (!REPOSITORY_PATTERN.test(repository)) fail(`--repository must be owner/name: ${repository}`);

  const notesPath = resolve(ROOT, options.notes);
  if (!existsSync(notesPath)) fail(`release notes do not exist: ${options.notes}`);
  const notesDocument = readFileSync(notesPath, "utf8");
  const assetNames = options.assets ? readAssetNames(resolve(ROOT, options.assets)) : [];

  let changelog = "";
  if (options.changelog && existsSync(resolve(ROOT, options.changelog))) {
    changelog = readFileSync(resolve(ROOT, options.changelog), "utf8");
  }
  const to = revisionFor(tag);
  const from = options.previousTag ?? previousTagFor(to);

  return buildReleaseBody({ version, repository, tag, notesDocument, assetNames, changelog, from });
}

function main(argv) {
  const options = parseArguments(argv);
  if (options.help) {
    console.log(USAGE);
    return;
  }
  const body = compose(options);
  if (!options.output) {
    process.stdout.write(body);
    return;
  }
  const output = resolve(ROOT, options.output);
  mkdirSync(dirname(output), { recursive: true });
  writeFileSync(output, body);
  console.log(`release body written to ${options.output}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}

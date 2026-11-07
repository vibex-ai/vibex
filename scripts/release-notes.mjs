#!/usr/bin/env node
// Prepare the bilingual release notes document a tag publishes.
//
// The published notes are authored prose, not a commit dump: they explain what
// a reader can now do. Everything a script can derive is gathered here instead,
// so writing a release starts from the real range rather than a blank page.
//
// Modes:
//   --context <file>   Write the release brief: range, grouped commits, changed
//                      areas, the previous release's structure, and the open
//                      pull requests that landed in the range.
//   --scaffold         Create docs/operations/release-notes-v<version>.md from
//                      the brief. Never overwrites an existing document, and
//                      every machine-written line is an HTML comment so an
//                      unfinished draft cannot reach a release page.
//   --check            Validate the document for <version>: header, both
//                      language sections, no empty or placeholder section.
//
// Usage:
//   node scripts/release-notes.mjs --version 0.2.0 --context target/release-notes/context.md
//   node scripts/release-notes.mjs --version 0.2.0 --scaffold
//   node scripts/release-notes.mjs --version 0.2.0 --check

import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const VERSION_PATTERN = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/u;
// The desktop resolves these headings; English is required and the Chinese
// section is what a Chinese interface shows instead of falling back to English.
const LANGUAGE_SECTIONS = ["English", "中文"];
const USAGE = `usage: node scripts/release-notes.mjs --version <version> [options]

  --version <version>   Release version, without the leading v
  --previous-tag <tag>  Tag the range starts after (default: git describe)
  --context <file>      Write the release brief instead of printing it
  --scaffold            Create the notes document for this version
  --check               Validate the notes document for this version
`;

function fail(message) {
  throw new Error(message);
}

function git(argumentsList, fallback) {
  try {
    return execFileSync("git", argumentsList, {
      cwd: ROOT,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
      stdio: ["ignore", "pipe", "pipe"]
    }).trim();
  } catch {
    if (fallback !== undefined) return fallback;
    throw new Error(`git ${argumentsList.join(" ")} failed`);
  }
}

function parseArguments(argv) {
  const values = { mode: "context" };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    if (flag === "--help" || flag === "-h") return { help: true };
    if (flag === "--scaffold" || flag === "--check") {
      values.mode = flag.slice(2);
      continue;
    }
    const key = {
      "--version": "version",
      "--previous-tag": "previousTag",
      "--context": "context"
    }[flag];
    if (!key) fail(`unknown argument ${flag}`);
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) fail(`${flag} requires a value`);
    index += 1;
    values[key] = value;
  }
  if (!values.version) fail("--version is required");
  if (!VERSION_PATTERN.test(values.version)) fail(`--version is not a release version: ${values.version}`);
  if (values.context) values.mode = "context";
  return values;
}

function notesPath(version) {
  return resolve(ROOT, "docs", "operations", `release-notes-v${version}.md`);
}

function previousTag(version) {
  const tag = `v${version}`;
  const exists = git(["rev-parse", "--verify", "--quiet", `${tag}^{commit}`], "");
  if (exists) return git(["describe", "--tags", "--abbrev=0", `${tag}^`], "");
  return git(["describe", "--tags", "--abbrev=0"], "");
}

// Conventional commit subjects carry the area a change belongs to in their
// scope, which is what a release brief groups by.
function classify(subject) {
  const match = /^([a-z]+)(?:\(([^)]*)\))?(!)?:[ \t]+(.+)$/u.exec(subject.trim());
  if (!match) return { type: "other", scope: "", breaking: false, summary: subject.trim() };
  const [, type, scope = "", bang, summary] = match;
  return { type, scope, breaking: bang === "!", summary };
}

function readCommits(from, to) {
  const range = from ? `${from}..${to}` : to;
  const raw = git(["log", "--no-merges", "--format=%H%x00%an%x00%ae%x00%s", range], "");
  return raw
    .split("\n")
    .filter((line) => line.length > 0)
    .map((line) => {
      const [sha, name, email, subject] = line.split("\u0000");
      return { sha, name, email, subject, ...classify(subject) };
    })
    .reverse();
}

function changedAreas(from, to) {
  const range = from ? `${from}..${to}` : to;
  const counts = new Map();
  for (const path of git(["diff", "--name-only", range], "").split("\n")) {
    if (path === "") continue;
    const parts = path.split("/");
    const area = parts.length === 1 ? "(root)" : parts.slice(0, 2).join("/");
    counts.set(area, (counts.get(area) ?? 0) + 1);
  }
  return [...counts.entries()].sort((left, right) => right[1] - left[1]);
}

function previousHeadings(tag) {
  if (!tag) return [];
  const document = git(["show", `${tag}:docs/operations/release-notes-v${tag.replace(/^v/u, "")}.md`], "");
  return document
    .split("\n")
    .filter((line) => /^#{2,3}[ \t]+\S/u.test(line))
    .slice(0, 40);
}

function openPullRequests(from) {
  // Pull requests are optional context: a repository that merges directly has
  // none, and a run without a token cannot list them.
  const since = from ? git(["log", "-1", "--format=%cI", from], "") : "";
  if (since === "") return [];
  try {
    const output = execFileSync(
      "gh",
      [
        "pr",
        "list",
        "--state",
        "merged",
        "--limit",
        "300",
        "--json",
        "number,title,mergedAt,author",
        "--jq",
        `.[] | select(.mergedAt >= "${since}") | "\\(.number)\t\\(.title)\t\\(.author.login)"`
      ],
      { cwd: ROOT, encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], maxBuffer: 16 * 1024 * 1024 }
    ).trim();
    return output === "" ? [] : output.split("\n");
  } catch {
    return [];
  }
}

export function renderContext({ version, tag, from, commits, areas, headings, pullRequests }) {
  const lines = [
    `# Release brief — Vibex ${tag}`,
    "",
    `- Range: \`${from || "(start of history)"}...${tag}\``,
    `- Commits: ${commits.length}`,
    `- Merged pull requests in range: ${pullRequests.length}`,
    ""
  ];

  const breaking = commits.filter((commit) => commit.breaking);
  if (breaking.length > 0) {
    lines.push("## Breaking changes", "");
    for (const commit of breaking) lines.push(`- ${commit.subject} (${commit.sha.slice(0, 7)})`);
    lines.push("");
  }

  const byType = new Map();
  for (const commit of commits) {
    if (commit.type === "chore" && /^chore\(release\)/u.test(commit.subject)) continue;
    const list = byType.get(commit.type) ?? [];
    list.push(commit);
    byType.set(commit.type, list);
  }
  for (const type of ["feat", "fix", "perf", "refactor", "docs", "test", "build", "ci", "chore", "other"]) {
    const list = byType.get(type);
    if (!list || list.length === 0) continue;
    lines.push(`## ${type} (${list.length})`, "");
    const byScope = new Map();
    for (const commit of list) {
      const key = commit.scope || "(none)";
      const entries = byScope.get(key) ?? [];
      entries.push(commit);
      byScope.set(key, entries);
    }
    for (const [scope, entries] of byScope) {
      lines.push(`**${scope}**`, "");
      for (const commit of entries) lines.push(`- ${commit.summary} — \`${commit.sha.slice(0, 7)}\``);
      lines.push("");
    }
  }

  if (pullRequests.length > 0) {
    lines.push("## Merged pull requests", "", ...pullRequests.map((line) => `- ${line}`), "");
  }

  lines.push("## Changed areas", "", ...areas.map(([area, count]) => `- \`${area}\` — ${count} files`), "");

  if (headings.length > 0) {
    lines.push(`## Structure of the previous release notes (${from || "none"})`, "", ...headings.map((line) => `- \`${line}\``), "");
  }

  return lines.join("\n").trimEnd();
}

// A draft that still carries only comments is not a release note; the composer
// strips them, so the page would render empty. --check rejects that.
const PLACEHOLDER = /<!--[\s\S]*?-->/gu;

export function checkNotes(document) {
  const problems = [];
  const heading = /^#[ \t]+Vibex[ \t]+v?\S+/mu.test(document);
  if (!heading) problems.push("the document does not open with a `# Vibex v<version> Release Notes` heading");

  const sections = new Map();
  for (const match of document.matchAll(/^##[ \t]+(.+?)[ \t]*$/gmu)) {
    const start = match.index + match[0].length;
    const rest = document.slice(start);
    const next = /^##[ \t]/mu.exec(rest);
    sections.set(match[1].toLocaleLowerCase("en-US"), (next ? rest.slice(0, next.index) : rest).replace(PLACEHOLDER, "").trim());
  }
  for (const language of LANGUAGE_SECTIONS) {
    const body = sections.get(language.toLocaleLowerCase("en-US"));
    if (body === undefined) problems.push(`the document has no \`## ${language}\` section`);
    else if (body === "") problems.push(`the \`## ${language}\` section is empty or only carries comments`);
  }
  return problems;
}

function scaffold(version, brief) {
  const path = notesPath(version);
  if (existsSync(path)) fail(`${path} already exists; edit it instead of regenerating it`);
  // The brief stays out of the document: it is working material with commit
  // subjects and internal paths, and an HTML comment would smuggle it into the
  // published page. It is written beside the notes under `target/` instead.
  const contextPath = resolve(ROOT, "target", "release-notes", `context-v${version}.md`);
  mkdirSync(dirname(contextPath), { recursive: true });
  writeFileSync(contextPath, `${brief}\n`);

  const document = [
    `# Vibex v${version} Release Notes`,
    "",
    `- Released: ${new Date().toISOString().slice(0, 10)}`,
    "",
    "---",
    "",
    "## English",
    "",
    "<!--",
    "Write the release for a reader deciding whether to upgrade. Group by product",
    "area, name the API a change adds, and cite the pull request when there is one.",
    `The brief for this range is in target/release-notes/context-v${version}.md.`,
    "-->",
    "",
    "## 中文",
    "",
    "<!-- 中文段落与英文段落一一对应，标题与分组保持一致。 -->",
    ""
  ].join("\n");
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, document);
  return { path, contextPath };
}

function main(argv) {
  const options = parseArguments(argv);
  if (options.help) {
    console.log(USAGE);
    return;
  }
  const version = options.version;
  const tag = `v${version}`;
  const from = options.previousTag ?? previousTag(version);
  const commits = readCommits(from, tag);
  const brief = renderContext({
    version,
    tag,
    from,
    commits,
    areas: changedAreas(from, tag),
    headings: previousHeadings(from),
    pullRequests: openPullRequests(from)
  });

  if (options.mode === "check") {
    const path = notesPath(version);
    if (!existsSync(path)) fail(`release notes are missing: docs/operations/release-notes-v${version}.md`);
    const problems = checkNotes(readFileSync(path, "utf8"));
    if (problems.length > 0) fail(`release notes for ${version} are not publishable:\n- ${problems.join("\n- ")}`);
    console.log(`release notes for ${version} are publishable`);
    return;
  }

  if (options.mode === "scaffold") {
    const { path, contextPath } = scaffold(version, brief);
    console.log(`release notes scaffold written to ${path}`);
    console.log(`release brief written to ${contextPath}`);
    return;
  }

  if (options.context) {
    const path = resolve(ROOT, options.context);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, `${brief}\n`);
    console.log(`release brief written to ${options.context}`);
    return;
  }
  process.stdout.write(`${brief}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}

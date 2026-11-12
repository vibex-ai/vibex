#!/usr/bin/env node
// Resolve every author in a release range to the GitHub account that owns the
// commit, so the release page can name each contributor with an @ mention.
//
// `git log` knows the author email but not the account behind it, and GitHub's
// generated release notes only name the authors of merged pull requests. A
// range built from direct commits therefore produces no credit at all. The
// commit API returns `author.login` for the commit, and one lookup per distinct
// author email is enough: a release has far fewer authors than commits.
//
// The output is an `email<TAB>login` map consumed by release-body.mjs. An
// author the API cannot attribute stays out of the map — the composer still
// credits them by name, but only a resolved login renders an avatar.
//
// Usage:
//   node scripts/release-contributors.mjs \
//     --version 0.1.4 --previous-tag v0.1.3 \
//     --output target/release-body/contributors.txt
//
// Every path is resolved against the repository root. `--output` is optional
// and prints the map instead of writing it; a missing `--previous-tag` resolves
// the same way release-body.mjs resolves it, and a tag that does not exist yet
// falls back to HEAD so a local draft can be composed before the tag is pushed.

import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DEFAULT_REPOSITORY = "vibex-ai/vibex";
const VERSION_PATTERN = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/u;
const REPOSITORY_PATTERN = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/u;
// GitHub logins are alphanumeric with single hyphens and no leading or trailing
// hyphen; bots carry a `[bot]` suffix and are not people to credit.
const LOGIN_PATTERN = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?$/u;

const USAGE = `usage: node scripts/release-contributors.mjs (--version <version> | --tag <tag>) [options]

  --version <version>       Release version, without the leading v
  --tag <tag>               Release tag (default: v<version>)
  --repository <owner/name> GitHub repository (default: $GITHUB_REPOSITORY or ${DEFAULT_REPOSITORY})
  --previous-tag <tag>      Tag the range starts after (default: git describe of the tag)
  --output <file>           Write the email-to-login map here instead of printing it
`;

function fail(message) {
  throw new Error(message);
}

function git(argumentsList) {
  return execFileSync("git", argumentsList, {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    stdio: ["ignore", "pipe", "ignore"]
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
      "--previous-tag": "previousTag",
      "--output": "output"
    }[flag];
    if (!key) fail(`unknown argument ${flag}`);
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) fail(`${flag} requires a value`);
    index += 1;
    values[key] = value;
  }
  if (!values.version && !values.tag) fail("--version or --tag is required");
  return values;
}

function previousTagFor(tag) {
  try {
    return git(["describe", "--tags", "--abbrev=0", `${tag}^`]);
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

// `git log` lists newest first and repeats an author once per commit. Keep the
// first commit for each email: the API needs one commit to attribute the
// account, and the composer restores first-appearance order from the range.
export function uniqueAuthors(output) {
  const byEmail = new Map();
  for (const line of output.replace(/\r\n?/gu, "\n").split("\n")) {
    if (line === "") continue;
    const [email, sha] = line.split("\t");
    if (!email || !sha) continue;
    const key = email.toLocaleLowerCase("en-US");
    if (!byEmail.has(key)) byEmail.set(key, { email, sha });
  }
  return [...byEmail.values()].sort((left, right) => left.email.localeCompare(right.email));
}

function lookupLogin(repository, sha) {
  return execFileSync(
    "gh",
    ["api", `repos/${repository}/commits/${sha}`, "--jq", ".author.login // empty"],
    {
      cwd: ROOT,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
      maxBuffer: 1024 * 1024
    }
  ).trim();
}

// One API call per author, and a failure only costs that author their mention:
// the release keeps going, and the composer falls back to the author name.
export function collectContributors({ repository, authors, lookup = lookupLogin, warn = console.error }) {
  const rows = [];
  for (const author of authors) {
    let login = "";
    try {
      login = lookup(repository, author.sha).trim();
    } catch (error) {
      const message = error instanceof Error ? error.message.split("\n")[0] : String(error);
      warn(`could not resolve ${author.email}: ${message}`);
      continue;
    }
    if (login === "") {
      warn(`no GitHub account owns ${author.email}`);
      continue;
    }
    if (!LOGIN_PATTERN.test(login)) {
      warn(`ignoring non-person login ${login} for ${author.email}`);
      continue;
    }
    rows.push(`${author.email}\t${login}`);
  }
  return rows;
}

function compose(options) {
  const version = options.version;
  if (version && !VERSION_PATTERN.test(version)) fail(`--version is not a release version: ${version}`);
  const tag = options.tag ?? `v${version}`;
  const repository = options.repository ?? process.env.GITHUB_REPOSITORY ?? DEFAULT_REPOSITORY;
  if (!REPOSITORY_PATTERN.test(repository)) fail(`--repository must be owner/name: ${repository}`);

  const to = revisionFor(tag);
  const from = options.previousTag ?? previousTagFor(to);
  const range = from ? `${from}..${to}` : to;
  let raw = "";
  try {
    raw = git(["log", "--no-merges", "--format=%ae%x09%H", range]);
  } catch {
    fail(`cannot read the release range ${range}`);
  }
  const authors = uniqueAuthors(raw);
  const rows = collectContributors({ repository, authors });
  console.error(`${rows.length} of ${authors.length} authors resolved to a GitHub login`);
  return rows.length > 0 ? `${rows.join("\n")}\n` : "";
}

function main(argv) {
  const options = parseArguments(argv);
  if (options.help) {
    console.log(USAGE);
    return;
  }
  const map = compose(options);
  if (!options.output) {
    process.stdout.write(map);
    return;
  }
  const output = resolve(ROOT, options.output);
  mkdirSync(dirname(output), { recursive: true });
  writeFileSync(output, map);
  console.log(`contributor map written to ${options.output}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}

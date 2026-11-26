#!/usr/bin/env node
// Pin the official Nix flake to one stable release and publish the bump.
//
// The flake installs the Linux `.deb` the tagged GitHub Release already
// publishes, so this script never builds Vibex. It records the version and the
// SHA-256 of both architecture payloads in `flake.nix`, commits the result on
// the default branch, and lets the workflow prove the package builds from
// those exact bytes before anything is pushed.
//
// Stable only: NixOS users pin the flake commit and read the version from it,
// so a prerelease would publish a package that claims to be a release while
// the release page still calls it a candidate.

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DEFAULT_FLAKE = "flake.nix";
const ARCHITECTURES = {
  "x86_64-linux": "x86_64",
  "aarch64-linux": "aarch64"
};
const REPOSITORY = "vibex-ai/vibex";
const DEFAULT_BRANCH = "main";
// The bot identity that authors the bump and also has to be available to a
// rebase, which runs `git commit` internally without the env the commit below
// passes.
const BOT_NAME = "github-actions[bot]";
const BOT_EMAIL = "41898282+github-actions[bot]@users.noreply.github.com";

function identity() {
  return {
    name: process.env.NIX_GIT_NAME || BOT_NAME,
    email: process.env.NIX_GIT_EMAIL || BOT_EMAIL
  };
}

// `git commit` needs an author and committer; a CI runner has neither. Passing
// them as `-c` flags covers the commands that create objects internally
// (rebase), while the commit below also passes them through the environment.
function gitIdentityArgs() {
  const { name, email } = identity();
  return ["-c", `user.name=${name}`, "-c", `user.email=${email}`];
}

function fail(message) {
  throw new Error(message);
}

function parseArguments(argv) {
  const values = {
    version: "",
    archiveDir: "",
    flake: DEFAULT_FLAKE,
    repository: REPOSITORY,
    branch: DEFAULT_BRANCH,
    dryRun: false,
    skipPush: false
  };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    switch (flag) {
      case "--version": values.version = argv[++index] ?? ""; break;
      case "--archive-dir": values.archiveDir = argv[++index] ?? ""; break;
      case "--flake": values.flake = argv[++index] ?? ""; break;
      case "--repository": values.repository = argv[++index] ?? ""; break;
      case "--branch": values.branch = argv[++index] ?? ""; break;
      case "--dry-run": values.dryRun = true; break;
      case "--skip-push": values.skipPush = true; break;
      case "--help": values.help = true; break;
      default: fail(`unexpected argument ${flag}`);
    }
  }
  if (!values.help && !values.version) fail("--version is required");
  return values;
}

// SemVer "MAJOR.MINOR.PATCH" with no prerelease or build suffix.
function assertStable(version) {
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/u.test(version)) {
    fail(`refusing to pin the Nix flake to a non-stable version: ${version}`);
  }
}

function sha256Sri(file) {
  return `sha256-${createHash("sha256").update(readFileSync(file)).digest("base64")}`;
}

function releaseAsset(version, arch) {
  return `vibex-${version}-linux-${arch}-deb.deb`;
}

async function download(url, destination) {
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) fail(`cannot download ${url}: ${response.status} ${response.statusText}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  if (bytes.length === 0) fail(`${url} answered with an empty body`);
  writeFileSync(destination, bytes);
  return bytes.length;
}

function run(command, args, options = {}) {
  return execFileSync(command, args, { encoding: "utf8", stdio: "pipe", ...options }).trim();
}

// The flake keeps the version in one `let` binding whose comment names this
// script. Rewriting the first bare `version = "x.y.z";` leaves the rest of the
// declaration untouched, including the comment above it.
function withVersion(source, version) {
  const pattern = /^\s*version = "[^"]*";$/mu;
  if (!pattern.test(source)) fail("flake.nix has no `version = \"...\";` line");
  return source.replace(pattern, (line) => line.replace(/"[^"]*"/u, `"${version}"`));
}

// One `debHashes` entry per platform. The attribute name is matched literally
// and anchored at the start of a line, so a `x86_64-linux` hash can never land
// in the `aarch64-linux` entry, and an architecture missing from the table
// fails instead of silently reusing another payload.
function withHash(source, system, hash) {
  const prefix = `${system} = "`;
  const lines = source.split("\n");
  const index = lines.findIndex((line) => line.trimStart().startsWith(prefix));
  if (index < 0) fail(`flake.nix has no ${system} entry in debHashes`);
  const line = lines[index];
  const start = line.indexOf(prefix) + prefix.length;
  const end = line.indexOf('"', start);
  if (end < 0) fail(`the ${system} entry in debHashes is unterminated`);
  lines[index] = `${line.slice(0, start)}${hash}${line.slice(end)}`;
  return lines.join("\n");
}

function pinnedVersion(source) {
  const match = /^\s*version = "([^"]+)";$/mu.exec(source);
  return match ? match[1] : null;
}

// A release can take tens of minutes, and another merge can land on the branch
// while the flake is being verified. The push is therefore retried once on top
// of the newest branch tip instead of failing a job that has already proven the
// package builds. The rebase only runs on a clean worktree: replaying someone
// else's uncommitted work is never the right recovery.
function push(branch, version) {
  const attempt = () => run("git", ["push", "origin", `HEAD:${branch}`], { cwd: ROOT });
  try {
    attempt();
  } catch (error) {
    const subject = error instanceof Error ? error.message : String(error);
    // Only tracked changes block the recovery: a rebase refuses to overwrite
    // local modifications to tracked files, while a stray untracked file (a
    // local `result` symlink, an editor backup) is not something the rebase
    // would touch.
    const status = run("git", ["status", "--porcelain", "--untracked-files=no"], { cwd: ROOT });
    if (status !== "") fail(`push rejected and the worktree is not clean: ${subject}`);
    run("git", ["fetch", "origin", branch], { cwd: ROOT });
    run("git", [...gitIdentityArgs(), "rebase", `origin/${branch}`], { cwd: ROOT });
    attempt();
  }
  console.log(`pushed the ${version} flake bump to ${branch}`);
}

async function main() {
  const args = parseArguments(process.argv.slice(2));
  if (args.help) {
    console.log(`Usage: node scripts/publish-nix.mjs --version <x.y.z> [options]

  --version <x.y.z>     stable release to pin (required)
  --archive-dir <dir>   use local .deb files instead of downloading them
  --flake <path>        flake to rewrite (default: ${DEFAULT_FLAKE})
  --repository <owner/repo>  release source (default: ${REPOSITORY})
  --branch <name>       branch to commit the bump on (default: ${DEFAULT_BRANCH})
  --dry-run             rewrite and verify, print the result, publish nothing
  --skip-push           rewrite and verify without committing or pushing`);
    return;
  }

  const version = args.version;
  assertStable(version);

  const staging = mkdtempSync(join(tmpdir(), "vibex-nix-"));
  const hashes = {};
  try {
    for (const [system, arch] of Object.entries(ARCHITECTURES)) {
      const name = releaseAsset(version, arch);
      const local = args.archiveDir ? join(resolve(args.archiveDir), name) : join(staging, name);
      if (args.archiveDir) {
        if (!existsSync(local)) fail(`missing local archive ${local}`);
      } else {
        const url = `https://github.com/${args.repository}/releases/download/v${version}/${name}`;
        const bytes = await download(url, local);
        console.log(`downloaded ${name} (${bytes} bytes)`);
      }
      hashes[system] = sha256Sri(local);
    }

    const flakePath = resolve(ROOT, args.flake);
    let source = readFileSync(flakePath, "utf8");
    source = withVersion(source, version);
    for (const [system, hash] of Object.entries(hashes)) {
      source = withHash(source, system, hash);
    }
    writeFileSync(flakePath, source);
    console.log(`rewrote ${args.flake} for ${version}`);

    // Read the file back instead of trusting the replacements: a formatting
    // change in the template must fail here rather than publish a flake that
    // still names the previous release.
    const written = readFileSync(flakePath, "utf8");
    if (pinnedVersion(written) !== version) {
      fail(`flake.nix reports ${pinnedVersion(written)} instead of ${version}`);
    }
    for (const [system, hash] of Object.entries(hashes)) {
      if (!written.includes(`${system} = "${hash}";`)) {
        fail(`flake.nix has no ${system} payload hash`);
      }
    }

    if (args.dryRun || args.skipPush) {
      console.log(args.dryRun ? "dry run: not publishing" : "skip-push: not publishing");
      return;
    }

    // One tracked file changes. `git add` names it explicitly so nothing else
    // in the worktree can ride along, exactly like the AUR publish.
    run("git", ["add", args.flake], { cwd: ROOT });
    // A replayed workflow rewrites the same bytes a completed run already
    // committed. `git commit` would then abort with "nothing to commit" and
    // turn a successful replay into a failure, so an empty staged diff means
    // the flake is already pinned and the push has nothing left to do.
    if (run("git", ["diff", "--cached", "--name-only"], { cwd: ROOT }) === "") {
      console.log(`${args.flake} already pins ${version}; nothing to publish`);
      return;
    }
    const { name, email } = identity();
    run("git", ["commit", "-m", `chore(nix): pin the flake to ${version}`], {
      cwd: ROOT,
      env: {
        ...process.env,
        GIT_AUTHOR_NAME: name,
        GIT_AUTHOR_EMAIL: email,
        GIT_COMMITTER_NAME: name,
        GIT_COMMITTER_EMAIL: email
      }
    });
    push(args.branch, version);
  } finally {
    rmSync(staging, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
});

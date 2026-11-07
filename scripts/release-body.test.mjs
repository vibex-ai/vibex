#!/usr/bin/env node
// Self-test for scripts/release-body.mjs.
//
// The release body is assembled from three inputs that CI cannot re-derive once
// the release is public: the tagged notes, the asset list and the changelog.
// These cases pin the parts that would silently ship an unreadable release page
// — language selection, the Downloads table, and the changelog fallback.
//
// Usage: node scripts/release-body.test.mjs

import assert from "node:assert/strict";

import {
  buildReleaseBody,
  parseNotesSections,
  renderCommitChangelog,
  renderDownloads
} from "./release-body.mjs";

const NOTES = [
  "# Vibex v0.2.0 Release Notes",
  "",
  "- Released: 2026-11-07 · Range: `v0.1.0...v0.2.0` · 3 commits",
  "",
  "---",
  "",
  "## English",
  "",
  "### Highlights",
  "",
  "- An English highlight.",
  "",
  "```diff",
  "## 中文",
  "-old",
  "+new",
  "```",
  "",
  "---",
  "",
  "## 中文",
  "",
  "### 亮点",
  "",
  "- 一个中文亮点。",
  ""
].join("\n");

// A fenced `## 中文` line inside a diff must not open a language section, or
// the English body would be cut in the middle of a migration snippet.
const sections = parseNotesSections(NOTES);
assert.deepEqual(
  sections.map((section) => section.heading),
  ["English", "中文"],
  "a heading inside a fenced block opened a section"
);
assert.ok(sections[0].body.includes("## 中文"), "the fenced heading was dropped from the body");
assert.ok(!sections[0].body.endsWith("---"), "the section separator leaked into a language body");

const body = buildReleaseBody({
  version: "0.2.0",
  repository: "vibex-ai/vibex",
  tag: "v0.2.0",
  notesDocument: NOTES,
  assetNames: [],
  changelog: "## What's Changed\n\n* a change by @someone in https://example.invalid/pull/1\n"
});
assert.ok(body.startsWith("## English\n"), "the body must open with the English section");
assert.ok(body.indexOf("## English") < body.indexOf("## 中文"), "Chinese must follow English");
assert.ok(
  body.indexOf("## 中文") < body.indexOf("## What's Changed"),
  "the changelog must follow both language sections"
);
assert.ok(body.includes("Thank") === false, "the composer must not invent closing prose");
assert.ok(
  body.trimEnd().endsWith("https://example.invalid/pull/1"),
  "a generated changelog must be carried through verbatim"
);

// The table lists only artifacts this release actually publishes, and pairs a
// desktop package with the server tarball for the same OS and architecture.
const downloads = renderDownloads({
  version: "0.2.0",
  repository: "vibex-ai/vibex",
  tag: "v0.2.0",
  assetNames: [
    "vibex-0.2.0-linux-x86_64-appimage.AppImage",
    "vibex-0.2.0-linux-x86_64-deb.deb",
    "vibex-0.2.0-macos-aarch64-app.dmg",
    "vibex-server-0.2.0-linux-x86_64.tar.gz",
    "SHA256SUMS"
  ]
});
assert.ok(downloads.startsWith("## Downloads\n"), "the downloads section lost its heading");
assert.ok(
  downloads.includes(
    "| Linux x64 | [AppImage](https://github.com/vibex-ai/vibex/releases/download/v0.2.0/vibex-0.2.0-linux-x86_64-appimage.AppImage) · [deb](https://github.com/vibex-ai/vibex/releases/download/v0.2.0/vibex-0.2.0-linux-x86_64-deb.deb) | [Server tarball](https://github.com/vibex-ai/vibex/releases/download/v0.2.0/vibex-server-0.2.0-linux-x86_64.tar.gz)"
  ),
  "the Linux row does not pair the desktop package with its server tarball"
);
assert.ok(downloads.includes("| macOS Apple Silicon |"), "the macOS row is missing");
assert.ok(!downloads.includes("Windows x64"), "an absent platform still claimed a row");
assert.ok(!downloads.includes("Linux ARM64"), "an absent architecture still claimed a row");
// A platform with no server tarball keeps a dash instead of a dead link.
assert.ok(
  downloads.includes(
    "| macOS Apple Silicon | [Disk image](https://github.com/vibex-ai/vibex/releases/download/v0.2.0/vibex-0.2.0-macos-aarch64-app.dmg) | — |"
  ),
  "a missing server tarball must render as a dash"
);
assert.ok(downloads.includes("[SHA256SUMS]"), "the aggregate checksum file was not linked");

// A range with no merged pull requests has no generated entries; the composer
// falls back to the commit log and keeps the headings a reader expects.
const fallback = renderCommitChangelog({
  repository: "vibex-ai/vibex",
  from: "v0.1.0",
  to: "v0.2.0",
  commits: [
    { sha: "a".repeat(40), subject: "feat(tui): add a button legend" },
    { sha: "b".repeat(40), subject: "fix(desktop): keep the caret visible" },
    { sha: "c".repeat(40), subject: "chore(release): prepare the v0.2.0 release" },
    { sha: "e".repeat(40), subject: "chore: update 0.2.0" },
    { sha: "d".repeat(40), subject: "update" }
  ],
  newContributors: ["Ada"]
});
assert.ok(fallback.startsWith("## What's Changed\n"), "the fallback lost the changelog heading");
assert.ok(fallback.includes("### Features"), "the fallback lost its feature group");
assert.ok(fallback.includes("### Fixes"), "the fallback lost its fix group");
assert.ok(fallback.includes("### Other changes"), "an unconventioned subject belongs in other changes");
assert.ok(!fallback.includes("prepare the v0.2.0 release"), "a release preparation commit reached the changelog");
assert.ok(!fallback.includes("chore: update 0.2.0"), "a bare version bump reached the changelog");
assert.ok(fallback.includes("* Ada made their first contribution"), "new contributors were dropped");
assert.ok(
  fallback.trimEnd().endsWith(
    "**Full Changelog**: https://github.com/vibex-ai/vibex/compare/v0.1.0...v0.2.0"
  ),
  "the fallback lost its comparison link"
);

// A body without an English section is unreleasable: the About page resolves
// English first and would otherwise render an empty changelog.
assert.throws(
  () =>
    buildReleaseBody({
      version: "0.2.0",
      repository: "vibex-ai/vibex",
      tag: "v0.2.0",
      notesDocument: "## 中文\n\n- 没有英文。\n",
      assetNames: [],
      changelog: ""
    }),
  /English/u,
  "a notes document without English was accepted"
);

console.log("release-body self-test passed");

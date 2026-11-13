#!/usr/bin/env node
// Self-test for scripts/release-body.mjs.
//
// The release body is assembled from inputs CI cannot re-derive once the release
// is public: the tagged notes, the asset list and the generated changelog.
// These cases pin the parts that would silently ship an unreadable release page
// — language selection, the Downloads table, and the contributor credits.
//
// Usage: node scripts/release-body.test.mjs

import assert from "node:assert/strict";

import {
  buildReleaseBody,
  parseContributorMap,
  parseGeneratedNotes,
  parseNotesSections,
  renderClosing,
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

// The page keeps the contributor names, the new-contributor credit and the
// comparison link from the generated notes; the per-change list is dropped on
// purpose.
const generated = [
  "## What's Changed",
  "* feat(desktop): a change by @ada in https://github.com/vibex-ai/vibex/pull/1",
  "* fix(tui): another change by @grace in https://github.com/vibex-ai/vibex/pull/2",
  "",
  "## New Contributors",
  "* @ada made their first contribution in https://github.com/vibex-ai/vibex/pull/1",
  "",
  "**Full Changelog**: https://github.com/vibex-ai/vibex/compare/v0.1.0...v0.2.0",
  ""
].join("\n");
const parsed = parseGeneratedNotes(generated);
assert.deepEqual(
  parsed.newContributors,
  ["* @ada made their first contribution in https://github.com/vibex-ai/vibex/pull/1"],
  "the contributor credit was not lifted from the generated notes"
);
assert.deepEqual(
  parsed.authors,
  ["ada", "grace"],
  "the pull request authors were not collected from the generated notes"
);
assert.equal(
  parsed.fullChangelog,
  "**Full Changelog**: https://github.com/vibex-ai/vibex/compare/v0.1.0...v0.2.0",
  "the comparison link was not lifted from the generated notes"
);

const body = buildReleaseBody({
  version: "0.2.0",
  repository: "vibex-ai/vibex",
  tag: "v0.2.0",
  notesDocument: NOTES,
  assetNames: [],
  changelog: generated
});
assert.ok(body.startsWith("## English\n"), "the body must open with the English section");
assert.ok(body.indexOf("## English") < body.indexOf("## 中文"), "Chinese must follow English");
assert.ok(
  body.indexOf("## 中文") < body.indexOf("## Contributors"),
  "the contributor list must follow both language sections"
);
assert.ok(
  body.includes("## Contributors\n\n* @ada\n* @grace\n"),
  "the contributor list must name every pull request author with an @ mention"
);
assert.ok(
  body.indexOf("## Contributors") < body.indexOf("## New Contributors"),
  "the full contributor list must precede the new-contributor credit"
);
assert.ok(
  !body.includes("## What's Changed"),
  "the per-change list must not reach the release page"
);
assert.ok(
  !body.includes("feat(desktop): a change"),
  "a generated change entry must not reach the release page"
);
assert.ok(body.includes("* @ada made their first contribution"), "the contributor credit was dropped");
assert.ok(
  body.trimEnd().endsWith(
    "**Full Changelog**: https://github.com/vibex-ai/vibex/compare/v0.1.0...v0.2.0"
  ),
  "the page must close with the comparison link"
);
assert.ok(body.includes("Thank") === false, "the composer must not invent closing prose");

// A release with no new contributor keeps the comparison link and drops the
// empty heading, so the page never ends on a section with nothing under it.
const noCredit = renderClosing({
  repository: "vibex-ai/vibex",
  from: "v0.1.0",
  to: "v0.2.0",
  newContributors: []
});
assert.ok(!noCredit.includes("New Contributors"), "an empty new-contributor section was rendered");
assert.ok(noCredit.startsWith("**Full Changelog**"), "the comparison link must still close the page");
assert.ok(
  renderClosing({ repository: "vibex-ai/vibex", from: "", to: "v0.0.1" }).includes("/releases/tag/v0.0.1"),
  "the first release needs a comparison link that points at its own tag"
);

// Every release names all of its authors, even when nobody is new: that list
// is what gives the release page's Contributors block every avatar.
const veterans = renderClosing({
  repository: "vibex-ai/vibex",
  from: "v0.1.0",
  to: "v0.2.0",
  contributors: ["* @ada", "* @grace"]
});
assert.ok(veterans.startsWith("## Contributors\n\n* @ada\n* @grace\n"), "the contributor list is malformed");
assert.ok(!veterans.includes("New Contributors"), "a release with no new contributor rendered the heading");
assert.ok(veterans.trimEnd().endsWith("/compare/v0.1.0...v0.2.0"), "the comparison link was dropped");

// The publish job writes an email-to-login map; only a valid person login is
// accepted, and a bot or malformed line is ignored instead of rendering a
// broken @ mention.
const map = parseContributorMap(
  [
    "# resolved by scripts/release-contributors.mjs",
    "peatboy@outlook.com\tPeatBoy",
    "0xbet4@gmail.com\t@bet4it",
    "",
    "bot@example.com\tgithub-actions[bot]",
    "broken@example.com",
    "\tnobody"
  ].join("\n")
);
assert.equal(map.get("peatboy@outlook.com"), "PeatBoy", "a resolved login was dropped");
assert.equal(map.get("0xbet4@gmail.com"), "bet4it", "a leading @ was not stripped");
assert.equal(map.size, 2, "a bot or malformed line entered the contributor map");
const botNotes = parseGeneratedNotes(
  [
    "## What's Changed",
    "* chore(deps): bump a dependency by @dependabot[bot] in https://github.com/vibex-ai/vibex/pull/9",
    "* fix: a change by @ada in https://github.com/vibex-ai/vibex/pull/10"
  ].join("\n")
);
assert.deepEqual(botNotes.authors, ["ada"], "a bot mention entered the contributor list");

// The table lists only artifacts this release actually publishes, and pairs a
// desktop package with the server tarball for the same OS and architecture.
const downloads = renderDownloads({
  version: "0.2.0",
  repository: "vibex-ai/vibex",
  tag: "v0.2.0",
  assetNames: [
    "vibex-0.2.0-linux-x86_64-appimage.AppImage",
    "vibex-0.2.0-linux-x86_64-deb.deb",
    "vibex-0.2.0-linux-x86_64-rpm.rpm",
    "vibex-0.2.0-macos-aarch64-app.dmg",
    "vibex-server-0.2.0-linux-x86_64.tar.gz",
    "SHA256SUMS"
  ]
});
assert.ok(downloads.startsWith("## Downloads\n"), "the downloads section lost its heading");
assert.ok(
  downloads.includes(
    "| Linux x64 | [AppImage](https://github.com/vibex-ai/vibex/releases/download/v0.2.0/vibex-0.2.0-linux-x86_64-appimage.AppImage) · [deb](https://github.com/vibex-ai/vibex/releases/download/v0.2.0/vibex-0.2.0-linux-x86_64-deb.deb) · [RPM](https://github.com/vibex-ai/vibex/releases/download/v0.2.0/vibex-0.2.0-linux-x86_64-rpm.rpm) | [Server tarball](https://github.com/vibex-ai/vibex/releases/download/v0.2.0/vibex-server-0.2.0-linux-x86_64.tar.gz)"
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

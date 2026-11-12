#!/usr/bin/env node
// Self-test for scripts/release-contributors.mjs.
//
// The map this script writes decides which authors the release page can mention
// by `@`; a bot or a malformed login would render a broken credit, and a
// repeated email would cost an extra API call. These cases pin both rules.
//
// Usage: node scripts/release-contributors.test.mjs

import assert from "node:assert/strict";

import { collectContributors, uniqueAuthors } from "./release-contributors.mjs";

// One commit per email is enough to resolve the account, and the range order is
// restored later by the composer. Newest-first output must keep the newest
// commit for each address.
const authors = uniqueAuthors(
  [
    "peatboy@outlook.com\tc3",
    "bet4@example.com\tc2",
    "peatboy@outlook.com\tc1",
    "Bet4@example.com\tc4"
  ].join("\n")
);
assert.deepEqual(
  authors,
  [
    { email: "bet4@example.com", sha: "c2" },
    { email: "peatboy@outlook.com", sha: "c3" }
  ],
  "repeated emails were not collapsed to one commit"
);

// A lookup failure or an unattributable commit only costs that author their
// mention; the rest of the map still ships. Bots never enter the map.
const logins = new Map([
  ["c1", "PeatBoy"],
  ["c2", "bet4"],
  ["c3", "github-actions[bot]"],
  ["c4", ""]
]);
const warned = [];
const rows = collectContributors({
  repository: "vibex-ai/vibex",
  authors: [
    { email: "peatboy@outlook.com", sha: "c1" },
    { email: "bet4@example.com", sha: "c2" },
    { email: "github-actions[bot]@users.noreply.github.com", sha: "c3" },
    { email: "missing@example.com", sha: "c4" },
    { email: "broken@example.com", sha: "c5" }
  ],
  lookup: (_repository, sha) => {
    if (sha === "c5") throw new Error("commit is not on GitHub");
    return logins.get(sha);
  },
  warn: (message) => warned.push(message)
});
assert.deepEqual(
  rows,
  ["peatboy@outlook.com\tPeatBoy", "bet4@example.com\tbet4"],
  "the email-to-login map does not name exactly the resolved people"
);
assert.equal(warned.length, 3, "a lookup failure, a bot and an unattributed commit must each warn");
assert.ok(warned.some((message) => message.includes("github-actions[bot]")), "the bot was not reported");
assert.ok(warned.some((message) => message.includes("missing@example.com")), "the unattributed author was not reported");
assert.ok(warned.some((message) => message.includes("broken@example.com")), "the failed lookup was not reported");

console.log("release-contributors self-test passed");

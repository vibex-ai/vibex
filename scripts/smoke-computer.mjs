#!/usr/bin/env node
// Contract smoke for computer use.
//
// The desktop probe already prints the computer-use contract; this script is the
// gate that decides whether it passes. It exists so the contract is checked on
// a build machine with no desktop, no engine and no OS permission — exactly the
// machine where a silent degradation would otherwise go unnoticed.

const chunks = [];
for await (const chunk of process.stdin) chunks.push(chunk);
const raw = Buffer.concat(chunks).toString("utf8");

let probe;
try {
  probe = JSON.parse(raw);
} catch (error) {
  console.error("the desktop probe did not print JSON:", error.message);
  process.exit(1);
}

const contract = probe.computerUseContract;
if (!contract) {
  console.error("the desktop probe did not report computerUseContract");
  process.exit(1);
}

const failures = [];
const expect = (field, value) => {
  if (contract[field] !== value) {
    failures.push(`${field} = ${JSON.stringify(contract[field])}, expected ${JSON.stringify(value)}`);
  }
};
const expectTrue = (field) => expect(field, true);

// The shape of the surface: eight tools, and the two delivery exceptions the
// product documents.
expect("toolCount", 8);
expect("cliSkillAgentCount", 1);
expect("nativeFeatureAgentCount", 1);

// Every safety property the contract probe can check without a desktop.
for (const field of [
  "unavailableReasonsAreExplicit",
  "remoteTransportReportsAReason",
  "credentialTargetsAreHardDenied",
  "secureFieldsAreHardDenied",
  "destructiveActionsAreNeverRemembered",
  "selfTargetsAreRefused",
  "missingVerificationIsUnverified",
  "staleReferencesAreRefused",
  "screenshotsAreTierGated",
  "toolDescriptionsCarryTheRules",
  "auditRecordsAreRedacted",
  "stopIsTerminal",
  "disconnectHasTwoThresholds",
  "waylandIsGraded",
]) {
  expectTrue(field);
}

if (failures.length > 0) {
  console.error("computer-use contract failed:");
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}
console.log("computer-use contract ok");

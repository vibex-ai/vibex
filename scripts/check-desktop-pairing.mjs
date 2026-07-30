import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function source(path) {
  return readFileSync(join(ROOT, path), "utf8");
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function includesAll(contents, values, label) {
  for (const value of values) {
    assert(contents.includes(value), `${label} is missing ${value}`);
  }
}

function runCargoTest(filter, packageName) {
  const result = spawnSync(
    "cargo",
    ["test", "-p", packageName, filter, "--locked"],
    { cwd: ROOT, encoding: "utf8", stdio: "inherit" }
  );
  assert(result.status === 0, `${packageName} pairing test failed`);
}

const app = source("apps/desktop/src/app.rs");
const management = source("apps/desktop/src/management.rs");
const pairing = source("apps/desktop/src/remote_access_pairing.rs");
const model = source("crates/desktop-model/src/management.rs");
const controller = source("crates/desktop-runtime/src/remote_connectivity.rs");
const gateway = source("crates/remote/src/gateway.rs");
const desktopCargo = source("apps/desktop/Cargo.toml");

includesAll(
  pairing,
  [
    "PAIRING_OFFER_TTL_MS: u32 = 90_000",
    "RemoteConnectivityMethod::TailscaleServe",
    "RemoteConnectivityMethod::Direct",
    "RemoteConnectivityMethod::SelfHostedRelay",
    "RemoteDevicePermissionLevel::ReadOnly",
    "RemoteDevicePermissionLevel::ApproveOnly",
    "RemoteDevicePermissionLevel::FullControl",
    "QrCode::new",
    "fn schedule_offer_poll",
    "fn dismiss",
    "fn copy_pairing_link"
  ],
  "remote pairing UI"
);
includesAll(
  controller,
  [
    "pub fn create_pairing_offer",
    "pub fn pairing_offer_status",
    "pub fn cancel_pairing_offer",
    "pub async fn record_claimed_pairing_entry",
    "claimed_offer_records_only_the_entry_that_completed_pairing"
  ],
  "remote connectivity controller"
);
includesAll(
  gateway,
  ["pub fn pairing_offer_status", "pub fn cancel_pairing_offer"],
  "remote gateway"
);
assert(desktopCargo.includes("qrcode.workspace = true"), "Desktop app is missing the QR dependency");
assert(app.includes("open_remote_access_pairing(runtime, window, cx)"), "title bar does not use the shared pairing dialog");
assert(management.includes("Button::new(\"manage-remote-access\")"), "Management is missing the shared remote access action");
assert(management.includes("open_remote_access_pairing(runtime, window, cx)"), "Management does not delegate to the shared pairing dialog");

for (const [label, contents] of [
  ["app", app],
  ["management", management],
  ["management model", model]
]) {
  assert(!contents.includes("http://127.0.0.1:1421/"), `${label} retains the static Web pairing URL`);
  assert(!contents.includes("?mock=1"), `${label} retains the fixture pairing URL`);
}
assert(!model.includes("pub web_url:"), "management model retains a static Web URL field");
assert(!model.includes("pub fixture_url:"), "management model retains a fixture URL field");
assert(!management.includes("Button::new(\"relay-start\")"), "Management retains a second Relay lifecycle owner");
assert(!management.includes("Button::new(\"relay-stop\")"), "Management retains a second Relay lifecycle owner");
assert(!pairing.includes("impl std::fmt::Debug for PrivateOfferMaterial"), "private offer material exposes Debug");
assert(!pairing.includes("impl fmt::Debug for PrivateOfferMaterial"), "private offer material exposes Debug");

runCargoTest("claimed_offer_records_only_the_entry_that_completed_pairing", "vibex-desktop-runtime");
runCargoTest("remote_access_pairing", "vibex-desktop");

console.log("GPUI Desktop pairing verified: shared methods, claimed-offer preference, redacted secrets, and no fixture fallback");

import { createHash, randomBytes } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { chromium } from "@playwright/test";
import {
  action,
  assert,
  fail,
  prepareWorkflowFixture,
  remoteState,
  runAgentWorkflow,
  runFileWorkflow,
  runProductMatrix,
  runTerminalWorkflow,
  waitForConnectedRuntime,
  waitForRuntime
} from "./e2e-workflows.mjs";
import {
  PRODUCT_PAIRING_CHECKS,
  PRODUCT_PAIRING_EVIDENCE_PATH,
  PRODUCT_PAIRING_PERMISSIONS,
  PRODUCT_PAIRING_WORKFLOWS,
  mergeProductPairingMode,
  resolveProductPairingCandidate,
  sha256Classification
} from "./product-pairing-evidence.mjs";
import { permissionContractSha256 } from "./workflow-e2e-evidence.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const MOBILE_VIEWPORT = { width: 390, height: 844 };
const CONTROL_RESPONSE_LIMIT = 64 * 1024;
const METHOD = Object.freeze({
  tailscale: "tailscale_serve",
  direct: "direct",
  relay: "self_hosted_relay"
});
const MODE_METHODS = Object.freeze({
  tailscale: [METHOD.tailscale],
  direct: [METHOD.direct],
  relay: [METHOD.relay],
  "relay-no-tailscale": [METHOD.relay],
  "direct-relay-fallback": [METHOD.relay, METHOD.direct]
});
const MODE_ENTRY = Object.freeze({
  tailscale: METHOD.tailscale,
  direct: METHOD.direct,
  relay: METHOD.relay,
  "relay-no-tailscale": METHOD.relay,
  "direct-relay-fallback": METHOD.direct
});
const PAIRING_PUBLIC_STATES = new Set([
  "camera_denied",
  "claiming",
  "connecting",
  "credential_corrupt",
  "expired",
  "idle",
  "identity_mismatch",
  "incompatible",
  "invalid",
  "offline",
  "online",
  "persisting",
  "preview",
  "revoked",
  "route_error",
  "scan_canceled",
  "scanner_unavailable",
  "scanning",
  "storage_error",
  "unpaired"
]);

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function passedMap(keys) {
  return Object.fromEntries(keys.map((key) => [key, "passed"]));
}

function unexpectedStageFailureCode(stage, error) {
  if (error?.name === "TimeoutError") return `product_pairing_${stage}_timeout`;
  const message = typeof error?.message === "string" ? error.message : "";
  if (/target page, context or browser has been closed/i.test(message)) {
    return `product_pairing_${stage}_page_closed`;
  }
  if (/execution context was destroyed/i.test(message)) {
    return `product_pairing_${stage}_context_destroyed`;
  }
  return `product_pairing_${stage}_failed`;
}

function sleep(milliseconds) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}

export async function controlJson(environment, path, { method = "GET", body = null } = {}) {
  let response;
  try {
    response = await fetch(`${environment.controlBase}${path}`, {
      method,
      headers: body ? { "content-type": "application/json" } : undefined,
      body: body ? JSON.stringify(body) : undefined,
      cache: "no-store",
      signal: AbortSignal.timeout(95_000)
    });
  } catch {
    fail("product_pairing_control_unavailable");
  }
  const bytes = new Uint8Array(await response.arrayBuffer());
  assert(bytes.byteLength <= CONTROL_RESPONSE_LIMIT, "product_pairing_control_response_unbounded");
  let value = null;
  try {
    value = JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    fail("product_pairing_control_response_invalid");
  }
  if (!response.ok) {
    const code = /^[a-z0-9_]+$/.test(value?.errorCode ?? "")
      ? value.errorCode
      : "product_pairing_control_rejected";
    fail(code);
  }
  return value;
}

function methodSnapshot(snapshot, method) {
  return snapshot.methods?.find((item) => item.method === method) ?? null;
}

function failMethodError(snapshot, method, fallbackCode) {
  if (snapshot.pendingAction === null && /^[a-z0-9_]+$/.test(snapshot.errorCode ?? "")) {
    fail(snapshot.errorCode);
  }
  const state = methodSnapshot(snapshot, method);
  if (
    snapshot.pendingAction !== null ||
    !["conflict", "error", "repair_required"].includes(state?.state)
  ) {
    return;
  }
  const code = /^[a-z0-9_]+$/.test(state.errorCode ?? "") ? state.errorCode : fallbackCode;
  fail(code);
}

function failMethodWait(snapshot, method, fallbackCode) {
  failMethodError(snapshot, method, fallbackCode);
  if (snapshot.pendingAction !== null) fail(`${fallbackCode}_pending_timeout`);
  const state = methodSnapshot(snapshot, method);
  if (!state) fail(`${fallbackCode}_snapshot_missing`);
  if (state.state === "confirmation_needed") fail(`${fallbackCode}_confirmation_unresolved`);
  if (["starting", "stopping"].includes(state.state)) {
    fail(`${fallbackCode}_transition_timeout`);
  }
  if (state.state === "disabled") fail(`${fallbackCode}_disabled`);
  fail(fallbackCode);
}

export async function desktopSnapshot(environment) {
  return controlJson(environment, "/pairing/snapshot");
}

export async function waitDesktop(environment, predicate, code, timeout = 95_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const value = await desktopSnapshot(environment);
    if (predicate(value)) return value;
    await sleep(100);
  }
  fail(code);
}

export async function desktopAction(environment, command) {
  await controlJson(environment, "/pairing/action", { method: "POST", body: command });
  return waitDesktop(
    environment,
    (snapshot) => snapshot.pendingAction === null,
    "product_pairing_desktop_action_timeout"
  );
}

export async function trustSummary(environment) {
  const value = await controlJson(environment, "/trust/summary");
  assert(value.schemaVersion === "remote-access-trust-summary.v1", "trust_summary_invalid");
  assert(Array.isArray(value.activeDeviceIdentitySha256), "trust_summary_invalid");
  return value;
}

export function activeDeviceDelta(before, after) {
  const baseline = new Set(before.activeDeviceIdentitySha256);
  return after.activeDeviceIdentitySha256.filter((value) => !baseline.has(value));
}

export async function revokeExact(environment, deviceIdentitySha256) {
  await controlJson(environment, "/trust/revoke", {
    method: "POST",
    body: { deviceIdentitySha256 }
  });
}

export async function setupFixture(environment) {
  const fixture = await controlJson(environment, "/fixture/setup", { method: "POST" });
  assert(fixture.schemaVersion === "vibex-workflow-fixture.v1", "fixture_setup_invalid");
  return fixture;
}

export async function cleanupFixture(environment) {
  await controlJson(environment, "/fixture/cleanup", { method: "POST" });
}

async function enableMethodAndWait(
  environment,
  method,
  {
    enableCode = "product_pairing_method_enable_failed",
    confirmationCode = "tailscale_confirmation_failed"
  } = {}
) {
  let snapshot = await desktopAction(environment, { kind: "enable_method", method });
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    failMethodError(snapshot, method, enableCode);
    const state = methodSnapshot(snapshot, method);
    if (state?.state === "online") return snapshot;
    if (method === METHOD.tailscale && state?.state === "confirmation_needed") {
      assert(Number.isInteger(snapshot.proposedTailscalePort), "tailscale_confirmation_port_missing");
      snapshot = await desktopAction(environment, {
        kind: "confirm_tailscale_port",
        port: snapshot.proposedTailscalePort
      });
      failMethodError(snapshot, method, confirmationCode);
      continue;
    }
    await sleep(100);
    snapshot = await desktopSnapshot(environment);
  }
  failMethodWait(await desktopSnapshot(environment), method, enableCode);
}

export async function configureMethods(environment) {
  const initial = await desktopSnapshot(environment);
  if (environment.mode === "relay-no-tailscale") {
    const tailscale = methodSnapshot(initial, METHOD.tailscale);
    assert(
      tailscale && tailscale.desiredEnabled === false && tailscale.state === "disabled",
      "relay_no_tailscale_precondition_failed"
    );
  }
  for (const method of MODE_METHODS[environment.mode]) {
    if (method === METHOD.direct || method === METHOD.relay) {
      const origin = method === METHOD.direct ? environment.directOrigin : environment.relayOrigin;
      assert(typeof origin === "string", "product_pairing_origin_missing");
      await desktopAction(environment, { kind: "configure_origin", method, origin });
    }
    const snapshot = await enableMethodAndWait(environment, method);
    assert(methodSnapshot(snapshot, method)?.candidateAvailable, "pairing_candidate_unavailable");
  }
  const configured = await desktopSnapshot(environment);
  if (environment.mode === "relay-no-tailscale") {
    const tailscale = methodSnapshot(configured, METHOD.tailscale);
    assert(
      tailscale && tailscale.desiredEnabled === false && tailscale.state === "disabled",
      "relay_no_tailscale_was_enabled"
    );
  }
  return configured;
}

async function reenableMethods(environment) {
  await configureMethods(environment);
}

export async function createOffer(environment, permission, { useDefault = false } = {}) {
  let snapshot = await desktopSnapshot(environment);
  const hadOffer = snapshot.offerStatus !== "none";
  if (hadOffer) {
    snapshot = await desktopAction(environment, { kind: "regenerate_offer" });
    snapshot = await waitDesktop(
      environment,
      (value) => value.pendingAction === null && value.offerStatus === "active",
      "product_pairing_offer_regeneration_failed"
    );
  }
  if (!useDefault && snapshot.permission !== permission) {
    snapshot = await desktopAction(environment, { kind: "set_permission", permission });
    if (hadOffer) {
      snapshot = await waitDesktop(
        environment,
        (value) =>
          value.pendingAction === null &&
          value.offerStatus === "active" &&
          value.permission === permission,
        "product_pairing_permission_regeneration_failed"
      );
    }
  }
  if (!hadOffer) {
    snapshot = await desktopAction(environment, { kind: "create_offer" });
  }
  snapshot = await waitDesktop(
    environment,
    (value) =>
      value.pendingAction === null && value.offerStatus === "active" && value.hasQr === true,
    "product_pairing_offer_not_ready"
  );
  assert(snapshot.permission === permission, "product_pairing_permission_mismatch");
  const entry = MODE_ENTRY[environment.mode];
  if (snapshot.selectedEntry !== entry) {
    await desktopAction(environment, { kind: "select_entry", method: entry });
  }
  const response = await controlJson(environment, "/pairing/link", { method: "POST" });
  assert(response.schemaVersion === "remote-access-pairing-link.v1", "pairing_link_response_invalid");
  assert(
    typeof response.value === "string" &&
      response.value.length <= 36 * 1024 &&
      response.value.includes("#/pair/"),
    "pairing_link_response_invalid"
  );
  return response.value;
}

function mutateOfferLink(value, mutate) {
  let url;
  try {
    url = new URL(value);
    const marker = "#/pair/";
    assert(url.hash.startsWith(marker), "pairing_link_shape_invalid");
    const offer = JSON.parse(Buffer.from(url.hash.slice(marker.length), "base64url").toString("utf8"));
    mutate(offer);
    url.hash = `${marker}${Buffer.from(JSON.stringify(offer)).toString("base64url")}`;
    return url.toString();
  } catch (error) {
    if (error?.code) throw error;
    fail("pairing_link_mutation_failed");
  }
}

function nativeFixtureScript() {
  const REMOTE_KEY = "vibex.remote-client.credentials.v1";
  const storage = new Map();
  let appUrlListener = null;
  let scannerResult = { kind: "canceled", value: null };
  let failCredentialStorage = false;
  const plugins = {
    SecureStoragePlugin: {
      async get({ key }) {
        return { value: storage.get(key) ?? null };
      },
      async set({ key, value }) {
        if (failCredentialStorage && key === REMOTE_KEY) {
          const error = new Error("secure storage unavailable");
          error.code = "secure_storage_write_failed";
          throw error;
        }
        storage.set(key, value);
      },
      async remove({ key }) {
        storage.delete(key);
      }
    },
    App: {
      async addListener(kind, callback) {
        if (kind === "appUrlOpen") appUrlListener = callback;
        return { remove() {} };
      }
    },
    CapacitorBarcodeScanner: {
      async scanBarcode() {
        const next = scannerResult;
        scannerResult = { kind: "canceled", value: null };
        if (next.kind === "denied") throw new Error("camera permission denied");
        if (next.kind === "throw_canceled") throw new Error("scan canceled");
        return { ScanResult: next.kind === "value" ? next.value : "" };
      }
    }
  };
  const controls = Object.freeze({
    appLink(value) {
      const callback = appUrlListener;
      if (callback) void callback({ url: value });
    },
    scanner(kind, value = null) {
      scannerResult = { kind, value };
    },
    failStorage(value) {
      failCredentialStorage = Boolean(value);
    }
  });
  Object.defineProperty(globalThis, "__VIBEX_E2E_NATIVE__", {
    configurable: false,
    enumerable: false,
    value: controls,
    writable: false
  });
  Object.defineProperty(globalThis, "Capacitor", {
    configurable: true,
    enumerable: false,
    value: {
      isNativePlatform: () => true,
      isPluginAvailable: (name) => Object.hasOwn(plugins, name),
      registerPlugin: (name) => plugins[name],
      Plugins: plugins
    },
    writable: false
  });
}

async function newProductContext(browser, native = false) {
  const context = await browser.newContext({
    viewport: MOBILE_VIEWPORT,
    serviceWorkers: "block"
  });
  if (native) await context.addInitScript(nativeFixtureScript);
  return context;
}

async function pairingProjection(page) {
  return page.evaluate(() => window.__VIBEX_GATE__?.remote?.pairing ?? null);
}

async function waitPairingState(page, states, code, timeout = 65_000) {
  const expected = new Set(Array.isArray(states) ? states : [states]);
  const deadline = Date.now() + timeout;
  let projection = null;
  while (Date.now() < deadline) {
    projection = await pairingProjection(page);
    if (expected.has(projection?.state)) return projection;
    await page.waitForTimeout(100);
  }
  const observedState = PAIRING_PUBLIC_STATES.has(projection?.state) ? projection.state : "unknown";
  fail(`${code}_state_${observedState}`);
}

async function gotoPublic(page, origin) {
  try {
    await page.goto(origin, { waitUntil: "domcontentloaded", timeout: 65_000 });
  } catch {
    fail("product_pairing_page_navigation_failed");
  }
  await waitForRuntime(page);
}

async function openPairingIntake(browser, link, intake) {
  const native = intake === "app_link" || intake === "scanner" || intake === "storage_failure";
  const context = await newProductContext(browser, native);
  const page = await context.newPage();
  if (intake === "browser_url") {
    await gotoPublic(page, link);
  } else {
    const cleanOrigin = new URL(link).origin;
    await gotoPublic(page, cleanOrigin);
    await waitPairingState(page, ["unpaired", "idle"], "product_pairing_unpaired_state_missing");
    if (intake === "app_link") {
      await page.evaluate((value) => globalThis.__VIBEX_E2E_NATIVE__.appLink(value), link);
    } else {
      if (intake === "storage_failure") {
        await page.evaluate(() => globalThis.__VIBEX_E2E_NATIVE__.failStorage(true));
      }
      await page.evaluate(
        (value) => globalThis.__VIBEX_E2E_NATIVE__.scanner("value", value),
        link
      );
      await page.locator("#pairing-scan").click();
    }
  }
  assert(await page.evaluate(() => location.hash === ""), "pairing_fragment_not_scrubbed");
  return { context, page };
}

async function confirmPairing(page) {
  await waitPairingState(page, "preview", "pairing_preview_missing");
  const button = page.locator("#pairing-confirm");
  await button.waitFor({ state: "visible", timeout: 10_000 });
  await button.click();
}

function operations(remote, domain) {
  return new Set(remote.capabilities?.[domain]?.operations ?? []);
}

function assertPermissionContract(remote, permission) {
  const requiredReads = {
    agent: ["agent_list_sessions", "agent_open_session", "agent_fetch_timeline"],
    file: ["file_tree", "file_search", "file_read"],
    git: ["git_status", "git_diff"],
    terminal: ["terminal_list", "terminal_attach"],
    management: ["management_profiles", "management_health"]
  };
  const mutations = {
    agent: ["agent_send_message", "agent_continue_turn", "agent_interrupt", "agent_switch_runtime"],
    file: ["file_write"],
    git: ["git_stage", "git_unstage", "git_commit"],
    terminal: ["terminal_create", "terminal_input", "terminal_resize", "terminal_close"],
    device: ["device_pairing", "device_list", "device_revoke"]
  };
  for (const [domain, names] of Object.entries(requiredReads)) {
    const available = operations(remote, domain);
    assert(names.every((name) => available.has(name)), `permission_${permission}_${domain}_read_missing`);
  }
  for (const [domain, names] of Object.entries(mutations)) {
    const available = operations(remote, domain);
    if (permission === "full_control") {
      assert(names.every((name) => available.has(name)), `permission_${domain}_mutation_missing`);
    } else {
      assert(names.every((name) => !available.has(name)), `permission_${permission}_${domain}_mutation_leaked`);
    }
  }
  const approvals = operations(remote, "agent").has("agent_resolve_approval");
  assert(
    approvals === ["approve_only", "full_control"].includes(permission),
    `permission_${permission}_approval_mismatch`
  );
}

async function pairSuccess(environment, browser, permission, intake, options = {}) {
  const before = await trustSummary(environment);
  let link = await createOffer(environment, permission, options);
  const { context, page } = await openPairingIntake(browser, link, intake);
  await confirmPairing(page);
  await waitForConnectedRuntime(page);
  const projection = await pairingProjection(page);
  assert(
    projection?.state === "online" || projection?.state === "idle",
    "product_pairing_online_state_missing"
  );
  const remote = await remoteState(page);
  assertPermissionContract(remote, permission);
  const after = await trustSummary(environment);
  const delta = activeDeviceDelta(before, after);
  assert(delta.length === 1, "product_pairing_device_delta_invalid");
  await waitDesktop(
    environment,
    (snapshot) => snapshot.offerStatus === "claimed",
    "desktop_offer_claim_not_observed"
  );
  if (environment.relayLogPath) {
    const log = readFileSync(environment.relayLogPath, "utf8");
    assert(!log.includes(link), "relay_pairing_material_leaked");
  }
  const replayLink = options.retainForReplay ? link : null;
  link = null;
  return {
    context,
    page,
    deviceIdentitySha256: delta[0],
    permissionContractSha256: permissionContractSha256(remote),
    replayLink
  };
}

async function clearRevokedPage(page) {
  await waitPairingState(page, "revoked", "revoked_state_missing", 65_000);
  await page.locator("#pairing-clear").click();
  await waitPairingState(page, "unpaired", "local_device_clear_failed");
  const state = await remoteState(page);
  assert(state.configured === false, "local_device_remained_configured");
}

async function finishPermissionDevice(environment, paired) {
  await revokeExact(environment, paired.deviceIdentitySha256);
  await clearRevokedPage(paired.page);
  await paired.context.close();
}

async function scannerNegative(environment, browser, kind, expectedState, value = null) {
  const before = await trustSummary(environment);
  const context = await newProductContext(browser, true);
  const page = await context.newPage();
  await gotoPublic(page, environment.entryOrigin);
  await waitPairingState(page, ["unpaired", "idle"], "scanner_negative_unpaired_missing");
  await page.evaluate(
    ({ fixtureKind, fixtureValue }) =>
      globalThis.__VIBEX_E2E_NATIVE__.scanner(fixtureKind, fixtureValue),
    { fixtureKind: kind, fixtureValue: value }
  );
  await page.locator("#pairing-scan").click();
  await waitPairingState(page, expectedState, "scanner_negative_state_mismatch");
  const after = await trustSummary(environment);
  assert(after.activeDeviceCount === before.activeDeviceCount, "scanner_negative_created_device");
  await context.close();
}

async function negativeOffer(environment, browser, kind) {
  const before = await trustSummary(environment);
  let link = await createOffer(environment, "read_only");
  if (kind === "expired") {
    link = mutateOfferLink(link, (offer) => {
      offer.expiresAtMs = Date.now() - 1;
    });
  } else if (kind === "tampered") {
    link = mutateOfferLink(link, (offer) => {
      const finalCharacter = offer.oneTimeChallenge.at(-1);
      offer.oneTimeChallenge = `${offer.oneTimeChallenge.slice(0, -1)}${
        finalCharacter === "A" ? "B" : "A"
      }`;
    });
  } else if (kind === "wrong_identity") {
    link = mutateOfferLink(link, (offer) => {
      offer.serverId = `${offer.serverId}-mismatch`;
    });
  } else if (kind === "incompatible") {
    link = mutateOfferLink(link, (offer) => {
      offer.protocolRange = { min: { major: 99, minor: 0 }, max: { major: 99, minor: 0 } };
    });
  } else if (kind === "canceled") {
    await desktopAction(environment, { kind: "cancel_offer" });
  }
  const context = await newProductContext(browser, false);
  const page = await context.newPage();
  await gotoPublic(page, link);
  const terminal =
    kind === "expired"
      ? ["expired"]
      : kind === "incompatible"
        ? ["incompatible"]
        : ["preview", "route_error", "identity_mismatch", "invalid"];
  const projection = await waitPairingState(
    page,
    terminal,
    `negative_offer_${kind}_preview_failed`,
    15_000
  );
  if (projection.state === "preview") {
    await page.locator("#pairing-confirm").click();
    await waitPairingState(
      page,
      ["route_error", "identity_mismatch", "incompatible", "invalid"],
      `negative_offer_${kind}_was_not_rejected`
    );
  }
  const after = await trustSummary(environment);
  assert(after.activeDeviceCount === before.activeDeviceCount, "negative_offer_created_device");
  await context.close();
  const snapshot = await desktopSnapshot(environment);
  if (snapshot.offerStatus === "active") await desktopAction(environment, { kind: "cancel_offer" });
  link = null;
}

async function replayOffer(environment, browser, link) {
  const before = await trustSummary(environment);
  const context = await newProductContext(browser, false);
  const page = await context.newPage();
  let claimHttpStatus = null;
  let claimNetworkFailed = false;
  const isClaimRequest = (value) => {
    try {
      return new URL(value).pathname === "/api/v2/pairing/claim";
    } catch {
      return false;
    }
  };
  page.on("response", (response) => {
    if (isClaimRequest(response.url())) claimHttpStatus = response.status();
  });
  page.on("requestfailed", (request) => {
    if (isClaimRequest(request.url())) claimNetworkFailed = true;
  });
  await gotoPublic(page, link);
  const projection = await waitPairingState(
    page,
    ["preview", "route_error", "invalid"],
    "replayed_offer_preview_failed"
  );
  if (projection.state === "preview") {
    await page.locator("#pairing-confirm").click();
    const resolution = await waitPairingState(
      page,
      [
        "route_error",
        "invalid",
        "expired",
        "storage_error",
        "offline",
        "revoked",
        "incompatible",
        "identity_mismatch",
        "credential_corrupt",
        "unpaired",
        "online",
        "idle"
      ],
      "replayed_offer_resolution_missing"
    );
    assert(!["online", "idle"].includes(resolution.state), "replayed_offer_was_accepted");
    assert(!claimNetworkFailed, "replayed_offer_claim_network_failed");
    if (MODE_ENTRY[environment.mode] !== METHOD.relay) {
      assert(claimHttpStatus !== null, "replayed_offer_claim_response_missing");
      assert([400, 409].includes(claimHttpStatus), "replayed_offer_claim_status_invalid");
    }
    if (resolution.state !== "route_error") {
      fail(`replayed_offer_rejection_state_${resolution.state}`);
    }
    assert(
      resolution.errorCode === "remote_pairing_offer_already_claimed",
      "replayed_offer_rejection_code_invalid"
    );
  }
  const after = await trustSummary(environment);
  assert(after.activeDeviceCount === before.activeDeviceCount, "replayed_offer_created_device");
  await context.close();
}

async function storageFailure(environment, browser) {
  const before = await trustSummary(environment);
  let link = await createOffer(environment, "read_only");
  const { context, page } = await openPairingIntake(browser, link, "storage_failure");
  await confirmPairing(page);
  await waitPairingState(page, "storage_error", "storage_failure_state_missing");
  const claimed = await trustSummary(environment);
  const delta = activeDeviceDelta(before, claimed);
  assert(delta.length === 1, "storage_failure_claim_delta_invalid");
  await revokeExact(environment, delta[0]);
  const cleaned = await trustSummary(environment);
  assert(
    cleaned.activeDeviceCount === before.activeDeviceCount,
    "storage_failure_orphan_device_remained"
  );
  link = null;
  await context.close();
}

async function refreshAndReopen(paired) {
  await paired.page.reload({ waitUntil: "domcontentloaded", timeout: 65_000 });
  await waitForRuntime(paired.page);
  await waitForConnectedRuntime(paired.page);
  const origin = new URL(paired.page.url()).origin;
  await paired.page.close();
  paired.page = await paired.context.newPage();
  await gotoPublic(paired.page, origin);
  await waitForConnectedRuntime(paired.page);
}

async function waitConnectionOffline(page) {
  const deadline = Date.now() + 65_000;
  while (Date.now() < deadline) {
    const state = await remoteState(page);
    if (state.connection?.state !== "online") return;
    await page.waitForTimeout(150);
  }
  fail("remote_disable_not_observed");
}

async function notifyNetwork(page) {
  await page.evaluate(() => {
    window.__VIBEX_GATE__.remoteLifecycle({ kind: "network_changed" });
  });
}

async function assertActiveRoute(page, expected, timeout = 65_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const state = await remoteState(page);
    const active = state.connection?.activeRoute;
    if (expected.includes(active)) return active;
    await page.waitForTimeout(150);
  }
  fail("active_route_mismatch");
}

async function waitForRestartedRuntime(page, previousEpoch, timeout = 120_000) {
  assert(Number.isSafeInteger(previousEpoch), "desktop_restart_previous_epoch_missing");
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const state = await remoteState(page);
    const connection = state.connection;
    if (
      connection?.state === "online" &&
      Number.isSafeInteger(connection.sessionEpoch) &&
      connection.sessionEpoch !== previousEpoch &&
      connection.reconnectAttempt === 0 &&
      connection.nextRetryAtMs === null &&
      connection.lastErrorCode === null
    ) {
      return;
    }
    await page.waitForTimeout(150);
  }
  fail("desktop_restart_new_session_timeout");
}

export function recoveryDriver(environment, page) {
  if (environment.mode === "direct-relay-fallback") {
    return {
      seamless: true,
      async disconnect() {
        await environment.directProxy.stop();
        await assertActiveRoute(page, ["relay"], 90_000);
      },
      async duringFallback() {
        const nonce = randomBytes(6).toString("hex");
        try {
          await runFileWorkflow(page, nonce, "web_browser");
        } catch (error) {
          const code = /^[a-z0-9_]+$/.test(error?.code ?? "")
            ? error.code
            : "file_workflow_failed";
          fail(`fallback_${code}`);
        }
        try {
          await runTerminalWorkflow(page, nonce, "web_browser");
        } catch (error) {
          const code = /^[a-z0-9_]+$/.test(error?.code ?? "")
            ? error.code
            : "terminal_workflow_failed";
          fail(`fallback_${code}`);
        }
      },
      async reconnect() {
        await environment.directProxy.start();
      }
    };
  }
  const method = MODE_ENTRY[environment.mode];
  return {
    async disconnect() {
      await desktopAction(environment, { kind: "disable_method", method });
    },
    async reconnect() {
      const snapshot = await enableMethodAndWait(environment, method, {
        enableCode: "recovery_method_reenable_failed",
        confirmationCode: "recovery_tailscale_confirmation_failed"
      });
      assert(
        methodSnapshot(snapshot, method)?.candidateAvailable,
        "recovery_pairing_candidate_unavailable"
      );
    }
  };
}

async function globalDisableReenable(environment, page) {
  await desktopAction(environment, { kind: "disable_all" });
  await waitConnectionOffline(page);
  await reenableMethods(environment);
  await notifyNetwork(page);
  await waitForConnectedRuntime(page);
}

async function rePairAfterClear(environment, browser) {
  const paired = await pairSuccess(environment, browser, "full_control", "browser_url");
  await revokeExact(environment, paired.deviceIdentitySha256);
  await clearRevokedPage(paired.page);
  await paired.context.close();
}

function browserArgs(environment) {
  const args = [
    "--enable-unsafe-webgpu",
    "--use-angle=vulkan",
    "--enable-features=Vulkan"
  ];
  if (environment.certificateSpkiSha256) {
    args.push(`--ignore-certificate-errors-spki-list=${environment.certificateSpkiSha256}`);
  }
  return args;
}

function routeSetDigest(environment) {
  const routes = [environment.directOrigin, environment.relayOrigin, environment.entryOrigin]
    .filter(Boolean)
    .map((value) => sha256Classification(value))
    .sort();
  return sha256(routes.join(":"));
}

function transportClassification(mode) {
  return {
    tailscale: "tailnet",
    direct: "direct",
    relay: "self_hosted_relay",
    "relay-no-tailscale": "self_hosted_relay",
    "direct-relay-fallback": "direct_with_relay_fallback"
  }[mode];
}

async function disableAllBestEffort(environment) {
  try {
    await desktopAction(environment, { kind: "disable_all" });
  } catch {
    // Exact-owned product cleanup is retried by the environment shutdown path.
  }
}

export async function runProductPairingMode(environment) {
  assert(MODE_METHODS[environment.mode], "product_pairing_mode_invalid");
  let browser;
  let fixture = null;
  let full = null;
  let stage = "configure_methods";
  try {
    await configureMethods(environment);
    environment.entryOrigin =
      MODE_ENTRY[environment.mode] === METHOD.direct
        ? environment.directOrigin
        : MODE_ENTRY[environment.mode] === METHOD.relay
          ? environment.relayOrigin
          : null;
    stage = "browser_launch";
    browser = await chromium.launch({ headless: true, args: browserArgs(environment) });

    stage = "read_only_pairing";
    const readOnly = await pairSuccess(environment, browser, "read_only", "app_link", {
      useDefault: true
    });
    if (!environment.entryOrigin) environment.entryOrigin = new URL(readOnly.page.url()).origin;
    const permissionHashes = { read_only: readOnly.permissionContractSha256 };
    await finishPermissionDevice(environment, readOnly);

    stage = "approve_only_pairing";
    const approveOnly = await pairSuccess(
      environment,
      browser,
      "approve_only",
      "scanner"
    );
    permissionHashes.approve_only = approveOnly.permissionContractSha256;
    await finishPermissionDevice(environment, approveOnly);

    stage = "full_control_pairing";
    full = await pairSuccess(environment, browser, "full_control", "browser_url", {
      retainForReplay: true
    });
    permissionHashes.full_control = full.permissionContractSha256;
    await replayOffer(environment, browser, full.replayLink);
    full.replayLink = null;

    stage = "scanner_negatives";
    await scannerNegative(environment, browser, "denied", "camera_denied");
    await scannerNegative(environment, browser, "canceled", "scan_canceled");
    await scannerNegative(
      environment,
      browser,
      "value",
      "invalid",
      "https://example.invalid/not-a-pairing-code"
    );
    await scannerNegative(environment, browser, "value", "invalid", "x".repeat(32 * 1024 + 1));

    stage = "offer_negatives";
    for (const kind of ["expired", "canceled", "tampered", "wrong_identity", "incompatible"]) {
      await negativeOffer(environment, browser, kind);
    }
    stage = "storage_failure";
    await storageFailure(environment, browser);

    stage = "fixture_setup_before_workflows";
    fixture = await setupFixture(environment);
    stage = "workflow_matrix";
    const matrix = await runProductMatrix(
      full.page,
      recoveryDriver(environment, full.page),
      "web_browser",
      environment.mode,
      null,
      environment.relayLogPath,
      {
        ...fixture,
        resolveDeviceIndex: async () => (await setupFixture(environment)).deviceIndex
      }
    );
    assert(
      matrix.permissionContractSha256 === permissionHashes.full_control,
      "full_control_permission_contract_drift"
    );
    if (environment.mode === "direct-relay-fallback") {
      await assertActiveRoute(full.page, ["direct", "tailnet"], 90_000);
    }

    stage = "refresh_and_reopen";
    await refreshAndReopen(full);
    const beforeRestart = await remoteState(full.page);
    stage = "desktop_restart";
    await environment.desktop.restart();
    stage = "desktop_restart_reconnect";
    await waitForRestartedRuntime(full.page, beforeRestart.connection?.sessionEpoch);
    stage = "fixture_setup_after_restart";
    fixture = await setupFixture(environment);
    await prepareWorkflowFixture(full.page, fixture);
    stage = "desktop_restart_agent_recovery";
    try {
      await runAgentWorkflow(
        full.page,
        randomBytes(6).toString("hex"),
        fixture.sessionIndex,
        "web_browser"
      );
    } catch (error) {
      if (/^[a-z0-9_]+$/.test(error?.code ?? "")) {
        fail(`desktop_restart_${error.code}`);
      }
      throw error;
    }

    stage = "global_disable_reenable";
    await globalDisableReenable(environment, full.page);
    stage = "device_revoke";
    await revokeExact(environment, full.deviceIdentitySha256);
    stage = "local_device_clear";
    await clearRevokedPage(full.page);
    await full.context.close();
    full = null;
    stage = "revoked_device_repair";
    await rePairAfterClear(environment, browser);

    stage = "evidence";
    const identity = await controlJson(environment, "/identity/summary");
    assert(
      identity.schemaVersion === "remote-access-identity-summary.v1" &&
        /^[0-9a-f]{64}$/.test(identity.serverIdentitySha256),
      "server_identity_summary_invalid"
    );
    const candidate = resolveProductPairingCandidate(ROOT);
    const result = {
      mode: environment.mode,
      status: "passed",
      capturedAt: new Date().toISOString(),
      candidateDigest: candidate.candidateDigest,
      desktopArtifactSha256: environment.desktopArtifactSha256,
      relayArtifactSha256: environment.relayArtifactSha256 ?? null,
      serverIdentitySha256: identity.serverIdentitySha256,
      routeSetSha256: routeSetDigest(environment),
      transport: transportClassification(environment.mode),
      tailscaleConfigured: environment.mode === "tailscale",
      permissions: permissionHashes,
      entries: passedMap(["browser_url", "app_link", "in_app_scanner"]),
      checks: passedMap(PRODUCT_PAIRING_CHECKS),
      workflows: passedMap(PRODUCT_PAIRING_WORKFLOWS),
      redactionScan: "passed"
    };
    if (environment.writeEvidence) {
      const path = join(ROOT, PRODUCT_PAIRING_EVIDENCE_PATH);
      const existing = existsSync(path) ? JSON.parse(readFileSync(path, "utf8")) : null;
      const evidence = mergeProductPairingMode(
        existing,
        candidate,
        environment.mode,
        result
      );
      writeFileSync(path, `${JSON.stringify(evidence, null, 2)}\n`);
    }
    return result;
  } catch (error) {
    if (/^[a-z0-9_]+$/.test(error?.code ?? "")) throw error;
    fail(unexpectedStageFailureCode(stage, error));
  } finally {
    if (full) await full.context.close().catch(() => {});
    if (fixture) await cleanupFixture(environment).catch(() => {});
    await browser?.close().catch(() => {});
    await disableAllBestEffort(environment);
  }
}

export async function runProductPairingCli({ workflowAlias = false } = {}) {
  if (workflowAlias) {
    const targetIndex = process.argv.indexOf("--target");
    const target = targetIndex >= 0 ? process.argv[targetIndex + 1] : null;
    assert(target === null || target === "web", "product_pairing_alias_target_invalid");
  }
  const { runProductPairingEnvironment } = await import(
    "./e2e-local-env/run-product-pairing.mjs"
  );
  return runProductPairingEnvironment({ runMode: runProductPairingMode });
}

const DIRECT_EXECUTION =
  process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);

if (DIRECT_EXECUTION) {
  try {
    await runProductPairingCli();
  } catch (error) {
    const code = /^[a-z0-9_]+$/.test(error?.code ?? "")
      ? error.code
      : "product_pairing_e2e_failed";
    console.error(`GPUI product pairing E2E failed: ${code}`);
    process.exitCode = 1;
  }
}

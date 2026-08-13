import { execFileSync, spawnSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import {
  existsSync,
  readFileSync,
  writeFileSync
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { chromium } from "@playwright/test";
import {
  WORKFLOW_EVIDENCE_PATH,
  endpointSha256,
  mergeWorkflowTarget,
  permissionContractSha256,
  resolveWorkflowCandidateIdentity
} from "./workflow-e2e-evidence.mjs";
import { startWasmServer } from "./mobile-wasm-test-server.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DIST = join(ROOT, "apps/mobile-wasm/dist");
const ANDROID_BUILD_EVIDENCE = join(
  ROOT,
  "docs/platform/evidence/wasm-android-build.json"
);
const APP_ID = "dev.vibex.remote";
const REMOTE_CREDENTIAL_STORAGE_KEY = "vibex.remote-client.credentials.v1";
const WRITE = process.argv.includes("--write");
const REUSE_INSTALLED = process.argv.includes("--reuse-installed");
const PAIR_VIA_PRODUCT = process.argv.includes("--pair-via-product");

function option(name, fallback = null) {
  const prefix = `--${name}=`;
  const inline = process.argv.find((argument) => argument.startsWith(prefix));
  if (inline) return inline.slice(prefix.length);
  const index = process.argv.indexOf(`--${name}`);
  return index >= 0 ? process.argv[index + 1] ?? fallback : fallback;
}

export function fail(code) {
  const error = new Error(code);
  error.code = code;
  throw error;
}

export function assert(condition, code) {
  if (!condition) fail(code);
}

async function stableWorkflowStep(code, operation) {
  try {
    return await operation();
  } catch (error) {
    if (/^[a-z0-9_]+$/.test(error?.code ?? "")) throw error;
    fail(code);
  }
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? ROOT,
    encoding: "utf8",
    env: options.env ?? process.env,
    maxBuffer: 32 * 1024 * 1024,
    stdio: options.inherit ? "inherit" : "pipe"
  });
  if (result.error || result.status !== 0) fail(options.code ?? "external_command_failed");
  return (result.stdout ?? "").trim();
}

function adb(serial, args, code = "adb_command_failed") {
  return run("adb", ["-s", serial, ...args], { code });
}

function adbMaybe(serial, args) {
  const result = spawnSync("adb", ["-s", serial, ...args], {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 4 * 1024 * 1024
  });
  return result.status === 0 ? (result.stdout ?? "").trim() : "";
}

function adbBytes(serial, args, code = "adb_command_failed") {
  const result = spawnSync("adb", ["-s", serial, ...args], {
    cwd: ROOT,
    encoding: null,
    maxBuffer: 64 * 1024 * 1024
  });
  if (result.error || result.status !== 0) fail(code);
  return result.stdout;
}

function loadCredentials(path) {
  assert(path && existsSync(path), "credential_bundle_missing");
  const bytes = readFileSync(path);
  assert(bytes.length > 0 && bytes.length <= 64 * 1024, "credential_bundle_size_invalid");
  const bundle = JSON.parse(bytes.toString("utf8"));
  assert(bundle.schemaVersion === "vibex-remote-client-credentials.v1", "credential_schema_invalid");
  assert(typeof bundle.record?.auth?.authToken === "string", "credential_auth_missing");
  assert(typeof bundle.identityPrivateKey === "string", "credential_identity_missing");
  return bundle;
}

function assertRoute(credentials, transport) {
  if (transport === "relay") {
    assert(credentials.route?.relay?.url, "self_hosted_relay_route_missing");
    assert(
      process.env.VIBEX_E2E_RELAY_OWNERSHIP === "user_self_hosted",
      "self_hosted_relay_ownership_unconfirmed"
    );
    return credentials.route.relay.url;
  }
  const endpoint = credentials.route?.directCandidates?.[0] ?? credentials.record?.serverUrl;
  assert(endpoint, "direct_route_missing");
  return endpoint;
}

function prepareDisposableFixture(hook, target, transport) {
  assert(hook && existsSync(hook), "fixture_hook_missing");
  try {
    const output = run(hook, ["setup", target, transport], {
      code: "fixture_setup_failed"
    });
    assert(Buffer.byteLength(output) <= 4 * 1024, "fixture_setup_output_unbounded");
    let fixture;
    try {
      fixture = JSON.parse(output);
    } catch {
      fail("fixture_setup_output_invalid");
    }
    assert(
      JSON.stringify(Object.keys(fixture).sort()) ===
        JSON.stringify(
          ["deviceIndex", "disposable", "schemaVersion", "sessionIndex", "workspaceIndex"].sort()
        ),
      "fixture_setup_output_invalid"
    );
    assert(
      fixture.schemaVersion === "vibex-workflow-fixture.v1" && fixture.disposable === true,
      "fixture_not_disposable"
    );
    for (const field of ["workspaceIndex", "sessionIndex", "deviceIndex"]) {
      assert(
        Number.isSafeInteger(fixture[field]) && fixture[field] >= 0,
        "fixture_selection_invalid"
      );
    }
    return fixture;
  } catch (error) {
    cleanupDisposableFixtureBestEffort(hook, target, transport);
    throw error;
  }
}

function cleanupDisposableFixture(hook, target, transport) {
  run(hook, ["cleanup", target, transport], { code: "fixture_cleanup_failed" });
}

function cleanupDisposableFixtureBestEffort(hook, target, transport) {
  spawnSync(hook, ["cleanup", target, transport], {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 4 * 1024 * 1024,
    stdio: "ignore",
    timeout: 60_000
  });
}

export async function waitForRuntime(page) {
  await page.waitForFunction(
    () => {
      const gate = window.__VIBEX_GATE__;
      return gate?.wasmReady && ["ready", "offline", "revoked", "incompatible"].includes(gate.state);
    },
    null,
    { timeout: 60_000 }
  );
}

export async function waitForConnectedRuntime(page, timeout = 60_000) {
  await page.waitForFunction(
    () => window.__VIBEX_GATE__?.workflowState?.()?.connection === "online",
    null,
    { timeout }
  );
}

export async function snapshot(page) {
  return page.evaluate(() => window.__VIBEX_GATE__.workflowState());
}

export async function remoteState(page) {
  return page.evaluate(() => window.__VIBEX_GATE__.remoteState());
}

export async function action(page, command) {
  return page.evaluate((value) => window.__VIBEX_GATE__.workflowAction(value), command);
}

export async function waitSnapshot(page, predicate, code, timeout = 45_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const value = await snapshot(page);
    if (predicate(value)) return value;
    await page.waitForTimeout(100);
  }
  fail(code);
}

async function canvasPoint(page, xFraction, yFraction) {
  return page.evaluate(
    ({ xFraction: x, yFraction: y }) => {
      const canvas = document.querySelector("canvas[data-vibex-gate-canvas]");
      if (!canvas) throw new Error("canvas_missing");
      const bounds = canvas.getBoundingClientRect();
      return { x: bounds.left + bounds.width * x, y: bounds.top + bounds.height * y };
    },
    { xFraction, yFraction }
  );
}

async function replaceInput(page, xFraction, yFraction, value, submit = false) {
  const point = await canvasPoint(page, xFraction, yFraction);
  await page.mouse.click(point.x, point.y);
  await page.keyboard.press("Control+A");
  await page.keyboard.type(value, { delay: 2 });
  if (submit) await page.keyboard.press("Enter");
}

function requirePermissions(state) {
  const required = {
    agent: ["agent_list_sessions", "agent_open_session", "agent_send_message"],
    file: ["file_tree", "file_read", "file_write"],
    git: ["git_status", "git_diff", "git_stage", "git_unstage", "git_commit"],
    terminal: [
      "terminal_list",
      "terminal_create",
      "terminal_attach",
      "terminal_input",
      "terminal_resize",
      "terminal_close"
    ],
    management: ["management_profiles", "management_health"],
    device: ["device_pairing", "device_list", "device_revoke"]
  };
  for (const [domain, operations] of Object.entries(required)) {
    const capability = state.capabilities?.[domain];
    assert(capability?.availability === "available", `${domain}_permission_unavailable`);
    const supported = new Set(capability.operations ?? []);
    const missing = operations.filter((operation) => !supported.has(operation));
    assert(missing.length === 0, `${domain}_operation_missing_${missing.join("_")}`);
  }
}

function passedResult() {
  return {
    status: "passed",
    permission: "passed",
    businessResult: "passed",
    liveEvent: "passed",
    reconnectRecovery: "passed",
    errorCode: null
  };
}

export async function runAgentWorkflow(page, nonce, sessionIndex, target) {
  await action(page, { kind: "select_surface", surface: "agent" });
  let state = await waitSnapshot(
    page,
    (value) => value.sessionCount > 0 && value.workspaceCount > 0,
    "agent_bootstrap_empty"
  );
  await action(page, { kind: "select_session", index: sessionIndex });
  state = await waitSnapshot(
    page,
    (value) =>
      value.navigationLevel === "session" &&
      value.activeSurface === "agent" &&
      value.agentRuntimeReady,
    "agent_session_open_failed"
  );
  const timelineBefore = state.timelineRowCount;
  const liveBefore = state.agentLiveEventCount;
  if (target === "android_physical") {
    await action(page, { kind: "fill_test_input", input: "agent_composer" });
    await action(page, { kind: "send_agent_message" });
  } else {
    await replaceInput(page, 0.72, 0.93, `Vibex E2E ${nonce}: reply OK`, true);
  }
  try {
    state = await waitSnapshot(
      page,
      (value) =>
        value.agentMutationPhase === "ready" &&
        value.timelineRowCount > timelineBefore &&
        value.agentLiveEventCount > liveBefore,
      "agent_message_or_live_event_missing",
      90_000
    );
  } catch (error) {
    if (error?.code !== "agent_message_or_live_event_missing") throw error;
    state = await snapshot(page);
    if (state.agentMutationPhase === "idle") fail("agent_message_submission_not_observed");
    if (state.agentMutationPhase === "failed") {
      const backendCode = state.errorCodes.find((code) => /^[a-z0-9_]+$/.test(code));
      fail(backendCode ? `agent_message_${backendCode}` : "agent_message_mutation_failed");
    }
    if (state.agentMutationPhase === "loading") fail("agent_message_mutation_timeout");
    if (state.timelineRowCount <= timelineBefore) fail("agent_message_timeline_missing");
    if (state.agentLiveEventCount <= liveBefore) fail("agent_message_live_event_missing");
    throw error;
  }
  while (state.pendingApprovalCount > 0) {
    await action(page, { kind: "resolve_approval", index: 0, approve: true });
    state = await waitSnapshot(
      page,
      (value) => value.pendingApprovalCount < state.pendingApprovalCount,
      "agent_approval_resolution_failed"
    );
  }
}

export async function runFileWorkflow(page, nonce, target) {
  await action(page, { kind: "select_surface", surface: "files" });
  let state = await waitSnapshot(
    page,
    (value) => value.activeSurface === "files" && value.fileRowCount > 0,
    "file_tree_empty"
  );
  for (let index = 0; index < Math.min(state.fileRowCount, 32); index += 1) {
    await action(page, { kind: "open_file_row", index });
    state = await waitSnapshot(
      page,
      (value) => value.fileEditorStatus !== "loading",
      "file_read_timeout",
      10_000
    );
    if (state.fileHasActiveFile && ["clean", "saved", "dirty"].includes(state.fileEditorStatus)) {
      break;
    }
  }
  assert(state.fileHasActiveFile, "editable_file_missing");
  await page.evaluate(
    () =>
      new Promise((resolvePromise) =>
        requestAnimationFrame(() => requestAnimationFrame(resolvePromise))
      )
  );
  if (target === "android_physical") {
    await action(page, { kind: "fill_test_input", input: "file_editor" });
  } else {
    await replaceInput(page, 0.72, 0.56, `vibex workflow e2e ${nonce}\n`);
  }
  state = await waitSnapshot(
    page,
    (value) => value.fileEditorStatus === "dirty",
    "file_edit_not_observed"
  );
  const liveBefore = state.fileLiveEventCount;
  await action(page, { kind: "save_file" });
  await waitSnapshot(
    page,
    (value) =>
      value.fileEditorStatus === "saved" && value.fileLiveEventCount > liveBefore,
    "file_save_or_live_event_failed"
  );
}

export async function runGitWorkflow(page, nonce, target) {
  await action(page, { kind: "select_surface", surface: "git" });
  let state = await waitSnapshot(
    page,
    (value) => value.activeSurface === "git" && value.gitChangeCount > 0,
    "git_changes_empty"
  );
  await action(page, { kind: "load_git_diff", index: 0 });
  const liveBefore = state.gitLiveEventCount;
  await action(page, { kind: "stage_git_change", index: 0 });
  state = await waitSnapshot(
    page,
    (value) => !value.gitMutationPending && value.gitLiveEventCount > liveBefore,
    "git_stage_or_live_event_failed"
  );
  assert(!state.errorCodes.some((code) => code.startsWith("git_")), "git_stage_error");
  if (target === "android_physical") {
    await action(page, { kind: "fill_test_input", input: "commit_message" });
  } else {
    await replaceInput(page, 0.73, 0.88, `test: gpui workflow ${nonce}`);
  }
  await action(page, { kind: "prepare_commit" });
  await action(page, { kind: "confirm_commit" });
  const liveAfterStage = state.gitLiveEventCount;
  state = await waitSnapshot(
    page,
    (value) =>
      value.gitCommitPhase === "ready" && value.gitLiveEventCount > liveAfterStage,
    "git_commit_or_live_event_failed",
    60_000
  );
  assert(!state.errorCodes.some((code) => code.startsWith("git_")), "git_workflow_error");
}

export async function runTerminalWorkflow(page, nonce, target) {
  await action(page, { kind: "select_surface", surface: "terminal" });
  let state = await snapshot(page);
  const terminalCount = state.terminalCount;
  await action(page, { kind: "create_terminal" });
  state = await waitSnapshot(
    page,
    (value) =>
      value.activeSurface === "terminal" &&
      value.terminalCount > terminalCount &&
      ["connected", "connecting"].includes(value.terminalConnection),
    "terminal_create_or_attach_failed"
  );
  const sequence = state.terminalSequence;
  if (target === "android_physical") {
    await action(page, { kind: "fill_test_input", input: "terminal_input" });
    await action(page, { kind: "send_terminal_input" });
  } else {
    await replaceInput(page, 0.5, 0.91, `printf vibex-e2e-${nonce}`, true);
  }
  await waitSnapshot(
    page,
    (value) => value.terminalSequence > sequence && value.terminalConnection === "connected",
    "terminal_live_output_missing",
    60_000
  );
  await action(page, { kind: "resize_terminal", rows: 30, cols: 100 });
  await page.waitForTimeout(300);
}

export async function runManagementWorkflow(page, deviceIndexOrResolver) {
  await action(page, { kind: "select_surface", surface: "management" });
  let state = await waitSnapshot(
    page,
    (value) =>
      value.activeSurface === "management" &&
      ["ready", "partial"].includes(value.managementLoadState) &&
      !value.managementOperationPending,
    "management_refresh_failed"
  );
  await action(page, { kind: "run_health_probes" });
  state = await waitSnapshot(
    page,
    (value) => !value.managementOperationPending && value.managementHealthCount > 0,
    "management_health_missing",
    60_000
  );
  assert(
    !state.errorCodes.some((code) => code.startsWith("remote_provider")),
    "management_health_failed"
  );
  await action(page, { kind: "select_management_section", section: "devices" });
  const liveBefore = state.managementLiveEventCount;
  await action(page, { kind: "create_pairing_offer" });
  state = await waitSnapshot(
    page,
    (value) =>
      !value.managementOperationPending &&
      value.hasPairingOffer &&
      value.managementLiveEventCount > liveBefore,
    "management_pairing_offer_or_live_event_missing"
  );
  const deviceIndex =
    typeof deviceIndexOrResolver === "function"
      ? await deviceIndexOrResolver()
      : deviceIndexOrResolver;
  assert(Number.isSafeInteger(deviceIndex) && deviceIndex >= 0, "disposable_device_index_invalid");
  assert(state.managementDeviceCount > deviceIndex, "disposable_device_missing");
  const revokedBefore = state.managementRevokedDeviceCount;
  const liveAfterPairing = state.managementLiveEventCount;
  await action(page, { kind: "revoke_device", index: deviceIndex });
  try {
    state = await waitSnapshot(
      page,
      (value) =>
        !value.managementOperationPending &&
        value.managementRevokedDeviceCount > revokedBefore &&
        value.managementLiveEventCount > liveAfterPairing,
      "management_disposable_revoke_or_live_event_missing"
    );
  } catch (error) {
    if (error?.code !== "management_disposable_revoke_or_live_event_missing") throw error;
    state = await snapshot(page);
    if (state.managementOperationPending) fail("management_device_revoke_pending_timeout");
    if (state.managementRevokedDeviceCount <= revokedBefore) {
      fail("management_device_revoke_projection_missing");
    }
    if (state.managementLiveEventCount <= liveAfterPairing) {
      fail("management_device_revoke_live_event_missing");
    }
    throw error;
  }
  assert(
    !state.errorCodes.some((code) => code.startsWith("remote_device")),
    "management_device_workflow_error"
  );
  const liveAfterRevoke = state.managementLiveEventCount;
  await action(page, { kind: "cancel_pairing_offer" });
  await waitSnapshot(
    page,
    (value) =>
      !value.managementOperationPending &&
      !value.hasPairingOffer &&
      value.managementLiveEventCount > liveAfterRevoke,
    "management_pairing_cancel_or_live_event_missing"
  );
}

async function invokeRecovery(recovery, phase, target, transport) {
  if (typeof recovery === "function") {
    await recovery(phase, { target, transport });
    return;
  }
  if (recovery && typeof recovery[phase] === "function") {
    await recovery[phase]({ target, transport });
    return;
  }
  assert(recovery && existsSync(recovery), "recovery_hook_missing");
  run(recovery, [phase, target, transport], {
    code: `recovery_${phase}_hook_failed`
  });
}

export async function runRecovery(page, recovery, target, transport, baseline) {
  const seamless = recovery && typeof recovery === "object" && recovery.seamless === true;
  await invokeRecovery(recovery, "disconnect", target, transport);
  if (seamless) {
    await waitForConnectedRuntime(page);
    if (typeof recovery.duringFallback === "function") {
      await recovery.duringFallback({ target, transport });
    }
  } else {
    await waitSnapshot(
      page,
      (value) => value.connection !== "online",
      "recovery_disconnect_not_observed"
    );
  }
  await invokeRecovery(recovery, "reconnect", target, transport);
  await page.evaluate(() => {
    window.__VIBEX_GATE__.remoteLifecycle({ kind: "network_changed" });
  });
  const deadline = Date.now() + 90_000;
  while (Date.now() < deadline) {
    const remote = await remoteState(page);
    if (remote.connection?.state === "online") break;
    await page.waitForTimeout(250);
  }
  await page.evaluate(() => {
    window.__VIBEX_GATE__.remoteLifecycle({ kind: "network_changed" });
  });
  await action(page, { kind: "refresh_all" });
  const recovered = await waitSnapshot(
    page,
    (value) =>
      value.connection === "online" &&
      value.sessionCount >= baseline.sessionCount &&
      value.agentRecoveryCount > baseline.agentRecoveryCount &&
      value.fileRecoveryCount > baseline.fileRecoveryCount &&
      value.gitRecoveryCount > baseline.gitRecoveryCount &&
      value.terminalRecoveryCount > baseline.terminalRecoveryCount &&
      value.managementRecoveryCount > baseline.managementRecoveryCount,
    "recovery_authoritative_refetch_failed",
    90_000
  );
  assert(recovered.workspaceCount >= baseline.workspaceCount, "recovery_workspace_loss");
}

export function relayRedactionScan(path, forbiddenSentinels = []) {
  assert(path && existsSync(path), "relay_log_missing");
  const bytes = readFileSync(path);
  assert(bytes.length <= 32 * 1024 * 1024, "relay_log_unbounded");
  const log = bytes.toString("utf8");
  const forbidden = forbiddenSentinels.filter(
    (value) => typeof value === "string" && value.length > 0
  );
  assert(forbidden.every((value) => !log.includes(value)), "relay_zero_knowledge_scan_failed");
}

export async function runProductMatrix(
  page,
  recoveryHook,
  target,
  transport,
  credentials,
  relayLog,
  fixture,
  relayForbiddenSentinels = []
) {
  const remote = await stableWorkflowStep("workflow_runtime_bootstrap_failed", async () => {
    await waitForRuntime(page);
    await waitForConnectedRuntime(page);
    const state = await remoteState(page);
    requirePermissions(state);
    return state;
  });
  await prepareWorkflowFixture(page, fixture);
  const nonce = randomBytes(6).toString("hex");
  await stableWorkflowStep("agent_workflow_failed", () =>
    runAgentWorkflow(page, nonce, fixture.sessionIndex, target)
  );
  await stableWorkflowStep("file_workflow_failed", () => runFileWorkflow(page, nonce, target));
  await stableWorkflowStep("git_workflow_failed", () => runGitWorkflow(page, nonce, target));
  await stableWorkflowStep("terminal_workflow_failed", () =>
    runTerminalWorkflow(page, nonce, target)
  );
  await stableWorkflowStep("management_workflow_failed", () =>
    runManagementWorkflow(page, fixture.resolveDeviceIndex ?? fixture.deviceIndex)
  );
  const recoveryBaseline = await snapshot(page);
  await stableWorkflowStep("workflow_recovery_failed", () =>
    runRecovery(page, recoveryHook, target, transport, recoveryBaseline)
  );
  await stableWorkflowStep("terminal_cleanup_failed", async () => {
    await action(page, { kind: "close_terminal" });
    await waitSnapshot(
      page,
      (value) => value.terminalConnection === "closed",
      "terminal_close_failed"
    );
  });
  if (["relay", "relay-no-tailscale", "direct-relay-fallback"].includes(transport)) {
    const legacySentinels = credentials
      ? [
          credentials.record?.auth?.authToken,
          credentials.identityPrivateKey,
          credentials.route?.relay?.roomId
        ]
      : [];
    relayRedactionScan(relayLog, [
      ...legacySentinels,
      ...relayForbiddenSentinels,
      target === "android_physical" ? "vibex-e2e-probe" : nonce
    ]);
  }
  return {
    workflows: Object.fromEntries(
      ["agent", "files", "git", "terminal", "management"].map((workflow) => [
        workflow,
        passedResult()
      ])
    ),
    permissionContractSha256: permissionContractSha256(remote)
  };
}

export async function prepareWorkflowFixture(page, fixture) {
  await stableWorkflowStep("workflow_workspace_selection_failed", async () => {
    await action(page, { kind: "refresh_all" });
    await waitSnapshot(
      page,
      (value) =>
        value.connection === "online" && value.workspaceCount > fixture.workspaceIndex,
      "workflow_workspace_bootstrap_incomplete"
    );
    await action(page, { kind: "select_workspace", index: fixture.workspaceIndex });
    await waitSnapshot(
      page,
      (value) =>
        value.connection === "online" && value.sessionCount > fixture.sessionIndex,
      "workflow_bootstrap_incomplete"
    );
  });
}

async function developmentHostTarget(credentials, recoveryHook, transport, relayLog, fixture) {
  run("pnpm", ["--filter", "@vibex/mobile-wasm", "build:release"], {
    inherit: true,
    code: "mobile_runtime_release_build_failed"
  });
  const candidateDigest = resolveWorkflowCandidateIdentity(ROOT).candidateDigest;
  const server = option("origin") ? null : await startWasmServer({ dist: DIST });
  const origin = option("origin", server?.origin);
  let browser;
  try {
    browser = await chromium.launch({
      headless: true,
      args: ["--enable-unsafe-webgpu", "--use-angle=vulkan", "--enable-features=Vulkan"]
    });
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
    await context.addInitScript(
      ({ key, value }) => window.localStorage.setItem(key, value),
      { key: REMOTE_CREDENTIAL_STORAGE_KEY, value: JSON.stringify(credentials) }
    );
    const page = await context.newPage();
    await page.goto(origin, { waitUntil: "domcontentloaded", timeout: 60_000 });
    const result = await runProductMatrix(
      page,
      recoveryHook,
      "mobile_wasm_host",
      transport,
      credentials,
      relayLog,
      fixture
    );
    return {
      ...result,
      candidateDigest,
      environment: {
        kind: "development_host",
        browserName: "chromium",
        browserVersion: browser.version(),
        platformSha256: sha256(`${process.platform}:${process.arch}`)
      }
    };
  } finally {
    await browser?.close();
    await server?.close();
  }
}

function authorizedDevice(requested) {
  const output = run("adb", ["devices"], { code: "adb_devices_failed" });
  const devices = output
    .split("\n")
    .slice(1)
    .map((line) => line.trim().split(/\s+/))
    .filter((parts) => parts.length >= 2 && parts[1] === "device")
    .map(([serial]) => serial);
  if (requested) assert(devices.includes(requested), "android_device_unauthorized");
  assert(requested || devices.length === 1, "android_device_selection_required");
  return requested ?? devices[0];
}

function assertAndroidBuildCurrent(identity) {
  assert(identity.androidApk, "current_android_apk_missing");
  assert(existsSync(ANDROID_BUILD_EVIDENCE), "android_build_evidence_missing");
  const build = JSON.parse(readFileSync(ANDROID_BUILD_EVIDENCE, "utf8"));
  assert(
    build.source?.mobileRuntimeSourceTreeSha256 === identity.source.sourceTreeSha256,
    "android_mobile_runtime_source_stale"
  );
  assert(
    build.source?.mobileShellTreeSha256 === identity.source.mobileShellTreeSha256,
    "android_shell_source_stale"
  );
  assert(build.source?.cargoLockfileSha256 === identity.source.cargoLockSha256, "android_cargo_lock_stale");
  assert(build.source?.pnpmLockfileSha256 === identity.source.pnpmLockSha256, "android_pnpm_lock_stale");
  assert(
    build.runtimeBuild?.buildId === identity.mobileRuntimeBuild.buildId,
    "android_mobile_runtime_build_stale"
  );
}

function assertInstalledApkCurrent(serial, identity) {
  const packagePaths = adb(serial, ["shell", "pm", "path", APP_ID], "android_package_missing")
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
  const basePaths = packagePaths
    .filter((line) => line.startsWith("package:") && line.endsWith("/base.apk"))
    .map((line) => line.slice("package:".length));
  assert(basePaths.length === 1, "android_installed_apk_ambiguous");
  const installed = adbBytes(
    serial,
    ["exec-out", "cat", basePaths[0]],
    "android_installed_apk_read_failed"
  );
  assert(
    installed.length === identity.androidApk.bytes &&
      sha256(installed) === identity.androidApk.sha256,
    "android_installed_apk_stale"
  );
}

async function waitForAndroidCdp(serial, port) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json/version`);
      if (response.ok) return;
    } catch {
      // WebView startup is asynchronous.
    }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 250));
  }
  fail("android_webview_debugging_unavailable");
}

function webViewVersion(serial) {
  for (const packageName of ["com.google.android.webview", "com.android.webview"]) {
    const output = adb(serial, ["shell", "dumpsys", "package", packageName]);
    const version = output.match(/versionName=([^\s]+)/)?.[1];
    if (version) return version;
  }
  return "unknown";
}

function physicalAndroidProperties(serial) {
  const keys = [
    "ro.kernel.qemu",
    "ro.boot.qemu",
    "ro.hardware",
    "ro.boot.hardware",
    "ro.product.brand",
    "ro.product.device",
    "ro.product.manufacturer",
    "ro.product.model",
    "ro.product.name",
    "ro.build.fingerprint"
  ];
  const properties = Object.fromEntries(
    keys.map((key) => [key, adb(serial, ["shell", "getprop", key]).trim()])
  );
  assert(
    properties["ro.kernel.qemu"] !== "1" && properties["ro.boot.qemu"] !== "1",
    "android_device_not_physical"
  );
  const fingerprint = properties["ro.build.fingerprint"];
  const identity = keys.slice(2).map((key) => properties[key]).join(" ");
  assert(
    fingerprint && properties["ro.product.model"],
    "android_device_identity_missing"
  );
  assert(
    !/^(?:generic|unknown)[/:]|sdk_gphone|generic_x86|generic_x86_64|emulator|vbox/i.test(
      fingerprint
    ) &&
      !/\b(?:ranchu|goldfish|vbox86p|nox|ttvm|sdk_gphone|generic_x86|android sdk built for|emulator)\b/i.test(
        identity
      ),
    "android_device_not_physical"
  );
  return properties;
}

async function androidTarget(identity, credentials, recoveryHook, transport, relayLog, fixture) {
  assertAndroidBuildCurrent(identity);
  const serial = authorizedDevice(option("device"));
  const properties = physicalAndroidProperties(serial);
  const apk = join(ROOT, identity.androidApk.path);
  if (REUSE_INSTALLED) assertInstalledApkCurrent(serial, identity);
  else adb(serial, ["install", "-r", apk], "android_apk_install_failed");
  adb(serial, ["shell", "am", "force-stop", APP_ID]);
  adb(serial, ["shell", "am", "start", "-n", `${APP_ID}/.MainActivity`], "android_launch_failed");
  let pid = "";
  const pidDeadline = Date.now() + 20_000;
  while (!pid && Date.now() < pidDeadline) {
    pid = adbMaybe(serial, ["shell", "pidof", APP_ID]);
    if (!pid) await new Promise((resolvePromise) => setTimeout(resolvePromise, 250));
  }
  assert(/^\d+$/.test(pid), "android_process_missing");
  const port = adb(serial, [
    "forward",
    "tcp:0",
    `localabstract:webview_devtools_remote_${pid}`
  ]);
  assert(/^\d+$/.test(port), "android_cdp_forward_failed");
  let browser;
  try {
    await waitForAndroidCdp(serial, port);
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
    const context = browser.contexts()[0];
    const page = context.pages()[0];
    assert(page, "android_webview_page_missing");
    await page.evaluate(async ({ key, value }) => {
      const capacitor = globalThis.Capacitor;
      let secure = capacitor?.Plugins?.SecureStoragePlugin;
      if (!secure && capacitor?.isPluginAvailable?.("SecureStoragePlugin")) {
        secure = capacitor.registerPlugin?.("SecureStoragePlugin");
      }
      if (!secure?.set) throw new Error("secure_storage_fixture_unavailable");
      await secure.set({ key, value });
    }, { key: REMOTE_CREDENTIAL_STORAGE_KEY, value: JSON.stringify(credentials) });
    await page.reload({ waitUntil: "domcontentloaded", timeout: 60_000 });
    const result = await runProductMatrix(
      page,
      recoveryHook,
      "android_physical",
      transport,
      credentials,
      relayLog,
      fixture
    );
    const model = properties["ro.product.model"];
    const osVersion = adb(serial, ["shell", "getprop", "ro.build.version.release"]);
    const fingerprint = properties["ro.build.fingerprint"];
    return {
      ...result,
      candidateDigest: identity.candidateDigest,
      environment: {
        kind: "android",
        physical: true,
        deviceIdentitySha256: sha256(`${serial}:${fingerprint}`),
        model: model.slice(0, 128),
        osVersion: osVersion.slice(0, 64),
        webViewVersion: webViewVersion(serial).slice(0, 128),
        apkSha256: identity.androidApk.sha256
      }
    };
  } finally {
    await browser?.close();
    adb(serial, ["forward", "--remove", `tcp:${port}`]);
  }
}

function targetEvidence(identity, target, transport, endpoint, captured) {
  assert(captured.candidateDigest === identity.candidateDigest, "candidate_changed_during_e2e");
  return {
    target,
    transport,
    status: "passed",
    capturedAt: new Date().toISOString(),
    candidateDigest: identity.candidateDigest,
    environment: captured.environment,
    topology: {
      kind: transport === "direct" ? "lan_or_private_network" : "self_hosted_relay",
      endpointSha256: endpointSha256(endpoint),
      selfHosted: true,
      e2ee: transport === "relay",
      officialService: false
    },
    workflows: captured.workflows,
    permissionContractSha256: captured.permissionContractSha256,
    redactionScan: "passed"
  };
}

async function main() {
  if (PAIR_VIA_PRODUCT) {
    const target = option("target");
    assert(
      target === null || target === "mobile-wasm-host",
      "product_pairing_alias_target_invalid"
    );
    const transport = option("transport", option("mode"));
    const args = [join(ROOT, "scripts/e2e-desktop-pairing.mjs")];
    if (transport) args.push("--transport", transport);
    if (WRITE) args.push("--write");
    run(process.execPath, args, {
      code: "product_pairing_alias_failed",
      inherit: true
    });
    return;
  }
  const requestedTarget = option("target");
  const transport = option("transport");
  assert(
    ["mobile-wasm-host", "android"].includes(requestedTarget),
    "target_must_be_mobile_wasm_host_or_android"
  );
  assert(["direct", "relay"].includes(transport), "transport_must_be_direct_or_relay");
  const credentials = loadCredentials(
    option("credentials", process.env.VIBEX_E2E_CREDENTIALS_FILE)
  );
  const endpoint = assertRoute(credentials, transport);
  const recoveryHook = option("recovery-hook", process.env.VIBEX_E2E_RECOVERY_HOOK);
  const fixtureHook = option("fixture-hook", process.env.VIBEX_E2E_FIXTURE_HOOK);
  const relayLog = option("relay-log", process.env.VIBEX_E2E_RELAY_LOG_FILE);
  const target = requestedTarget === "mobile-wasm-host" ? "mobile_wasm_host" : "android_physical";
  const fixture = prepareDisposableFixture(fixtureHook, target, transport);

  let identity;
  let captured;
  try {
    identity = resolveWorkflowCandidateIdentity(ROOT);
    captured =
      requestedTarget === "mobile-wasm-host"
        ? await developmentHostTarget(credentials, recoveryHook, transport, relayLog, fixture)
        : await androidTarget(
            identity,
            credentials,
            recoveryHook,
            transport,
            relayLog,
            fixture
          );
  } finally {
    cleanupDisposableFixture(fixtureHook, target, transport);
  }
  identity = resolveWorkflowCandidateIdentity(ROOT);
  const result = targetEvidence(identity, target, transport, endpoint, captured);

  if (WRITE) {
    const path = join(ROOT, WORKFLOW_EVIDENCE_PATH);
    const existing = existsSync(path) ? JSON.parse(readFileSync(path, "utf8")) : null;
    const evidence = mergeWorkflowTarget(existing, identity, target, transport, result);
    writeFileSync(path, `${JSON.stringify(evidence, null, 2)}\n`);
  }
  console.log(`GPUI workflow E2E passed: ${target}/${transport}`);
}

const DIRECT_EXECUTION =
  process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);

if (DIRECT_EXECUTION) {
  try {
    await main();
  } catch (error) {
    const code = /^[a-z0-9_]+$/.test(error?.code ?? "")
      ? error.code
      : "workflow_e2e_failed";
    console.error(`GPUI workflow E2E failed: ${code}`);
    process.exitCode = 1;
  }
}

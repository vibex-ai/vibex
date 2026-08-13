// Verbose replica of the Mobile WASM development-host workflow against the
// local harness. Prints every stage and relevant state snapshots so failures
// pinpoint the exact step, which the sealed runner reports only as an opaque
// code.
import { readFileSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { chromium } from "@playwright/test";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
for (const key of Object.keys(process.env)) {
  if (/^(https?_proxy|all_proxy|no_proxy)$/i.test(key)) delete process.env[key];
}
const origin = process.argv[2] ?? "https://dev-home.tail525c5d.ts.net:8443";
const bundlePath = process.argv[3] ?? join(ROOT, ".e2e-local-env/mobile-wasm-direct/credentials.json");
const controlPort = process.env.VIBEX_E2E_CONTROL_PORT ?? "14321";
const REMOTE_CREDENTIAL_STORAGE_KEY = "vibex.remote-client.credentials.v1";
const credentials = JSON.parse(readFileSync(bundlePath, "utf8"));

const { startWasmServer } = await import(join(ROOT, "scripts/mobile-wasm-test-server.mjs"));
const server = await startWasmServer({ port: 14322 });

const fixtureResponse = await fetch(`http://127.0.0.1:${controlPort}/fixture/setup`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: "{}"
});
const fixture = await fixtureResponse.json();
console.log("fixture:", JSON.stringify(fixture));

const browser = await chromium.launch({
  headless: true,
  args: ["--enable-unsafe-webgpu", "--use-angle=vulkan", "--enable-features=Vulkan"]
});

function summarize(state) {
  const keys = [
    "connection", "workspaceCount", "sessionCount", "timelineRowCount", "pendingApprovalCount",
    "agentRuntimeReady", "agentMutationPhase", "agentLiveEventCount", "navigationLevel",
    "activeSurface", "fileRowCount", "fileHasActiveFile", "fileEditorStatus", "fileLiveEventCount",
    "gitChangeCount", "gitMutationPending", "gitCommitPhase", "gitLiveEventCount",
    "terminalCount", "terminalConnection", "terminalSequence",
    "managementLoadState", "managementProfileCount", "managementHealthCount",
    "managementDeviceCount", "managementRevokedDeviceCount", "managementLiveEventCount",
    "agentRecoveryCount", "fileRecoveryCount", "gitRecoveryCount", "terminalRecoveryCount",
    "managementRecoveryCount",
    "hasPairingOffer", "errorCodes"
  ];
  return Object.fromEntries(keys.map((key) => [key, state?.[key]]));
}

try {
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
  await context.addInitScript(
    ({ key, value }) => window.localStorage.setItem(key, value),
    { key: REMOTE_CREDENTIAL_STORAGE_KEY, value: JSON.stringify(credentials) }
  );
  const page = await context.newPage();
  page.on("pageerror", (error) => console.log(`[pageerror] ${error.message}`));
  const snapshot = () => page.evaluate(() => window.__VIBEX_GATE__.workflowState());
  const action = (command) =>
    page.evaluate((value) => window.__VIBEX_GATE__.workflowAction(value), command);
  const waitSnap = async (predicate, label, timeout = 45_000) => {
    const deadline = Date.now() + timeout;
    let last = null;
    while (Date.now() < deadline) {
      last = await snapshot();
      if (predicate(last)) {
        console.log(`OK ${label}`);
        return last;
      }
      await page.waitForTimeout(150);
    }
    console.log(`FAIL ${label}: ${JSON.stringify(summarize(last))}`);
    throw new Error(label);
  };
  const canvasPoint = (x, y) =>
    page.evaluate(
      ({ x, y }) => {
        const canvas = document.querySelector("canvas[data-vibex-gate-canvas]");
        const bounds = canvas.getBoundingClientRect();
        return { x: bounds.left + bounds.width * x, y: bounds.top + bounds.height * y };
      },
      { x, y }
    );
  const replaceInput = async (x, y, value, submit = false) => {
    const point = await canvasPoint(x, y);
    await page.mouse.click(point.x, point.y);
    await page.keyboard.press("Control+A");
    await page.keyboard.type(value, { delay: 2 });
    if (submit) await page.keyboard.press("Enter");
  };

  await page.goto(origin, { waitUntil: "domcontentloaded", timeout: 60_000 });
  await page.waitForFunction(
    () => {
      const gate = window.__VIBEX_GATE__;
      return gate?.wasmReady && ["ready", "offline", "revoked", "incompatible"].includes(gate.state);
    },
    null,
    { timeout: 60_000 }
  );
  await waitSnap((s) => s.connection === "online", "connect online");

  const remote = await page.evaluate(() => window.__VIBEX_GATE__.remoteState());
  for (const [domain, capability] of Object.entries(remote.capabilities)) {
    if (typeof capability === "object" && capability?.availability) {
      console.log(`capability ${domain}: ${capability.availability} [${(capability.operations ?? []).join(",")}]`);
    }
  }

  await action({ kind: "select_workspace", index: fixture.workspaceIndex });
  await waitSnap(
    (s) => s.connection === "online" && s.workspaceCount > 0 && s.sessionCount > 0,
    "bootstrap workspace+session"
  );

  const nonce = Math.random().toString(16).slice(2, 10);

  // Agent
  await action({ kind: "select_surface", surface: "agent" });
  let state = await waitSnap((s) => s.sessionCount > 0 && s.workspaceCount > 0, "agent bootstrap");
  await action({ kind: "select_session", index: fixture.sessionIndex });
  state = await waitSnap(
    (s) => s.navigationLevel === "session" && s.activeSurface === "agent" && s.agentRuntimeReady,
    "agent session open"
  );
  const timelineBefore = state.timelineRowCount;
  const liveBefore = state.agentLiveEventCount;
  await replaceInput(0.72, 0.93, `Vibex E2E ${nonce}: reply OK`, true);
  state = await waitSnap(
    (s) =>
      s.agentMutationPhase === "ready" &&
      s.timelineRowCount > timelineBefore &&
      s.agentLiveEventCount > liveBefore,
    "agent message + live event",
    90_000
  );
  while (state.pendingApprovalCount > 0) {
    await action({ kind: "resolve_approval", index: 0, approve: true });
    state = await waitSnap(
      (s) => s.pendingApprovalCount < state.pendingApprovalCount,
      "agent approval resolution"
    );
  }

  // Files
  await action({ kind: "select_surface", surface: "files" });
  state = await waitSnap((s) => s.activeSurface === "files" && s.fileRowCount > 0, "file tree");
  for (let index = 0; index < Math.min(state.fileRowCount, 32); index += 1) {
    await action({ kind: "open_file_row", index });
    state = await waitSnap((s) => s.fileEditorStatus !== "loading", `file open ${index}`, 10_000);
    if (state.fileHasActiveFile && ["clean", "saved", "dirty"].includes(state.fileEditorStatus)) break;
  }
  if (!state.fileHasActiveFile) throw new Error("editable_file_missing");
  await replaceInput(0.72, 0.56, `vibex workflow e2e ${nonce}\n`);
  state = await waitSnap((s) => s.fileEditorStatus === "dirty", "file edit observed");
  const fileLiveBefore = state.fileLiveEventCount;
  await action({ kind: "save_file" });
  await waitSnap(
    (s) => s.fileEditorStatus === "saved" && s.fileLiveEventCount > fileLiveBefore,
    "file save + live event"
  );

  // Git
  await action({ kind: "select_surface", surface: "git" });
  state = await waitSnap((s) => s.activeSurface === "git" && s.gitChangeCount > 0, "git changes");
  await action({ kind: "load_git_diff", index: 0 });
  const gitLiveBefore = state.gitLiveEventCount;
  await action({ kind: "stage_git_change", index: 0 });
  state = await waitSnap(
    (s) => !s.gitMutationPending && s.gitLiveEventCount > gitLiveBefore,
    "git stage + live event"
  );
  await page.screenshot({ path: "/tmp/git-surface.png" });
  await replaceInput(0.73, 0.88, `test: gpui workflow ${nonce}`);
  await action({ kind: "prepare_commit" });
  await action({ kind: "confirm_commit" });
  const gitLiveAfterStage = state.gitLiveEventCount;
  state = await waitSnap(
    (s) => s.gitCommitPhase === "ready" && s.gitLiveEventCount > gitLiveAfterStage,
    "git commit + live event",
    60_000
  );

  // Terminal
  await action({ kind: "select_surface", surface: "terminal" });
  state = await snapshot();
  const terminalCount = state.terminalCount;
  await action({ kind: "create_terminal" });
  state = await waitSnap(
    (s) =>
      s.activeSurface === "terminal" &&
      s.terminalCount > terminalCount &&
      ["connected", "connecting"].includes(s.terminalConnection),
    "terminal create/attach"
  );
  const sequence = state.terminalSequence;
  await page.screenshot({ path: "/tmp/terminal-surface.png" });
  await replaceInput(0.5, 0.91, `printf vibex-e2e-${nonce}`, true);
  await waitSnap(
    (s) => s.terminalSequence > sequence && s.terminalConnection === "connected",
    "terminal live output",
    60_000
  );
  await action({ kind: "resize_terminal", rows: 30, cols: 100 });
  await page.waitForTimeout(300);

  // Management
  await action({ kind: "select_surface", surface: "management" });
  state = await waitSnap(
    (s) =>
      s.activeSurface === "management" &&
      ["ready", "partial"].includes(s.managementLoadState) &&
      !s.managementOperationPending,
    "management refresh"
  );
  await action({ kind: "run_health_probes" });
  state = await waitSnap(
    (s) => !s.managementOperationPending && s.managementHealthCount > 0,
    "management health",
    60_000
  );
  await action({ kind: "select_management_section", section: "devices" });
  console.log("deviceCount", state.managementDeviceCount, "fixture deviceIndex", fixture.deviceIndex);
  const mgmtLiveBefore = state.managementLiveEventCount;
  await action({ kind: "create_pairing_offer" });
  state = await waitSnap(
    (s) => !s.managementOperationPending && s.hasPairingOffer && s.managementLiveEventCount > mgmtLiveBefore,
    "management pairing offer + live event"
  );
  const revokedBefore = state.managementRevokedDeviceCount;
  const mgmtLiveAfterPairing = state.managementLiveEventCount;
  await action({ kind: "revoke_device", index: fixture.deviceIndex });
  state = await waitSnap(
    (s) =>
      !s.managementOperationPending &&
      s.managementRevokedDeviceCount > revokedBefore &&
      s.managementLiveEventCount > mgmtLiveAfterPairing,
    "management revoke + live event"
  );
  const mgmtLiveAfterRevoke = state.managementLiveEventCount;
  await action({ kind: "cancel_pairing_offer" });
  await waitSnap(
    (s) => !s.managementOperationPending && !s.hasPairingOffer && s.managementLiveEventCount > mgmtLiveAfterRevoke,
    "management pairing cancel + live event"
  );

  // Recovery
  const { execFileSync } = await import("node:child_process");
  const recoveryHook = join(ROOT, "scripts/e2e-local-env/recovery-hook.sh");
  const transport = process.env.VIBEX_E2E_TRANSPORT ?? "direct";
  const runHook = (hookAction) =>
    execFileSync(recoveryHook, [hookAction, "mobile_wasm_host", transport], {
      env: { ...process.env, VIBEX_E2E_CONTROL_PORT: controlPort }
    });
  const baseline = await snapshot();
  console.log("recovery baseline:", JSON.stringify(summarize(baseline)));
  runHook("disconnect");
  await waitSnap((s) => s.connection !== "online", "recovery disconnect observed");
  runHook("reconnect");
  await page.evaluate(() => {
    window.__VIBEX_GATE__.remoteLifecycle({ kind: "network_changed" });
  });
  const deadline = Date.now() + 90_000;
  while (Date.now() < deadline) {
    const remote = await page.evaluate(() => window.__VIBEX_GATE__.remoteState());
    if (remote.connection?.state === "online") break;
    await page.waitForTimeout(250);
  }
  await page.evaluate(() => {
    window.__VIBEX_GATE__.remoteLifecycle({ kind: "network_changed" });
  });
  await action({ kind: "refresh_all" });
  await waitSnap(
    (s) =>
      s.connection === "online" &&
      s.sessionCount >= baseline.sessionCount &&
      s.agentRecoveryCount > baseline.agentRecoveryCount &&
      s.fileRecoveryCount > baseline.fileRecoveryCount &&
      s.gitRecoveryCount > baseline.gitRecoveryCount &&
      s.terminalRecoveryCount > baseline.terminalRecoveryCount &&
      s.managementRecoveryCount > baseline.managementRecoveryCount,
    "recovery authoritative refetch",
    90_000
  );
  await action({ kind: "close_terminal" });
  await waitSnap((s) => s.terminalConnection === "closed", "terminal close");
  console.log("ALL WORKFLOW STEPS PASSED");
} finally {
  await fetch(`http://127.0.0.1:${controlPort}/fixture/cleanup`, { method: "POST", body: "{}" }).catch(() => {});
  await browser.close();
  await server.close();
}

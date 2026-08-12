import { createPlatformCompatibility } from "./platform-compat.js";
import {
  createHostServices,
  validateCredentialBundle
} from "./host-services.js";

const statusLayer = document.querySelector("#gate-status-layer");
const statusTitle = document.querySelector("#gate-status-title");
const statusDetail = document.querySelector("#gate-status-detail");
const statusCode = document.querySelector("#gate-status-code");
const safeAreaProbe = document.querySelector("#safe-area-probe");
const pairingLayer = document.querySelector("#pairing-layer");
const pairingSheet = pairingLayer.querySelector(".pairing-sheet");
const pairingTitle = document.querySelector("#pairing-title");
const pairingDetail = document.querySelector("#pairing-detail");
const pairingPreview = document.querySelector("#pairing-preview");
const pairingDesktop = document.querySelector("#pairing-desktop");
const pairingPermission = document.querySelector("#pairing-permission");
const pairingEntry = document.querySelector("#pairing-entry");
const pairingCountdown = document.querySelector("#pairing-countdown");
const pairingStatus = document.querySelector("#pairing-status");
const pairingConfirm = document.querySelector("#pairing-confirm");
const pairingCancel = document.querySelector("#pairing-cancel");
const pairingScan = document.querySelector("#pairing-scan");
const pairingClear = document.querySelector("#pairing-clear");
const parameters = new URLSearchParams(location.search);
const diagnosticsGate = parameters.get("diagnostics") === "gate";
let wasmRuntime = null;
let hostServices = null;
let pairingContext = null;
let previousCredentialBundle = null;
let pairingExpiryTimer = null;
let pairingGeneration = 0;
let pairingPreviousFocus = null;
let remoteStateTimer = null;
const REMOTE_STATE_FAST_POLL_MS = 250;
const REMOTE_STATE_OFFLINE_POLL_MS = 1000;
const REMOTE_STATE_STEADY_POLL_MS = 2500;
const REMOTE_STATE_TRANSITIONAL = new Set([
  "resolving",
  "probing",
  "connecting",
  "authenticating",
  "syncing",
  "reconnecting"
]);

const runtime = {
  schemaVersion: "vibex-browser-host.v1",
  state: "loading",
  bootStartedAt: performance.now(),
  readyAt: null,
  contract: null,
  compatibility: null,
  adapter: null,
  pixelMetrics: null,
  build: null,
  errors: [],
  events: [],
  probes: {
    storage: { status: "pending" },
    secureStorage: { status: "pending" },
    fetch: { status: "pending" },
    webSocket: { status: "pending" }
  },
  interactions: {
    pointer: 0,
    touch: 0,
    wheel: 0,
    keyboard: 0,
    beforeInput: 0,
    input: 0,
    paste: 0,
    compositionStart: 0,
    compositionUpdate: 0,
    compositionEnd: 0
  },
  wasm: null,
  sequence: 0,
  gpuiBooted: false,
  wasmReady: false,
  host: null,
  remote: {
    state: "unconfigured",
    pairing: {
      schemaVersion: "vibex-pairing-state.v1",
      state: "idle"
    },
    pendingDeepLink: null,
    lastError: null,
    page: null
  }
};

Object.defineProperty(window, "__VIBEX_GATE__", {
  configurable: false,
  enumerable: false,
  value: runtime,
  writable: false
});

function setState(state, title, detail, code) {
  runtime.state = state;
  document.body.dataset.gateState = state;
  statusTitle.textContent = title;
  statusDetail.textContent = detail;
  statusCode.textContent = code;
  statusLayer.classList.toggle("is-hidden", state === "ready");
}

function unsupported(code, detail) {
  setState("unsupported", "This browser cannot run Vibex", detail, code);
}

function fail(code, error) {
  const message = error instanceof Error ? error.message : String(error);
  runtime.errors.push({ code, message });
  setState("error", "GPUI could not start", message, code);
}

function withTimeout(promise, milliseconds, label) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${label} timed out after ${milliseconds}ms`)), milliseconds);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

function finite(value, fallback = 0) {
  return Number.isFinite(value) ? value : fallback;
}

function cssPixels(value) {
  const parsed = Number.parseFloat(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

function safeArea() {
  const style = getComputedStyle(safeAreaProbe);
  return {
    top: cssPixels(style.paddingTop),
    right: cssPixels(style.paddingRight),
    bottom: cssPixels(style.paddingBottom),
    left: cssPixels(style.paddingLeft)
  };
}

function viewportEvent() {
  const visual = window.visualViewport;
  const width = finite(visual?.width, window.innerWidth);
  const height = finite(visual?.height, window.innerHeight);
  const keyboard = runtime.compatibility?.keyboardSnapshot() ?? {
    visible: false,
    inset: 0,
    source: "none"
  };
  return {
    kind: "viewport",
    width,
    height,
    device_pixel_ratio: finite(window.devicePixelRatio, 1),
    keyboard_visible: keyboard.visible,
    keyboard_inset: keyboard.inset,
    keyboard_source: keyboard.source,
    safe_area: safeArea()
  };
}

function emitHostEvent(event, source = "browser") {
  if (!wasmRuntime) return null;
  const envelope = { sequence: ++runtime.sequence, ...event };
  const result = JSON.parse(wasmRuntime.host_event(JSON.stringify(envelope)));
  runtime.events.push({ envelope, result, source, at: performance.now() });
  if (runtime.events.length > 256) runtime.events.shift();
  return result;
}

runtime.emitHostEvent = (event) => emitHostEvent(event, "test_bridge");
runtime.hostSnapshot = () => (wasmRuntime ? JSON.parse(wasmRuntime.host_snapshot()) : null);
runtime.fixtureState = () => (wasmRuntime ? JSON.parse(wasmRuntime.fixture_state()) : null);
runtime.rootState = () => (wasmRuntime ? JSON.parse(wasmRuntime.root_state()) : null);
runtime.compatibilitySnapshot = () => runtime.compatibility?.snapshot() ?? null;
runtime.remoteState = () => (wasmRuntime ? JSON.parse(wasmRuntime.remote_state()) : null);
runtime.workflowState = () => {
  if (!wasmRuntime || typeof wasmRuntime.workflow_state !== "function") return null;
  return JSON.parse(wasmRuntime.workflow_state());
};
runtime.workflowAction = (command) => {
  if (!wasmRuntime || typeof wasmRuntime.workflow_action !== "function") {
    throw new Error("GPUI workflow workbench is not ready");
  }
  return JSON.parse(wasmRuntime.workflow_action(JSON.stringify(command)));
};
runtime.navigationState = () => (wasmRuntime ? JSON.parse(wasmRuntime.navigation_state()) : null);
runtime.navigationAction = (command) => {
  if (!wasmRuntime) throw new Error("GPUI runtime is not ready");
  return JSON.parse(wasmRuntime.navigation_action(JSON.stringify(command)));
};
runtime.remoteLifecycle = (command) => {
  if (!wasmRuntime || typeof wasmRuntime.remote_lifecycle !== "function") {
    throw new Error("remote runtime is not ready");
  }
  return JSON.parse(wasmRuntime.remote_lifecycle(JSON.stringify(command)));
};
runtime.sampleFrames = (count = 90) =>
  new Promise((resolve) => {
    const samples = [];
    let previous = performance.now();
    function frame(now) {
      samples.push(now - previous);
      previous = now;
      if (samples.length >= count) resolve(samples);
      else requestAnimationFrame(frame);
    }
    requestAnimationFrame(frame);
  });
runtime.requestFullscreen = async () => {
  const canvas = document.querySelector("canvas[data-vibex-gate-canvas]");
  if (!canvas?.requestFullscreen) return false;
  await canvas.requestFullscreen();
  return document.fullscreenElement === canvas;
};

function parseRemoteError(error, fallbackCode = "remote_operation_failed") {
  if (typeof error === "string") {
    try {
      const parsed = JSON.parse(error);
      if (parsed && typeof parsed.code === "string") return parsed;
    } catch {
      // The bridge may reject with a plain JS message; keep it out of the
      // durable/runtime event log and expose only a stable code.
    }
  }
  if (error && typeof error === "object" && typeof error.code === "string") return error;
  return { code: fallbackCode, kind: "failed", message: "remote operation failed" };
}

const PAIRING_UI = Object.freeze({
  unpaired: {
    title: "Pair this device",
    detail: "Open a current pairing link from Vibex Desktop.",
    status: "No Desktop is paired with this browser."
  },
  preview: {
    title: "Pair with Desktop",
    detail: "Confirm the Desktop and access level before pairing.",
    status: "The offer is ready for confirmation."
  },
  claiming: {
    title: "Pairing with Desktop",
    detail: "The one-time offer is being claimed.",
    status: "Waiting for the Desktop to approve this device."
  },
  persisting: {
    title: "Securing this device",
    detail: "The paired device credential is being committed.",
    status: "Verifying secure storage before connecting."
  },
  connecting: {
    title: "Connecting to Desktop",
    detail: "The saved device is opening a secure session.",
    status: "Selecting the best available route."
  },
  online: {
    title: "Device paired",
    detail: "This device is connected to Vibex Desktop.",
    status: "Remote access is online."
  },
  expired: {
    title: "Pairing offer expired",
    detail: "Create a new pairing offer on Vibex Desktop.",
    status: "No credential was changed."
  },
  invalid: {
    title: "Pairing link is invalid",
    detail: "Open a newly generated Vibex pairing link.",
    status: "The link was rejected before any network request."
  },
  route_error: {
    title: "Pairing entry does not match",
    detail: "Open the selected Desktop or Relay entry and generate a new offer.",
    status: "The offer was not replayed on another route."
  },
  access_error: {
    title: "Desktop entry was blocked",
    detail: "Check Tailscale, TLS, and Desktop remote access, then scan a new offer.",
    status: "The app could not use the selected pairing entry."
  },
  storage_error: {
    title: "This device could not be saved",
    detail: "Local secure storage did not verify the new credential.",
    status: "The previous local device was restored when possible."
  },
  offline: {
    title: "Desktop is offline",
    detail: "The saved device will reconnect when its Desktop is reachable.",
    status: "The local credential has been retained."
  },
  revoked: {
    title: "This device was revoked",
    detail: "Clear this local device, then pair it again from Desktop.",
    status: "Automatic reconnect has stopped."
  },
  incompatible: {
    title: "Vibex needs an update",
    detail: "The Desktop and this client do not share a protocol version.",
    status: "Update Vibex before pairing again."
  },
  identity_mismatch: {
    title: "Desktop identity changed",
    detail: "The saved server identity does not match this Desktop.",
    status: "The connection was blocked."
  },
  credential_corrupt: {
    title: "Local device data is damaged",
    detail: "Clear this local device, then pair it again.",
    status: "No connection attempt was made."
  },
  scanning: {
    title: "Scan pairing QR",
    detail: "Point the camera at the QR shown by Vibex Desktop.",
    status: "Waiting for a QR result."
  },
  scan_canceled: {
    title: "Scan canceled",
    detail: "No pairing data was retained.",
    status: "Scan again when ready."
  },
  camera_denied: {
    title: "Camera access is off",
    detail: "Allow camera access in system settings to scan a pairing QR.",
    status: "No pairing data was retained."
  },
  scanner_unavailable: {
    title: "QR scanner is unavailable",
    detail: "Open a Vibex App Link from the system camera instead.",
    status: "This installation cannot start the native scanner."
  }
});

const PAIRING_BUSY_STATES = new Set(["claiming", "persisting", "connecting", "scanning"]);
const PAIRING_CLEAR_STATES = new Set([
  "offline",
  "revoked",
  "incompatible",
  "identity_mismatch",
  "credential_corrupt",
  "storage_error"
]);
const PAIRING_SCAN_STATES = new Set([
  "unpaired",
  "expired",
  "invalid",
  "route_error",
  "access_error",
  "scan_canceled",
  "camera_denied",
  "scanner_unavailable",
  "credential_corrupt",
  "revoked"
]);

function permissionLabel(value) {
  return {
    read_only: "Read only",
    approve_only: "Approve actions",
    full_control: "Full control"
  }[value] ?? "Limited access";
}

function entryLabel(source, entryType) {
  if (source === "qr_scan") return "QR scan";
  if (source === "app_link") return entryType === "custom_scheme" ? "App Link" : "Universal Link";
  return "Browser link";
}

function pairingErrorState(error, phase = "pairing") {
  const parsed = parseRemoteError(error, `${phase}_failed`);
  const code = String(parsed.code ?? "").toLowerCase();
  if (code.includes("expired")) return { state: "expired", code: parsed.code };
  if (code.includes("entry") || code.includes("route") || code.includes("candidate")) {
    return { state: "route_error", code: parsed.code };
  }
  if (code.includes("browser_network_policy") || code.includes("origin_rejected")) {
    return { state: "access_error", code: parsed.code };
  }
  if (code.includes("storage")) return { state: "storage_error", code: parsed.code };
  if (code.includes("identity") || code.includes("server_pin")) {
    return { state: "identity_mismatch", code: parsed.code };
  }
  if (code.includes("credential")) return { state: "credential_corrupt", code: parsed.code };
  if (code.includes("incompatible") || code.includes("protocol")) {
    return { state: "incompatible", code: parsed.code };
  }
  if (code.includes("revoked")) return { state: "revoked", code: parsed.code };
  if (code.includes("permission") && phase === "scan") {
    return { state: "camera_denied", code: parsed.code };
  }
  if (code.includes("unsupported") && phase === "scan") {
    return { state: "scanner_unavailable", code: parsed.code };
  }
  if (code.includes("cancel") && phase === "scan") {
    return { state: "scan_canceled", code: parsed.code };
  }
  if (
    parsed.kind === "offline" ||
    code.includes("offline") ||
    code.includes("connect") ||
    code.includes("network") ||
    code.includes("socket")
  ) {
    return { state: "offline", code: parsed.code };
  }
  return { state: phase === "preview" || phase === "scan" ? "invalid" : "route_error", code: parsed.code };
}

function pairingPublicProjection(state, preview = null, errorCode = null) {
  const projection = {
    schemaVersion: "vibex-pairing-state.v1",
    state
  };
  if (preview) {
    projection.desktopIdentity = preview.desktopIdentity ?? "Desktop";
    projection.permissionLevel = preview.permissionLevel ?? null;
    projection.expiresAtMs = Number.isFinite(preview.expiresAtMs) ? preview.expiresAtMs : null;
    projection.source = preview.source ?? null;
    projection.entryType = preview.entryType ?? null;
    projection.hasDirectCandidate = Boolean(preview.hasDirectCandidate);
    projection.hasTailnetCandidate = Boolean(preview.hasTailnetCandidate);
    projection.hasRelayCandidate = Boolean(preview.hasRelayCandidate);
  }
  if (errorCode) projection.errorCode = errorCode;
  return projection;
}

function pairingFocusableElements() {
  return [...pairingLayer.querySelectorAll("button:not([disabled]):not(.is-hidden), [tabindex='0']")]
    .filter((element) => !element.hidden);
}

function openPairingLayer() {
  if (pairingLayer.classList.contains("is-hidden")) {
    pairingPreviousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  }
  pairingLayer.classList.remove("is-hidden");
  pairingLayer.setAttribute("aria-hidden", "false");
  requestAnimationFrame(() => (pairingFocusableElements()[0] ?? pairingSheet).focus());
}

function closePairingLayer() {
  pairingLayer.classList.add("is-hidden");
  pairingLayer.setAttribute("aria-hidden", "true");
  if (pairingPreviousFocus?.isConnected) pairingPreviousFocus.focus();
  pairingPreviousFocus = null;
}

function updatePairingCountdown() {
  const expiresAtMs = runtime.remote.pairing?.expiresAtMs;
  if (!Number.isFinite(expiresAtMs)) {
    pairingCountdown.textContent = "--:--";
    return;
  }
  const remaining = Math.max(0, expiresAtMs - Date.now());
  const seconds = Math.ceil(remaining / 1000);
  pairingCountdown.textContent = `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
  pairingCountdown.dateTime = new Date(expiresAtMs).toISOString();
  if (remaining === 0 && runtime.remote.pairing.state === "preview") {
    void expirePendingPairing();
  }
}

function setPairingState(state, { preview = pairingContext?.preview ?? null, errorCode = null, status = null } = {}) {
  const copy = PAIRING_UI[state] ?? PAIRING_UI.invalid;
  runtime.remote.pairing = pairingPublicProjection(state, preview, errorCode);
  pairingTitle.textContent = copy.title;
  pairingDetail.textContent = copy.detail;
  pairingStatus.textContent = status ?? copy.status;
  pairingDesktop.textContent = preview?.desktopIdentity ?? "Desktop";
  pairingPermission.textContent = permissionLabel(preview?.permissionLevel);
  pairingEntry.textContent = entryLabel(preview?.source, preview?.entryType);
  const showPreview = Boolean(preview) && !["unpaired", "scanning", "scan_canceled", "camera_denied", "scanner_unavailable"].includes(state);
  pairingPreview.classList.toggle("is-hidden", !showPreview);

  const busy = PAIRING_BUSY_STATES.has(state);
  const canScan = hostServices?.snapshot().capabilities?.qrScanner === "supported";
  pairingConfirm.classList.toggle("is-hidden", !["preview", "offline"].includes(state));
  pairingConfirm.textContent = state === "offline" ? "Retry" : "Pair device";
  pairingConfirm.disabled = busy;
  pairingCancel.classList.toggle("is-hidden", state === "unpaired" || state === "online");
  pairingCancel.textContent = state === "preview" ? "Cancel" : "Close";
  pairingCancel.disabled = busy;
  pairingScan.classList.toggle("is-hidden", !(canScan && PAIRING_SCAN_STATES.has(state)));
  pairingScan.disabled = busy;
  pairingClear.classList.toggle("is-hidden", !PAIRING_CLEAR_STATES.has(state));
  pairingClear.disabled = busy;

  if (pairingExpiryTimer !== null) {
    clearInterval(pairingExpiryTimer);
    pairingExpiryTimer = null;
  }
  if (state === "preview") {
    updatePairingCountdown();
    pairingExpiryTimer = setInterval(updatePairingCountdown, 500);
  }
  if (state === "idle") closePairingLayer();
  else openPairingLayer();
}

function releasePairingSecret() {
  pairingContext = null;
  if (pairingExpiryTimer !== null) clearInterval(pairingExpiryTimer);
  pairingExpiryTimer = null;
}

function projectRemoteRecovery(snapshot) {
  if (pairingContext || PAIRING_BUSY_STATES.has(runtime.remote.pairing.state)) return;
  const connection = snapshot?.connection;
  const state = connection?.state;
  const errorCode = String(connection?.lastErrorCode ?? "");
  if (state === "online") {
    if (["offline", "connecting"].includes(runtime.remote.pairing.state)) {
      setPairingState("idle", { preview: null });
    }
    return;
  }
  if (state === "revoked") {
    setPairingState("revoked", { preview: null, errorCode: errorCode || null });
  } else if (state === "incompatible") {
    setPairingState("incompatible", { preview: null, errorCode: errorCode || null });
  } else if (errorCode.includes("identity")) {
    setPairingState("identity_mismatch", { preview: null, errorCode });
  } else if (snapshot?.configured && ["offline", "degraded"].includes(state)) {
    setPairingState("offline", { preview: null, errorCode: errorCode || null });
  }
}

function updateRemoteSnapshot() {
  if (!wasmRuntime || typeof wasmRuntime.remote_state !== "function") return null;
  try {
    const snapshot = JSON.parse(wasmRuntime.remote_state());
    runtime.remote.state = snapshot.connection?.state ?? "unconfigured";
    runtime.remote.snapshot = snapshot;
    projectRemoteRecovery(snapshot);
    return snapshot;
  } catch {
    runtime.remote.lastError = { code: "remote_state_unavailable" };
    return null;
  }
}

function scheduleRemoteStateSync() {
  if (remoteStateTimer !== null) return;
  const poll = () => {
    remoteStateTimer = null;
    const snapshot = updateRemoteSnapshot();
    const state = snapshot?.connection?.state;
    if (!snapshot?.configured || ["revoked", "incompatible"].includes(state)) return;
    const delay = REMOTE_STATE_TRANSITIONAL.has(state)
      ? REMOTE_STATE_FAST_POLL_MS
      : ["offline", "degraded"].includes(state)
        ? REMOTE_STATE_OFFLINE_POLL_MS
        : REMOTE_STATE_STEADY_POLL_MS;
    remoteStateTimer = setTimeout(poll, delay);
  };
  remoteStateTimer = setTimeout(poll, 100);
}

function recordRemoteError(error, fallbackCode = "remote_operation_failed") {
  const parsed = parseRemoteError(error, fallbackCode);
  runtime.remote.lastError = {
    code: parsed.code,
    kind: parsed.kind ?? "failed",
    recoveryHint: typeof parsed.recoveryHint === "string" ? parsed.recoveryHint : null
  };
  const code = String(parsed.code ?? "");
  if (code.includes("identity") || code.includes("server_pin")) {
    runtime.remote.page = {
      state: "identity_mismatch",
      title: "Desktop identity changed",
      detail: "The saved server identity does not match this Desktop.",
      code: "REMOTE_IDENTITY_MISMATCH"
    };
  } else if (code.includes("credential")) {
    runtime.remote.page = {
      state: "credential_corrupt",
      title: "Local device data is damaged",
      detail: "Clear this local device, then pair it again.",
      code: "REMOTE_CREDENTIAL_CORRUPT"
    };
  } else if (code.includes("incompatible") || code.includes("protocol")) {
    runtime.remote.page = {
      state: "incompatible",
      title: "Vibex needs an update",
      detail: "The paired desktop does not support this protocol version.",
      code: "REMOTE_PROTOCOL_INCOMPATIBLE"
    };
  } else if (code.includes("revoked") || code.includes("permission")) {
    runtime.remote.page = {
      state: "revoked",
      title: "This device is no longer paired",
      detail: "Re-pair this device from the desktop ManagementCenter.",
      code: "REMOTE_DEVICE_REVOKED"
    };
  } else if (parsed.kind === "offline" || code.includes("offline") || code.includes("connect")) {
    runtime.remote.page = {
      state: "offline",
      title: "Vibex is offline",
      detail: "The desktop runtime could not be reached. Vibex will retry when the network returns.",
      code: "REMOTE_OFFLINE"
    };
  }
  // Connection failures are product workbench states. Keep the GPUI canvas
  // visible and expose only the typed, redacted page snapshot to diagnostics.
  updateRemoteSnapshot();
  return runtime.remote.lastError;
}

function configureRemote(bundle) {
  const valid = validateCredentialBundle(bundle);
  wasmRuntime.configure_remote(JSON.stringify(valid));
  updateRemoteSnapshot();
  scheduleRemoteStateSync();
  return valid;
}

async function connectConfigured() {
  try {
    const state = JSON.parse(await wasmRuntime.connect_remote());
    runtime.remote.state = state.connection?.state ?? "online";
    runtime.remote.snapshot = state;
    runtime.remote.lastError = null;
    runtime.remote.page = null;
    await resolvePendingDeepLink();
    if (runtime.gpuiBooted) {
      setState("ready", "Vibex is ready", "The GPUI canvas rendered.", "GATE_READY");
    }
    return state;
  } catch (error) {
    const parsed = parseRemoteError(error, "remote_connect_failed");
    throw Object.assign(new Error("remote connection failed"), {
      code: parsed.code,
      kind: parsed.kind,
      cause: error
    });
  }
}

async function configureAndConnect(bundle) {
  configureRemote(bundle);
  return connectConfigured();
}

async function disconnectConfigured() {
  const snapshot = updateRemoteSnapshot();
  if (!snapshot?.configured || typeof wasmRuntime?.disconnect_remote !== "function") return;
  try {
    await wasmRuntime.disconnect_remote();
  } catch {
    // Reconfiguration replaces the runtime below; disconnect is best-effort.
  }
  updateRemoteSnapshot();
}

async function rollbackStoredCredential(backup) {
  if (backup) await hostServices.writeCredentialBundle(backup);
  else await hostServices.clearCredentialBundle();
}

async function resumeCredential(bundle, { preservePairingState = false } = {}) {
  if (!bundle) return false;
  try {
    await configureAndConnect(bundle);
    if (!preservePairingState) setPairingState("idle", { preview: null });
    return true;
  } catch (error) {
    recordRemoteError(error, "previous_credential_restore_failed");
    if (!preservePairingState) {
      const recovery = pairingErrorState(error, "restore");
      setPairingState(recovery.state, { preview: null, errorCode: recovery.code });
    }
    return false;
  }
}

async function restorePreviousConnection({ preservePairingState = false } = {}) {
  let previous = previousCredentialBundle;
  previousCredentialBundle = null;
  if (!previous) {
    try {
      previous = await hostServices.readCredentialBundle();
    } catch (error) {
      recordRemoteError(error, "stored_credentials_invalid");
      if (!preservePairingState) {
        setPairingState("credential_corrupt", {
          preview: null,
          errorCode: parseRemoteError(error, "stored_credentials_invalid").code
        });
      }
      return false;
    }
  }
  if (!previous) {
    runtime.remote.state = "unconfigured";
    if (!preservePairingState) setPairingState("unpaired", { preview: null });
    return false;
  }
  return resumeCredential(previous, { preservePairingState });
}

async function claimPendingPairing() {
  const context = pairingContext;
  if (!context || context.claimStarted || runtime.remote.pairing.state !== "preview") return;
  context.claimStarted = true;
  const preview = context.preview;
  setPairingState("claiming", { preview });
  const clientType = hostServices.isNative ? "mobile" : "browser";
  const allowInsecureLocalDev = runtime.build?.profile !== "release" &&
    ["localhost", "127.0.0.1", "::1"].includes(location.hostname);
  const options = {
    displayName: clientType === "mobile" ? "Vibex Mobile" : "Vibex Web",
    clientType,
    allowInsecureLocalDev,
    nowMs: Date.now(),
    entryHint: context.entryHint
  };
  let claim;
  try {
    claim = JSON.parse(await wasmRuntime.claim_pairing_fragment(
      context.fragment,
      JSON.stringify(options)
    ));
    if (claim?.schemaVersion !== "vibex-pairing-claim.v1") {
      throw Object.assign(new Error("pairing claim response is incompatible"), {
        code: "remote_pairing_claim_response_invalid"
      });
    }
  } catch (error) {
    releasePairingSecret();
    recordRemoteError(error, "pairing_claim_failed");
    const recovery = pairingErrorState(error, "claim");
    setPairingState(recovery.state, { preview, errorCode: recovery.code });
    void restorePreviousConnection({ preservePairingState: true });
    return;
  }

  let credentials;
  let backup = previousCredentialBundle;
  try {
    credentials = validateCredentialBundle(claim.credentials);
    if (!backup) backup = await hostServices.readCredentialBundle();
    setPairingState("persisting", { preview });
    await hostServices.writeCredentialBundle(credentials);
    const verified = await hostServices.readCredentialBundle();
    if (!verified || JSON.stringify(verified) !== JSON.stringify(credentials)) {
      throw Object.assign(new Error("credential storage round-trip did not match"), {
        code: "credential_storage_verification_failed"
      });
    }
  } catch (error) {
    try {
      await rollbackStoredCredential(backup);
    } catch {
      // The visible state remains storage_error; do not expose storage values.
    }
    releasePairingSecret();
    recordRemoteError(error, "pairing_storage_failed");
    setPairingState("storage_error", {
      preview,
      errorCode: parseRemoteError(error, "pairing_storage_failed").code
    });
    if (backup) void resumeCredential(backup, { preservePairingState: true });
    return;
  }

  try {
    configureRemote(credentials);
  } catch (error) {
    try {
      await rollbackStoredCredential(backup);
    } catch {
      // Keep the typed configuration error as the primary recovery state.
    }
    releasePairingSecret();
    recordRemoteError(error, "pairing_configuration_failed");
    const recovery = pairingErrorState(error, "configure");
    setPairingState(recovery.state, { preview, errorCode: recovery.code });
    if (backup) void resumeCredential(backup, { preservePairingState: true });
    return;
  }

  releasePairingSecret();
  previousCredentialBundle = null;
  setPairingState("connecting", { preview });
  try {
    await connectConfigured();
  } catch (error) {
    recordRemoteError(error, "paired_connection_failed");
    const recovery = pairingErrorState(error, "connect");
    setPairingState(recovery.state, { preview, errorCode: recovery.code });
    return;
  }
  runtime.remote.pairedDevice = {
    displayName: claim.device?.displayName ?? null,
    permissionLevel: claim.device?.permissionLevel ?? null
  };
  setPairingState("online", { preview });
  setTimeout(() => {
    if (runtime.remote.pairing.state === "online") setPairingState("idle", { preview: null });
  }, 1200);
}

async function handlePairingFragment(item) {
  if (item?.errorCode) {
    const restorePendingConnection = Boolean(pairingContext);
    releasePairingSecret();
    setPairingState("invalid", { preview: null, errorCode: item.errorCode });
    if (restorePendingConnection) {
      void restorePreviousConnection({ preservePairingState: true });
    }
    return;
  }
  if (!wasmRuntime || !item?.fragment || !item?.entryHint) return;
  const generation = ++pairingGeneration;
  try {
    const preview = {
      ...JSON.parse(wasmRuntime.pairing_preview(item.fragment, Date.now())),
      entryType: item.entryType ?? "web_origin",
      source: item.source ?? "app_link"
    };
    releasePairingSecret();
    pairingContext = {
      fragment: item.fragment,
      entryHint: item.entryHint,
      preview,
      generation,
      claimStarted: false
    };
    setPairingState("preview", { preview });

    try {
      const stored = await hostServices.readCredentialBundle();
      if (pairingContext?.generation !== generation) return;
      previousCredentialBundle = stored;
    } catch {
      previousCredentialBundle = null;
    }
    if (pairingContext?.generation === generation) await disconnectConfigured();
  } catch (error) {
    const restorePendingConnection = Boolean(pairingContext);
    releasePairingSecret();
    recordRemoteError(error, "pairing_preview_failed");
    const recovery = pairingErrorState(error, "preview");
    setPairingState(recovery.state, {
      preview: {
        desktopIdentity: "Desktop",
        permissionLevel: null,
        entryType: item.entryType ?? null,
        source: item.source ?? null
      },
      errorCode: recovery.code
    });
    if (restorePendingConnection) {
      void restorePreviousConnection({ preservePairingState: true });
    }
  }
}

async function expirePendingPairing() {
  if (runtime.remote.pairing.state !== "preview") return;
  const preview = pairingContext?.preview ?? null;
  releasePairingSecret();
  setPairingState("expired", { preview });
  await restorePreviousConnection({ preservePairingState: true });
}

async function cancelPendingPairing() {
  if (runtime.remote.pairing.state !== "preview") return;
  releasePairingSecret();
  setPairingState("idle", { preview: null });
  await restorePreviousConnection();
}

async function retryStoredConnection() {
  setPairingState("connecting", { preview: null });
  try {
    const stored = await hostServices.readCredentialBundle();
    if (!stored) {
      setPairingState("unpaired", { preview: null });
      return;
    }
    await configureAndConnect(stored);
    setPairingState("idle", { preview: null });
  } catch (error) {
    recordRemoteError(error, "stored_credential_retry_failed");
    const recovery = pairingErrorState(error, "restore");
    setPairingState(recovery.state, { preview: null, errorCode: recovery.code });
  }
}

async function clearLocalDevice() {
  pairingClear.disabled = true;
  try {
    await disconnectConfigured();
    await hostServices.clearCredentialBundle();
    if (typeof wasmRuntime?.forget_remote === "function") {
      runtime.remote.snapshot = JSON.parse(wasmRuntime.forget_remote());
    }
    previousCredentialBundle = null;
    runtime.remote.pairedDevice = null;
    runtime.remote.lastError = null;
    runtime.remote.page = null;
    runtime.remote.state = "unconfigured";
    setPairingState("unpaired", { preview: null });
  } catch (error) {
    recordRemoteError(error, "local_device_clear_failed");
    setPairingState("storage_error", {
      preview: null,
      errorCode: parseRemoteError(error, "local_device_clear_failed").code
    });
  } finally {
    pairingClear.disabled = false;
  }
}

async function scanPairingQr() {
  setPairingState("scanning", { preview: null });
  try {
    const result = await hostServices.scanQr();
    if (result?.status === "canceled" || !result?.value) {
      setPairingState("scan_canceled", { preview: null });
      return;
    }
    const receipt = await hostServices.dispatchPairing(result.value, "qr_scan");
    if (!receipt?.accepted) {
      setPairingState("invalid", {
        preview: null,
        errorCode: receipt?.errorCode ?? "pairing_qr_invalid"
      });
    }
  } catch (error) {
    const recovery = pairingErrorState(error, "scan");
    setPairingState(recovery.state, { preview: null, errorCode: recovery.code });
  }
}

function dismissPairingRecovery() {
  if (
    runtime.remote.pairing.state === "unpaired" ||
    PAIRING_BUSY_STATES.has(runtime.remote.pairing.state)
  ) return;
  if (runtime.remote.pairing.state === "preview") {
    void cancelPendingPairing();
    return;
  }
  setPairingState("idle", { preview: null });
}

async function resolvePendingDeepLink() {
  const pending = runtime.remote.pendingDeepLink;
  if (!wasmRuntime || !pending) return null;
  const snapshot = updateRemoteSnapshot();
  if (!snapshot?.configured) {
    runtime.remote.pendingDeepLink = {
      ...pending,
      status: "waiting_for_pairing"
    };
    return runtime.remote.pendingDeepLink;
  }
  if (typeof wasmRuntime.resolve_deep_link !== "function") {
    runtime.remote.pendingDeepLink = {
      ...pending,
      status: "authoritative_fetch_unavailable",
      authoritativeFetch: "unsupported"
    };
    return runtime.remote.pendingDeepLink;
  }
  try {
    const resolution = JSON.parse(await wasmRuntime.resolve_deep_link(JSON.stringify({
      notificationId: pending.notificationId,
      opaqueLocator: pending.opaqueLocator
    })));
    const resolved = resolution.status === "resolved" && typeof resolution.sessionId === "string";
    if (resolved) {
      runtime.navigationAction({ kind: "enter_session", sessionId: resolution.sessionId });
    }
    runtime.remote.pendingDeepLink = {
      ...pending,
      status: resolved ? "authoritative_fetched" : `authoritative_fetch_${resolution.status ?? "failed"}`,
      authoritativeFetch: "remote_deep_link",
      targetSessionId: resolved ? resolution.sessionId : null,
      targetPermissionRequestId: resolution.permissionRequestId ?? null,
      connectionState: snapshot.connection?.state ?? null,
      fetchedAtMs: Date.now()
    };
  } catch (error) {
    const parsed = parseRemoteError(error, "remote_deep_link_fetch_failed");
    runtime.remote.pendingDeepLink = {
      ...pending,
      status: "authoritative_fetch_failed",
      authoritativeFetch: "remote_deep_link",
      errorCode: parsed.code,
      connectionState: snapshot.connection?.state ?? null
    };
  }
  return runtime.remote.pendingDeepLink;
}

async function handleOpaqueDeepLink(item) {
  if (!item?.opaqueLocator || !item?.notificationId) return;
  runtime.remote.pendingDeepLink = {
    notificationId: item.notificationId,
    opaqueLocator: item.opaqueLocator,
    source: item.source ?? "app_link",
    status: "pending_authoritative_fetch",
    receivedAtMs: Date.now()
  };
  await resolvePendingDeepLink();
}

function handleHostLifecycle(event) {
  if (!wasmRuntime || typeof wasmRuntime.remote_lifecycle !== "function") return;
  let command = null;
  switch (event.kind) {
    case "visibility_changed":
      command = { kind: "visibility_changed", visible: Boolean(event.visible) };
      break;
    case "network_changed":
      if (event.online !== false) command = { kind: "network_changed" };
      break;
    case "app_backgrounded":
      command = { kind: "app_backgrounded" };
      break;
    case "app_resumed":
      command = { kind: "app_resumed" };
      break;
    default:
      break;
  }
  if (!command) {
    if (event.kind === "network_lost") {
      runtime.remote.state = "offline";
      if (runtime.remote.snapshot?.configured) {
        recordRemoteError({ code: "remote_offline", kind: "offline" }, "remote_offline");
      }
    }
    return;
  }
  try {
    runtime.remote.snapshot = JSON.parse(wasmRuntime.remote_lifecycle(JSON.stringify(command)));
    runtime.remote.state = runtime.remote.snapshot.connection?.state ?? runtime.remote.state;
    scheduleRemoteStateSync();
  } catch (error) {
    if (String(error).includes("not been configured")) return;
    const parsed = parseRemoteError(error, "remote_lifecycle_failed");
    if (parsed.code !== "remote_runtime_not_configured") recordRemoteError(error, parsed.code);
  }
}

function prepareHostServices() {
  if (hostServices) return;
  hostServices = createHostServices({
    onPairingFragment: handlePairingFragment,
    onOpaqueDeepLink: handleOpaqueDeepLink,
    onLifecycle: handleHostLifecycle,
    onPushToken: (token) => {
      // Push delivery belongs to the Relay task.  Keep only a diagnostic shape
      // here; never put a provider token in runtime.events or snapshots.
      runtime.push = { status: "received", hasToken: Boolean(token), length: token?.length ?? 0 };
    }
  });
  exposeHostServices();
  runtime.hostCapabilities = () => hostServices.snapshot();
  runtime.handleDeepLinkUrl = (url, source = "push_notification") =>
    hostServices.dispatchOpaqueDeepLink(url, source);
}

async function initializeHostServices() {
  prepareHostServices();
  await hostServices.initialize();
  exposeHostServices();

  let stored;
  try {
    stored = await hostServices.readCredentialBundle();
  } catch (error) {
    if (pairingContext) return;
    recordRemoteError(error, "stored_credentials_invalid");
    setPairingState("credential_corrupt", {
      preview: null,
      errorCode: parseRemoteError(error, "stored_credentials_invalid").code
    });
    return;
  }
  // An initial link can be rejected before a pairing context exists (for
  // example an expired or malformed offer). Preserve that public recovery
  // state instead of letting the no-credential branch overwrite it.
  if (
    pairingContext ||
    !["idle", "unpaired"].includes(runtime.remote.pairing.state)
  ) {
    previousCredentialBundle = stored;
    return;
  }
  if (!stored) {
    runtime.remote.state = "unconfigured";
    if (!diagnosticsGate) setPairingState("unpaired", { preview: null });
    return;
  }
  try {
    await configureAndConnect(stored);
  } catch (error) {
    recordRemoteError(error, "stored_credentials_or_connect_failed");
    const recovery = pairingErrorState(error, "restore");
    setPairingState(recovery.state, { preview: null, errorCode: recovery.code });
  }
}

function exposeHostServices() {
  const services = hostServices;
  if (!services) return;
  runtime.host = {
    ...services.snapshot(),
    snapshot: () => services.snapshot(),
    safeArea: () => services.safeArea(),
    viewport: () => services.viewport(),
    pickFile: (accept) => services.pickFile(accept),
    captureImage: () => services.captureImage(),
    share: (request) => services.share(request),
    download: (request) => services.download(request),
    openSystemUrl: (url) => services.openSystemUrl(url),
    handleDeepLinkUrl: (url, source = "host_bridge") => services.dispatchOpaqueDeepLink(url, source)
  };
}

function installPairingControls() {
  pairingConfirm.addEventListener("click", () => {
    if (runtime.remote.pairing.state === "preview") void claimPendingPairing();
    else if (runtime.remote.pairing.state === "offline") void retryStoredConnection();
  });
  pairingCancel.addEventListener("click", dismissPairingRecovery);
  pairingScan.addEventListener("click", () => void scanPairingQr());
  pairingClear.addEventListener("click", () => void clearLocalDevice());
  pairingLayer.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      dismissPairingRecovery();
      return;
    }
    if (event.key !== "Tab") return;
    const focusable = pairingFocusableElements();
    if (focusable.length === 0) {
      event.preventDefault();
      pairingSheet.focus();
      return;
    }
    const first = focusable[0];
    const last = focusable.at(-1);
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  });
}

function registerServiceWorker() {
  if (!("serviceWorker" in navigator)) return;
  void navigator.serviceWorker.register("./service-worker.js", { scope: "./" }).catch(() => {
    runtime.serviceWorker = { status: "unsupported" };
  });
  runtime.serviceWorker = { status: "requested" };
}

function scheduleViewport() {
  if (scheduleViewport.pending) return;
  scheduleViewport.pending = true;
  requestAnimationFrame(() => {
    scheduleViewport.pending = false;
    emitHostEvent(viewportEvent());
  });
}
scheduleViewport.pending = false;

function installHostBridge() {
  const appearance = matchMedia("(prefers-color-scheme: dark)");
  window.addEventListener("resize", scheduleViewport);
  window.addEventListener("orientationchange", scheduleViewport);
  window.visualViewport?.addEventListener("resize", scheduleViewport);
  window.visualViewport?.addEventListener("scroll", scheduleViewport);
  window.addEventListener("focus", () => emitHostEvent({ kind: "focus", focused: true }));
  window.addEventListener("blur", () => emitHostEvent({ kind: "focus", focused: false }));
  document.addEventListener("visibilitychange", () =>
    emitHostEvent({ kind: "visibility", visible: document.visibilityState === "visible" })
  );
  appearance.addEventListener("change", (event) =>
    emitHostEvent({ kind: "appearance", dark_mode: event.matches })
  );
  document.addEventListener("fullscreenchange", () =>
    emitHostEvent({ kind: "fullscreen", fullscreen: document.fullscreenElement !== null })
  );

  emitHostEvent(viewportEvent());
  emitHostEvent({ kind: "visibility", visible: document.visibilityState === "visible" });
  emitHostEvent({ kind: "focus", focused: document.hasFocus() });
  emitHostEvent({ kind: "appearance", dark_mode: appearance.matches });
  emitHostEvent({ kind: "fullscreen", fullscreen: document.fullscreenElement !== null });
}

function recordInteraction(name, event) {
  runtime.interactions[name] += 1;
  runtime.interactions.last = {
    name,
    type: event.type,
    inputType: event.inputType ?? null,
    data: typeof event.data === "string" ? event.data.slice(0, 32) : null,
    at: performance.now()
  };
}

function instrumentGpuiElements() {
  const canvas = document.querySelector("canvas");
  const input = document.querySelector("body > input");
  if (!canvas || !input) return false;
  if (!canvas.dataset.vibexGateCanvas) {
    canvas.dataset.vibexGateCanvas = "true";
    canvas.addEventListener("pointerdown", (event) => recordInteraction("pointer", event), true);
    canvas.addEventListener("touchstart", (event) => recordInteraction("touch", event), true);
    canvas.addEventListener("wheel", (event) => recordInteraction("wheel", event), true);
  }
  if (!input.dataset.vibexGateInput) {
    input.dataset.vibexGateInput = "true";
    input.setAttribute("aria-label", "GPUI canvas text input bridge");
    input.addEventListener("keydown", (event) => recordInteraction("keyboard", event), true);
    input.addEventListener("beforeinput", (event) => recordInteraction("beforeInput", event), true);
    input.addEventListener("input", (event) => recordInteraction("input", event), true);
    input.addEventListener("paste", (event) => recordInteraction("paste", event), true);
    input.addEventListener("compositionstart", (event) => recordInteraction("compositionStart", event), true);
    input.addEventListener("compositionupdate", (event) => recordInteraction("compositionUpdate", event), true);
    input.addEventListener("compositionend", (event) => recordInteraction("compositionEnd", event), true);
  }
  runtime.compatibility?.installGpuiElements(canvas, input);
  return true;
}

async function canvasPixelMetrics(canvas) {
  const blob = await new Promise((resolve) => canvas.toBlob(resolve, "image/png"));
  if (!blob) return { uniqueColors: 0, standardDeviation: 0 };
  const bitmap = await createImageBitmap(blob);
  const sample = document.createElement("canvas");
  sample.width = 64;
  sample.height = 64;
  const context = sample.getContext("2d", { willReadFrequently: true });
  context.drawImage(bitmap, 0, 0, sample.width, sample.height);
  bitmap.close();
  const pixels = context.getImageData(0, 0, sample.width, sample.height).data;
  const colors = new Set();
  let sum = 0;
  let sumOfSquares = 0;
  for (let index = 0; index < pixels.length; index += 4) {
    const red = pixels[index];
    const green = pixels[index + 1];
    const blue = pixels[index + 2];
    colors.add(`${red >> 3}:${green >> 3}:${blue >> 3}`);
    const luminance = 0.2126 * red + 0.7152 * green + 0.0722 * blue;
    sum += luminance;
    sumOfSquares += luminance * luminance;
  }
  const count = pixels.length / 4;
  const mean = sum / count;
  return {
    uniqueColors: colors.size,
    standardDeviation: Math.sqrt(Math.max(0, sumOfSquares / count - mean * mean))
  };
}

async function waitForCanvas() {
  const deadline = performance.now() + 6000;
  let sawSizedCanvas = false;
  while (performance.now() < deadline) {
    if (instrumentGpuiElements()) {
      const canvas = document.querySelector("canvas[data-vibex-gate-canvas]");
      if (canvas.width > 0 && canvas.height > 0) {
        sawSizedCanvas = true;
        await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
        runtime.pixelMetrics = await canvasPixelMetrics(canvas);
        if (
          runtime.pixelMetrics.uniqueColors >= 8 &&
          runtime.pixelMetrics.standardDeviation >= 2
        ) {
          runtime.readyAt = performance.now();
          setState("ready", "Vibex is ready", "The GPUI canvas rendered.", "GATE_READY");
          return;
        }
      }
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  fail(
    sawSizedCanvas ? "GPUI_BLANK_CANVAS" : "GPUI_CANVAS_TIMEOUT",
    new Error(
      sawSizedCanvas
        ? "GPUI created a canvas but did not present credible application pixels within 6 seconds"
        : "GPUI did not create a sized canvas within 6 seconds"
    )
  );
}

window.__vibexGpuiRuntimeBooted = () => {
  runtime.gpuiBooted = true;
  void waitForCanvas();
};
window.__vibexGpuiRuntimeFailed = (message) => fail("GPUI_RUNTIME_INIT", message);

function probeWebStorage() {
  const key = `vibex-gate-${crypto.randomUUID()}`;
  try {
    localStorage.setItem(key, "probe");
    const passed = localStorage.getItem(key) === "probe";
    localStorage.removeItem(key);
    runtime.probes.storage = {
      status: passed ? "passed" : "failed",
      implementation: "localStorage"
    };
  } catch (error) {
    runtime.probes.storage = { status: "failed", error: String(error) };
  }
  emitHostEvent({ kind: "storage_probe", status: runtime.probes.storage.status });
}

async function probeSecureStorage() {
  const capacitor = globalThis.Capacitor;
  let plugin = capacitor?.Plugins?.SecureStoragePlugin;
  if (
    !plugin &&
    capacitor?.isNativePlatform?.() &&
    capacitor?.isPluginAvailable?.("SecureStoragePlugin") &&
    typeof capacitor.registerPlugin === "function"
  ) {
    plugin = capacitor.registerPlugin("SecureStoragePlugin");
  }
  if (!plugin) {
    runtime.probes.secureStorage = {
      status: "unsupported",
      implementation: "capacitor_secure_storage_plugin",
      reason: "plugin_not_available_in_this_host"
    };
    return;
  }

  const key = "vibex-gate-probe";
  try {
    await plugin.set({ key, value: "probe" });
    const value = await plugin.get({ key });
    await plugin.remove({ key });
    runtime.probes.secureStorage = {
      status: value?.value === "probe" ? "passed" : "failed",
      implementation: "capacitor_secure_storage_plugin"
    };
  } catch (error) {
    runtime.probes.secureStorage = { status: "failed", error: String(error) };
  }
}

async function probeFetch(url) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 3000);
  try {
    const response = await fetch(url, { cache: "no-store", signal: controller.signal });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const bytes = await response.arrayBuffer();
    if (bytes.byteLength > 4096) throw new Error(`probe exceeded 4096 bytes: ${bytes.byteLength}`);
    return { status: "passed", bytes: bytes.byteLength, url };
  } catch (error) {
    return { status: "failed", error: String(error), url };
  } finally {
    clearTimeout(timer);
  }
}

function probeWebSocket(url) {
  return new Promise((resolve) => {
    const expected = "vibex-gate";
    let socket;
    const timer = setTimeout(() => {
      socket?.close();
      resolve({ status: "failed", error: "echo timed out", url });
    }, 3000);
    try {
      socket = new WebSocket(url);
      socket.addEventListener("open", () => socket.send(expected), { once: true });
      socket.addEventListener(
        "message",
        (event) => {
          clearTimeout(timer);
          socket.close();
          resolve({ status: event.data === expected ? "passed" : "failed", url });
        },
        { once: true }
      );
      socket.addEventListener(
        "error",
        () => {
          clearTimeout(timer);
          resolve({ status: "failed", error: "WebSocket connection failed", url });
        },
        { once: true }
      );
    } catch (error) {
      clearTimeout(timer);
      resolve({ status: "failed", error: String(error), url });
    }
  });
}

async function probeNetwork() {
  const isNative = Boolean(globalThis.Capacitor?.isNativePlatform?.());
  const explicitFetch = parameters.get("fetchProbe");
  const explicitWebSocket = parameters.get("wsProbe");
  if (isNative && (!explicitFetch || !explicitWebSocket)) {
    const pending = {
      status: "unsupported",
      reason: "physical capture must provide bounded HTTPS and WSS probe endpoints"
    };
    runtime.probes.fetch = pending;
    runtime.probes.webSocket = pending;
    emitHostEvent({ kind: "network_probe", status: "unsupported" });
    return;
  }

  const fetchUrl = explicitFetch || new URL("/__gate/fetch", location.href).href;
  const socketUrl =
    explicitWebSocket ||
    `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/__gate/ws`;
  [runtime.probes.fetch, runtime.probes.webSocket] = await Promise.all([
    probeFetch(fetchUrl),
    probeWebSocket(socketUrl)
  ]);
  const passed =
    runtime.probes.fetch.status === "passed" && runtime.probes.webSocket.status === "passed";
  emitHostEvent({ kind: "network_probe", status: passed ? "passed" : "failed" });
}

async function webGpuPreflight() {
  if (parameters.has("forceUnsupported")) {
    return { supported: false, code: "WEBGPU_FORCED_UNSUPPORTED", detail: "Unsupported mode was requested for the negative-path gate." };
  }
  if (!window.isSecureContext) {
    return { supported: false, code: "SECURE_CONTEXT_REQUIRED", detail: "WebGPU requires HTTPS, localhost, or a trusted application scheme." };
  }
  if (!navigator.gpu) {
    return { supported: false, code: "WEBGPU_UNAVAILABLE", detail: "navigator.gpu is not available in this browser or WebView." };
  }

  try {
    const adapter = await withTimeout(navigator.gpu.requestAdapter(), 3000, "WebGPU adapter request");
    if (!adapter) {
      return { supported: false, code: "WEBGPU_ADAPTER_UNAVAILABLE", detail: "The browser exposed WebGPU but did not provide an adapter." };
    }
    const info = adapter.info ?? {};
    runtime.adapter = {
      vendor: info.vendor ?? null,
      architecture: info.architecture ?? null,
      device: info.device ?? null,
      description: info.description ?? null,
      fallback: Boolean(info.isFallbackAdapter)
    };
    return { supported: true };
  } catch (error) {
    return { supported: false, code: "WEBGPU_PREFLIGHT_FAILED", detail: String(error) };
  }
}

async function boot() {
  const preflight = await webGpuPreflight();
  if (!preflight.supported) {
    unsupported(preflight.code, preflight.detail);
    return;
  }

  try {
    runtime.build = await fetch("./build.json", { cache: "no-store" }).then((response) => response.json());
    const wasm = await import("./pkg/vibex_web.js");
    await wasm.default();
    wasmRuntime = wasm;
    runtime.wasmReady = true;
    runtime.contract = JSON.parse(wasm.gate_contract());
    runtime.remote.contract = JSON.parse(wasm.remote_contract());
    runtime.compatibility = createPlatformCompatibility({
      onViewportChange: scheduleViewport,
      requestPlatformBack: async (source) => {
        if (typeof wasmRuntime?.platform_back !== "function") return "unhandled";
        const result = JSON.parse(wasmRuntime.platform_back());
        runtime.events.push({ platformBack: result, source, at: performance.now() });
        if (runtime.events.length > 256) runtime.events.shift();
        return result.status;
      }
    });
    runtime.platformBack = () => runtime.compatibility.platformBack("test_bridge");
    installHostBridge();
    void runtime.compatibility.installNativeBridges().catch((error) => {
      runtime.errors.push({ code: "NATIVE_BRIDGE_INIT", message: String(error) });
    });
    probeWebStorage();
    void probeSecureStorage();
    void probeNetwork();
    registerServiceWorker();
    wasm.start(diagnosticsGate);
    void initializeHostServices().catch((error) => {
      recordRemoteError(error, "HOST_SERVICES_INIT");
    });
  } catch (error) {
    fail("GPUI_BOOT_FAILED", error);
  }
}

window.addEventListener("error", (event) => {
  if (runtime.state !== "ready") fail("WINDOW_ERROR", event.error ?? event.message);
});
window.addEventListener("unhandledrejection", (event) => {
  if (runtime.state !== "ready") fail("UNHANDLED_REJECTION", event.reason);
});

installPairingControls();
prepareHostServices();
void boot();

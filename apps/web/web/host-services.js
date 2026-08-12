/**
 * Typed, capability-first host services for the browser and Capacitor shell.
 *
 * This module deliberately contains no product navigation, RPC envelopes, or
 * rendering.  It only normalizes platform capabilities and keeps durable
 * credentials behind a narrow storage boundary.  The Rust runtime remains the
 * authority for pairing validation and remote business state.
 */

export const HOST_SERVICE_SCHEMA_VERSION = "vibex-host-services.v1";
export const REMOTE_CREDENTIAL_STORAGE_KEY = "vibex.remote-client.credentials.v1";
export const PAIRING_ENTRY_HINT_SCHEMA_VERSION = "vibex-pairing-entry.v1";
export const MAX_PAIRING_FRAGMENT_BYTES = 32 * 1024;
export const MAX_DEEP_LINK_FRAGMENT_BYTES = 4096;
export const MAX_CREDENTIAL_BUNDLE_BYTES = 64 * 1024;
export const MAX_FILE_BYTES = 16 * 1024 * 1024;

const SAFE_URL_SCHEMES = new Set(["http:", "https:", "ws:", "wss:"]);
const EXTERNAL_URL_SCHEMES = new Set(["http:", "https:", "mailto:", "tel:"]);
const PAIRING_URL_SCHEMES = new Set(["http:", "https:", "vibex:", "dev.vibex.remote:"]);
const OPAQUE_DEEP_LINK_SCHEMES = new Set(["http:", "https:", "vibex:", "dev.vibex.remote:"]);
const OPAQUE_DEEP_LINK_PATTERN = /^[A-Za-z0-9_.-]+$/;

function clone(value) {
  return value == null ? value : JSON.parse(JSON.stringify(value));
}

function errorCode(error, fallback = "host_service_failed") {
  if (error && typeof error === "object" && typeof error.code === "string") return error.code;
  return fallback;
}

function plugin(capacitor, name) {
  let instance = capacitor?.Plugins?.[name];
  if (
    !instance &&
    capacitor?.isNativePlatform?.() &&
    capacitor?.isPluginAvailable?.(name) &&
    typeof capacitor.registerPlugin === "function"
  ) {
    try {
      instance = capacitor.registerPlugin(name);
    } catch {
      instance = null;
    }
  }
  return instance ?? null;
}

function parseSafeUrl(value, { allowWebSocket = true } = {}) {
  if (typeof value !== "string" || value.length === 0 || value.length > 4096) {
    throw new Error("url is empty or exceeds the bounded size");
  }
  let url;
  try {
    url = new URL(value);
  } catch {
    throw new Error("url is invalid");
  }
  const schemes = allowWebSocket ? SAFE_URL_SCHEMES : new Set(["http:", "https:"]);
  if (!schemes.has(url.protocol) || url.username || url.password) {
    throw new Error("url scheme or embedded credentials are not allowed");
  }
  return url;
}

function parsePairingIntake(value, base = "https://vibex.invalid/") {
  if (typeof value !== "string" || value.length > MAX_PAIRING_FRAGMENT_BYTES + 4096) {
    throw new Error("pairing URL is too large");
  }
  let url;
  try {
    url = new URL(value, base);
  } catch {
    throw new Error("pairing URL is invalid");
  }
  if (!PAIRING_URL_SCHEMES.has(url.protocol)) throw new Error("pairing URL scheme is not allowed");
  const fragment = url.hash || "";
  if (!fragment.startsWith("#/pair/")) return null;
  if (fragment.length > MAX_PAIRING_FRAGMENT_BYTES) {
    throw new Error("pairing fragment exceeds the bounded size");
  }
  const trustedWebEntry = url.protocol === "http:" || url.protocol === "https:";
  return {
    fragment,
    // Pairing links never need query state; dropping it prevents an operator
    // from accidentally retaining a copied grant/token in browser history.
    cleanUrl: trustedWebEntry ? url.pathname || "/" : new URL(base).pathname || "/",
    entryHint: trustedWebEntry
      ? {
          schemaVersion: PAIRING_ENTRY_HINT_SCHEMA_VERSION,
          kind: "origin",
          origin: url.origin
        }
      : {
          schemaVersion: PAIRING_ENTRY_HINT_SCHEMA_VERSION,
          kind: "untrusted_custom_scheme"
        },
    receipt: {
      schemaVersion: "vibex-pairing-intake.v1",
      accepted: true,
      entryType: trustedWebEntry ? "web_origin" : "custom_scheme"
    }
  };
}

function rejectedPairingCleanUrl(value, base = "https://vibex.invalid/") {
  if (typeof value !== "string" || !value.includes("#/pair/")) return null;
  try {
    const url = new URL(value, base);
    return url.protocol === "http:" || url.protocol === "https:"
      ? url.pathname || "/"
      : new URL(base).pathname || "/";
  } catch {
    return "/";
  }
}

function pairingEntryType(value, base = "https://vibex.invalid/") {
  try {
    const url = new URL(value, base);
    return url.protocol === "http:" || url.protocol === "https:"
      ? "web_origin"
      : "custom_scheme";
  } catch {
    return "unknown";
  }
}

/** Inspect a pairing URL without exposing its fragment or exact entry origin. */
export function extractPairingFragment(value, base = "https://vibex.invalid/") {
  return parsePairingIntake(value, base)?.receipt ?? null;
}

/** Remove a pairing fragment before any asynchronous claim/network work. */
export function clearPairingFragment(windowLike, cleanUrl) {
  if (!windowLike?.history?.replaceState) return false;
  const value = typeof cleanUrl === "string" && cleanUrl ? cleanUrl : "/";
  try {
    windowLike.history.replaceState({}, windowLike.document?.title ?? "Vibex", value);
    return true;
  } catch {
    return false;
  }
}

/**
 * Extract a Relay push/deep-link locator without decoding business content.
 * Supported shapes:
 *   https://app.example/#/notify/<notificationId>/<opaqueLocator>
 *   vibex://notify/<notificationId>/<opaqueLocator>
 */
export function extractOpaqueDeepLink(value, base = "https://vibex.invalid/") {
  if (typeof value !== "string" || value.length > MAX_DEEP_LINK_FRAGMENT_BYTES + 4096) {
    throw new Error("deep link URL is too large");
  }
  let url;
  try {
    url = new URL(value, base);
  } catch {
    throw new Error("deep link URL is invalid");
  }
  if (!OPAQUE_DEEP_LINK_SCHEMES.has(url.protocol)) throw new Error("deep link URL scheme is not allowed");
  let parts = [];
  if (url.hash.startsWith("#/notify/")) {
    parts = url.hash.slice("#/notify/".length).split("/");
  } else if (url.protocol === "vibex:" || url.protocol === "dev.vibex.remote:") {
    const pathname = [url.hostname, ...url.pathname.split("/")].filter(Boolean).join("/");
    if (pathname.startsWith("notify/")) parts = pathname.slice("notify/".length).split("/");
  }
  if (parts.length === 0) return null;
  if (parts.length !== 2) throw new Error("deep link locator shape is invalid");
  const [notificationId, opaqueLocator] = parts.map((part) => decodeURIComponent(part || ""));
  if (
    !notificationId ||
    !opaqueLocator ||
    notificationId.length > 256 ||
    opaqueLocator.length > 512 ||
    !OPAQUE_DEEP_LINK_PATTERN.test(notificationId) ||
    !OPAQUE_DEEP_LINK_PATTERN.test(opaqueLocator)
  ) {
    throw new Error("deep link locator is invalid");
  }
  return {
    notificationId,
    opaqueLocator,
    cleanUrl: url.pathname || "/",
    source: url.protocol === "http:" || url.protocol === "https:" ? "browser_url" : "app_link"
  };
}

/** Remove an opaque deep-link locator before authoritative fetch begins. */
export function clearOpaqueDeepLink(windowLike, cleanUrl) {
  if (!windowLike?.history?.replaceState) return false;
  const value = typeof cleanUrl === "string" && cleanUrl ? cleanUrl : "/";
  try {
    windowLike.history.replaceState({}, windowLike.document?.title ?? "Vibex", value);
    return true;
  } catch {
    return false;
  }
}

function validateDeviceId(value) {
  return typeof value === "string" && /^device_[A-Za-z0-9_-]+$/.test(value) && value.length <= 256;
}

/** Validate the non-cryptographic shape before passing credentials to Rust. */
export function validateCredentialBundle(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("credential bundle must be an object");
  }
  if (value.schemaVersion !== "vibex-remote-client-credentials.v1") {
    throw new Error("credential bundle schema is incompatible");
  }
  const record = value.record;
  if (!record || typeof record !== "object" || Array.isArray(record)) {
    throw new Error("credential record is missing");
  }
  const serverUrl = parseSafeUrl(record.serverUrl, { allowWebSocket: true });
  if (serverUrl.search || serverUrl.hash) {
    throw new Error("credential server URL cannot contain query or fragment data");
  }
  if (
    typeof value.identityPrivateKey !== "string" ||
    value.identityPrivateKey.length < 40 ||
    value.identityPrivateKey.length > 256 ||
    typeof value.expectedServerId !== "string" ||
    value.expectedServerId.trim().length === 0 ||
    value.expectedServerId.length > 256 ||
    !validateDeviceId(record.auth?.deviceId) ||
    typeof record.auth?.authToken !== "string" ||
    record.auth.authToken.trim().length < 16 ||
    record.auth.authToken.length > 4096 ||
    typeof record.deviceIdentityPublicKey !== "string" ||
    record.deviceIdentityPublicKey.length < 16 ||
    record.deviceIdentityPublicKey.length > 256 ||
    (record.serverIdentityPublicKey != null &&
      (typeof record.serverIdentityPublicKey !== "string" ||
        record.serverIdentityPublicKey.length < 16 ||
        record.serverIdentityPublicKey.length > 256))
  ) {
    throw new Error("credential identity or grant fields are invalid");
  }
  if (!["browser", "mobile", "desktop_web"].includes(value.clientType)) {
    throw new Error("credential client type is unsupported");
  }
  const encoded = JSON.stringify(value);
  if (encoded.length > MAX_CREDENTIAL_BUNDLE_BYTES) {
    throw new Error("credential bundle exceeds the bounded size");
  }
  return clone(value);
}

function bytesToBase64(bytes) {
  let binary = "";
  const chunk = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += chunk) {
    binary += String.fromCharCode(...bytes.subarray(offset, Math.min(offset + chunk, bytes.length)));
  }
  if (typeof globalThis.btoa !== "function") throw new Error("base64 encoding is unavailable");
  return globalThis.btoa(binary);
}

async function fileToSelection(file) {
  if (typeof globalThis.Blob !== "function" || !(file instanceof globalThis.Blob)) {
    throw new Error("selected item is not a file");
  }
  if (file.size > MAX_FILE_BYTES) throw new Error("selected file exceeds the bounded size");
  const bytes = new Uint8Array(await file.arrayBuffer());
  return {
    name: typeof file.name === "string" && file.name ? file.name.slice(0, 512) : "upload",
    mimeType: file.type || null,
    byteLength: bytes.byteLength,
    bytesBase64: bytesToBase64(bytes)
  };
}

function createBrowserStorage(windowLike) {
  return {
    kind: "local_storage",
    status: "degraded",
    async get(key) {
      const storage = windowLike?.localStorage;
      if (!storage) throw Object.assign(new Error("localStorage is unavailable"), { code: "storage_unavailable" });
      return storage.getItem(key);
    },
    async set(key, value) {
      const storage = windowLike?.localStorage;
      if (!storage) throw Object.assign(new Error("localStorage is unavailable"), { code: "storage_unavailable" });
      storage.setItem(key, value);
    },
    async remove(key) {
      windowLike?.localStorage?.removeItem(key);
    }
  };
}

function secureStorageKeyMissing(error) {
  const code = String(error?.code ?? "").toLowerCase();
  const message = String(error?.message ?? error ?? "").toLowerCase();
  return code === "not_found" ||
    message === "key not found" ||
    message === "item with given key does not exist";
}

function createSecureStorage(windowLike, capacitor) {
  const secure = plugin(capacitor, "SecureStoragePlugin");
  if (!secure || typeof secure.get !== "function" || typeof secure.set !== "function") {
    return createBrowserStorage(windowLike);
  }
  return {
    kind: "capacitor_secure_storage",
    status: "supported",
    async get(key) {
      try {
        const result = await secure.get({ key });
        return typeof result?.value === "string" ? result.value : null;
      } catch (error) {
        // The plugin uses a rejected promise for a missing key on some
        // platforms.  Treat only an explicit not-found result as empty.
        if (secureStorageKeyMissing(error)) return null;
        throw Object.assign(new Error("secure storage read failed"), { code: errorCode(error, "secure_storage_read_failed") });
      }
    },
    async set(key, value) {
      await secure.set({ key, value });
    },
    async remove(key) {
      if (typeof secure.remove === "function") await secure.remove({ key });
    }
  };
}

function createFileInput(documentLike, accept, capture = undefined) {
  if (!documentLike?.createElement) throw Object.assign(new Error("file picker is unavailable"), { code: "file_picker_unsupported" });
  return new Promise((resolve, reject) => {
    const input = documentLike.createElement("input");
    input.type = "file";
    input.accept = Array.isArray(accept) ? accept.join(",") : typeof accept === "string" ? accept : "";
    if (capture) input.capture = capture;
    input.addEventListener("change", async () => {
      try {
        resolve(input.files?.[0] ? await fileToSelection(input.files[0]) : null);
      } catch (error) {
        reject(error);
      } finally {
        input.remove();
      }
    }, { once: true });
    input.addEventListener("cancel", () => {
      input.remove();
      resolve(null);
    }, { once: true });
    input.click();
  });
}

/**
 * Build the host adapter.  All returned methods are safe to call in a plain
 * browser; unsupported Capacitor capabilities return a structured error.
 */
export function createHostServices({
  windowLike = globalThis.window,
  documentLike = globalThis.document,
  capacitor = globalThis.Capacitor,
  onPairingFragment,
  onOpaqueDeepLink,
  onLifecycle,
  onPushToken
} = {}) {
  const native = Boolean(capacitor?.isNativePlatform?.());
  const storage = createSecureStorage(windowLike, capacitor);
  const app = plugin(capacitor, "App");
  const keyboard = plugin(capacitor, "Keyboard");
  const camera = plugin(capacitor, "Camera");
  const sharePlugin = plugin(capacitor, "Share");
  const browserPlugin = plugin(capacitor, "Browser");
  const networkPlugin = plugin(capacitor, "Network");
  const barcodeScanner = plugin(capacitor, "CapacitorBarcodeScanner");
  const subscriptions = [];
  let initialPairing = null;
  let initialPairingError = null;
  const statuses = {
    safeArea: "supported",
    storage: storage.status,
    secureStorage: storage.kind === "capacitor_secure_storage" ? "supported" : "degraded",
    deepLink: app ? "supported" : "degraded",
    lifecycle: app ? "supported" : "browser_fallback",
    network: networkPlugin ? "supported" : "browser_fallback",
    keyboard: keyboard ? "supported" : "browser_fallback",
    camera: camera ? "supported" : "browser_fallback",
    qrScanner: barcodeScanner ? "supported" : "unsupported",
    filePicker: "browser_fallback",
    share: sharePlugin || typeof globalThis.navigator?.share === "function" ? "supported" : "unsupported",
    systemUrl: browserPlugin || typeof windowLike?.open === "function" ? "supported" : "unsupported"
  };

  function emitLifecycle(event) {
    const normalized = { ...event, atMs: Date.now() };
    try {
      onLifecycle?.(normalized);
    } catch {
      // Host callbacks must never break platform listeners.
    }
  }

  function pairingReceipt(intake, source) {
    return { ...intake.receipt, source };
  }

  async function deliverPairing(intake, source) {
    await onPairingFragment?.({
      fragment: intake.fragment,
      entryHint: intake.entryHint,
      source,
      entryType: intake.receipt.entryType
    });
    return pairingReceipt(intake, source);
  }

  async function dispatchPairing(url, source = "app_link") {
    try {
      const intake = parsePairingIntake(url, windowLike?.location?.href ?? undefined);
      if (!intake) return null;
      // Clear the fragment synchronously, before invoking Rust or storage.
      clearPairingFragment(windowLike, intake.cleanUrl);
      return await deliverPairing(intake, source);
    } catch (error) {
      const cleanUrl = rejectedPairingCleanUrl(url, windowLike?.location?.href ?? undefined);
      if (cleanUrl) clearPairingFragment(windowLike, cleanUrl);
      const code = errorCode(error, "pairing_link_invalid");
      const entryType = pairingEntryType(url, windowLike?.location?.href ?? undefined);
      await onPairingFragment?.({ errorCode: code, source, entryType });
      return {
        schemaVersion: "vibex-pairing-intake.v1",
        accepted: false,
        source,
        entryType,
        errorCode: code
      };
    }
  }

  async function dispatchOpaqueDeepLink(url, source = "app_link") {
    try {
      const extracted = extractOpaqueDeepLink(url, windowLike?.location?.href ?? undefined);
      if (!extracted) return null;
      clearOpaqueDeepLink(windowLike, extracted.cleanUrl);
      const item = { ...extracted, source };
      await onOpaqueDeepLink?.(item);
      return item;
    } catch (error) {
      return { errorCode: errorCode(error, "opaque_deep_link_invalid") };
    }
  }

  async function dispatchInboundUrl(url, source = "app_link") {
    const pairing = await dispatchPairing(url, source);
    if (pairing && !pairing.errorCode) return pairing;
    const deepLink = await dispatchOpaqueDeepLink(url, source);
    return deepLink ?? pairing;
  }

  async function initialize() {
    if (windowLike?.addEventListener) {
      const online = () => emitLifecycle({ kind: "network_changed", online: true });
      const offline = () => emitLifecycle({ kind: "network_lost", online: false });
      const visibility = () => emitLifecycle({
        kind: "visibility_changed",
        visible: documentLike?.visibilityState !== "hidden"
      });
      windowLike.addEventListener("online", online);
      windowLike.addEventListener("offline", offline);
      documentLike?.addEventListener?.("visibilitychange", visibility);
      subscriptions.push(() => {
        windowLike.removeEventListener("online", online);
        windowLike.removeEventListener("offline", offline);
        documentLike?.removeEventListener?.("visibilitychange", visibility);
      });
      emitLifecycle({ kind: "visibility_changed", visible: documentLike?.visibilityState !== "hidden" });
      if (windowLike.navigator && windowLike.navigator.onLine === false) emitLifecycle({ kind: "network_lost", online: false });
    }

    if (app?.addListener) {
      try {
        const urlListener = await app.addListener("appUrlOpen", ({ url }) => {
          void dispatchInboundUrl(url, "app_link");
        });
        const stateListener = await app.addListener("appStateChange", ({ isActive }) => {
          emitLifecycle({ kind: isActive ? "app_resumed" : "app_backgrounded", active: Boolean(isActive) });
        });
        subscriptions.push(() => urlListener?.remove?.(), () => stateListener?.remove?.());
      } catch {
        statuses.deepLink = "degraded";
        statuses.lifecycle = "browser_fallback";
      }
    }
    if (networkPlugin?.addListener) {
      try {
        const networkListener = await networkPlugin.addListener("networkStatusChange", ({ connected }) => {
          emitLifecycle({ kind: connected ? "network_changed" : "network_lost", online: Boolean(connected) });
        });
        subscriptions.push(() => networkListener?.remove?.());
        const current = await networkPlugin.getStatus?.();
        if (current && current.connected === false) emitLifecycle({ kind: "network_lost", online: false });
      } catch {
        statuses.network = "browser_fallback";
      }
    }
    if (initialPairing) {
      const intake = initialPairing;
      initialPairing = null;
      await deliverPairing(intake, "browser_url");
    } else if (initialPairingError) {
      const errorCode = initialPairingError;
      initialPairingError = null;
      await onPairingFragment?.({ errorCode, source: "browser_url", entryType: "web_origin" });
    } else if (windowLike?.location?.href) {
      await dispatchInboundUrl(windowLike.location.href, "browser_url");
    }
    return snapshot();
  }

  async function readCredentialBundle() {
    const raw = await storage.get(REMOTE_CREDENTIAL_STORAGE_KEY);
    if (!raw) return null;
    try {
      return validateCredentialBundle(JSON.parse(raw));
    } catch (error) {
      throw Object.assign(new Error("stored remote credentials are invalid"), { code: errorCode(error, "stored_credentials_invalid") });
    }
  }

  async function writeCredentialBundle(bundle) {
    const valid = validateCredentialBundle(bundle);
    const encoded = JSON.stringify(valid);
    await storage.set(REMOTE_CREDENTIAL_STORAGE_KEY, encoded);
    const stored = await storage.get(REMOTE_CREDENTIAL_STORAGE_KEY);
    if (stored !== encoded) {
      throw Object.assign(new Error("stored remote credentials failed verification"), {
        code: "credential_storage_verification_failed"
      });
    }
    validateCredentialBundle(JSON.parse(stored));
    return { stored: true, storage: storage.kind };
  }

  async function clearCredentialBundle() {
    await storage.remove(REMOTE_CREDENTIAL_STORAGE_KEY);
    return { cleared: true };
  }

  async function pickFile(accept = []) {
    return createFileInput(documentLike, accept);
  }

  async function captureImage() {
    if (camera?.getPhoto) {
      try {
        const photo = await camera.getPhoto({
          quality: 90,
          allowEditing: false,
          resultType: "base64",
          source: "camera"
        });
        const base64 = typeof photo?.base64String === "string" ? photo.base64String : "";
        if (!base64) return null;
        if (base64.length > MAX_FILE_BYTES * 2) throw new Error("captured image exceeds the bounded size");
        return {
          name: `capture-${Date.now()}.jpg`,
          mimeType: "image/jpeg",
          byteLength: Math.floor(base64.length * 0.75),
          bytesBase64: base64
        };
      } catch (error) {
        if (error?.message === "User cancelled photos app" || error?.code === "canceled") return null;
        throw Object.assign(new Error("camera capture failed"), { code: errorCode(error, "camera_failed") });
      }
    }
    return createFileInput(documentLike, ["image/*"], "environment");
  }

  async function scanQr() {
    if (barcodeScanner?.scanBarcode) {
      let result;
      try {
        result = await barcodeScanner.scanBarcode({
          hint: 0,
          cameraDirection: 1,
          scanOrientation: 3,
          cancelButtonAccessibilityLabel: "Cancel scan"
        });
      } catch (error) {
        const message = String(error?.message ?? "").toLowerCase();
        const code = message.includes("cancel")
          ? "qr_scan_canceled"
          : message.includes("permission") || message.includes("denied")
            ? "qr_camera_permission_denied"
            : errorCode(error, "qr_scan_failed");
        throw Object.assign(new Error("QR scan did not complete"), { code });
      }
      const value = typeof result?.ScanResult === "string" ? result.ScanResult : "";
      if (!value) return { status: "canceled" };
      if (new TextEncoder().encode(value).byteLength > MAX_PAIRING_FRAGMENT_BYTES) {
        throw Object.assign(new Error("QR result exceeds the pairing size limit"), {
          code: "qr_scan_result_too_large"
        });
      }
      return { status: "scanned", value, format: "qr_code" };
    }
    throw Object.assign(new Error("QR scanning is unavailable in this host"), { code: "qr_scanner_unsupported" });
  }

  async function share({ title = "Vibex", text = "", url = "" } = {}) {
    if (sharePlugin?.share) return sharePlugin.share({ title, text, url });
    if (typeof globalThis.navigator?.share === "function") return globalThis.navigator.share({ title, text, url });
    throw Object.assign(new Error("share is unsupported in this host"), { code: "share_unsupported" });
  }

  async function download({ name = "download", bytesBase64, mimeType = "application/octet-stream" } = {}) {
    if (typeof bytesBase64 !== "string" || bytesBase64.length > MAX_FILE_BYTES * 2) {
      throw Object.assign(new Error("download payload is invalid"), { code: "download_payload_invalid" });
    }
    if (!windowLike?.atob || !documentLike?.createElement || !windowLike.URL?.createObjectURL) {
      throw Object.assign(new Error("download is unsupported in this host"), { code: "download_unsupported" });
    }
    const binary = windowLike.atob(bytesBase64);
    const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
    const blob = new globalThis.Blob([bytes], { type: mimeType });
    const href = windowLike.URL.createObjectURL(blob);
    const anchor = documentLike.createElement("a");
    anchor.href = href;
    anchor.download = String(name).slice(0, 256) || "download";
    anchor.click();
    windowLike.setTimeout?.(() => windowLike.URL.revokeObjectURL(href), 0);
    return { downloaded: true };
  }

  async function openSystemUrl(value) {
    let url;
    try {
      url = new URL(value);
    } catch {
      throw Object.assign(new Error("system URL is invalid"), { code: "system_url_invalid" });
    }
    if (!EXTERNAL_URL_SCHEMES.has(url.protocol)) {
      throw Object.assign(new Error("system URL scheme is not allowed"), { code: "system_url_scheme_unsupported" });
    }
    if (browserPlugin?.open && (url.protocol === "http:" || url.protocol === "https:")) {
      return browserPlugin.open({ url: url.toString() });
    }
    if (typeof windowLike?.open === "function") {
      windowLike.open(url.toString(), "_blank", "noopener,noreferrer");
      return { opened: true };
    }
    throw Object.assign(new Error("system URL is unsupported in this host"), { code: "system_url_unsupported" });
  }

  function installPushHook() {
    const push = plugin(capacitor, "PushNotifications");
    if (!push?.addListener) return;
    void push.addListener("registration", ({ value }) => {
      if (typeof value === "string" && value.length <= 4096) onPushToken?.(value);
    }).then((listener) => subscriptions.push(() => listener?.remove?.())).catch(() => {
      // Push is optional until the Relay/push task installs a provider.
    });
  }

  function snapshot() {
    return {
      schemaVersion: HOST_SERVICE_SCHEMA_VERSION,
      host: native ? "capacitor" : "browser",
      secureContext: Boolean(windowLike?.isSecureContext),
      capabilities: clone(statuses)
    };
  }

  function safeArea() {
    const probe = documentLike?.querySelector?.("#safe-area-probe") ?? documentLike?.documentElement;
    if (!probe || typeof windowLike?.getComputedStyle !== "function") {
      return { top: 0, right: 0, bottom: 0, left: 0 };
    }
    const style = windowLike.getComputedStyle(probe);
    const pixels = (value) => {
      const parsed = Number.parseFloat(value);
      return Number.isFinite(parsed) ? parsed : 0;
    };
    return {
      top: pixels(style.paddingTop),
      right: pixels(style.paddingRight),
      bottom: pixels(style.paddingBottom),
      left: pixels(style.paddingLeft)
    };
  }

  function viewport() {
    const visual = windowLike?.visualViewport;
    const width = Number.isFinite(visual?.width) ? visual.width : Number(windowLike?.innerWidth ?? 0);
    const height = Number.isFinite(visual?.height) ? visual.height : Number(windowLike?.innerHeight ?? 0);
    const layoutHeight = Number(windowLike?.innerHeight ?? height);
    const keyboardInset = Math.max(0, layoutHeight - height - Number(visual?.offsetTop ?? 0));
    return {
      width,
      height,
      devicePixelRatio: Number(windowLike?.devicePixelRatio ?? 1),
      safeArea: safeArea(),
      keyboardVisible: keyboardInset > 0.5,
      keyboardInset,
      keyboardSource: keyboardInset > 0.5 ? "visual_viewport" : "none"
    };
  }

  function dispose() {
    for (const remove of subscriptions.splice(0)) {
      try {
        remove();
      } catch {
        // Disposal is best-effort and must not surface platform errors.
      }
    }
  }

  installPushHook();

  // Capture and scrub the startup pairing URL synchronously. Rust preview and
  // every storage/network operation happen later during initialize().
  if (windowLike?.location?.href) {
    try {
      initialPairing = parsePairingIntake(
        windowLike.location.href,
        windowLike.location.href
      );
      if (initialPairing) clearPairingFragment(windowLike, initialPairing.cleanUrl);
    } catch (error) {
      initialPairing = null;
      const cleanUrl = rejectedPairingCleanUrl(
        windowLike.location.href,
        windowLike.location.href
      );
      if (cleanUrl) {
        clearPairingFragment(windowLike, cleanUrl);
        initialPairingError = errorCode(error, "pairing_link_invalid");
      }
    }
  }

  return Object.freeze({
    schemaVersion: HOST_SERVICE_SCHEMA_VERSION,
    isNative: native,
    initialize,
    snapshot,
    safeArea,
    viewport,
    readCredentialBundle,
    writeCredentialBundle,
    clearCredentialBundle,
    extractPairingFragment,
    extractOpaqueDeepLink,
    dispatchPairing,
    dispatchOpaqueDeepLink,
    dispatchInboundUrl,
    pickFile,
    captureImage,
    scanQr,
    share,
    download,
    openSystemUrl,
    keyboardPlugin: keyboard,
    dispose
  });
}

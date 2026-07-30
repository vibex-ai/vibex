import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  closeSync,
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  rmSync,
  writeFileSync
} from "node:fs";
import { createServer, connect } from "node:net";
import { networkInterfaces } from "node:os";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { PRODUCT_PAIRING_MODES } from "../product-pairing-evidence.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const STATE_ROOT = join(ROOT, ".e2e-local-env", "product-pairing");
const DIRECT_GATEWAY_PORT = 1428;
const MAX_COMMAND_OUTPUT = 64 * 1024 * 1024;
const CONTROL_RESPONSE_LIMIT = 64 * 1024;
const START_TIMEOUT_MS = 120_000;
const RELAY_MODES = new Set(["relay", "relay-no-tailscale", "direct-relay-fallback"]);
const DIRECT_MODES = new Set(["direct", "direct-relay-fallback"]);
const activeChildren = new Set();

function fail(code) {
  const error = new Error(code);
  error.code = code;
  throw error;
}

function assert(condition, code) {
  if (!condition) fail(code);
}

function option(name, fallback = null) {
  const inline = process.argv.find((argument) => argument.startsWith(`--${name}=`));
  if (inline) return inline.slice(name.length + 3);
  const index = process.argv.indexOf(`--${name}`);
  return index >= 0 ? process.argv[index + 1] ?? fallback : fallback;
}

function cleanProxyEnvironment() {
  for (const key of Object.keys(process.env)) {
    if (/^(?:https?_proxy|all_proxy|no_proxy)$/i.test(key)) delete process.env[key];
  }
}

function run(command, args, { code, env = {}, input = undefined, encoding = "utf8" } = {}) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    env: { ...process.env, ...env },
    input,
    encoding,
    maxBuffer: MAX_COMMAND_OUTPUT,
    stdio: [input === undefined ? "ignore" : "pipe", "pipe", "pipe"]
  });
  if (result.error || result.status !== 0) fail(code ?? "product_pairing_command_failed");
  return result.stdout;
}

function sha256File(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function cargoExecutable(args, targetName, env, code) {
  const output = run(
    "cargo",
    [...args, "--message-format=json-render-diagnostics"],
    { code, env }
  );
  let executable = null;
  for (const line of output.split("\n")) {
    if (!line.startsWith("{")) continue;
    try {
      const message = JSON.parse(line);
      if (
        message.reason === "compiler-artifact" &&
        message.target?.name === targetName &&
        typeof message.executable === "string"
      ) {
        executable = message.executable;
      }
    } catch {
      // Cargo can interleave non-JSON compiler diagnostics with artifact messages.
    }
  }
  assert(executable, `${code}_artifact_missing`);
  return executable;
}

function readWebBuild() {
  let build;
  try {
    build = JSON.parse(readFileSync(join(ROOT, "apps/web/dist/build.json"), "utf8"));
  } catch {
    fail("product_pairing_web_build_missing");
  }
  assert(
    build.schemaVersion === "vibex-web-build.v1" &&
      build.profile === "release" &&
      /^[0-9a-f]{24}$/.test(build.buildId ?? "") &&
      /^[0-9a-f]{40}$/.test(build.gitCommit ?? ""),
    "product_pairing_web_build_invalid"
  );
  return build;
}

function buildArtifacts() {
  run("pnpm", ["--filter", "@vibex/web", "build:release"], {
    code: "product_pairing_web_build_failed"
  });
  const webBuild = readWebBuild();
  const configuredHarness = process.env.VIBEX_E2E_HARNESS_BINARY;
  const harnessBinary = configuredHarness
    ? resolve(configuredHarness)
    : cargoExecutable(
        [
          "test",
          "-p",
          "vibex-desktop",
          "--test",
          "product_pairing_harness",
          "--features",
          "e2e-test-support",
          "--no-run",
          "--locked"
        ],
        "product_pairing_harness",
        {},
        "product_pairing_harness_build_failed"
      );
  assert(existsSync(harnessBinary), "product_pairing_harness_artifact_missing");
  const relayBinary = cargoExecutable(
    ["build", "-p", "vibex-relay-server", "--bin", "vibex-relay-server", "--locked"],
    "vibex-relay-server",
    {
      VIBEX_RELAY_WEB_BUILD_ID: webBuild.buildId,
      VIBEX_RELAY_WEB_GIT_COMMIT: webBuild.gitCommit
    },
    "product_pairing_relay_build_failed"
  );
  return {
    webBuild,
    harnessBinary,
    relayBinary,
    desktopArtifactSha256: sha256File(harnessBinary),
    relayArtifactSha256: sha256File(relayBinary)
  };
}

function selectLanIpv4() {
  let interfaces;
  try {
    interfaces = networkInterfaces();
  } catch {
    fail("product_pairing_network_interfaces_unavailable");
  }
  const candidates = [];
  for (const [name, addresses] of Object.entries(interfaces)) {
    for (const address of addresses ?? []) {
      if (
        (address.family === "IPv4" || address.family === 4) &&
        !address.internal &&
        address.address !== "0.0.0.0" &&
        !address.address.startsWith("127.")
      ) {
        candidates.push({ name, address: address.address });
      }
    }
  }
  const override = process.env.VIBEX_E2E_LAN_IP;
  if (override) {
    assert(
      candidates.some((candidate) => candidate.address === override),
      "product_pairing_lan_ip_override_invalid"
    );
    return override;
  }
  candidates.sort((left, right) => {
    const virtual = (name) => /^(?:tailscale|tun|tap|wg|docker|br-|veth|virbr)/i.test(name);
    return Number(virtual(left.name)) - Number(virtual(right.name));
  });
  assert(candidates.length > 0, "product_pairing_lan_ip_missing");
  return candidates[0].address;
}

async function reservePort(host = "0.0.0.0") {
  return new Promise((resolvePromise, rejectPromise) => {
    const server = createServer();
    server.once("error", rejectPromise);
    server.listen(0, host, () => {
      const address = server.address();
      server.close((error) => {
        if (error || !address || typeof address === "string") {
          rejectPromise(error ?? new Error("port_reservation_failed"));
          return;
        }
        resolvePromise(address.port);
      });
    });
  }).catch(() => fail("product_pairing_port_reservation_failed"));
}

async function assertGatewayPortAvailable() {
  await new Promise((resolvePromise, rejectPromise) => {
    const server = createServer();
    server.once("error", rejectPromise);
    server.listen(DIRECT_GATEWAY_PORT, "127.0.0.1", () => {
      server.close((error) => (error ? rejectPromise(error) : resolvePromise()));
    });
  }).catch(() => fail("product_pairing_gateway_port_occupied"));
}

function generateCertificate(root, lanIp) {
  const certificateRoot = join(root, "tls");
  mkdirSync(certificateRoot, { recursive: true, mode: 0o700 });
  const caKey = join(certificateRoot, "ca.key");
  const caCert = join(certificateRoot, "ca.pem");
  const serverKey = join(certificateRoot, "server.key");
  const serverCsr = join(certificateRoot, "server.csr");
  const serverCert = join(certificateRoot, "server.pem");
  const serverChain = join(certificateRoot, "server-chain.pem");
  const extensions = join(certificateRoot, "server-extensions.cnf");
  writeFileSync(
    extensions,
    `[v3_leaf]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:${lanIp},IP:127.0.0.1,DNS:localhost\n`,
    { mode: 0o600 }
  );
  run(
    "openssl",
    [
      "req",
      "-x509",
      "-newkey",
      "rsa:2048",
      "-sha256",
      "-nodes",
      "-days",
      "1",
      "-subj",
      "/CN=Vibex Product Pairing E2E CA",
      "-addext",
      "basicConstraints=critical,CA:TRUE",
      "-addext",
      "keyUsage=critical,keyCertSign,cRLSign",
      "-keyout",
      caKey,
      "-out",
      caCert
    ],
    { code: "product_pairing_ca_generation_failed" }
  );
  run(
    "openssl",
    [
      "req",
      "-new",
      "-newkey",
      "rsa:2048",
      "-sha256",
      "-nodes",
      "-subj",
      "/CN=vibex-product-pairing.invalid",
      "-keyout",
      serverKey,
      "-out",
      serverCsr
    ],
    { code: "product_pairing_certificate_request_failed" }
  );
  run(
    "openssl",
    [
      "x509",
      "-req",
      "-sha256",
      "-days",
      "1",
      "-in",
      serverCsr,
      "-CA",
      caCert,
      "-CAkey",
      caKey,
      "-CAcreateserial",
      "-extfile",
      extensions,
      "-extensions",
      "v3_leaf",
      "-out",
      serverCert
    ],
    { code: "product_pairing_certificate_sign_failed" }
  );
  writeFileSync(
    serverChain,
    Buffer.concat([readFileSync(serverCert), readFileSync(caCert)]),
    { mode: 0o600 }
  );
  const publicKey = run("openssl", ["x509", "-in", serverCert, "-pubkey", "-noout"], {
    code: "product_pairing_certificate_public_key_failed",
    encoding: null
  });
  const publicDer = run("openssl", ["pkey", "-pubin", "-outform", "DER"], {
    code: "product_pairing_certificate_spki_failed",
    encoding: null,
    input: publicKey
  });
  return {
    caCert,
    serverCert: serverChain,
    serverKey,
    certificateSpkiSha256: createHash("sha256").update(publicDer).digest("base64")
  };
}

function resolveCertificate(root, lanIp) {
  const configured = {
    serverCert: process.env.VIBEX_E2E_TLS_CERT,
    serverKey: process.env.VIBEX_E2E_TLS_KEY,
    caCert: process.env.VIBEX_E2E_TLS_CA_CERT,
    publicHost: process.env.VIBEX_E2E_PUBLIC_HOST
  };
  const configuredCount = Object.values(configured).filter(Boolean).length;
  if (configuredCount === 0) {
    return { ...generateCertificate(root, lanIp), publicationHost: lanIp };
  }
  assert(configuredCount === 4, "product_pairing_tls_configuration_incomplete");
  assert(
    existsSync(configured.serverCert) &&
      existsSync(configured.serverKey) &&
      existsSync(configured.caCert),
    "product_pairing_tls_file_missing"
  );
  assert(
    /^(?:[A-Za-z0-9](?:[A-Za-z0-9.-]{0,251}[A-Za-z0-9])?|\d{1,3}(?:\.\d{1,3}){3})$/.test(
      configured.publicHost
    ),
    "product_pairing_public_host_invalid"
  );
  const publicKey = run(
    "openssl",
    ["x509", "-in", configured.serverCert, "-pubkey", "-noout"],
    { code: "product_pairing_certificate_public_key_failed", encoding: null }
  );
  const publicDer = run("openssl", ["pkey", "-pubin", "-outform", "DER"], {
    code: "product_pairing_certificate_spki_failed",
    encoding: null,
    input: publicKey
  });
  return {
    ...configured,
    certificateSpkiSha256: createHash("sha256").update(publicDer).digest("base64"),
    publicationHost: configured.publicHost
  };
}

function spawnManaged(command, args, { env = {}, logPath, code }) {
  const log = openSync(logPath, "a", 0o600);
  let child;
  try {
    child = spawn(command, args, {
      cwd: ROOT,
      env: { ...process.env, ...env },
      detached: true,
      stdio: ["ignore", log, log]
    });
  } finally {
    closeSync(log);
  }
  const handle = {
    child,
    code,
    exited: false,
    exit: null,
    exitPromise: null
  };
  handle.exitPromise = new Promise((resolvePromise) => {
    child.once("error", () => {
      handle.exited = true;
      handle.exit = { code: null, signal: null };
      activeChildren.delete(handle);
      resolvePromise(handle.exit);
    });
    child.once("exit", (exitCode, signal) => {
      handle.exited = true;
      handle.exit = { code: exitCode, signal };
      activeChildren.delete(handle);
      resolvePromise(handle.exit);
    });
  });
  activeChildren.add(handle);
  return handle;
}

function signalProcess(handle, signal) {
  if (!handle || handle.exited) return;
  try {
    process.kill(-handle.child.pid, signal);
  } catch {
    try {
      handle.child.kill(signal);
    } catch {
      // The exact-owned process already exited.
    }
  }
}

function sleep(milliseconds) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}

async function stopManaged(handle) {
  if (!handle || handle.exited) return;
  signalProcess(handle, "SIGTERM");
  await Promise.race([handle.exitPromise, sleep(5_000)]);
  if (!handle.exited) {
    signalProcess(handle, "SIGKILL");
    await Promise.race([handle.exitPromise, sleep(5_000)]);
  }
}

async function waitFor(probe, code, handle = null, timeout = START_TIMEOUT_MS) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (handle?.exited) fail(handle.code ?? code);
    try {
      if (await probe()) return;
    } catch {
      // The service can reject connections until all listeners are installed.
    }
    await sleep(100);
  }
  fail(code);
}

async function waitForTcp(port, handle) {
  await waitFor(
    () =>
      new Promise((resolvePromise) => {
        const socket = connect({ host: "127.0.0.1", port });
        socket.setTimeout(500);
        socket.once("connect", () => {
          socket.destroy();
          resolvePromise(true);
        });
        const failed = () => {
          socket.destroy();
          resolvePromise(false);
        };
        socket.once("error", failed);
        socket.once("timeout", failed);
      }),
    "product_pairing_tls_proxy_start_timeout",
    handle
  );
}

async function boundedJson(url, options = {}) {
  const response = await fetch(url, {
    ...options,
    cache: "no-store",
    signal: AbortSignal.timeout(5_000)
  });
  if (!response.ok) return null;
  const bytes = new Uint8Array(await response.arrayBuffer());
  if (bytes.byteLength > CONTROL_RESPONSE_LIMIT) return null;
  return JSON.parse(new TextDecoder().decode(bytes));
}

function tlsProxy({ port, targetPort, certificate, logPath }) {
  let handle = null;
  return {
    async start() {
      assert(!handle || handle.exited, "product_pairing_tls_proxy_already_running");
      handle = spawnManaged(
        "socat",
        [
          `OPENSSL-LISTEN:${port},bind=0.0.0.0,reuseaddr,fork,cert=${certificate.serverCert},key=${certificate.serverKey},verify=0`,
          `TCP:127.0.0.1:${targetPort}`
        ],
        {
          logPath,
          code: "product_pairing_tls_proxy_exited"
        }
      );
      await waitForTcp(port, handle);
    },
    async stop() {
      await stopManaged(handle);
      handle = null;
    }
  };
}

function stableJson(value) {
  if (Array.isArray(value)) return value.map(stableJson);
  if (!value || typeof value !== "object") return value;
  return Object.fromEntries(
    Object.keys(value)
      .sort()
      .map((key) => [key, stableJson(value[key])])
  );
}

function tailscaleSnapshot() {
  const statusBytes = run("tailscale", ["status", "--json"], {
    code: "product_pairing_tailscale_status_failed"
  });
  const serveBytes = run("tailscale", ["serve", "status", "--json"], {
    code: "product_pairing_tailscale_serve_status_failed"
  });
  let status;
  let serve;
  try {
    status = JSON.parse(statusBytes);
    serve = JSON.parse(serveBytes || "{}");
  } catch {
    fail("product_pairing_tailscale_status_invalid");
  }
  assert(
    status.BackendState === "Running" && typeof status.Self?.DNSName === "string",
    "product_pairing_tailscale_not_ready"
  );
  return JSON.stringify(stableJson(serve));
}

function desktopSupervisor({ binary, mode, root, controlPort, caCert, logPath }) {
  let handle = null;
  const controlBase = `http://127.0.0.1:${controlPort}`;
  async function start() {
    assert(!handle || handle.exited, "product_pairing_desktop_already_running");
    handle = spawnManaged(
      binary,
      ["--ignored", "--nocapture", "--exact", "product_pairing_harness"],
      {
        env: {
          ...(caCert ? { SSL_CERT_FILE: caCert } : {}),
          VIBEX_E2E_CONTROL_PORT: String(controlPort),
          VIBEX_E2E_PRESERVE_ROOT: "1",
          VIBEX_E2E_ROOT: root,
          VIBEX_E2E_TRANSPORT: mode
        },
        logPath,
        code: "product_pairing_desktop_exited"
      }
    );
    await waitFor(
      async () => (await boundedJson(`${controlBase}/health`))?.ready === true,
      "product_pairing_desktop_start_timeout",
      handle
    );
  }
  async function stop() {
    if (!handle || handle.exited) return;
    try {
      await boundedJson(`${controlBase}/lifecycle/shutdown`, { method: "POST" });
    } catch {
      // Fall through to exact process-group termination.
    }
    await Promise.race([handle.exitPromise, sleep(10_000)]);
    await stopManaged(handle);
    handle = null;
  }
  return {
    controlBase,
    start,
    stop,
    async restart() {
      await stop();
      await start();
    }
  };
}

async function disableAllBestEffort(controlBase) {
  try {
    await boundedJson(`${controlBase}/pairing/action`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ kind: "disable_all" })
    });
    await waitFor(async () => {
      const snapshot = await boundedJson(`${controlBase}/pairing/snapshot`);
      return (
        snapshot?.pendingAction === null &&
        snapshot.methods?.every((method) => method.desiredEnabled === false)
      );
    }, "product_pairing_cleanup_timeout", null, 30_000);
  } catch {
    // The Desktop may already be stopped; process cleanup still remains exact-owned.
  }
}

async function createModeEnvironment(mode, artifacts, certificate, writeEvidence) {
  const root = join(STATE_ROOT, mode);
  rmSync(root, { recursive: true, force: true });
  mkdirSync(root, { recursive: true, mode: 0o700 });
  const controlPort = await reservePort("127.0.0.1");
  const directPort = DIRECT_MODES.has(mode) ? await reservePort() : null;
  const relayPort = RELAY_MODES.has(mode) ? await reservePort("127.0.0.1") : null;
  const relayTlsPort = RELAY_MODES.has(mode) ? await reservePort() : null;
  const relayLogPath = RELAY_MODES.has(mode) ? join(root, "relay.log") : null;
  const processLogPath = join(root, "process.log");
  writeFileSync(processLogPath, "", { mode: 0o600 });
  if (relayLogPath) writeFileSync(relayLogPath, "", { mode: 0o600 });

  let relay = null;
  let relayProxy = null;
  if (RELAY_MODES.has(mode)) {
    relay = spawnManaged(artifacts.relayBinary, [], {
      env: {
        VIBEX_RELAY_BIND_ADDR: `127.0.0.1:${relayPort}`,
        VIBEX_RELAY_MAX_TOTAL_CONNECTIONS: "64",
        VIBEX_RELAY_MAX_DEVICES_PER_ROOM: "8",
        VIBEX_RELAY_MAX_QUEUE_BYTES_PER_CONNECTION: "1048576",
        VIBEX_RELAY_WEB_STATIC_DIR: join(ROOT, "apps/web/dist")
      },
      logPath: relayLogPath,
      code: "product_pairing_relay_exited"
    });
    await waitFor(
      async () => (await boundedJson(`http://127.0.0.1:${relayPort}/health`))?.status === "ok",
      "product_pairing_relay_start_timeout",
      relay
    );
    relayProxy = tlsProxy({
      port: relayTlsPort,
      targetPort: relayPort,
      certificate,
      logPath: processLogPath
    });
    await relayProxy.start();
  }

  let directProxy = null;
  if (DIRECT_MODES.has(mode)) {
    directProxy = tlsProxy({
      port: directPort,
      targetPort: DIRECT_GATEWAY_PORT,
      certificate,
      logPath: processLogPath
    });
    await directProxy.start();
  }

  const tailscaleBefore = mode === "tailscale" ? tailscaleSnapshot() : null;
  const desktop = desktopSupervisor({
    binary: artifacts.harnessBinary,
    mode,
    root: join(root, "desktop"),
    controlPort,
    caCert: mode === "tailscale" ? null : certificate.caCert,
    logPath: processLogPath
  });
  await desktop.start();

  return {
    environment: {
      mode,
      controlBase: desktop.controlBase,
      directOrigin: directPort ? `https://${certificate.publicationHost}:${directPort}` : null,
      relayOrigin: relayTlsPort ? `https://${certificate.publicationHost}:${relayTlsPort}` : null,
      relayLogPath,
      certificateSpkiSha256: certificate.certificateSpkiSha256,
      desktopArtifactSha256: artifacts.desktopArtifactSha256,
      relayArtifactSha256: RELAY_MODES.has(mode) ? artifacts.relayArtifactSha256 : null,
      writeEvidence,
      desktop,
      directProxy
    },
    async close() {
      await disableAllBestEffort(desktop.controlBase);
      await desktop.stop();
      await directProxy?.stop();
      await relayProxy?.stop();
      await stopManaged(relay);
      if (mode === "tailscale") {
        assert(tailscaleSnapshot() === tailscaleBefore, "product_pairing_tailscale_cleanup_mismatch");
      }
    },
    root
  };
}

function killChildrenSynchronously() {
  for (const handle of activeChildren) signalProcess(handle, "SIGKILL");
}

process.once("exit", killChildrenSynchronously);
process.once("SIGINT", () => {
  killChildrenSynchronously();
  process.exit(130);
});
process.once("SIGTERM", () => {
  killChildrenSynchronously();
  process.exit(143);
});

export async function runProductPairingEnvironment({ runMode } = {}) {
  assert(typeof runMode === "function", "product_pairing_mode_runner_missing");
  cleanProxyEnvironment();
  await assertGatewayPortAvailable();
  const requested = option("transport", option("mode"));
  const modes = requested ? [requested] : PRODUCT_PAIRING_MODES;
  assert(
    modes.every((mode) => PRODUCT_PAIRING_MODES.includes(mode)),
    "product_pairing_mode_invalid"
  );
  const writeEvidence = process.argv.includes("--write");
  rmSync(STATE_ROOT, { recursive: true, force: true });
  mkdirSync(STATE_ROOT, { recursive: true, mode: 0o700 });
  const lanIp = selectLanIpv4();
  const certificate = resolveCertificate(STATE_ROOT, lanIp);
  const artifacts = buildArtifacts();
  const results = [];
  try {
    for (const mode of modes) {
      let context = null;
      let failure = null;
      try {
        context = await createModeEnvironment(
          mode,
          artifacts,
          certificate,
          writeEvidence
        );
        results.push(await runMode(context.environment));
      } catch (error) {
        failure = error;
      }
      try {
        await context?.close();
      } catch (error) {
        failure ??= error;
      } finally {
        if (context) rmSync(context.root, { recursive: true, force: true });
      }
      if (failure) throw failure;
      console.log(`GPUI product pairing mode passed: ${mode}`);
    }
    return results;
  } finally {
    killChildrenSynchronously();
    rmSync(STATE_ROOT, { recursive: true, force: true });
  }
}

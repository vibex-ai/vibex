// Local controlled-environment orchestrator for one GPUI workflow E2E target.
//
//   node scripts/e2e-local-env/run-target.mjs --target web|android --transport direct|relay [--device <serial>] [--reuse-installed]
//
// Starts the headless desktop harness (crates/vibex-remote-client/tests/
// e2e_gateway_harness.rs), fronts it with Tailscale Serve so the release WASM
// client sees a trusted HTTPS/WSS endpoint, provisions the credential bundle
// plus fixture/recovery hooks, and then invokes scripts/e2e-workflows.mjs
// with --write. By default this requires `tailscale serve` to be enabled for
// this node. A non-privileged TLS forwarder can be selected by setting
// VIBEX_E2E_TLS_CERT, VIBEX_E2E_TLS_KEY, and VIBEX_E2E_PUBLIC_HOST. The Android
// target additionally requires the phone to be joined to the tailnet and to
// trust the selected TLS endpoint.
import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const PORTS = { gateway: 14320, control: 14321, web: 14322, relay: 14323 };
const TAILSCALE_SERVE_HTTPS = { gateway: 443, web: 8443, relay: 9443 };
const TLS_FORWARD_HTTPS = { gateway: 10443, web: 10444, relay: 10445 };

// The tailnet HTTPS endpoints must be reached directly; a local forward proxy
// (e.g. 127.0.0.1:7890) cannot dial tailscale addresses. Strip proxy settings
// from everything this orchestrator spawns, including the Playwright browser.
for (const key of Object.keys(process.env)) {
  if (/^(https?_proxy|all_proxy|no_proxy)$/i.test(key)) delete process.env[key];
}

function option(name, fallback = null) {
  const index = process.argv.indexOf(`--${name}`);
  return index >= 0 ? (process.argv[index + 1] ?? fallback) : fallback;
}

const target = option("target");
const transport = option("transport");
const device = option("device");
const reuseInstalled = process.argv.includes("--reuse-installed");
if (!["web", "android"].includes(target) || !["direct", "relay"].includes(transport)) {
  console.error(
    "usage: run-target.mjs --target web|android --transport direct|relay [--device serial] [--reuse-installed]"
  );
  process.exit(2);
}
if (reuseInstalled && target !== "android") {
  console.error("--reuse-installed is valid only for the Android target");
  process.exit(2);
}

const stateRoot = join(ROOT, ".e2e-local-env", `${target}-${transport}`);
rmSync(stateRoot, { recursive: true, force: true });
mkdirSync(stateRoot, { recursive: true });
const bundleFile = join(stateRoot, "credentials.json");
const relayLogFile = join(stateRoot, "relay-server.log");

function sh(command, args, options = {}) {
  return execFileSync(command, args, { cwd: ROOT, encoding: "utf8", ...options });
}

const tsStatus = JSON.parse(sh("tailscale", ["status", "--json"]));
const tsName = tsStatus.Self.DNSName.replace(/\.$/, "");
const tlsCert = process.env.VIBEX_E2E_TLS_CERT;
const tlsKey = process.env.VIBEX_E2E_TLS_KEY;
if (Boolean(tlsCert) !== Boolean(tlsKey)) {
  throw new Error("VIBEX_E2E_TLS_CERT and VIBEX_E2E_TLS_KEY must be set together");
}
const tlsForward = Boolean(tlsCert);
const httpsPorts = tlsForward ? TLS_FORWARD_HTTPS : TAILSCALE_SERVE_HTTPS;
const publicHost = tlsForward
  ? process.env.VIBEX_E2E_PUBLIC_HOST ?? tsStatus.Self.TailscaleIPs?.[0]
  : tsName;
if (!publicHost) throw new Error("unable to resolve the E2E public host");

function publicHttpsUrl(port) {
  return `https://${publicHost}${port === 443 ? "" : `:${port}`}`;
}

const publicGatewayUrl = publicHttpsUrl(httpsPorts.gateway);
const publicWebUrl = publicHttpsUrl(httpsPorts.web);
const publicRelayUrl = publicHttpsUrl(httpsPorts.relay);

function ensureServe(httpsPort, localPort) {
  sh("tailscale", ["serve", "--bg", `--https=${httpsPort}`, `http://127.0.0.1:${localPort}`]);
}

function ensureTlsForward(httpsPort, localPort, label) {
  if (!existsSync(tlsCert) || !existsSync(tlsKey)) {
    throw new Error("configured E2E TLS certificate or key is missing");
  }
  spawnChild(
    "socat",
    [
      `OPENSSL-LISTEN:${httpsPort},reuseaddr,fork,cert=${tlsCert},key=${tlsKey},verify=0`,
      `TCP:127.0.0.1:${localPort}`
    ],
    {},
    `${label}-tls-forward`
  );
}

function ensureFront(httpsPort, localPort, label) {
  if (tlsForward) ensureTlsForward(httpsPort, localPort, label);
  else ensureServe(httpsPort, localPort);
}

async function waitFor(probe, label, attempts = 240, delayMs = 500) {
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    try {
      if (await probe()) return;
    } catch {
      // keep polling
    }
    await new Promise((resolveSleep) => setTimeout(resolveSleep, delayMs));
  }
  throw new Error(`timed out waiting for ${label}`);
}

const children = [];
function cleanup() {
  for (const child of children) {
    try {
      process.kill(-child.pid, "SIGKILL");
    } catch {
      try {
        child.kill("SIGKILL");
      } catch {
        // already gone
      }
    }
  }
}
process.on("exit", cleanup);
process.on("SIGINT", () => process.exit(130));
process.on("SIGTERM", () => process.exit(143));

function spawnChild(command, args, env, logPrefix, stdio = "inherit") {
  const child = spawn(command, args, {
    cwd: ROOT,
    env: { ...process.env, ...env },
    stdio,
    detached: true
  });
  child.on("exit", (code, signal) => {
    if (code !== null && code !== 0) console.error(`[${logPrefix}] exited with code ${code} ${signal ?? ""}`);
  });
  children.push(child);
  return child;
}

try {
  ensureFront(httpsPorts.gateway, PORTS.gateway, "gateway");
  if (target === "web") ensureFront(httpsPorts.web, PORTS.web, "web");
  if (transport === "relay") ensureFront(httpsPorts.relay, PORTS.relay, "relay");

  if (transport === "relay") {
    sh("cargo", ["build", "-p", "vibex-relay-server", "--locked"], { stdio: "inherit" });
    const relayLog = (await import("node:fs")).openSync(relayLogFile, "a");
    const relayChild = spawnChild(
      join(ROOT, "target/debug/vibex-relay-server"),
      [],
      {
        VIBEX_RELAY_BIND_ADDR: `127.0.0.1:${PORTS.relay}`,
        VIBEX_RELAY_MAX_TOTAL_CONNECTIONS: "64",
        VIBEX_RELAY_MAX_DEVICES_PER_ROOM: "4",
        VIBEX_RELAY_MAX_QUEUE_BYTES_PER_CONNECTION: "262144"
      },
      "relay-server",
      ["ignore", relayLog, relayLog]
    );
    const { writeFileSync } = await import("node:fs");
    writeFileSync(join(stateRoot, "relay.pid"), String(relayChild.pid));
    await waitFor(async () => {
      const response = await fetch(`http://127.0.0.1:${PORTS.relay}/health`);
      return response.ok;
    }, "relay server health");
  }

  // Resolve a pinned harness binary: parallel development keeps mutating the
  // desktop-side agent crates, so recompiling per run would test a moving
  // snapshot. VIBEX_E2E_HARNESS_BIN pins a known-good build; otherwise compile
  // once here.
  let harnessBin = process.env.VIBEX_E2E_HARNESS_BIN;
  if (!harnessBin) {
    const buildOutput = sh(
      "cargo",
      ["test", "-p", "vibex-remote-client", "--test", "e2e_gateway_harness", "--no-run", "--offline"],
      { stdio: ["ignore", "pipe", "pipe"] }
    );
    void buildOutput;
    const listing = sh("bash", ["-c", "ls -t target/debug/deps/e2e_gateway_harness-* | grep -v '\\.d$' | head -1"]);
    harnessBin = listing.trim();
  }
  spawnChild(
    harnessBin,
    ["--ignored", "--nocapture"],
    {
      VIBEX_E2E_ROOT: join(stateRoot, "harness"),
      VIBEX_E2E_GATEWAY_PORT: String(PORTS.gateway),
      VIBEX_E2E_CONTROL_PORT: String(PORTS.control),
      VIBEX_E2E_BUNDLE_FILE: bundleFile,
      VIBEX_E2E_TRANSPORT: transport,
      VIBEX_E2E_CLIENT_TYPE: target === "android" ? "mobile" : "browser",
      VIBEX_E2E_PUBLIC_URL: publicGatewayUrl,
      VIBEX_E2E_RELAY_URL: transport === "relay" ? `http://127.0.0.1:${PORTS.relay}` : "",
      VIBEX_E2E_PUBLIC_RELAY_URL: transport === "relay" ? publicRelayUrl : "",
      VIBEX_E2E_EXTRA_ORIGINS: target === "web" ? publicWebUrl : ""
    },
    "harness"
  );
  await waitFor(async () => {
    const response = await fetch(`http://127.0.0.1:${PORTS.control}/health`);
    return response.ok;
  }, "harness control API");
  await waitFor(async () => existsSync(bundleFile), "credential bundle");

  let webServer = null;
  if (target === "web") {
    const { startWasmServer } = await import(join(ROOT, "scripts/wasm-test-server.mjs"));
    webServer = await startWasmServer({ port: PORTS.web });
  }

  const runnerArgs = [
    "scripts/e2e-workflows.mjs",
    "--target",
    target,
    "--transport",
    transport,
    "--write"
  ];
  if (target === "web") runnerArgs.push("--origin", publicWebUrl);
  if (target === "android" && device) runnerArgs.push("--device", device);
  if (reuseInstalled) runnerArgs.push("--reuse-installed");

  const runner = spawn("node", runnerArgs, {
    cwd: ROOT,
    env: {
      ...process.env,
      VIBEX_E2E_CONTROL_PORT: String(PORTS.control),
      VIBEX_E2E_CREDENTIALS_FILE: bundleFile,
      VIBEX_E2E_FIXTURE_HOOK: join(ROOT, "scripts/e2e-local-env/fixture-hook.sh"),
      VIBEX_E2E_RECOVERY_HOOK: join(ROOT, "scripts/e2e-local-env/recovery-hook.sh"),
      ...(transport === "relay"
        ? {
            VIBEX_E2E_RELAY_LOG_FILE: relayLogFile,
            VIBEX_E2E_RELAY_OWNERSHIP: "user_self_hosted",
            VIBEX_E2E_RELAY_PIDFILE: join(stateRoot, "relay.pid"),
            VIBEX_E2E_RELAY_BIN: join(ROOT, "target/debug/vibex-relay-server"),
            VIBEX_E2E_RELAY_LOG: relayLogFile,
            VIBEX_E2E_RELAY_PORT: String(PORTS.relay)
          }
        : {})
    },
    stdio: "inherit"
  });
  const exitCode = await new Promise((resolvePromise) => runner.on("exit", resolvePromise));
  await webServer?.close();
  if (exitCode !== 0) {
    const bundle = existsSync(bundleFile) ? JSON.parse(readFileSync(bundleFile, "utf8")) : null;
    console.error(`runner failed (${exitCode}); serverUrl=${bundle?.record?.serverUrl ?? "unknown"}`);
    process.exit(exitCode ?? 1);
  }
  console.log(`E2E ${target}/${transport} completed`);
} finally {
  cleanup();
}

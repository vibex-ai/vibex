import { spawn, spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createServer } from "node:net";
import { join } from "node:path";

const repoRoot = process.cwd();
validateDeploymentContract();
const timeoutMs = Number(process.env.VIBEX_RELAY_SMOKE_TIMEOUT_MS ?? 60_000);
const port = process.env.VIBEX_RELAY_SMOKE_PORT
  ? Number(process.env.VIBEX_RELAY_SMOKE_PORT)
  : await findFreePort();

if (!Number.isInteger(port) || port <= 0 || port > 65_535) {
  console.error("VIBEX_RELAY_SMOKE_PORT must be a valid TCP port");
  process.exit(1);
}

const build = spawnSync("cargo", ["build", "-p", "vibex-relay-server"], {
  cwd: repoRoot,
  encoding: "utf8",
  stdio: "inherit",
  env: process.env
});

if (build.status !== 0) {
  process.exit(build.status ?? 1);
}

const binary = join(
  repoRoot,
  "target",
  "debug",
  process.platform === "win32" ? "vibex-relay-server.exe" : "vibex-relay-server"
);
const baseUrl = `http://127.0.0.1:${port}`;
const child = spawn(binary, [], {
  cwd: repoRoot,
  env: {
    ...process.env,
    VIBEX_RELAY_BIND_ADDR: `127.0.0.1:${port}`,
    VIBEX_RELAY_MAX_TOTAL_CONNECTIONS: "64",
    VIBEX_RELAY_MAX_DEVICES_PER_ROOM: "3",
    VIBEX_RELAY_MAX_QUEUE_BYTES_PER_CONNECTION: "262144"
  },
  stdio: ["ignore", "pipe", "pipe"]
});

let output = "";
child.stdout.on("data", (chunk) => {
  output = appendOutput(output, chunk);
});
child.stderr.on("data", (chunk) => {
  output = appendOutput(output, chunk);
});

let childExit = null;
child.on("exit", (code, signal) => {
  childExit = { code, signal };
});

try {
  const health = await waitForJson(`${baseUrl}/health`, timeoutMs, () => childExit);
  assertEqual(health.status, "ok", "health.status");
  assertEqual(health.activeRooms, 0, "health.activeRooms");
  assertEqual(health.activeConnections, 0, "health.activeConnections");

  const info = await waitForJson(`${baseUrl}/api/info`, timeoutMs, () => childExit);
  assertEqual(info.features?.pcWebsocket, true, "info.features.pcWebsocket");
  assertEqual(info.features?.deviceWebsocket, true, "info.features.deviceWebsocket");
  assertEqual(info.features?.websocketFrames, true, "info.features.websocketFrames");
  assertEqual(info.features?.httpPairBridge, true, "info.features.httpPairBridge");
  assertEqual(info.features?.httpCommandBridge, true, "info.features.httpCommandBridge");
  assertEqual("staticRoomAssets" in info.features, false, "info.features.staticRoomAssets");
  assertEqual("webBuild" in info, false, "info.webBuild");
  assertEqual(info.features?.pushProviderConfigured, false, "info.features.pushProviderConfigured");
  assertEqual(info.limits?.maxConnectionsPerRoom, 1, "info.limits.maxConnectionsPerRoom");
  assertEqual(info.limits?.maxTotalConnections, 64, "info.limits.maxTotalConnections");
  assertEqual(info.limits?.maxDevicesPerRoom, 3, "info.limits.maxDevicesPerRoom");
  assertEqual(
    info.limits?.maxQueueBytesPerConnection,
    262144,
    "info.limits.maxQueueBytesPerConnection"
  );
  for (const path of ["/", "/index.html", "/build.json", "/manifest.webmanifest"]) {
    const response = await fetch(`${baseUrl}${path}`);
    assertEqual(response.status, 404, `${path} static hosting`);
  }

  console.log(
    JSON.stringify(
      {
        ok: true,
        relayUrl: baseUrl,
        protocolVersion: info.protocolVersion,
        features: info.features,
        limits: info.limits
      },
      null,
      2
    )
  );
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  if (output.trim()) {
    console.error("\nRelay process output:");
    console.error(output.trim());
  }
  process.exitCode = 1;
} finally {
  await stopChild(child);
}

function validateDeploymentContract() {
  const dockerfile = readFileSync(join(repoRoot, "deploy/relay/Dockerfile"), "utf8");
  const compose = readFileSync(join(repoRoot, "deploy/relay/docker-compose.yml"), "utf8");
  const dockerignore = readFileSync(join(repoRoot, ".dockerignore"), "utf8");
  if (!dockerfile.includes("cargo build --release -p vibex-relay-server --locked")) {
    throw new Error("Relay Dockerfile does not build the native transport service");
  }
  for (const forbidden of [
    "apps/mobile-wasm",
    "wasm-bindgen",
    "VIBEX_RELAY_WEB",
    "relay-image.json"
  ]) {
    if (dockerfile.includes(forbidden) || compose.includes(forbidden)) {
      throw new Error(`Relay deployment still contains retired WebUI marker ${forbidden}`);
    }
  }
  for (const marker of [".e2e-local-env/", ".git/*", "!.git/HEAD", "!.git/refs/**"]) {
    if (!dockerignore.includes(marker)) throw new Error(`Relay Docker context policy is missing ${marker}`);
  }
  if (dockerignore.includes("!.git/config") || dockerignore.includes("!.git/objects")) {
    throw new Error("Relay Docker context must not include Git configuration or objects");
  }
}

function findFreePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => {
        if (address && typeof address === "object") {
          resolve(address.port);
        } else {
          reject(new Error("Could not allocate a local Relay smoke port"));
        }
      });
    });
  });
}

async function waitForJson(url, timeout, getChildExit) {
  const startedAt = Date.now();
  let lastError = null;

  while (Date.now() - startedAt < timeout) {
    const exit = getChildExit();
    if (exit) {
      throw new Error(
        `Relay server exited before ${url} became ready: code=${exit.code} signal=${exit.signal}`
      );
    }

    try {
      const response = await fetch(url);
      if (!response.ok) {
        throw new Error(`${url} returned HTTP ${response.status}`);
      }
      return await response.json();
    } catch (error) {
      lastError = error;
      await sleep(250);
    }
  }

  throw new Error(
    `Timed out waiting for ${url}: ${
      lastError instanceof Error ? lastError.message : String(lastError)
    }`
  );
}

function assertEqual(actual, expected, label) {
  if (actual !== expected) {
    throw new Error(`${label} expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

function appendOutput(current, chunk) {
  const next = current + chunk.toString("utf8");
  return next.length > 12_000 ? next.slice(next.length - 12_000) : next;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function stopChild(childProcess) {
  return new Promise((resolve) => {
    if (childProcess.exitCode !== null || childProcess.signalCode !== null) {
      resolve();
      return;
    }

    const timer = setTimeout(() => {
      childProcess.kill("SIGKILL");
    }, 2_000);

    childProcess.once("exit", () => {
      clearTimeout(timer);
      resolve();
    });
    childProcess.kill("SIGTERM");
  });
}

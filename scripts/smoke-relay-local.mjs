import { spawn, spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { createServer } from "node:net";
import { join, resolve } from "node:path";

const repoRoot = process.cwd();
const webui = process.argv.includes("--webui");
const webDist = resolve(repoRoot, "apps/web/dist");
const webBuild = webui ? readWebBuild() : null;
if (webui) validateDeploymentContract();
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
  env: webBuild
    ? {
        ...process.env,
        VIBEX_RELAY_WEB_BUILD_ID: webBuild.buildId,
        VIBEX_RELAY_WEB_GIT_COMMIT: webBuild.gitCommit
      }
    : process.env
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
    VIBEX_RELAY_MAX_QUEUE_BYTES_PER_CONNECTION: "262144",
    ...(webBuild ? { VIBEX_RELAY_WEB_STATIC_DIR: webDist } : {})
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
  assertEqual(info.features?.staticRoomAssets, webui, "info.features.staticRoomAssets");
  assertEqual(info.features?.pushProviderConfigured, false, "info.features.pushProviderConfigured");
  assertEqual(info.limits?.maxConnectionsPerRoom, 1, "info.limits.maxConnectionsPerRoom");
  assertEqual(info.limits?.maxTotalConnections, 64, "info.limits.maxTotalConnections");
  assertEqual(info.limits?.maxDevicesPerRoom, 3, "info.limits.maxDevicesPerRoom");
  assertEqual(
    info.limits?.maxQueueBytesPerConnection,
    262144,
    "info.limits.maxQueueBytesPerConnection"
  );
  if (webBuild) {
    assertEqual(
      JSON.stringify(info.webBuild),
      JSON.stringify(webBuildDescriptor(webBuild)),
      "info.webBuild"
    );
    await verifyWebAssets(baseUrl, webBuild);
    await verifyFreshBrowserBootstrap(baseUrl, webBuild);
    if (output.includes("relay-bootstrap-fragment-sentinel")) {
      throw new Error("Relay process output retained a browser URL fragment");
    }
  }

  console.log(
    JSON.stringify(
      {
        ok: true,
        relayUrl: baseUrl,
        protocolVersion: info.protocolVersion,
        features: info.features,
        limits: info.limits,
        webBuild: info.webBuild ?? null
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

function webBuildDescriptor(build) {
  return {
    schemaVersion: build.schemaVersion,
    buildId: build.buildId,
    packageVersion: build.packageVersion,
    profile: build.profile,
    gitCommit: build.gitCommit,
    wasmSha256: build.wasmSha256,
    glueSha256: build.glueSha256,
    staticSha256: build.staticSha256
  };
}

function validateDeploymentContract() {
  const dockerfile = readFileSync(join(repoRoot, "deploy/relay/Dockerfile"), "utf8");
  const compose = readFileSync(join(repoRoot, "deploy/relay/docker-compose.yml"), "utf8");
  const dockerignore = readFileSync(join(repoRoot, ".dockerignore"), "utf8");
  const webBuildIndex = dockerfile.indexOf("node apps/web/scripts/build.mjs --release");
  const relayBuildIndex = dockerfile.indexOf("cargo build --release -p vibex-relay-server --locked");
  if (webBuildIndex < 0 || relayBuildIndex < 0 || webBuildIndex >= relayBuildIndex) {
    throw new Error("Relay image must build the release WebUI before the native server");
  }
  for (const marker of [
    "nightly-2026-07-24",
    "WASM_BINDGEN_CLI_VERSION=0.2.125",
    "VIBEX_RELAY_WEB_BUILD_ID",
    "VIBEX_RELAY_WEB_GIT_COMMIT",
    "/workspace/apps/web/dist /app/web",
    "/workspace/target/relay-image.json /app/relay-image.json",
    "VIBEX_RELAY_WEB_STATIC_DIR=/app/web"
  ]) {
    if (!dockerfile.includes(marker)) throw new Error(`Relay Dockerfile is missing ${marker}`);
  }
  if (!compose.includes("VIBEX_RELAY_WEB_STATIC_DIR: /app/web")) {
    throw new Error("Relay Compose does not enable the packaged WebUI");
  }
  for (const marker of [".e2e-local-env/", ".git/*", "!.git/HEAD", "!.git/refs/**"]) {
    if (!dockerignore.includes(marker)) throw new Error(`Relay Docker context policy is missing ${marker}`);
  }
  if (dockerignore.includes("!.git/config") || dockerignore.includes("!.git/objects")) {
    throw new Error("Relay Docker context must not include Git configuration or objects");
  }
}

function readWebBuild() {
  const path = join(webDist, "build.json");
  if (!existsSync(path)) {
    throw new Error("Web release is missing; run pnpm --filter @vibex/web build:release");
  }
  const build = JSON.parse(readFileSync(path, "utf8"));
  if (
    build.schemaVersion !== "vibex-web-build.v1" ||
    build.profile !== "release" ||
    !/^[0-9a-f]{24}$/.test(build.buildId ?? "") ||
    !/^[0-9a-f]{40}$/.test(build.gitCommit ?? "")
  ) {
    throw new Error("Web release identity is invalid");
  }
  return build;
}

async function verifyWebAssets(baseUrl, build) {
  for (const [path, contentType] of [
    ["/", "text/html"],
    ["/host.js", "text/javascript"],
    ["/pkg/vibex_web.js", "text/javascript"],
    ["/pkg/vibex_web_bg.wasm", "application/wasm"],
    ["/manifest.webmanifest", "application/manifest+json"],
    ["/service-worker.js", "text/javascript"],
    ["/build.json", "application/json"]
  ]) {
    const response = await fetch(`${baseUrl}${path}`, { redirect: "manual" });
    if (!response.ok) throw new Error(`${path} returned HTTP ${response.status}`);
    if (!(response.headers.get("content-type") ?? "").startsWith(contentType)) {
      throw new Error(`${path} returned the wrong content type`);
    }
    await response.arrayBuffer();
  }
  const navigation = await fetch(`${baseUrl}/management/remote`);
  if (!navigation.ok || !(await navigation.text()).includes("data-gate-state")) {
    throw new Error("Relay SPA navigation did not return the Web entry");
  }
  for (const path of ["/missing.js", "/api/missing", "/ws/missing"]) {
    const response = await fetch(`${baseUrl}${path}`);
    if (response.status !== 404) throw new Error(`${path} unexpectedly returned ${response.status}`);
  }
  const servedBuild = await (await fetch(`${baseUrl}/build.json`, { cache: "no-store" })).json();
  assertEqual(JSON.stringify(servedBuild), JSON.stringify(build), "served build.json");
}

async function verifyFreshBrowserBootstrap(baseUrl, build) {
  const { chromium } = await import("@playwright/test");
  const browser = await chromium.launch({
    headless: true,
    args: ["--enable-unsafe-webgpu", "--use-angle=vulkan", "--enable-features=Vulkan"]
  });
  try {
    const context = await browser.newContext();
    const before = await context.storageState();
    assertEqual(before.cookies.length, 0, "fresh browser cookies");
    const page = await context.newPage();
    await page.goto(`${baseUrl}/#/relay-bootstrap-fragment-sentinel`, {
      waitUntil: "domcontentloaded",
      timeout: 45_000
    });
    await page.waitForFunction(
      () => ["ready", "unsupported", "error"].includes(document.body.dataset.gateState),
      null,
      { timeout: 45_000 }
    );
    const snapshot = await page.evaluate(() => {
      const gate = window.__VIBEX_GATE__;
      return {
        state: document.body.dataset.gateState,
        build: gate?.build ?? null,
        remoteState: gate?.remote?.state ?? null,
        root: gate?.rootState?.() ?? null
      };
    });
    assertEqual(snapshot.state, "ready", "fresh browser GPUI state");
    assertEqual(snapshot.build?.buildId, build.buildId, "fresh browser build id");
    assertEqual(snapshot.remoteState, "unconfigured", "fresh browser remote state");
    assertEqual(snapshot.root?.mode, "workbench", "fresh browser product entry");
    await context.close();
  } finally {
    await browser.close();
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

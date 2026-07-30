// Diagnostic probe for the web workflow E2E path. Mirrors the first steps of
// scripts/e2e-workflows.mjs but surfaces the underlying errors, console
// output, and state snapshots that the sealed runner does not print.
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
const bundlePath = process.argv[3] ?? join(ROOT, ".e2e-local-env/web-direct/credentials.json");
const REMOTE_CREDENTIAL_STORAGE_KEY = "vibex.remote-client.credentials.v1";
const credentials = JSON.parse(readFileSync(bundlePath, "utf8"));

const { startWasmServer } = await import(join(ROOT, "scripts/wasm-test-server.mjs"));
const server = await startWasmServer({ port: 14322 });

const browser = await chromium.launch({
  headless: true,
  args: ["--enable-unsafe-webgpu", "--use-angle=vulkan", "--enable-features=Vulkan"]
});
try {
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
  await context.addInitScript(
    ({ key, value }) => window.localStorage.setItem(key, value),
    { key: REMOTE_CREDENTIAL_STORAGE_KEY, value: JSON.stringify(credentials) }
  );
  const page = await context.newPage();
  await page.addInitScript(() => {
    window.__wsLog = [];
    const NativeWebSocket = window.WebSocket;
    window.WebSocket = class extends NativeWebSocket {
      constructor(url, protocols) {
        super(url, protocols);
        const entry = { url: String(url), protocols, events: [] };
        window.__wsLog.push(entry);
        this.addEventListener("open", () =>
          entry.events.push({ kind: "open", protocol: this.protocol })
        );
        this.addEventListener("close", (event) =>
          entry.events.push({
            kind: "close",
            code: event.code,
            reason: event.reason,
            wasClean: event.wasClean
          })
        );
        this.addEventListener("error", () => entry.events.push({ kind: "error" }));
      }
    };
  });
  const consoleLines = [];
  const live = (line) => {
    consoleLines.push(line);
    console.log(line);
  };
  page.on("console", (message) => live(`[${message.type()}] ${message.text()}`));
  page.on("pageerror", (error) => live(`[pageerror] ${error.message}`));
  page.on("requestfailed", (request) =>
    live(`[requestfailed] ${request.url()} ${request.failure()?.errorText}`)
  );
  page.on("request", (request) => {
    if (!request.url().includes(":8443")) live(`[request] ${request.method()} ${request.url()}`);
  });
  page.on("websocket", (socket) => {
    live(`[websocket] ${socket.url()}`);
    socket.on("close", () => live(`[websocket closed] ${socket.url()}`));
    socket.on("socketerror", (error) => live(`[websocket error] ${error}`));
  });

  console.log(`navigating to ${origin}`);
  await page.goto(origin, { waitUntil: "domcontentloaded", timeout: 60_000 });
  await page.waitForFunction(
    () => {
      const gate = window.__VIBEX_GATE__;
      return gate?.wasmReady && ["ready", "offline", "revoked", "incompatible"].includes(gate.state);
    },
    null,
    { timeout: 60_000 }
  );
  console.log("runtime state:", await page.evaluate(() => window.__VIBEX_GATE__.state));

  for (let attempt = 0; attempt < 30; attempt += 1) {
    const workflow = await page.evaluate(() => window.__VIBEX_GATE__.workflowState?.());
    if (workflow?.connection === "online") break;
    await page.waitForTimeout(1000);
  }
  const workflow = await page.evaluate(() => window.__VIBEX_GATE__.workflowState?.());
  console.log("workflowState:", JSON.stringify(workflow).slice(0, 3000));
  const remote = await page.evaluate(() => window.__VIBEX_GATE__.remoteState?.());
  console.log("remoteState:", JSON.stringify(remote).slice(0, 3000));
  const wsLog = await page.evaluate(() => window.__wsLog);
  console.log("wsLog:", JSON.stringify(wsLog, null, 1));
  console.log("--- console log tail ---");
  for (const line of consoleLines.slice(-40)) console.log(line);
} finally {
  await browser.close();
  await server.close();
}

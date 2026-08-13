import { createHash } from "node:crypto";
import { createReadStream, existsSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { dirname, extname, join, normalize, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DEFAULT_DIST = join(ROOT, "apps/mobile-wasm/dist");
const WEB_SOCKET_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const CONTENT_TYPES = new Map([
  [".css", "text/css; charset=utf-8"],
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".json", "application/json; charset=utf-8"],
  [".png", "image/png"],
  [".wasm", "application/wasm"]
]);

function webSocketFrame(payload, opcode = 0x1) {
  const bytes = Buffer.from(payload);
  if (bytes.length < 126) return Buffer.concat([Buffer.from([0x80 | opcode, bytes.length]), bytes]);
  if (bytes.length <= 0xffff) {
    const header = Buffer.alloc(4);
    header[0] = 0x80 | opcode;
    header[1] = 126;
    header.writeUInt16BE(bytes.length, 2);
    return Buffer.concat([header, bytes]);
  }
  throw new Error("gate WebSocket payload exceeds the bounded echo limit");
}

function decodeClientFrames(buffer) {
  const frames = [];
  let offset = 0;
  while (buffer.length - offset >= 2) {
    const first = buffer[offset];
    const second = buffer[offset + 1];
    let length = second & 0x7f;
    let headerLength = 2;
    if (length === 126) {
      if (buffer.length - offset < 4) break;
      length = buffer.readUInt16BE(offset + 2);
      headerLength = 4;
    } else if (length === 127) {
      throw new Error("gate WebSocket does not accept 64-bit payload lengths");
    }
    const masked = (second & 0x80) !== 0;
    const maskLength = masked ? 4 : 0;
    const frameLength = headerLength + maskLength + length;
    if (buffer.length - offset < frameLength) break;
    const maskOffset = offset + headerLength;
    const payloadOffset = maskOffset + maskLength;
    const payload = Buffer.from(buffer.subarray(payloadOffset, payloadOffset + length));
    if (masked) {
      const mask = buffer.subarray(maskOffset, maskOffset + 4);
      for (let index = 0; index < payload.length; index += 1) {
        payload[index] ^= mask[index % 4];
      }
    }
    frames.push({ fin: (first & 0x80) !== 0, opcode: first & 0x0f, payload });
    offset += frameLength;
  }
  return { frames, remaining: buffer.subarray(offset) };
}

function attachWebSocket(server) {
  server.on("upgrade", (request, socket) => {
    const pathname = new URL(request.url ?? "/", "http://localhost").pathname;
    const key = request.headers["sec-websocket-key"];
    if (pathname !== "/__gate/ws" || typeof key !== "string") {
      socket.destroy();
      return;
    }
    const accept = createHash("sha1").update(key + WEB_SOCKET_GUID).digest("base64");
    socket.write(
      "HTTP/1.1 101 Switching Protocols\r\n" +
        "Upgrade: websocket\r\n" +
        "Connection: Upgrade\r\n" +
        `Sec-WebSocket-Accept: ${accept}\r\n\r\n`
    );

    let pending = Buffer.alloc(0);
    socket.on("data", (chunk) => {
      try {
        pending = Buffer.concat([pending, chunk]);
        const decoded = decodeClientFrames(pending);
        pending = decoded.remaining;
        for (const frame of decoded.frames) {
          if (!frame.fin || frame.payload.length > 4096) {
            socket.end(webSocketFrame("", 0x8));
          } else if (frame.opcode === 0x1) {
            socket.write(webSocketFrame(frame.payload));
          } else if (frame.opcode === 0x8) {
            socket.end(webSocketFrame("", 0x8));
          } else if (frame.opcode === 0x9) {
            socket.write(webSocketFrame(frame.payload, 0xa));
          }
        }
      } catch {
        socket.destroy();
      }
    });
  });
}

function serveStatic(response, dist, pathname) {
  const relative = pathname === "/" ? "index.html" : decodeURIComponent(pathname.slice(1));
  const path = normalize(join(dist, relative));
  if (!path.startsWith(`${dist}/`) || !existsSync(path) || !statSync(path).isFile()) {
    response.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" });
    response.end("Not found\n");
    return;
  }
  response.writeHead(200, {
    "Cache-Control": "no-store",
    "Content-Type": CONTENT_TYPES.get(extname(path)) ?? "application/octet-stream",
    "X-Content-Type-Options": "nosniff"
  });
  createReadStream(path).pipe(response);
}

export async function startWasmServer({ dist = DEFAULT_DIST, host = "127.0.0.1", port = 0 } = {}) {
  const absoluteDist = resolve(dist);
  if (!existsSync(join(absoluteDist, "index.html"))) {
    throw new Error(`Mobile runtime dist is missing: ${absoluteDist}`);
  }
  const server = createServer((request, response) => {
    const url = new URL(request.url ?? "/", `http://${request.headers.host ?? "localhost"}`);
    if (url.pathname === "/__gate/fetch") {
      const body = JSON.stringify({ schemaVersion: "vibex-network-probe.v1", ok: true });
      response.writeHead(200, {
        "Cache-Control": "no-store",
        "Content-Length": Buffer.byteLength(body),
        "Content-Type": "application/json; charset=utf-8"
      });
      response.end(body);
      return;
    }
    serveStatic(response, absoluteDist, url.pathname);
  });
  attachWebSocket(server);
  await new Promise((resolvePromise, reject) => {
    server.once("error", reject);
    server.listen(port, host, resolvePromise);
  });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("gate server did not expose a TCP port");
  return {
    origin: `http://${host}:${address.port}`,
    close: () => new Promise((resolvePromise, reject) => server.close((error) => (error ? reject(error) : resolvePromise())))
  };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const requestedPort = Number(process.env.PORT ?? process.argv[2] ?? 4173);
  const running = await startWasmServer({ port: requestedPort });
  console.log(`Vibex mobile WASM development host: ${running.origin}`);
  const stop = async () => {
    await running.close();
    process.exit(0);
  };
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);
}

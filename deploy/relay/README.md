# Vibex Self-Hosted Relay Deployment

This directory contains the self-hosted deployment for `vibex-relay-server`.
The Relay is a zero-knowledge room router. The PC and paired devices use
full-duplex WebSocket connections on `/ws`; Remote v2 control, RPC, events, and
binary frames stay E2EE between the device and PC.

The same origin serves one source-bound release build of `apps/web` for
fresh browsers. Static files are public bootstrap code only; room traffic,
pairing claims, device grants, and product data remain inside the existing E2EE
and PC authorization boundary.

The HTTP pair and command routes under `/api/rooms/:room_id/*` remain a
versioned compatibility bridge. They are not the primary Web/mobile transport.
The Relay must not decrypt, authorize, store, or log Vibex business payloads.

## Quick Start

Run the Relay locally from the repository root:

```bash
docker compose -f deploy/relay/docker-compose.yml up --build -d relay-server
curl -fsS http://127.0.0.1:9700/health
curl -fsS http://127.0.0.1:9700/api/info
curl -fsS http://127.0.0.1:9700/build.json
```

Stop it with:

```bash
docker compose -f deploy/relay/docker-compose.yml down
```

Compose publishes `127.0.0.1:9700` by default. Keep that default when Caddy or
Tailscale Serve runs on the same host. For an explicitly managed host network,
override `VIBEX_RELAY_HOST_BIND`; do not expose plain HTTP to the public internet.

## Public HTTPS With Caddy

Point a DNS record at the host, then start the Caddy profile:

```bash
VIBEX_RELAY_SITE_ADDRESS=relay.example.com \
  docker compose -f deploy/relay/docker-compose.yml --profile caddy up --build -d
```

Caddy proxies WebUI files, `/ws`, `/health`, and `/api/*` on one HTTPS origin.
WebSocket upgrades work for both PC and device peers. Use
`https://relay.example.com` as the configured Relay origin; clients derive
`wss://relay.example.com/ws`.

## Private Tailnet With Tailscale Serve

Start the loopback-bound Relay, then publish it only inside the tailnet:

```bash
docker compose -f deploy/relay/docker-compose.yml up --build -d relay-server
tailscale serve --bg http://127.0.0.1:9700
tailscale serve status
```

Use the HTTPS URL shown by `tailscale serve status` as the Relay origin. Serve
terminates tailnet HTTPS and proxies WebSocket upgrades, so PC and device peers
still use `/ws`. Remove only this mapping with
`tailscale serve --https=443 off`.

## Runtime Configuration

The image sets `VIBEX_RELAY_BIND_ADDR=0.0.0.0:9700`. `RELAY_PORT` is a local
shorthand used only when `VIBEX_RELAY_BIND_ADDR` is absent; it binds
`127.0.0.1:{port}`. It also sets
`VIBEX_RELAY_WEB_STATIC_DIR=/app/web`; startup fails before bind when that
release is missing, incomplete, debug-only, tampered, or does not match the
build identity compiled into the Relay binary.

All hard limits are configurable and the effective values are reported by
`/api/info`:

| Variable | Default | `/api/info` field |
| --- | ---: | --- |
| `VIBEX_RELAY_ROOM_TTL_MS` | `3600000` | `roomTtlMs` |
| `VIBEX_RELAY_BRIDGE_TIMEOUT_MS` | `30000` | `bridgeTimeoutMs` |
| `VIBEX_RELAY_HEARTBEAT_TIMEOUT_MS` | `45000` | `heartbeatTimeoutMs` |
| `VIBEX_RELAY_MAX_ROOMS` | `1024` | `maxRooms` |
| `VIBEX_RELAY_MAX_TOTAL_CONNECTIONS` | `4096` | `maxTotalConnections` |
| `VIBEX_RELAY_MAX_PENDING_PER_ROOM` | `64` | `maxPendingPerRoom` |
| `VIBEX_RELAY_MAX_CONNECTIONS_PER_ROOM` | `1` | `maxConnectionsPerRoom` |
| `VIBEX_RELAY_MAX_DEVICES_PER_ROOM` | `8` | `maxDevicesPerRoom` |
| `VIBEX_RELAY_MAX_BODY_BYTES` | `1048576` | `maxBodyBytes` |
| `VIBEX_RELAY_RATE_LIMIT_WINDOW_MS` | `1000` | `rateLimitWindowMs` |
| `VIBEX_RELAY_MAX_REQUESTS_PER_WINDOW_PER_ROOM` | `120` | `maxRequestsPerWindowPerRoom` |
| `VIBEX_RELAY_MAX_QUEUE_BYTES_PER_CONNECTION` | `4194304` | `maxQueueBytesPerConnection` |
| `VIBEX_RELAY_MAX_BANDWIDTH_BYTES_PER_WINDOW` | `16777216` | `maxBandwidthBytesPerWindow` |
| `VIBEX_RELAY_MAX_PUSH_INSTALLATIONS` | `256` | `maxPushInstallations` |
| `VIBEX_RELAY_MAX_PUSH_DEDUP_ENTRIES` | `4096` | `maxPushDedupEntries` |
| `VIBEX_RELAY_PUSH_ADAPTER_TIMEOUT_MS` | `10000` | `pushAdapterTimeoutMs` |

`VIBEX_RELAY_MAX_CONNECTIONS_PER_ROOM` must remain `1`: one authoritative PC
owns a room, while `VIBEX_RELAY_MAX_DEVICES_PER_ROOM` bounds concurrent paired
device routes. All numeric values must be positive; the push adapter timeout is
also capped at 60 seconds.

## Optional Push Adapter

Push is disabled by default and does not affect Direct or Relay correctness.
Vibex does not use an official Relay or shared provider account. To enable push,
run an operator-owned adapter that implements the selected WebPush, APNs, or FCM
provider protocol, then configure all four values:

```bash
VIBEX_RELAY_PUSH_PROVIDER=web_push
VIBEX_RELAY_PUSH_AUTH_TOKEN='<inbound-random-token-at-least-24-bytes>'
VIBEX_RELAY_PUSH_ADAPTER_URL='https://push-adapter.example.com/v1/dispatch'
VIBEX_RELAY_PUSH_ADAPTER_AUTH_TOKEN='<adapter-random-token-at-least-24-bytes>'
```

`VIBEX_RELAY_PUSH_AUTH_TOKEN` protects the Relay registration and dispatch
endpoints. `VIBEX_RELAY_PUSH_ADAPTER_AUTH_TOKEN` is a separate bearer credential
used only for the outbound adapter request. Store both in a protected deployment
environment or secret manager; never put them in client URLs or screenshots.

The adapter receives one bounded JSON request:

```json
{
  "registration": {
    "installationId": "opaque-installation-id",
    "provider": "web_push",
    "providerToken": "provider-routing-token-or-subscription"
  },
  "notification": {
    "notificationId": "opaque-notification-id",
    "installationId": "opaque-installation-id",
    "opaqueLocator": "short-lived-opaque-locator",
    "expiresAtMs": 0,
    "ciphertext": "optional-e2ee-payload"
  }
}
```

The adapter owns provider credentials, VAPID/APNs/FCM protocol details, and the
generic notification presentation. It must not add prompt text, approval detail,
paths, diffs, Terminal output, or Provider configuration. A notification opens a
`#/notify/<notificationId>/<opaqueLocator>` or equivalent `vibex://notify/...`
link; the app clears that locator and fetches authoritative state from the PC.

The Relay records deduplication only after a 2xx adapter response. Adapter
timeout, network failure, or non-2xx response returns HTTP 502 with
`relay_push_provider_unavailable`; an unconfigured adapter returns HTTP 503.
`/api/info.features.pushProviderConfigured` is true only when the complete
adapter configuration is present.

## Health And Features

```bash
curl -fsS http://127.0.0.1:9700/health
curl -fsS http://127.0.0.1:9700/api/info
```

The primary transport flags should be:

- `pcWebsocket: true`
- `deviceWebsocket: true`
- `websocketFrames: true`

`httpPairBridge` and `httpCommandBridge` remain true for compatibility.
`staticRoomAssets: true` and a complete `webBuild` descriptor are expected from
the container deployment. Library and local test configurations that omit
`VIBEX_RELAY_WEB_STATIC_DIR` remain transport-only and report
`staticRoomAssets: false`. Feature flags never enable Agent, Git, File,
Terminal, or Provider data without the encrypted PC handshake.

## Production Notes

- Use HTTPS/WSS outside explicit loopback development.
- Keep the Relay disabled in the client until the user configures this
  self-hosted origin and room id.
- Treat room ids as routing metadata, not authorization secrets. PC-side device
  auth, permissions, revocation, and audit remain authoritative.
- Expose only `80/443` publicly when Caddy is on the same host. Keep port `9700`
  loopback-bound or on an internal container network.
- Logs may contain bounded room/connection/correlation/status/count metadata.
  They must not contain auth tokens, provider tokens, private keys, opaque
  notification fields, decrypted payloads, raw ciphertext, or nonces.

The Dockerfile builds the pinned GPUI-WASM toolchain and Relay binary from the
same source context, compiles the Web build id and revision into the binary,
and copies only the binary, validated Web dist, and `/app/relay-image.json` into
the runtime image. It does not package desktop SDKs, local databases, device
grants, Git configuration, repository objects, or provider credentials.

## Verification

Run deterministic local checks:

```bash
pnpm smoke:relay:local
pnpm check:relay-webui-package
cargo test -p vibex-remote-client --test relay_smoke --locked -- --nocapture
docker compose -f deploy/relay/docker-compose.yml config
docker build -f deploy/relay/Dockerfile -t vibex-relay-webui:test .
```

The Rust smoke covers Relay-only pairing, E2EE RPC/event/binary transport,
FileTransfer, Terminal output, Direct fallback and recovery, and device revoke.
Use `docs/smoke/relay-nat.md` for physical NAT, Wi-Fi/cellular, background, and
PC sleep/reconnect verification.

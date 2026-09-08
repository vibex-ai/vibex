# Vibex Headless Runtime Deployment

This directory deploys `vibex-server`, the authoritative headless Vibex
runtime for cloud servers. All development environments (workspaces, agent
CLIs, terminals, provider configuration) run on the server; PCs and phones
connect as paired remote clients over Remote v2. The server is a **single
user's** runtime: it owns one Vibex home, one database, and one gateway. It
is not a multi-tenant service.

Architecture in one line: the desktop runtime and the cloud server are the
same `DesktopRuntime` core behind different frontends — the desktop adds a
GPUI window, the server adds a daemon lifecycle. Both expose the identical
Remote v2 gateway, so a paired mobile client cannot tell them apart.

## What the server is responsible for

- the authoritative SQLite database (`VIBEX_DB_PATH`),
- managed agent installations and sessions (ACP agents run on the server),
- the Remote v2 Gateway (`/ws/v2`, `/api/v2/*`) with device pairing,
- workspaces, Git operations, terminals, and provider settings.

Provider API keys are stored in the **server** database in cloud mode. That
is the deliberate trade-off of self-hosting: the host operator is the user.
The gateway never logs secrets, pairing codes, or auth tokens; provider
credential values are never returned to clients after storage (they are
write-only), and device permissions gate every management operation.

## Quick Start (local smoke)

```bash
docker compose -f deploy/server/docker-compose.yml up --build -d vibex-server
docker logs vibex-server | grep pairing_code=
curl -fsS http://127.0.0.1:8765/api/v2/info
```

Stop it with:

```bash
docker compose -f deploy/server/docker-compose.yml down
```

The startup log prints `server_id`, `endpoint`, and a one-time numeric
`pairing_code` (grouped `NNN-NNN-NNN`, expires after 15 minutes by default).
Only its SHA-256 hash is persisted, it is single-use, and claiming it is
rate-limited like every other unauthenticated route.

## Pairing a device

### Mobile (native client)

1. Open Vibex on the phone and choose **Pair with a Cloud Server**.
2. Enter the server address (`https://vibex.example.com`) and the numeric
   code printed by the server.
3. The claim request carries a fresh client identity key in its JSON body;
   the server issues an active device grant, and the phone pins the
   server's identity key from `/api/v2/info` before storing the credential.
4. The code is never placed in a URL, so it stays out of proxy and access
   logs.

### Managing devices headlessly

```bash
docker exec vibex-server vibex-server pairing-code --permission read-only
docker exec vibex-server vibex-server status
docker exec vibex-server vibex-server revoke DEVICE_ID --reason "lost device"
```

`read-only` devices may read sessions, files, Git state, and provider
storage records but cannot mutate anything. `approve-only` adds permission
prompt resolution. `full-control` is required for provider mutations,
device management, session lifecycle, and Terminal/file writes.

## Public HTTPS

Direct public exposure requires `VIBEX_DEPLOYMENT_MODE=public` plus one of:

- `VIBEX_TLS_MODE=trusted_https_proxy` — a reverse proxy terminates TLS. Set
  `VIBEX_TRUST_FORWARDED_HEADERS=true` **only** in this mode; the gateway
  derives per-peer rate-limit keys from the forwarded client address and
  refuses forwarded headers everywhere else. The bundled Caddy profile does
  exactly this:

  ```bash
  VIBEX_PUBLIC_HOST=vibex.example.com \
  VIBEX_DEPLOYMENT_MODE=public \
  VIBEX_TLS_MODE=trusted_https_proxy \
  VIBEX_TRUST_FORWARDED_HEADERS=true \
  VIBEX_ALLOWED_HOSTS=vibex.example.com \
  VIBEX_ALLOWED_ORIGINS=https://vibex.example.com \
    docker compose -f deploy/server/docker-compose.yml --profile caddy up -d
  ```

- `VIBEX_TLS_MODE=server_certificate` — the gateway terminates TLS itself.
  Mount `VIBEX_TLS_CERT_FILE` / `VIBEX_TLS_KEY_FILE` (PEM) into the
  container and publish the port directly.

The gateway validates the combination at startup: a Public listener without
trusted TLS, with loopback hosts in `VIBEX_ALLOWED_HOSTS`, or with forwarded
headers outside a trusted proxy **refuses to boot**. Misconfiguration cannot
silently degrade into an unauthenticated Internet service.

`VIBEX_ALLOWED_HOSTS` must list the exact public hostnames; requests whose
`Host` header is not on the list are rejected (`421`). Browser origins must
match `VIBEX_ALLOWED_ORIGINS` (`403` otherwise). Native clients send no
`Origin` and are unaffected.

## Environment Reference

| Variable | Default | Meaning |
| --- | --- | --- |
| `VIBEX_HOME` | `/data` | Runtime home; database, agent installs, identity keys. |
| `VIBEX_DB_PATH` | `$VIBEX_HOME/vibex.db` | Authoritative SQLite path. |
| `VIBEX_BIND_ADDR` | `127.0.0.1:8765` | Gateway listener address. |
| `VIBEX_DEPLOYMENT_MODE` | `loopback` | `loopback`, `lan`, or `public`. |
| `VIBEX_TLS_MODE` | `loopback_http` for loopback, `trusted_https_proxy` otherwise | `loopback_http`, `trusted_https_proxy`, `pinned_certificate`, `server_certificate`. |
| `VIBEX_TLS_CERT_FILE` / `VIBEX_TLS_KEY_FILE` | unset | Required when `server_certificate`. |
| `VIBEX_PUBLIC_HOST` | unset | Advertised direct candidate for pairing clients. |
| `VIBEX_ALLOWED_HOSTS` | `localhost,127.0.0.1,::1` | Comma-separated accepted `Host` values. |
| `VIBEX_ALLOWED_ORIGINS` | loopback HTTP origins | Comma-separated accepted browser origins. |
| `VIBEX_TRUST_FORWARDED_HEADERS` | `false` | Trust `X-Forwarded-For` (trusted proxy only). |
| `VIBEX_GATEWAY_ENABLED` | `true` | Master switch for the Remote v2 listener. |
| `VIBEX_MAX_CONNECTIONS` | gateway default | Concurrent WS connections. |
| `VIBEX_MAX_IN_FLIGHT_RPCS` | gateway default | Per-connection RPC concurrency bound. |
| `VIBEX_OUTBOUND_QUEUE_CAPACITY` | gateway default | Per-connection event queue bound. |
| `VIBEX_UNAUTHENTICATED_REQUESTS_PER_WINDOW` | gateway default | Per-peer limit for unauth routes (info, claim, ws-ticket). |
| `VIBEX_AUTH_FAILURES_PER_WINDOW` | gateway default | Per-peer auth-failure budget before temporary block. |
| `VIBEX_RATE_LIMIT_WINDOW_MS` | gateway default | Rate-limit window length. |
| `VIBEX_MAX_TRACKED_PEERS` | gateway default | Bounded peer-tracking table size. |
| `VIBEX_ACQUIRE_HOME_LOCK` | `true` | Fail fast when another runtime owns the home. |
| `VIBEX_INSTALL_MANAGED_ADAPTERS` | `true` | Provision managed agent runtimes at boot. |
| `VIBEX_EVENT_CAPACITY` | `512` | Runtime broadcast event backlog. |
| `VIBEX_APPLICATION_ID` | `dev.vibex.server` | Home-lock application identity. |
| `VIBEX_DELEGATION_SIDECAR_COMMAND` | unset | Optional agent delegation sidecar command. |

All limits stay bounded: oversized bodies are rejected before parse, tracked
peers are capped, and pairing codes live at most
`RemoteTrustService::MAX_PAIRING_TTL_MS`.

## Systemd (no Docker)

See `systemd.md` for a hardened unit that runs `vibex-server` directly on
the host with the same environment contract.

## Cloud server as a remote client (PC → cloud)

A desktop can connect to a cloud runtime the same way the phone does: pair
once with a code, then point the desktop's remote-client mode at the
credential. The cloud server owns the room's authority seat; connected
desktops and phones are clients. (Desktop client-mode UI is landing
separately; the transport, credential, and pairing layers shipped here are
the pieces it plugs into.)

## Production Notes

- One runtime per `VIBEX_HOME`. The home lock (`VIBEX_ACQUIRE_HOME_LOCK`)
  prevents two servers from sharing a database.
- Back up `/data` (database + `relay/desktop-identity.json`): losing the
  identity key forces every device to re-pair.
- Keep the database and TLS keys owned by the runtime user; the container
  runs as a non-root `vibex` user with no shell.
- Logs contain bounded metadata (device ids, permission decisions, audit
  summaries). They never contain auth tokens, pairing codes, provider
  secrets, or payload contents — the trust service redacts them at the
  source.
- The server is single-user by design. Do not run a shared/multi-tenant
  deployment: device grants are per-person, provider credentials are stored
  server-side, and there is no tenant isolation.

## Verification

```bash
cargo build -p vibex-server --locked
VIBEX_HOME=$(mktemp -d) cargo run -p vibex-server -- config-check
cargo test -p vibex-remote --lib --locked          # trust service + gateway perimeter
docker compose -f deploy/server/docker-compose.yml config
docker build -f deploy/server/Dockerfile -t vibex-server:test .
```

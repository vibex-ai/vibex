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

Provider API keys are stored on the **server host**, never in the database and
never returned to clients. A desktop runtime writes them to the OS keychain; a
headless server has no usable keychain (the default container seccomp profile
rejects the keyutils syscalls the Linux keychain backend needs), so it writes
them to `provider-secrets.json` inside its runtime home (`/data` by default,
owner-only file permissions) instead. That is the deliberate trade-off of
self-hosting: the host operator is the user. Set
`VIBEX_PROVIDER_SECRET_STORE=keychain` to force the OS keychain (for a server
host that has a real one), or `=file` to force the host secret file.
The gateway never logs secrets, pairing codes, or auth tokens; provider
credential values are never returned to clients after storage (they are
write-only), and device permissions gate every management operation.

## Quick Start (local smoke)

```bash
docker compose -f deploy/server/docker-compose.yml up --build -d vibex-server
docker logs vibex-server | grep pairing_code=
curl -fsS http://127.0.0.1:8765/api/v2/info
```

The stack runs on the host network (`network_mode: host`), and the gateway
binds the host loopback `127.0.0.1:8765`. `RemoteGatewayConfig::validate`
refuses a loopback deployment on a non-loopback bind, and a loopback bind
inside a bridge-network container would be unreachable through published
ports — host networking keeps the bind, the validation, and the reachability
consistent. No ports are published: nothing listens beyond the loopback.

To pair a desktop dev build against this container, enter
`http://127.0.0.1:8765` and the pairing code under **Settings → Remote
Runtime**. Plain HTTP over loopback is a development-only exception
(`cfg!(debug_assertions)`); a release build refuses it, so a packaged desktop
or a phone pairs through the **Local network** setup below instead.

Stop it with:

```bash
docker compose -f deploy/server/docker-compose.yml down
```

The startup log prints `server_id`, `endpoint`, and a one-time numeric
`pairing_code` (grouped `NNN-NNN-NNN`, expires after 5 minutes by default,
capped at 30 via `pairing-code --ttl-ms`). Only its SHA-256 hash is
persisted, it is single-use, and claiming it is rate-limited like every
other unauthenticated route. Alongside the code it prints a `pairing_link`
(`vibex://pair#/code/…`), the certificate `tls_fingerprint`, and a QR
rendering of the link; `docker exec vibex-server vibex-server pairing-code`
mints a fresh set at any time.

## Local network (home or office LAN)

The common self-hosted shape is one machine running `vibex-server` with a
desktop and a phone on the same network. There is no domain name and no
certificate authority, so the runtime serves its **own** certificate:
a self-signed certificate derived deterministically from its runtime
identity. Clients learn that certificate from the connection string the
server prints — not from the network — and pin it, which is what makes the
first request trustworthy.

```bash
VIBEX_BIND_ADDR=0.0.0.0:8765 \
VIBEX_DEPLOYMENT_MODE=lan \
VIBEX_TLS_MODE=pinned_certificate \
VIBEX_PUBLIC_HOST=192.168.1.10:8765 \
VIBEX_HEALTHCHECK_URL=https://127.0.0.1:8765/api/v2/info \
  docker compose -f deploy/server/docker-compose.yml up --build -d vibex-server

docker logs vibex-server | grep -E 'pairing_(code|link|hint)|tls_fingerprint'
```

- `VIBEX_PUBLIC_HOST` must be the address the clients dial, including the
  port, and it must be a numeric LAN address (`192.168.x.x`, `10.x.x.x`,
  `172.16–31.x.x`, or a link-local/unique-local IPv6 address). A pinned link
  is restricted to local addresses on purpose: pairing must not become a way
  to bypass the public CA ecosystem for an Internet host.
- The same certificate is served for the lifetime of the runtime identity, so
  the pin is stable across restarts — restarting the container does **not**
  require re-pairing. Deleting `/data` (which holds
  `relay/desktop-identity.json`) does.
- `VIBEX_TLS_MODE=pinned_certificate` really terminates TLS on the gateway.
  The listener is never plaintext, and a client without the pin cannot
  connect to it at all.

Pair from the printed material:

- **Desktop** — **Settings → Remote Runtime → Pair with connection string**,
  paste the whole `vibex://pair#/code/…` value and press **Pair**. The
  **Server certificate** row then shows the pinned `sha256:…` fingerprint so
  it can be compared with `tls_fingerprint` in the server log.
- **Mobile** — on the pairing screen either scan the QR with the in-app
  scanner or paste the same string into the connection-string field.

The QR is dense (the certificate travels inside it, about 100 columns
wide); widen the terminal before reading it, or use the connection string.
`tls_fingerprint` is `sha256:<base64url>` over the DER certificate.

Stop the LAN listener with `docker compose -f deploy/server/docker-compose.yml
down`. Revoke a device with
`docker exec vibex-server vibex-server revoke DEVICE_ID`; use **Forget** on
the client to drop its stored credential.

## Pairing a device

### Mobile (native client)

1. Open Vibex on the phone and choose **Pair with a Cloud Server**.
2. Enter the server address (`https://vibex.example.com`) and the numeric
   code printed by the server — or, for a runtime that serves its own
   certificate, scan the printed QR or paste the `vibex://pair#/code/…`
   connection string into the connection-string field.
3. The claim request carries a fresh client identity key in its JSON body;
   the server issues an active device grant, and the phone pins the
   server's identity key from `/api/v2/info` before storing the credential.
   A connection string additionally pins the server's TLS certificate, which
   arrives inside the string rather than over the network.
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

Direct public exposure requires `VIBEX_DEPLOYMENT_MODE=public` plus one of
the TLS policies below. The stack keeps host networking in both profiles;
the two modes differ only in what the gateway binds and who terminates TLS.

- `VIBEX_TLS_MODE=trusted_https_proxy` — a reverse proxy terminates TLS. Set
  `VIBEX_TRUST_FORWARDED_HEADERS=true` **only** in this mode; the gateway
  derives per-peer rate-limit keys from the forwarded client address and
  refuses forwarded headers everywhere else. The bundled Caddy profile does
  exactly this: it runs on the host network and forwards to the gateway on
  the host's loopback, so keep the gateway bound to `127.0.0.1:8765` in this
  mode:

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
  container, bind it on `0.0.0.0:8765` (host networking puts that on the
  host's interfaces), and publish nothing — TLS is end to end to the
  gateway.

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
| `VIBEX_PUBLIC_HOST` | unset | Address advertised to pairing clients, and the address used to build the printed connection string. Include the port (`192.168.1.10:8765`) for a direct LAN runtime. |
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
| `VIBEX_DELEGATION_SIDECAR_COMMAND` | `vibex-server` itself | Executable that serves `--agent-delegation-mcp`. The headless binary implements that entry point, so sub-agent delegation works without a second artifact. |

All limits stay bounded: oversized bodies are rejected before parse, tracked
peers are capped, and pairing codes live at most
`RemoteTrustService::MAX_PAIRING_TTL_MS`.

## Systemd (no Docker)

See `systemd.md` for a hardened unit that runs `vibex-server` directly on
the host with the same environment contract.

## Desktop as a remote client (PC → cloud)

A desktop connects to a cloud runtime the same way the phone does:

1. Open the desktop workbench and go to **Settings → Remote Runtime**.
2. Enter the server address and the one-time pairing code the server printed
   at startup, then press **Pair**. For a runtime that serves its own
   certificate, paste the printed `vibex://pair#/code/…` connection string
   into **Pair with connection string** instead: it carries the certificate,
   so no system CA or `mkcert` root is needed.
3. The workbench claims the code, pins the server identity, stores the
   credential under the desktop home with restrictive permissions, and
   switches to remote-client mode. Sessions, timelines, messaging, runtime
   selection, and session management are driven over Remote v2.
4. **Forget** clears the stored credential and boots the local runtime again.
   A stored credential also reconnects automatically on the next launch;
   set `VIBEX_DISABLE_REMOTE_CLIENT=1` to force the local authority once.

Authority-local features degrade explicitly in remote-client mode. The desktop's
own self-update, remote-access publication setup, and the stored provider
credential read-back stay on the client machine; a temporary session asks the
authority for its workspace root, and managed worktree creation/lifecycle,
usage statistics, and the live per-session token snapshot are served over
Remote v2. Local CLI-history import and the client's storage usage/cleanup
remain client-side.

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
cargo test -p vibex-remote-client --test pinned_pairing_link_smoke --locked
cargo test -p vibex-core --lib pairing_code_link --locked
docker compose -f deploy/server/docker-compose.yml config
docker build -f deploy/server/Dockerfile -t vibex-server:test .
```

# Relay NAT Smoke Checklist

Use this checklist to verify the self-hosted Relay path when the PC is behind
NAT and only opens an outbound connection to the Relay server.

The Relay is not a trusted business server. It must forward opaque encrypted
Relay frames only; the PC runtime remains authoritative for remote auth,
permissions, Agent sessions, files, Git, terminal, Provider settings, audit, and
timeline catch-up.

## Preconditions

- A self-hosted Relay is running from `deploy/relay/` or an equivalent build of
  `vibex-relay-server`.
- The Relay is reachable from the browser/mobile network at a final URL, for
  example `https://relay.example.com`.
- The PC desktop runtime can reach that Relay URL outbound.
- The PC Relay client is intentionally enabled with a known room id.
- A browser or mobile shell can use Web/PWA Relay mode.
- A test device auth proof exists for the PC remote runtime.
- A disposable workspace/session exists for harmless remote actions.

Do not capture auth tokens, pairing codes, private keys, decrypted payloads,
raw ciphertext, nonce values, prompt bodies, file paths, terminal content, Git
diffs, Provider settings details, or provider secrets in logs, screenshots, or
bug reports.

## Local Deterministic Check

Run this from the repository root before doing a physical NAT test:

```bash
pnpm smoke:relay:local
```

Pass criteria:

- the script builds `vibex-relay-server`.
- `/health` is reachable.
- `/api/info` is reachable.
- `/api/info` reports:
  - `pcWebsocket: true`
  - `deviceWebsocket: true`
  - `websocketFrames: true`
  - `httpPairBridge: true`
  - `httpCommandBridge: true`
  - `staticRoomAssets: false`
- environment overrides for device and queue limits appear in `/api/info`.
- the global WebSocket connection limit appears in `/api/info`.

Run the transport integration smoke as well:

```bash
cargo test -p vibex-remote-client --test relay_smoke --locked -- --nocapture
```

It verifies Relay-only pairing, E2EE RPC/events, FileTransfer and Terminal
binary frames, Direct-to-Relay fallback, Relay-to-Direct recovery without
re-pairing, and immediate revoke.

This local check does not prove NAT traversal or mobile UX. It proves the Relay
binary can start and report the bridge features that the NAT path needs.

## Deployment Check

1. Start the Relay on the target host.

   ```bash
   docker compose -f deploy/relay/docker-compose.yml up --build -d relay-server
   ```

2. If using Caddy, start the proxy profile with the public host.

   ```bash
   VIBEX_RELAY_SITE_ADDRESS=relay.example.com \
     docker compose -f deploy/relay/docker-compose.yml --profile caddy up --build -d
   ```

   For a private tailnet deployment instead, keep the Relay loopback-bound and
   publish it with Tailscale Serve:

   ```bash
   tailscale serve --bg http://127.0.0.1:9700
   tailscale serve status
   ```

3. Verify health and info through the same URL the PC and phone will use.

   ```bash
   curl -fsS https://relay.example.com/health
   curl -fsS https://relay.example.com/api/info
   ```

4. Confirm the Relay URL has one origin for both PC and browser/mobile:

   - PC Relay WebSocket: `wss://relay.example.com/ws`.
   - browser/mobile full-duplex WebSocket: `wss://relay.example.com/ws`.
   - compatibility pair endpoint:
     `https://relay.example.com/api/rooms/:room_id/pair`.
   - compatibility command endpoint:
     `https://relay.example.com/api/rooms/:room_id/command`.

## PC Outbound Room Check

1. In the PC desktop Relay settings, set:

   - enabled: true.
   - Relay URL: the public Relay origin, for example
     `https://relay.example.com`.
   - room id: the non-secret room id shown by the PC settings.

2. Start the PC Relay client.

3. Check Relay health again.

   ```bash
   curl -fsS https://relay.example.com/health
   ```

4. Pass criteria:

   - `activeRooms` increases to `1`.
   - `activeConnections` is `1` before a device connects and increases by one
     per active device, up to the configured room limit.
   - no Relay log line contains auth tokens, pairing codes, private keys,
     prompt bodies, file paths, terminal content, Git diffs, Provider settings,
     raw ciphertext, or nonce values.

Room ids and connection ids may appear in logs. They are routing metadata, not
business authorization.

## Browser Or Mobile Relay Check

1. Open Web/PWA Relay mode in a browser or the mobile shell.

2. Enter only non-secret Relay metadata:

   - Relay URL.
   - room id.

3. Enter the normal PC remote device auth proof through the Web/PWA auth form.
   Do not put auth tokens in launch links, QR URLs, screenshots, or log output.

4. Test the connection.

5. Pass criteria:

   - Web/PWA validates Relay `/api/info`.
   - a first Relay-only claim can use the bounded compatibility pair/command
     bridge without exposing the pairing challenge.
   - normal Web/PWA Remote v2 handshake, RPC, events, and binary frames use the
     device WebSocket on `/ws`.
   - the UI enables business surfaces from the decrypted PC
     `RemoteHandshakeResponse`, not from Relay `/api/info`.

## Encrypted Business Operation Check

Run one harmless non-secret business action through Relay, for example:

- list sessions.
- open a known disposable session.
- send a short non-secret Agent message in a disposable workspace.
- fetch Git status for a disposable repository.
- fetch a terminal snapshot that contains no private output.
- download a disposable file through the binary FileTransfer attachment.

Pass criteria:

- the operation succeeds through Relay mode.
- the same permission level behavior is observed as direct LAN mode.
- revoked or under-permissioned devices fail inside the encrypted PC business
  response, not as Relay-side authorization.

## Zero-Knowledge Log Check

Use a disposable marker that is not a secret and is not reused outside this
smoke. Put it only in a harmless business action, then search Relay logs on the
Relay host.

Example command shape:

```bash
docker compose -f deploy/relay/docker-compose.yml logs relay-server | grep -F "<your-disposable-marker>"
```

Pass criteria:

- the marker is absent from Relay logs.
- Relay logs show only room/connection/correlation/status/timing/count metadata.

Do not paste the marker, prompt body, command output, file path, Git diff, or
Provider setting value into captured evidence.

## Reconnect And Catch-Up Check

1. While the browser/mobile client is connected through Relay, record the latest
   visible timeline item or session state.
2. Refresh the browser/mobile app, or temporarily stop and restart the PC Relay
   client.
3. Create one safe timeline change from the PC runtime while the remote client
   is disconnected.
4. Reconnect through the same Relay URL and room id.

Pass criteria:

- Web/PWA pairs again with memory-only Relay key material.
- the existing device auth proof remains subject to PC revocation and
  permission checks.
- missed timeline/session state appears after authoritative fetch/catch-up
  before relying on live-derived state.

## Network Route Recovery Check

1. Configure Auto mode with one healthy Direct candidate and the same
   self-hosted Relay candidate.
2. Make Direct temporarily unreachable and connect through Relay.
3. Restore Direct reachability, then change Wi-Fi/cellular network or resume the
   app to trigger a route probe.
4. Repeat in the other direction by making Direct unreachable again.

Pass criteria:

- Auto selects Relay only after Direct probe/connect failure.
- after network recovery Auto selects healthy Direct without claiming a new
  pairing offer or rotating the durable device identity.
- route changes preserve cursors and require authoritative catch-up on a new
  session epoch; in-flight mutations are never replayed automatically.

## Optional Push And Deep Link Check

Skip this section when push is intentionally disabled; `/api/info` must then
report `pushProviderConfigured: false` and Relay connectivity must remain normal.

When enabled, configure an operator-owned WebPush/APNs/FCM adapter as documented
in `deploy/relay/README.md`, register one disposable installation, and dispatch
an opaque notification.

Pass criteria:

- the operator adapter receives only provider routing data, notification id,
  short-lived opaque locator, expiry, and optional ciphertext.
- adapter failure returns `relay_push_provider_unavailable` and a retry does not
  get incorrectly deduplicated.
- the opened app immediately clears the locator from history, reconnects using
  the paired device, and performs an authoritative PC fetch before displaying
  session or approval details.
- resolved, expired, revoked, offline, and duplicate notifications do not show
  stale business state.

## Evidence To Capture

Safe evidence:

- Relay URL host only, without auth tokens or pairing codes.
- Relay `/health` counts.
- Relay `/api/info` feature flags and limits.
- PC Relay status state, room id, and connection state.
- browser/mobile screenshot showing Relay mode connected.
- structured error code if a step fails.
- reconnect/catch-up observation notes.

Do not capture:

- auth tokens or pairing codes.
- private keys.
- decrypted payloads.
- raw ciphertext or nonce values.
- prompt bodies.
- file paths or file contents.
- terminal content.
- Git diffs.
- Provider setting details.

## Pass Criteria

- Relay starts independently and health/info are reachable through the intended
  public URL.
- PC connects outbound and keeps exactly one active room connection for the
  smoke room.
- browser/mobile reaches the PC through Relay mode.
- encrypted remote handshake and at least one business operation succeed.
- Relay logs do not contain plaintext business payload material.
- reconnect/catch-up restores missed authoritative state after refresh or
  temporary failure.

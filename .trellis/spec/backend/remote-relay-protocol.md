# Remote and Relay Protocol

Vibex remote access must let the mobile client control the PC runtime
without moving trust to the Relay server. The PC remains authoritative for local
files, Git, terminal, Agent sessions, Provider profiles, and timeline history.

Current evidence: [Architecture Baseline](../guides/architecture-baseline.md), `crates/core/src/remote.rs`,
`crates/remote`, `crates/relay`, and their tests.

> Legacy cutover note (2026-07-29): later Tauri wiring examples from the former
> desktop shell are historical evidence. The current GPUI Desktop reuses the
> `apps/desktop` path and is the only product runtime consumer; current composition
> enters through `DesktopRuntime` and the GPUI Backend facade.

## Scenario: Online Mobile Agent Notifications

### 1. Scope / Trigger

- Trigger: changing `AgentNotificationIntent`, Remote live-event topics,
  native Android/iOS notification presentation, or notification-tap routing.
- This scenario covers an installed mobile client with an active authenticated
  Direct or self-hosted Relay Remote v2 connection.

### 2. Signatures

```text
Remote v2 topic/channel = "agent_notification"
BackendEvent::Notification(AgentNotificationIntent)
AgentBackend::resolve_opaque_locator(notification_id, opaque_locator)
```

### 3. Contracts

- The PC gateway subscribes to the authoritative Agent notification stream and
  emits a typed `agent_notification` live event. Access requires
  `ReadAgentSession`; the topic is available to read-only and stronger paired
  device grants.
- Direct and Relay clients subscribe to the same topic and apply contiguous
  generation/sequence duplicate rules within one connection before exposing a
  typed backend event. The topic is ephemeral: never persist or hand off its
  cursor across reconnect because it has no authoritative catch-up operation;
  stable notification IDs provide replacement across connections. Shared
  timeline controllers ignore this event because it is presentation intent,
  not timeline state.
- Native mobile rejects expired intents and suppresses the OS notification only
  while its target session is visibly selected in the foreground. Otherwise it
  presents generic, localized copy with the stable notification ID so the OS can
  replace a duplicate delivery.
- Ask for OS notification permission only after a paired backend connects.
  Existing paired installs receive the same one-time Android request on upgrade;
  Android persists that the request was made rather than prompting at every
  launch.
- Notification taps carry only `notification_id + opaque_locator`. Buffer a
  cold-start tap until the GPUI app subscribes and the paired backend is online,
  then call the authenticated PC resolver. Navigate only when it returns a
  resolved authoritative session; invalid, expired, deleted, or revoked targets
  never navigate directly from native payload data.
- Lock-screen copy and native payloads must not include prompt/answer text,
  commands, file paths, tool output, approval details, or any auth material.

### 4. Background Delivery Boundary

- A live Remote event plus a local native notification works while the mobile
  process retains its authenticated connection. It does not promise delivery
  after iOS/Android suspends or kills the process.
- True background delivery requires the user's self-hosted Relay and its
  operator-configured APNs/FCM adapter. Never copy the Relay operator bearer
  into pairing offers or mobile credentials. Until paired-device registration
  and dispatch have their own authenticated Relay contract, UI and docs must
  not claim killed-process background push.

### 5. Tests Required

- Core/Agent tests cover stable privacy-bounded intent production.
- Gateway tests include `agent_notification` in permission-filtered topics.
- Remote-client tests decode the topic into `BackendEvent::Notification`.
- Mobile tests cover bounded action validation and cold-start buffering;
  `pnpm check:mobile-native` covers Android/iOS host contract drift.

### 6. Wrong vs Correct

```text
Wrong: native payload session id -> open session without backend validation
Correct: buffered id + opaque locator -> authenticated PC resolve -> open session

Wrong: put the self-hosted Relay operator bearer in a pairing bundle
Correct: keep online delivery live-only until a paired-device push auth contract exists
```

## Scenario: Remote Protocol v2 Gateway And Pairing

### 1. Scope / Trigger

- Trigger: changing protocol v2 wire types, the Direct RemoteGateway,
  pairing-offer persistence, WS tickets, device identity proof, or remote binary
  streams.
- `docs/remote/protocol-v2.md` documents the externally consumed compatibility
  and deployment contract. This scenario owns implementation invariants.

### 2. Signatures

```text
RemoteJsonMessageV2 = control | rpc_request | rpc_response | event | unknown
RemoteBinaryFrame = "VBX2" + header_length + RemoteBinaryFrameHeader + raw bytes
RemoteAttachRequestV2 { attachment_id, kind, resource_id, scope_id,
  generation, after_sequence }
RemoteGateway::start/stop/restart/status/disconnect_device
RemoteGatewayPairingRoutes { direct_candidates, relay_candidate }
RemoteTrustService::create/cancel/claim_pairing_offer
POST /api/v2/pairing/claim
POST /api/v2/ws-ticket
GET /ws/v2
SQLite migration v29 = pairing-offer fields + remote_devices.grant_revision
```

### 3. Contracts

- Keep legacy `0.4` envelopes frozen while new clients use version-range-negotiated
  `2.0`; Direct and Relay wrap the same v2 business frames.
- A WS connection must consume a short-lived single-use ticket, send hello first,
  prove possession of the paired X25519 device identity, and bind the proof to the
  ticket challenge, hello transcript, desktop identity, and session epoch.
- Use X25519 + HKDF-SHA256 + HMAC-SHA256 from reviewed crates; never invent a
  string-concatenated or unhashed proof. Each connection uses a fresh client and
  server ephemeral key and confirms the derived session key.
- Pairing offers live 60-120 seconds. SQLite stores only challenge/grant/claim-nonce
  hashes. Device creation, conditional offer consumption, and audit append share
  one transaction; concurrent claim has one winner.
- Pairing routes are server-owned deployment configuration. A remote
  `create_pairing_offer` request may select permission/TTL, but
  `RemoteGatewayPairingRoutes` replaces any client-supplied Direct/Relay candidates
  before offer creation. `server_info.enabled_features` advertises
  `device_pairing` only when at least one validated Direct/Tailnet or self-hosted
  Relay route exists; `device_management` alone must not enable pairing UI.
- RemoteGateway is disabled and loopback by default. LAN requires explicit opt-in
  plus trusted HTTPS/WSS proxy policy. Validate Host, exact/same-authority Origin,
  CORS/PNA, URL secret keys, and WebSocket subprotocol.
- RPC timeout is request-local and per-connection RPC concurrency is bounded.
  Mutation retries require an idempotency key.
  Revision/generation/cursor gaps surface structured resync contracts.
- Terminal binary frames carry raw bytes and sequence/generation. File binary
  framing is frozen but upload remains capability-gated until its transactional
  sink is implemented. Live attachments reauthenticate and validate their
  workspace/domain scope before streaming; Terminal input is generation-checked,
  separately authorized, and audited without retaining bytes.
- Revoke signals every active device connection immediately. Stop/restart drains
  listeners and connections and advances `session_epoch`.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Protocol ranges do not overlap | Close with `unsupported_version`; do not enter active state. |
| WS ticket is missing, expired, or reused | Reject upgrade with `remote_ws_ticket_*`; a consumed ticket never returns to the map. |
| Device key is malformed/non-contributory, proof is invalid, or device is revoked | Reject hello with the matching `remote_device_*` / `remote_client_ephemeral_key_invalid` error. |
| LAN mode lacks trusted HTTPS proxy policy or uses a loopback/default Host allowlist | Configuration fails before binding. |
| Host/Origin is rejected or a secret-like query key is present | Reject before routing; never echo the URL value. |
| Pairing offer is expired, canceled, tampered, replayed, or loses a concurrent claim | Return the stable `remote_pairing_offer_*` error and create no orphan device. |
| Pairing routes are empty, use the wrong transport kind, exceed eight Direct candidates, or advertise Direct while the listener is disabled | Reject with `remote_pairing_routes_unavailable`, `remote_pairing_*_candidate_invalid`, `remote_pairing_candidates_too_many`, or `remote_pairing_direct_gateway_disabled`; create no offer. |
| Mutation omits a required idempotency key or exceeds the per-connection RPC limit | Return `remote_idempotency_key_required` or `remote_rpc_concurrency_limit`; keep the socket alive. |
| Live Terminal scope differs from its workspace or binary generation is stale | Return `remote_terminal_scope_mismatch` or `remote_binary_generation_stale`; write zero bytes. |

### 5. Good / Base / Bad Cases

- Good: a paired device exchanges its grant for one WS ticket, proves its static
  identity, confirms an ephemeral session key, attaches to a workspace-scoped
  Terminal, and every input write is authorized and audited without bytes.
- Good: a full-control device requests a new pairing offer while the Gateway
  injects its validated LAN/Tailnet/self-hosted Relay routes; client-supplied
  routes never enter the launch fragment.
- Base: RemoteGateway remains disabled on loopback; legacy `0.4` routes still
  serve migration clients while v2 bindings and docs remain available.
- Bad: put a long-lived grant in a query string, accept default loopback hosts
  in LAN mode, stream a Terminal by id without workspace scope, compare HMAC
  with ordinary equality, or permit unlimited spawned RPC tasks.

### 6. Tests Required

- Core golden JSON and binary round-trip, including unknown enum/message safety.
- Offer success, cancel, expiry, wrong desktop identity, challenge tamper, replay,
  and concurrent claim; plaintext secrets must be absent from SQLite/debug/audit.
- Host/Origin/PNA/secret-query negative tests.
- Real WebSocket single-use ticket, hello identity proof, server_info, RPC error,
  revoke close, and listener start/stop/restart tests.
- Device-management tests must prove that route-less servers omit
  `device_pairing`, read-only devices cannot create/cancel offers, and a malicious
  client route is replaced by server configuration without appearing in Debug or
  the resulting offer.
- Binding drift plus `cargo test -p vibex-core remote`, `cargo test -p vibex-db
  remote`, `cargo test -p vibex-remote`, and `cargo test -p
  vibex-desktop-runtime remote`.

### 7. Wrong vs Correct

#### Wrong

```text
wss://desktop/ws/v2?grant=<long-lived-secret>
attach { kind: terminal, resourceId: terminal_id }
spawn one unbounded task per inbound RPC
create_pairing_offer { directCandidates: [client_controlled_url] }
```

#### Correct

```text
POST /api/v2/ws-ticket with authenticated body -> one-use subprotocol ticket
hello -> constant-time identity proof -> server_info key confirmation
attach { kind: terminal, resourceId: terminal_id, scopeId: workspace_id }
bounded per-connection RPC semaphore + bounded outbound queue
create_pairing_offer request -> Gateway injects RemoteGatewayPairingRoutes
```

## Scenario: LAN Discovery Pairing

### 1. Scope / Trigger

- Trigger: changing LAN pairing DTOs, `_vibex._tcp.local.` advertisement or
  native discovery bridges, the Desktop approval window, or mobile nearby-pairing
  claim flow.
- LAN discovery is a short-lived pairing entry over an existing trusted Direct
  HTTPS route. It is not a long-term transport and does not move authoritative
  state out of `DesktopRuntime` / `RemoteGateway`.

### 2. Signatures

```text
GET  /api/v2/pairing/lan
  -> RemoteLanPairingDiscoverySummary
POST /api/v2/pairing/lan/request
  RemoteLanPairingRequest -> RemoteLanPairingRequestAccepted
POST /api/v2/pairing/lan/status
  RemoteLanPairingStatusRequest -> RemoteLanPairingStatusResponse

RemoteGateway::{start_lan_pairing_window,lan_pairing_window_status,
  approve_lan_pairing_request,reject_lan_pairing_request,
  cancel_lan_pairing_window}
RemoteConnectivityController::{start_lan_pairing_window,
  lan_pairing_window_status,approve_lan_pairing_request,
  reject_lan_pairing_request,cancel_lan_pairing_window}
LanPairingSession::{start,poll,claim_approved}
DNS-SD service type = _vibex._tcp.local.
```

### 3. Contracts

- At most one in-memory Desktop LAN window is active. The default product TTL
  is 90 seconds; offer TTL remains bounded by the existing 60-120 second pairing
  contract. Cancel, expiry, successful claim, Gateway stop/restart, Direct route
  loss, or dialog close clears the window and stops advertisement.
- TXT contains exactly `version=1`, `advertisement_id`, `display_name`,
  `protocol_min=2`, `protocol_max=2`, and `pairing=available`. Service instance
  is at most 63 bytes, display name at most 192 bytes, advertisement id 16-128
  bytes, and the bounded TXT representation at most 512 bytes. Offer/window/
  request ids, challenge, request secret, grant, credentials, workspace data,
  and identity fingerprints are forbidden.
- Mobile treats DNS-SD as untrusted addressing. It builds a credential-free,
  path/query/fragment-free exact HTTPS origin from resolved SRV hostname/port,
  disables redirects, performs normal certificate/hostname validation, then
  validates discovery and `/api/v2/info`. Server id/key and v2 protocol must
  match; deployment mode must be `lan`, TLS policy `trusted_https_proxy`, and
  fixed paths must be `/ws/v2`, `/api/v2/pairing/claim`,
  `/api/v2/pairing/lan`, `/api/v2/pairing/lan/request`,
  `/api/v2/pairing/lan/status`, and `/api/v2/ws-ticket`.
- `RemoteLanPairingRequest` binds window id, X25519 device public key, bounded
  display name, client nonce, request secret, and idempotency key. The client
  generates a 32-byte request secret and retains it only in memory. Desktop
  retains only HMAC-SHA256 under a process-random key. A window permits at most
  eight pending requests; a repeated idempotency tuple returns the same request,
  while any changed binding conflicts.
- Both devices derive the six-digit SAS from the length-prefixed transcript:
  LAN schema version, window id, request id, server id, server identity public
  key, device identity public key, and client nonce. Mobile recomputes and
  compares the returned code before waiting. Desktop must show the same code,
  device name, and short public-key fingerprint before explicit approval.
- Status polling is no faster than once per 500ms and puts the request secret in
  the JSON body, never URL/log/Debug. Pending/rejected/expired/claimed status has
  no offer. Only an approved request with matching secret returns the existing
  `RemotePairingOffer`; approving one request rejects every other pending request.
- Mobile validates the approved offer with the shared offer validator and
  requires exactly one `RemotePairingTransport::Direct` candidate whose
  normalized HTTPS origin equals the validated discovery origin. It then uses
  the existing claim endpoint and persists the unchanged
  `MobileCredentialBundle`, including all validated post-pairing routes.
- Android discovery uses `NsdManager` with local-network/Wi-Fi permissions for
  target SDK 35; iOS declares `NSLocalNetworkUsageDescription` and
  `NSBonjourServices = [_vibex._tcp]`. Browsing runs only in the foreground
  pairing flow and stops on selection, cancellation, success, or page exit.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Malformed/oversized service instance, SRV authority, TXT, unknown TXT key, or non-v2 advertisement | `remote_lan_discovery_invalid`; do not contact or list it as trusted. |
| HTTP origin, credentials, path/query/fragment, TLS failure, or redirect | `remote_lan_discovery_invalid` / normal TLS-network error; never downgrade to HTTP. |
| Discovery and `/api/v2/info` identity differ | `remote_lan_server_identity_mismatch`; submit no request. |
| Fixed path, deployment mode, or TLS policy differs | `remote_lan_gateway_policy_invalid`; submit no request. |
| Window absent/expired or reaches eight pending requests | `remote_lan_pairing_window_unavailable` / `remote_lan_pairing_request_limit`. |
| Idempotency binding changes | `remote_lan_pairing_request_conflict`; create no second request. |
| Request secret is malformed or wrong | `remote_lan_pairing_request_unauthorized`; return no enumerable request detail. |
| Poll interval is below 500ms | `remote_lan_pairing_poll_rate_limited`; retain the request. |
| Returned SAS does not match the shared transcript | `remote_lan_pairing_verification_invalid`; stop the flow. |
| Approved offer identity or Direct origin differs | `remote_lan_server_identity_mismatch` / `remote_pairing_entry_route_mismatch`; do not claim. |
| Native local-network permission is denied | `mobile_local_network_permission_denied` UI state; QR and stored routes remain usable. |

### 5. Good / Base / Bad Cases

- Good: mobile lists two same-name advertisements as distinct candidates, the
  user selects one, both devices show the same SAS, Desktop approves it, and the
  existing claim returns a normal mobile credential bundle.
- Base: Desktop has a valid Direct route but no active LAN window, so it emits no
  advertisement and LAN endpoints return window unavailable; QR/Tailnet/Relay
  pairing remains unchanged.
- Bad: trust TXT identity, auto-select the first service, advertise an offer id
  or challenge, store a plaintext request secret on Desktop, approve without
  SAS comparison, return an offer while pending, follow an HTTPS redirect, or
  replace the offer-owned route with an mDNS URL.

### 6. Tests Required

- Core tests freeze DTO JSON, unknown-safe request state, deterministic SAS, and
  redacted `Debug` for secret-bearing request/status/offer values.
- Coordinator tests cover single-window replacement refusal, eight-request
  bound, idempotent retry/conflict, secret hashing/wrong secret, 500ms polling,
  one approval winner, reject, expiry, cancel, and claim cleanup.
- Gateway integration proves discovery/request/status routing, 8KiB request
  body and 16-request concurrency bounds, pending offer absence, approved offer
  presence, and no secret reflection.
- Desktop tests use a fake advertiser/probe to prove mobile-compatible HTTPS
  validation before start and advertisement stop on every terminal lifecycle.
- Remote-client tests cover strict origin normalization, identity/path/policy
  mismatch, SAS recomputation, exact Direct route match, and secret redaction.
- Native mobile tests cover duplicate display names, malicious/oversized TXT,
  permission/discovering/empty/waiting/rejected/expired/success states, and QR
  fallback. Run `pnpm check:mobile-native`; real iOS and Android same-LAN tests
  remain required before release.

### 7. Wrong vs Correct

#### Wrong

```text
TXT: offer_id=<id>, challenge=<secret>, origin=http://192.168.1.2
first discovered service -> auto approve -> save a new LAN transport
GET /status?requestSecret=<secret> -> offer while pending
```

#### Correct

```text
bounded non-secret TXT -> user selects -> exact HTTPS + info validation
request body secret -> shared SAS -> explicit Desktop approval
approved body-secret status -> existing offer validator + exact Direct route
existing claim transaction -> existing MobileCredentialBundle
```

## Scenario: Zero-Configuration LAN Pairing Bootstrap

### 1. Scope / Trigger

- Trigger: changing zero-configuration discovery, the temporary encrypted HTTP
  bootstrap, the pinned-TLS local Gateway, or mobile LAN credential persistence.
- The UI entry remains separate from Direct/Tailnet/Relay publication. The
  resulting local HTTPS/WSS route is independently usable and must never require
  Tailnet, a user-managed Direct proxy, or self-hosted Relay.

### 2. Signatures

```text
POST /api/v2/pairing/lan-zero/hello
  RemoteZeroConfigLanPairingHello
    -> RemoteZeroConfigLanPairingHelloAccepted
POST /api/v2/pairing/lan-zero/{request,status,claim}
  RelayEncryptedFrame -> RelayEncryptedFrame

RemoteGateway::{start_zero_config_lan_pairing,
  start_local_lan_gateway,stop_local_lan_gateway,local_lan_gateway_info,
  zero_config_lan_pairing_window_status,
  approve_zero_config_lan_pairing_request,
  reject_zero_config_lan_pairing_request,
  cancel_zero_config_lan_pairing}
RemoteConnectivityController mirrors the same lifecycle.
ZeroConfigLanPairingSession::{start,poll,claim_approved}
RemoteZeroConfigLanPairingHelloAccepted {
  lan_gateway_port, lan_gateway_tls_certificate, ...
}
MobileRemoteRouteBundle {
  local_network?: { url, tls_certificate_der }, direct_candidates, relay
}
DNS-SD service type = _vibex._tcp.local.; TXT mode = zero_config
```

### 3. Contracts

- Desktop starts a separate pinned-TLS Gateway on `0.0.0.0:1429` before opening
  the temporary `0.0.0.0:0` HTTP bootstrap. The bootstrap serves only the four
  `lan-zero` endpoints and stops after claim/cancel/expiry; the TLS Gateway serves
  the normal typed RemoteGateway router and remains available for paired devices.
- `remote-access.json.localNetworkEnabled` is backward-compatible and independent
  of the three publication methods. Starting zero-config pairing persists it;
  startup reconciliation restores it; global disable clears it and stops both
  bootstrap advertisement and TLS listener. Route reconfiguration must restore
  the local listener after a normal Gateway stop/restart.
- The local certificate/key are deterministically derived from the long-term
  Desktop X25519 private identity with HKDF-SHA256 domain
  `vibex.remote.local-lan-tls.v1`, then encoded as an Ed25519 PKCS#8 key and a
  self-signed certificate for `vibex-lan.local`. A Desktop identity therefore
  yields the same certificate across listener and process restarts without a
  second secret file.
- Zero-config pairing requires the local TLS Gateway, not another published
  route. The encrypted offer includes exactly one Direct placeholder origin
  `https://vibex-lan.local:<lan_gateway_port>` plus any optional existing routes.
  Mobile must never resolve or persist that placeholder.
- TXT is bounded to 512 bytes and contains exactly `version=1`,
  `mode=zero_config`, advertisement/display fields, v2 protocol bounds,
  `pairing=available`, server id, and the Desktop X25519 public identity.
  Treat the advertised identity as an untrusted key-agreement input, not a
  trust root. Offer/window/request ids, challenge, request secret, grant, and
  private keys remain forbidden.
- Desktop emits canonical TXT key spelling. Native DNS-SD bridges normalize
  keys case-insensitively at the Rust event boundary, accept standard
  non-secret `txtvers=1` metadata when a platform supplies it, and reject
  duplicate keys after normalization. A bridge that omits `mode` is accepted as
  zero-config only when both zero-config identity keys are present and the
  remaining key set is exact; desktop advertisements always include
  `mode=zero_config`.
- Both zero-config listeners are currently IPv4-only because Desktop binds
  `0.0.0.0`. Native resolvers must pass a resolved numeric address to Rust
  instead of the advertised `.local`/mDNS hostname and prefer IPv4 when the
  platform returns multiple addresses; Rust must not perform a second mDNS
  lookup for the pairing listener. Android scans
  `NsdServiceInfo.getHostAddresses()` where available and iOS searches for an
  `AF_INET` service address before falling back to another numeric family. Rust
  revalidates loopback/private/link-local IPv4 for zero-config while Direct HTTPS
  retains its existing numeric IPv6 support. If zero-config resolution supplies
  only IPv6, discard that candidate and keep browsing; do not claim zero-config
  IPv6 support until both Desktop listeners are dual-stack.
- A malformed, stale, or incompatible candidate is local to that candidate:
  discard it and continue browsing. Only the native discovery service's own
  permission or browsing failure terminates the active discovery flow. One
  untrusted LAN advertisement must never suppress later valid candidates.
- Mobile constructs one credential-free exact `http` origin from a resolved
  loopback/private/link-local numeric IPv4 DNS-SD address, bypasses all proxies,
  disables redirects, and uses it only for bootstrap. It constructs the durable
  route by replacing only scheme and port with
  `https://<same-numeric-address>:<lan_gateway_port>`.
- Hello carries a fresh mobile peer id, ephemeral X25519 public key, and
  16-256 byte nonce. Public keys use unpadded base64url on this wire. Both peers
  convert the key bytes to the Relay library's standard-base64 representation
  before `RelaySession::establish_with_suite`, so the DirectionalV2 KDF sees an
  identical canonical string on both ends. The length-prefixed KDF context binds
  the hello nonce, LAN Gateway port, and full base64url certificate DER; changing
  any route field makes subsequent encrypted frames fail authentication.
- Mobile validates a nonzero port and bounded valid DER certificate, confirms the
  encrypted offer contains exactly the placeholder for that port, then stores the
  numeric route and certificate in `MobileRemoteRouteBundle.localNetwork`.
  Existing credentials deserialize with `localNetwork = None`. The local route is
  first in Auto candidate order; optional Direct/Tailnet/Relay routes remain
  fallback candidates, not prerequisites.
- Pinned HTTP uses a proxy-free, redirect-free reqwest client whose only root is
  the stored certificate and whose hostname exception applies only to that trust
  store. Pinned WSS uses an exact-leaf rustls verifier, validates chain/time/TLS
  signatures against the same single root, and accepts no intermediates. Both
  paths require HTTPS/WSS plus a local numeric address; an accept-all verifier is
  forbidden.
- The local Gateway accepts only loopback/private/link-local numeric Host or
  HTTP/2 `:authority`, requires HTTPS Origin, and retains normal device grant,
  ticket, identity-proof, permission, and protocol checks.
- Request, status, offer challenge, claim, and grant exist only inside
  `RelayEncryptedFrame`. Desktop still shows device name, fingerprint, and the
  shared six-digit SAS before explicit approval. At most 16 bootstrap sessions,
  16 concurrent requests, one active window, eight pending devices, an 8KiB
  body, and the existing 500ms minimum status interval are permitted.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Local TLS Gateway is absent or its port/certificate is invalid | `remote_local_lan_gateway_unavailable` / `remote_local_lan_route_invalid`; create no bootstrap session. |
| Listener bind/address or advertiser setup fails | `remote_zero_config_pairing_listener_*` / `remote_zero_config_advertisement_failed`; cancel the offer and listener. |
| Unknown/oversized TXT, invalid server id/key, non-v2 mode, or malformed origin | Reject that candidate with `remote_lan_discovery_invalid` / `remote_zero_config_pairing_origin_invalid`, keep browsing, and send no hello. |
| Zero-config resolution has no IPv4 address, or Rust receives an IPv6/hostname origin | Discard that candidate locally and keep browsing; show no pairing-origin error and send no hello. |
| Hello nonce/key is malformed or non-contributory | `remote_zero_config_pairing_hello_invalid` / `remote_zero_config_pairing_hello_rejected`; create no session. |
| Session is unknown, expired, replayed, or frame authentication fails | `remote_zero_config_pairing_session_*` / `remote_zero_config_pairing_frame_invalid`; expose no plaintext payload. |
| Session/request limits are reached | Existing bounded LAN busy/limit response; allocate no unbounded task or session. |
| Advertised, hello, discovery, or offer server identity differs | `remote_zero_config_pairing_identity_mismatch`; do not request, claim, or persist credentials. |
| Offer placeholder, LAN port, or certificate differs from encrypted hello | `remote_zero_config_pairing_route_mismatch` / `_route_invalid`; persist nothing. |
| Pinned route is HTTP, hostname/public IP, oversized/malformed DER, or presents another leaf | `remote_pinned_tls_route_invalid` / `remote_tls_certificate_invalid` / transport TLS failure; do not fall back by weakening verification. |
| HTTP/2 request has no Host header but has valid local `:authority` | Apply the same local numeric allowlist and continue; do not report `remote_host_required`. |

### 5. Good / Base / Bad Cases

- Good: with no Tailnet, Direct publication, or Relay configured, Mobile discovers
  Desktop, compares SAS, claims through the encrypted bootstrap, persists the
  resolved LAN IP/certificate, and reaches v2 Online over pinned HTTPS/WSS.
- Base: the three remote method controls and QR flow work unchanged while no
  zero-config window is active; the fourth entry has its own permission and
  start/stop state.
- Base: an IPv6 result arrives before a usable IPv4 result; Mobile ignores the
  IPv6-only candidate and presents the later IPv4 candidate without surfacing a
  pairing-origin error.
- Bad: attach a nearby button to every transport row, advertise the claim
  challenge, send request/status/claim as plaintext HTTP JSON, save
  `vibex-lan.local`, use the first resolved address without checking the Desktop
  listener family, require Tailnet/Relay, or disable all certificate checks.

### 6. Tests Required

- Core golden JSON freezes the strict hello shape and secret-bearing DTO Debug
  output remains redacted.
- Gateway integration proves encrypted request/status/claim, stable derived
  certificate, local-only offer creation, listener cleanup, bounds, local Host
  and HTTP/2 authority validation, and request secrets whose base64url payload
  contains `-` (prefix parsing removes only the first separator).
- Remote-client integration must start both listeners on random ports, configure
  no published route and no Relay, complete approval/claim, connect with pinned
  HTTPS/WSS, reach Online, and heartbeat. Unit tests cover numeric-address policy,
  malformed pins, proxy/redirect refusal, KDF binding, identity, and SAS.
- Desktop advertiser/controller tests cover strict TXT, start failure cleanup,
  expiry, route-change cancellation, and separate UI state. Native mobile tests
  cover mode filtering, invalid identity metadata, duplicate names, IPv4
  selection, IPv6-only candidate discard, lifecycle, claim persistence, and QR
  fallback. Run `pnpm check:mobile-native`.

### 7. Wrong vs Correct

#### Wrong

```text
_vibex TXT challenge=<secret> -> POST plaintext claim -> save mDNS HTTP as route
pair locally -> save vibex-lan.local -> require Tailnet when DNS fails
first DNS-SD address is IPv6 -> build an origin for an IPv4-only listener
Direct / Tailnet / Relay rows each render "Allow nearby device"
```

#### Correct

```text
bounded TXT -> hello binds nonce + LAN port + certificate -> encrypted claim
resolved IPv4 matching Desktop listener + exact pin -> HTTPS/WSS RemoteGateway v2
Tailnet/Direct/Relay = optional fallback routes; bootstrap HTTP stops after claim
```

## Scenario: GPUI Desktop Remote Publication Controller

### 1. Scope / Trigger

- Trigger: changing `RemoteConnectivityController`, the pinned local-network
  listener, Direct/Tailnet publication, self-hosted Relay publication
  validation, or `RemoteGateway` reconfiguration.
- The GPUI Desktop runtime is the only product consumer. Frozen Tauri/React
  clients do not receive a parallel command layer.

### 2. Signatures

```text
~/.vibex/remote-access.json -> RemoteConnectivitySettingsV1 {
  schema_version, desired_enabled, local_network_enabled, generation,
  tailscale, direct, relay, last_successful_pairing_entry, updated_at_ms
}
RemoteConnectivityController::{snapshot,reconcile_on_startup,
  enable_direct,enable_tailscale,enable_relay,disable_method,
  disable_all,repair_method,create_pairing_offer,pairing_offer_status,
  cancel_pairing_offer,record_claimed_pairing_entry,
  start_zero_config_lan_pairing,cancel_zero_config_lan_pairing}
RemoteGateway::{current_config,apply_config_while_stopped,set_pairing_routes,
  create_pairing_offer,pairing_offer_status,cancel_pairing_offer,
  start_local_lan_gateway,stop_local_lan_gateway}
LOCAL_LAN_GATEWAY_PORT = 1429
TailscalePublication::{inspect,create,remove_owned}
GET <direct-origin>/api/v2/info -> DirectProbeInfo
GET <relay-origin>/api/info -> RelayPublicationInfo
```

### 3. Contracts

- `remote-access.json` is versioned, atomically replaced, Unix-private, and
  contains intent plus exact non-secret route metadata only. Corrupt, symlinked,
  or future-version state is quarantined and starts fail-closed; pairing grants,
  challenges, private keys, credentials, and raw CLI output are forbidden.
- `localNetworkEnabled` is an independent durable intent, defaults to false when
  absent in an older settings file, and participates in `desiredEnabled` without
  appearing as a Direct/Tailnet/Relay method row. Production binds its pinned-TLS
  Gateway on `0.0.0.0:1429`; startup restores it without probing or enabling any
  other route. Opening zero-config pairing starts and persists this listener;
  closing the pairing window leaves it available to already paired devices.
- A local-network settings transition is committed to disk before replacing the
  controller's in-memory settings. Failed enable persistence cancels bootstrap
  state, advertisement, and the newly started local listener. Failed disable
  persistence preserves the enabled in-memory intent and listener so restart and
  UI state cannot falsely report a completed disable.
- One async operation lock serializes enable, disable, repair, startup
  reconciliation, Gateway configuration, and Relay lifecycle changes. Startup
  may restore local Gateway/Relay state and inspect external systems, but never
  creates or removes a Tailscale handler or edits a user reverse proxy.
- Direct and Tailnet always target `http://127.0.0.1:1428`. The Gateway uses a
  validated immutable config snapshot for each listener epoch. Full config
  replacement is stopped-only and serialized with route replacement; changing
  only the Relay pairing route updates the live server-owned route without
  restarting or advancing the Direct listener epoch.
- Tailscale uses the executable `tailscale` without a shell, `sudo`, Funnel,
  reset, or broad clear. Inspection uses only `status --json` and
  `serve status --json`. Create/remove use exact
  `serve --bg --https=<port> <target>` and `serve --https=<port> off` argv plus
  before/after inspection. Explicit background mode is required because current
  CLI releases otherwise keep the create command attached. Port 443 is
  preferred; a conflict proposes the first free port in 8443-8450 and requires
  confirmation before mutation. Confirmation is operation-local: disabling an
  owned route clears its stored port, so a later re-enable must inspect again
  and may require a new confirmation even when it proposes the same fallback
  port. A previous confirmation never authorizes a new Serve mutation.
- A reused handler is external unless its exact origin, port, root path, target,
  and `desktop_created` ownership were persisted. Removal is idempotent when the
  owned route is already absent, but refuses mutation when the port has a
  mismatched or sibling handler. A failed disable retains cleanup metadata;
  repair retries disable when desired state is false instead of re-enabling.
- Direct and Relay probes accept an exact HTTPS origin without credentials,
  query, fragment, or path. They disable redirects, bound timeout/body size, and
  validate Desktop identity where applicable, protocol, fixed paths, and
  transport features before publishing a pairing candidate. Persistence
  failure after validation withdraws the candidate and exposes the stable
  storage error.
- Tailscale Serve validation always uses a proxy-bypassing HTTP client. A
  user-managed Direct origin also bypasses environment/system proxies when its
  host is loopback, RFC1918 IPv4, private or link-local IPv6, Tailscale CGNAT
  `100.64.0.0/10`, or an exact/child `ts.net` name; public Direct and self-hosted
  Relay origins retain system proxy behavior. Requested bypass must never fall
  back to a proxy-aware client or mutate process proxy variables/global proxy
  configuration.
- A published Direct/Tailscale Gateway separates network Host authority from
  client Origin authority. Allowed Hosts contain only validated published Direct
  origins; configured HTTPS origins are exact entries for claim, ticket, and
  WebSocket requests. An application origin must never add a host to the LAN
  Gateway Host allowlist.
- Native mobile credential storage treats only an absent sandbox file as an
  unpaired device. Writes are atomic and owner-only on Unix; malformed or
  invalid records are removed and reported as typed credential failures rather
  than silently becoming an empty credential store. Auth grants and identity
  private keys stay out of `Debug` output.
- Desktop pairing creates a 90-second offer through the controller with only the
  selected permission level; the Gateway injects every validated route. Status
  polling and cancel use the offer id and return only
  `RemotePairingOfferSummary`. The one-time challenge and composed launch URL
  stay in a private, non-`Debug` GPUI field and are cleared on claim, cancel,
  expiry, regeneration failure, global disable, or dialog close.
- Pairing entry selection is presentation state over one offer. Restore
  `last_successful_pairing_entry` only when that method is still present in the
  offer, otherwise prefer Tailnet and then the first deterministic entry.
  Switching entries recomposes the same fragment and must not create a new
  offer. Persist the preference only after `pairing_offer_status` reports a
  claimed device and the selected method is verified as one of that offer's
  routes; enabling, validating, selecting, copying, canceling, or expiring a
  route must not update it.
- Desktop packages and RemoteGateway do not embed or serve mobile application
  assets. Native mobile construction and source identity are owned by the
  Android/iOS package pipeline.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Missing, corrupt, symlinked, or future settings | Quarantine when possible; `remote_connectivity_settings_invalid` or `remote_connectivity_settings_version_unsupported`; enable nothing. |
| Validated method state cannot persist | `remote_connectivity_settings_*`; remove its candidate and expose `retry`. |
| Local listener bind/TLS initialization fails | `remote_local_lan_listener_*` / `remote_local_lan_tls_config_failed`; advertise nothing and do not persist local intent. |
| Enabling the local listener cannot persist | Return `remote_connectivity_settings_*`; cancel bootstrap/advertisement and stop the listener while retaining disabled in-memory state. |
| Disabling the local listener cannot persist | Return `remote_connectivity_settings_*`; retain enabled in-memory state and keep the listener running. |
| Direct identity/protocol/path/security mismatch | `remote_direct_identity_mismatch`, `remote_direct_protocol_incompatible`, `remote_direct_paths_invalid`, or `remote_direct_security_policy_invalid`; publish no candidate. |
| A proxy-bypassing probe client cannot initialize, or a private/Tailnet origin cannot be reached directly | `remote_direct_probe_client_unavailable` or `remote_direct_probe_direct_failed`; publish no candidate and show actionable recovery. |
| Tailscale daemon/DNS/CLI is unavailable | Stable `tailscale_daemon_offline`, `tailscale_dns_unavailable`, `tailscale_binary_missing`, or `tailscale_cli_unsupported`; never mutate Serve. |
| HTTPS 443 is occupied | `tailscale_https_port_confirmation_required` with one free 8443-8450 proposal; no create before confirmation. |
| An owned fallback route was disabled and Tailscale is re-enabled while 443 remains occupied | Return `tailscale_https_port_confirmation_required` again; do not reuse the earlier confirmation or run `serve --bg` before the new confirmation. |
| Owned port has a sibling or mismatched handler | `tailscale_route_ownership_mismatch`; do not run `off`. |
| Relay lacks required device WebSocket/frame or pairing-bridge features | Reject the Relay publication as incompatible; Direct remains online. |
| Offer status references an unknown id | `remote_pairing_offer_unknown`; retain no secret error detail. |
| Entry preference is requested before claim or for a method absent from the offer | `remote_pairing_offer_not_claimed` or `remote_pairing_entry_not_offered`; persist nothing. |

### 5. Good / Base / Bad Cases

- Good: Direct is online, Relay becomes compatible, and only the live Relay
  pairing route changes; the Direct bound address and session epoch are stable.
- Good: with all three publication methods disabled, zero-config pairing enables
  the pinned local listener, and a later process startup restores only that
  listener from `localNetworkEnabled`.
- Good: an interrupted owned Tailscale disable is repaired while desired state
  remains false, then its persisted origin/port/ownership are cleared.
- Good: after an owned fallback Tailscale route is disabled, re-enable performs
  fresh inspection and asks the user to confirm the current fallback proposal
  before recreating the route.
- Good: a user switches an active offer from Tailnet to Direct, Direct completes
  the claim, and only then is Direct stored as the next healthy entry preference.
- Base: default settings perform no process/network side effect and bind no
  listener. An existing exact Tailscale handler is reused as external and is
  never removed by Desktop.
- Base: an older settings file without `localNetworkEnabled` loads it as false
  and retains its existing Direct/Tailnet/Relay intent.
- Bad: follow a probe redirect, publish before durable commit, run `serve reset`,
  infer ownership from target alone, remove a whole port containing sibling
  handlers, restart Direct for a Relay-only route change, store an entry merely
  because it was enabled or selected, or retain a terminal offer's QR material.
  Do not clear proxy environment variables or
  silently retry a requested proxy bypass through the system proxy.
- Bad: treat nearby pairing as a Tailnet enable action, require a pre-existing
  Direct candidate, or mutate local-network memory before a failing disk write.

### 6. Tests Required

- Store tests cover round-trip, atomic/private permissions, corrupt and unknown
  schema quarantine, secret absence, interrupted transitions, and persistence
  failure after successful validation.
- Controller tests must cover a LAN-only persisted startup restore, bootstrap
  cleanup after local enable persistence failure, and listener/state retention
  after local disable persistence failure.
- Adapter tests assert exact Tailscale program/argv, bounded output/timeout,
  daemon and Self DNS parsing, 443 confirmation, deterministic fallback,
  disable/re-enable confirmation renewal, post-mutation verification,
  absent-route idempotency, and sibling refusal.
- Controller/Gateway tests cover duplicate operation serialization, simultaneous
  Direct/Tailnet routes, Relay-only pairing, route-only update without listener
  restart, stopped-only config replacement, immutable epochs, method-independent
  failure/disable, repair of an interrupted disable, offer status/cancel, and a
  real claim proving that only the entry selected at claim time is persisted.
- Probe tests cover proxy bypass for Tailscale and private/Tailnet Direct origins,
  system proxy retention for public Direct origins, and fail-closed behavior when
  the proxy-bypassing client is unavailable.
- Gateway perimeter tests cover exact configured HTTPS preflight origins while
  proving that the published LAN Host allowlist still rejects `localhost`.
- GPUI pairing tests cover the read-only default, all three permission choices,
  healthy preference fallback, entry switching without offer replacement,
  cancel/expiry/regenerate/close cleanup, narrow layout, clipboard failure, and
  safe snapshots whose Debug output contains no fragment, challenge, URL, or QR
  payload.
- Run `cargo test -p vibex-core remote --locked`, `cargo test -p vibex-db remote
  --locked`, `cargo test -p vibex-remote --locked`, `cargo test -p
  vibex-desktop-runtime remote --locked`, `cargo test -p vibex-desktop-runtime
  relay --locked`, `cargo test -p vibex-remote-client --locked`, and
  `pnpm smoke:relay:local`.

### 7. Wrong vs Correct

#### Wrong

```text
tailscale serve reset
nearby pairing -> enable Tailscale -> advertise its route
settings.local_network_enabled = false -> save fails -> stop listener anyway
reuse a previous 8443 confirmation after disabling the owned route
HTTP probe -> follow 302 -> accept copied /api/v2/info
Relay route changed -> stop Direct -> replace config -> start Direct
enable or select route -> persist last_successful_pairing_entry
staticSha256 = index/host/glue only
```

#### Correct

```text
inspect exact handler -> confirm bounded port -> exact create/remove -> verify
start pinned listener:1429 -> persist cloned settings -> publish bootstrap
disable: persist cloned settings -> commit memory -> stop local listener
disable owned route -> inspect again -> request fresh fallback-port confirmation
exact HTTPS origin + redirects disabled + bounded typed probe
Relay route changed -> set_pairing_routes while Direct epoch stays running
offer status reports claimed -> verify offered entry -> persist preference
staticSha256 = static browser assets + normalized built Service Worker
```

## Connection Modes

Support these modes without changing the business protocol:

- NativeBackend in-process desktop access (not a network transport).
- LAN direct access to the PC service.
- User-managed private networks such as Tailscale, ZeroTier, or WireGuard.
- Self-hosted Relay where PC maintains an outbound connection.

V1 does not provide or depend on an official/public Vibex Relay. Future protocol
extensibility must not be presented as a shipped endpoint.

The same request, response, and event envelopes should work across direct and
Relay transports.

## Pairing and Device Trust

First pairing uses QR code or one-time pairing code. Pairing must exchange
device public keys and create a named device record.

Device records need:

- Device id and public key.
- Display name.
- Last online time.
- Revocation state.
- Permission level: read-only, approve-only, or full-control.
- Audit log references.

Revoked devices cannot reconnect even if they still know old Relay room data.

## Transport Envelope

Use a single WebSocket connection for JSON-RPC style requests/responses and live
events. Required fields:

- Message id or correlation id for requests and responses.
- Session/workspace context where applicable.
- Event sequence for live events.
- Timestamp.
- Protocol version.
- Capability/version metadata during handshake.

Terminal high-throughput data may use binary frames or a dedicated channel, but
it must still be scoped to an authorized terminal session.

## Direct WebSocket Attach and Reconnect

Direct LAN/mobile WebSocket mode must support the same attach semantics as the
Agent timeline service:

- The client authenticates during handshake using the current remote auth proof,
  token, or protocol-specific subprotocol.
- The client attaches subscriptions with stable ids and `since_sequence` values.
- The PC service returns replay or snapshot before sending new live events.
- Ping/pong or heartbeat/ack tracks liveness.
- `auth_expired` is a structured protocol condition that asks the client to
  refresh credentials or re-pair; it must not look like an arbitrary socket
  close.
- Reconnect uses exponential backoff and a bounded send queue with idempotency
  keys for user messages, permission resolutions, file/Git mutations, and
  terminal commands.
- Installed mobile registers the native GPUI application lifecycle callback on
  the same platform instance used to construct `gpui::Application`. Entering the
  background sends `AppBackgrounded` to the remote transport owner, stops
  foreground-only discovery/polling work, and preserves the credential bundle.
  Mobile operating systems may suspend or close a background WebSocket, so the
  product must preserve pairing and recover the route rather than claim that a
  socket remains permanently runnable in the background.
- Returning to the foreground sends `AppResumed` to that same transport owner.
  After the transport reports `Online`, mobile replaces the domain-event
  consumer, triggers an authoritative session/timeline/workspace refetch, and
  only then treats the background disconnect as recovered. A disconnect event
  observed while the app is hidden must not leave a stale "connection lost"
  notice after this recovery completes.

Do not let feature hooks own independent reconnect loops. One transport layer
owns reconnect, replay, auth refresh, and queue draining.

## Optional Chat and IM Channels

Chat/IM integrations such as Telegram, Lark/Feishu, Weixin/WeCom, or DingTalk
are weaker-trust control surfaces than paired installed mobile clients. They belong
behind a channel plugin/gateway boundary and must dispatch into the same
`RemoteRequestEnvelope`/timeline/permission path as direct remote clients.

Channel integrations require local approval, pairing code or equivalent proof,
whitelist controls, per-channel permissions, rate limits, encrypted secret
storage, audit logs, and platform-specific card/button mapping. Channel button
presses may resolve permission requests only through the normal permission
service, and duplicate notifications across channels/devices must not create
duplicate resolutions.

## Scenario: Phase 5 Relay Protocol And E2EE Transport Foundation

### 1. Scope / Trigger

- Trigger: Phase 5 first child introduces `crates/relay`, Relay protocol DTOs,
  and E2EE transport helpers.
- This scenario establishes the encrypted transport foundation only. Relay
  server routes, PC outbound connection loops, native mobile Relay mode, mobile
  packaging, Docker/Caddy deployment, and public hosted Relay remain later
  children.

### 2. Signatures

Core DTO source of truth:

```text
RelayRoomId
RelayConnectionId
RelayPeerId
RelayFrameId
RelaySessionId
RelayProtocolVersion { major, minor }
RelayFrameKind =
  pair_request | pair_response | command | response | event |
  heartbeat | heartbeat_ack | error
RelayErrorCode =
  unsupported_protocol | invalid_frame | invalid_room | invalid_correlation |
  crypto_setup_failed | decrypt_failed | replay_detected | frame_out_of_order
RelayHandshakeHello { protocol_version, room_id, peer_id, public_key,
  supported_versions, timestamp_ms }
RelayHandshakeReady { protocol_version, selected_version, room_id, session_id,
  peer_id, public_key, timestamp_ms }
RelayEncryptedFrame { protocol_version, room_id, session_id, frame_id,
  sender_peer_id, recipient_peer_id, correlation_id, kind, nonce, ciphertext,
  counter, created_at_ms }
RelayPlaintextEnvelope { room_id, session_id, sender_peer_id, recipient_peer_id,
  correlation_id, kind, counter, issued_at_ms, business_payload_json }
RelayHeartbeat { room_id, peer_id, connection_id, sequence, sent_at_ms }
RelayHeartbeatAck { room_id, peer_id, connection_id, sequence,
  acknowledged_at_ms }
RelayError { code, message, correlation_id, retryable, created_at_ms }
RelayControlMessage =
  hello | ready | encrypted | heartbeat | heartbeat_ack | error
```

Relay helper API:

```text
RelayKeypair::generate() -> keypair
RelayKeypair::public_key_base64() -> String
RelaySession::establish(local_keypair, remote_public_key_base64,
  RelaySessionConfig { room_id, session_id, local_peer_id, remote_peer_id })
  -> RelaySession
RelaySession::seal_json(kind, correlation_id, business_payload_json)
  -> RelayEncryptedFrame
RelaySession::open_json(RelayEncryptedFrame) -> RelayPlaintextEnvelope
```

### 3. Contracts

- `crates/core/src/relay.rs` owns Relay DTOs. GPUI and native mobile Relay mode consume
  those Rust types through the shared Backend contracts instead of redefining
  frame variants.
- `crates/relay` owns E2EE helpers, encrypted frame encode/decode, room and
  correlation validation, and replay/counter state. It must not depend on
  `crates/remote`, `crates/agent`, `crates/db`, `crates/fs`, `crates/git`,
  `crates/terminal`, Tauri, or provider SDKs.
- Relay transport wraps existing `RemoteRequestEnvelope`,
  `RemoteResponseEnvelope`, and `RemoteLiveEventEnvelope` values as encrypted
  JSON payloads. It must not introduce a second Agent/file/Git/terminal/provider
  business API.
- E2EE uses established Rust crypto crates: X25519-compatible key agreement,
  HKDF-SHA256 session key derivation, and XChaCha20-Poly1305 authenticated
  encryption.
- Session key derivation is bound to room id, session id, peer ids, and public
  keys so copied key material cannot silently apply to a different Relay room or
  peer pair.
- Every encrypted frame authenticates visible metadata as AEAD associated data
  and repeats the same metadata inside the decrypted plaintext envelope:
  protocol version, room id, session id, sender peer, recipient peer,
  correlation id, kind, and counter.
- MVP replay protection is strict and direction-local: each session direction
  starts at counter `1`; the receiver accepts only the next expected counter.
  Duplicate or lower counters fail as replayed. Higher counters fail as
  out-of-order.
- Relay E2EE does not create a `RemoteAuthContext`. After a later PC Relay
  client decrypts a business payload, it must still pass the embedded
  `RemoteAuthProof` through the Phase 4 remote auth and permission helpers.

### 4. Validation & Error Matrix

- Unsupported Relay protocol version -> `remote/relay_unsupported_protocol`.
- Frame room/session mismatch -> `remote/relay_invalid_room`.
- Frame sender or recipient does not match the current session ->
  `remote/relay_invalid_frame`.
- Public key, nonce, ciphertext, plaintext, or associated data is malformed ->
  `remote/relay_invalid_frame` or `remote/relay_crypto_setup_failed`.
- Authenticated decryption fails due to tampering, wrong key, wrong session
  key, or wrong associated metadata -> `remote/relay_decrypt_failed`.
- Duplicate or previously accepted counter -> `remote/relay_replay_detected`.
- Future counter before the expected counter -> `remote/relay_frame_out_of_order`.
- Error/debug output must redact private keys, plaintext business payloads,
  auth tokens, pairing codes, nonce bytes, and ciphertext bytes.

### 5. Good/Base/Bad Cases

- Good: a mobile Relay peer encrypts a `RemoteRequestEnvelope` as an opaque
  `RelayEncryptedFrame`; the PC Relay session decrypts it, verifies metadata and
  counter order, then later dispatches the recovered business envelope through
  `crates/remote` auth and permission checks.
- Base: Relay control DTOs serialize with stable `type` tags, even before
  `apps/relay-server` exists.
- Bad: Relay server code attempts to parse `business_payload_json`, decide
  whether a Git/file/terminal operation is allowed, or log decrypted payload
  fields. Relay servers must forward opaque encrypted frames only.

### 6. Tests Required

- Core serde tests for Relay ids, protocol version, frame
  kinds, control-message tags, heartbeat, error, and encrypted frame DTOs.
- Relay crate tests for encrypt/decrypt happy path with a serialized
  `RemoteRequestEnvelope`.
- Plaintext-leak tests proving serialized `RelayEncryptedFrame` values do not
  contain prompt bodies, auth tokens, file paths, terminal content, Git diffs,
  or Provider details.
- Debug/error redaction tests proving `Debug` output does not include nonce,
  ciphertext, private keys, or decrypted business payloads.
- Tamper, wrong-key, wrong-room/session, replay, and out-of-order tests with
  exact error-code assertions.
- Regression checks: `cargo test -p vibex-core remote`,
  `cargo test -p vibex-remote`, and `pnpm check`.

### 7. Wrong vs Correct

#### Wrong

```text
Relay server receives /api/rooms/:room_id/command
  -> decrypt JSON
  -> inspect operation = git
  -> decide permission from room id
  -> forward or reject
```

This makes Relay trusted, leaks business metadata, and creates a second
authorization path outside PC device permissions.

#### Correct

```text
Relay server receives /api/rooms/:room_id/command
  -> validates room-level transport constraints
  -> forwards RelayEncryptedFrame to PC WebSocket by correlation id

PC Relay client
  -> RelaySession::open_json(frame)
  -> parses RemoteRequestEnvelope from business_payload_json
  -> authenticates RemoteAuthProof through crates/remote
  -> authorizes RemoteActionClass before local side effects
```

The Relay server sees only room/correlation/timing/size metadata and cannot
decrypt, forge, replay, or authorize business operations.

## Scenario: Phase 5 Self-Hosted Relay Server Room Bridge

### 1. Scope / Trigger

- Trigger: Phase 5 second child introduces `apps/relay-server` as an
  independently runnable Axum app.
- This scenario establishes the self-hosted room bridge only. PC outbound Relay
  client wiring, desktop settings, native mobile Relay mode, mobile packaging,
  deployment assets, NAT smoke automation, and public hosted Relay remain later
  children.

### 2. Signatures

Relay server routes:

```text
GET /health -> RelayHealthStatus
GET /api/info -> RelayServerInfo
GET /ws -> PC WebSocket room connection
POST /api/rooms/:room_id/pair RelayControlMessage::Hello
  -> RelayControlMessage::Ready | RelayControlMessage::Error
POST /api/rooms/:room_id/command RelayEncryptedFrame(kind = command)
  -> RelayControlMessage::Encrypted(kind = response) |
     RelayControlMessage::Error
```

Server-private bridge wrapper:

```text
RelayBridgeMessage {
  correlation_id: CorrelationId,
  room_id: RelayRoomId,
  message: RelayControlMessage
}
```

### 3. Contracts

- `apps/relay-server` is a workspace package named `vibex-relay-server` with a
  library router builder and a binary entrypoint.
- The server depends on `crates/core` Relay transport DTOs only. It must not
  depend on `crates/relay` session/decrypt helpers, `crates/remote`,
  `crates/agent`, `crates/db`, `crates/fs`, `crates/git`, `crates/terminal`,
  `apps/desktop`, Tauri, or provider SDKs.
- PC room registration is the first WebSocket frame and must be
  `RelayControlMessage::Hello`. For MVP, a room has at most one active PC
  WebSocket connection.
- HTTP pair and command routes forward a `RelayBridgeMessage` to the room's PC
  WebSocket and wait for the matching `correlation_id`.
- Command routes require the path room to match `RelayEncryptedFrame.room_id`,
  require `kind = command`, and require a visible `correlation_id`.
- The Relay server must not decrypt, re-encrypt, mutate, inspect, or authorize
  encrypted business payloads. It forwards `RelayEncryptedFrame` values as
  opaque JSON.
- Heartbeats update room liveness and receive `RelayHeartbeatAck`. Room TTL,
  heartbeat freshness, pending request timeout, body size, pending count, room
  count, and fixed-window per-room rate limits are in-memory for this child.
- Unregistering the active PC connection removes the whole room and drops its
  disconnect broadcaster. Every device socket subscribed to that room must then
  leave its receive loop, abort its writer, and unregister its exact connection;
  a device socket must not remain attached to a room with no authoritative PC.
- `/r/:room_id/*path` static room assets are intentionally deferred until
  native mobile Relay mode or deployment packaging consumes them.

### 4. Validation & Error Matrix

- Invalid JSON or wrong control-message shape -> HTTP 400 +
  `RelayErrorCode::InvalidFrame`.
- Unsupported Relay protocol version -> HTTP 400 +
  `RelayErrorCode::UnsupportedProtocol`.
- Invalid path room, path/body room mismatch, missing room, expired room, or
  duplicate active room connection -> HTTP 400/404/409 +
  `RelayErrorCode::InvalidRoom` as appropriate.
- Missing command correlation id, pending bridge limit, rate limit, unknown PC
  response correlation, or bridge timeout -> structured
  `RelayErrorCode::InvalidCorrelation`; timeouts are retryable HTTP 504.
- Body size violations -> HTTP 413 + `RelayErrorCode::InvalidFrame`.
- PC responses with an unknown or mismatched correlation id must not complete an
  unrelated HTTP request.
- PC WebSocket close, error, or replacement -> unregister that exact room
  connection and close every device socket registered under it; a stale PC
  connection id must not tear down its replacement.

### 5. Good/Base/Bad Cases

- Good: a browser posts an encrypted command frame to
  `/api/rooms/:room_id/command`; the Relay validates room/correlation metadata,
  forwards the opaque frame to the PC WebSocket, receives a correlated encrypted
  response, and returns it without seeing plaintext.
- Good: when the PC outbound socket closes, all device sockets in that room
  observe room teardown and reconnect instead of staying falsely online.
- Base: `/health` and `/api/info` can be tested without opening a public
  listener and report active room, connection, pending request, protocol, and
  limit metadata.
- Bad: the Relay server imports `crates/remote`, calls `RelaySession::open_json`,
  logs ciphertext/nonce values, inspects `RemoteAuthProof`, or treats a room id
  as business authorization.

### 6. Tests Required

- Health/info route tests for protocol, counts, features, and configured
  limits.
- WebSocket room registration and duplicate active room rejection tests.
- Room teardown test proving unregistering the exact PC connection notifies all
  registered device sockets, while unregistering a stale connection id leaves a
  replacement room intact.
- HTTP pair bridge test proving `Hello` reaches the PC WebSocket and a
  correlated `Ready` response completes the HTTP request.
- HTTP command bridge test proving opaque `RelayEncryptedFrame` forwarding and
  correlated encrypted response return.
- Missing room, room mismatch, missing correlation, bridge timeout, body limit,
  pending limit, rate limit, heartbeat, and stale room cleanup tests.
- Zero-knowledge regression proving forwarded command JSON and structured
  errors do not contain business payload plaintext.

### 7. Wrong vs Correct

#### Wrong

```text
apps/relay-server
  -> accepts RelayEncryptedFrame
  -> calls RelaySession::open_json
  -> parses RemoteRequestEnvelope
  -> rejects read-only device mutation
  -> keeps device sockets open after the PC room disappears
```

This moves E2EE decryption and business authorization into an untrusted Relay
server and creates a second remote permission path.

#### Correct

```text
apps/relay-server
  -> accepts RelayEncryptedFrame
  -> validates room id, kind, correlation id, size, limits, and TTL
  -> wraps as RelayBridgeMessage
  -> forwards to PC WebSocket
  -> returns correlated encrypted PC response
  -> removes the room on PC disconnect and closes its device sockets

PC Relay client
  -> decrypts with RelaySession::open_json
  -> dispatches RemoteRequestEnvelope through crates/remote auth/permissions
```

The Relay server remains a room/correlation bridge. The PC remains the only
business runtime.

## Scenario: Phase 5 PC Relay Client And Desktop Settings

### 1. Scope / Trigger

- Trigger: Phase 5 third child wires the PC desktop runtime to a configured
  self-hosted Relay server through an outbound WebSocket client.
- This scenario covers the PC-side Relay client, shared bridge envelope,
  in-process remote dispatcher, desktop settings/status commands, heartbeat,
  reconnect, E2EE decrypt, and business dispatch. Native mobile Relay transport,
  client-side E2EE, mobile packaging, deployment assets, NAT smoke, and
  public hosted Relay remain later children.

### 2. Signatures

Shared bridge DTO:

```text
RelayBridgeMessage {
  correlation_id: CorrelationId,
  room_id: RelayRoomId,
  message: RelayControlMessage
}
```

Remote dispatcher API:

```text
RemoteDispatcher::new(config) -> RemoteDispatcher
RemoteDispatcher::with_agent_manager(config, agent_manager) -> RemoteDispatcher
RemoteDispatcher::with_agent_and_workbench(config, agent_manager, workbench)
  -> RemoteDispatcher
RemoteDispatcher::dispatch(RemoteRequestEnvelope) -> RemoteResponseEnvelope
RemoteDispatcher::health() -> RemoteHealthStatus
RemoteDispatcher::info() -> RemoteServiceInfo
build_router_with_dispatcher(RemoteDispatcher) -> RemoteRouter
```

Desktop Tauri commands:

```text
relay_get_settings() -> RelayClientSettings
relay_update_settings(RelayClientSettingsUpdate) -> RelayClientSettings
relay_get_status() -> RelayClientStatus
relay_start() -> RelayClientStatus
relay_stop() -> RelayClientStatus
```

Desktop settings/status shape:

```text
RelayClientSettings {
  enabled,
  relay_url,
  room_id,
  pc_peer_id,
  heartbeat_interval_ms,
  reconnect_initial_ms,
  reconnect_max_ms
}

RelayClientStatus {
  state = disabled | disconnected | connecting | connected | retrying |
    degraded | error,
  room_id,
  pc_peer_id,
  relay_url,
  connected_at_ms,
  last_heartbeat_ack_ms,
  reconnect_attempt,
  next_retry_at_ms,
  last_error
}
```

### 3. Contracts

- `RelayBridgeMessage` lives in `crates/core` and is the single JSON contract
  shared by `apps/relay-server` and the PC Relay client. The server must not
  keep a private divergent bridge shape.
- `RemoteDispatcher` is the only in-process way to execute a
  `RemoteRequestEnvelope` outside Axum routes. HTTP routes, WebSocket frames,
  and Relay-decrypted commands must call the same dispatcher so request id,
  correlation id, capability, auth, permission, audit, and error behavior stay
  identical.
- The desktop Relay runtime is disabled by default. `relay_start` must not open
  a WebSocket when settings remain disabled, and enabled settings require a
  valid `http`, `https`, `ws`, or `wss` Relay URL.
- The PC client connects to `{relay_url}/ws`, sends
  `RelayControlMessage::Hello` with room id, PC peer id, and PC public key, then
  maintains `RelayHeartbeat` frames and records `RelayHeartbeatAck` timestamps.
- Pair bridge requests are `RelayBridgeMessage` values containing
  `RelayControlMessage::Hello` from the remote peer. The PC validates room and
  protocol, establishes a `RelaySession`, stores the session by
  `RelaySessionId`, and returns a correlated `RelayControlMessage::Ready`.
- A remote pairing claim is encrypted as `RelayFrameKind::PairRequest`. The PC
  returns `RelayFrameKind::PairResponse` whose decrypted business payload is a
  `RemoteRpcResponseV2` result envelope, not a raw device grant. Its
  `request_id` equals the offer id, its `correlation_id` equals the bridge/frame
  correlation, and it contains exactly one of `payload` or `error`.
- Command bridge requests are `RelayBridgeMessage` values containing
  `RelayControlMessage::Encrypted` with `kind = command`. The PC opens the
  frame with `RelaySession::open_json`, parses the plaintext
  `business_payload_json` as `RemoteRequestEnvelope`, dispatches through
  `RemoteDispatcher`, encrypts the resulting `RemoteResponseEnvelope` as
  `kind = response`, and returns it in a correlated `RelayBridgeMessage`.
- Relay E2EE success never creates `RemoteAuthContext`. Business authorization
  still comes from the `RemoteAuthProof` embedded in typed remote request
  payloads and checked by `RemoteTrustService`.
- Desktop settings are in-memory for this child; durable user preferences can be
  added later when the final native mobile Relay UX and deployment smoke define the
  storage requirements.

### 4. Validation & Error Matrix

- Disabled settings + `relay_start` -> status remains `disabled`, no WebSocket
  connection.
- Enabled settings without Relay URL -> validation error `relay_url_required`.
- Relay URL with unsupported scheme -> validation error `relay_url_invalid`.
- Zero heartbeat/reconnect duration -> validation error for the specific Relay
  duration setting.
- Reconnect initial delay greater than max delay ->
  `relay_reconnect_range_invalid`.
- Bridge room mismatch -> `RelayErrorCode::InvalidRoom`.
- Pair request with wrong protocol or wrong room ->
  `RelayErrorCode::UnsupportedProtocol`.
- Pair response that is not encrypted `PairResponse`, is not a valid
  `RemoteRpcResponseV2`, has mismatched request/correlation identity, or has an
  invalid payload/error combination -> `relay_pairing_response_invalid` or
  `remote_pairing_claim_response_invalid`; never accept a raw grant.
- Unknown Relay session for command -> `RelayErrorCode::InvalidFrame`.
- Decrypt, replay, or out-of-order failure -> corresponding Relay crypto error
  mapped from `RelaySession::open_json`.
- Decrypted payload that is not `RemoteRequestEnvelope` ->
  `RelayErrorCode::InvalidFrame`.
- Revoked, unknown, invalid-token, or under-permissioned device after decrypt ->
  encrypted `RemoteResponseEnvelope::error` from `RemoteDispatcher`, not a
  Relay-layer auth decision.

### 5. Good/Base/Bad Cases

- Good: a phone completes Relay E2EE pairing, sends an encrypted
  `RemoteAgentRequest::ListSessions` with a valid `RemoteAuthProof`, and receives
  an encrypted `RemoteResponseEnvelope::ok` produced by the same dispatcher as
  direct HTTP remote mode.
- Good: Relay offer claim success and failure both use the same typed v2 result
  envelope shape, so transport errors cannot be confused with business claim
  errors.
- Base: desktop Relay settings report disabled status until explicitly enabled,
  so direct LAN/native mobile mode remains unaffected by Phase 5 Relay work.
- Bad: the PC Relay client parses a command payload and directly calls Agent,
  file, Git, terminal, or Provider services without using `RemoteDispatcher`.
  This creates a second auth/permission/audit path and violates the one business
  protocol rule.

### 6. Tests Required

- Core serde/binding test for `RelayBridgeMessage` preserving correlation id,
  room id, and inner `RelayControlMessage`.
- Relay server tests proving the shared bridge DTO keeps existing HTTP-to-PC
  WebSocket correlation behavior.
- Remote dispatcher parity test proving the same `RemoteRequestEnvelope` returns
  matching request id, correlation id, status, and payload through HTTP and
  in-process dispatch.
- Desktop tests for disabled-by-default settings/status and `relay_start`
  leaving disabled settings disconnected.
- Desktop Relay pair/command test proving a mobile `RelaySession` can pair with
  the PC, encrypt a remote command, and decrypt a response generated by
  `RemoteDispatcher`.
- GPUI Relay pairing tests must decrypt `PairResponse`, validate the
  `RemoteRpcResponseV2` request/correlation ids and payload/error exclusivity,
  and reject a raw grant or mismatched result envelope.
- Revoked-device Relay test proving decryption succeeds but business dispatch
  returns encrypted `remote_device_revoked` without a Relay auth bypass.
- Binding drift check after exporting new Relay DTOs.

### 7. Wrong vs Correct

#### Wrong

```text
PC Relay client
  -> RelaySession::open_json(command_frame)
  -> inspect operation = agent_session
  -> call AgentManager::list_sessions directly
  -> synthesize a JSON response
  -> return a raw device grant for PairResponse
```

This duplicates the remote business API and can skip device revocation,
permission audit, request id propagation, and future operation-level checks.

#### Correct

```text
PC Relay client
  -> decrypt PairRequest and claim the one-time offer
  -> wrap claim payload/error in RemoteRpcResponseV2
  -> seal the result as correlated PairResponse
  -> RelaySession::open_json(command_frame)
  -> serde_json::from_value::<RemoteRequestEnvelope>(business_payload_json)
  -> RemoteDispatcher::dispatch(request)
  -> RelaySession::seal_json(kind = response, RemoteResponseEnvelope)
```

Relay transport handles encrypted delivery only. `RemoteDispatcher` remains the
single business execution boundary for direct HTTP, direct WebSocket, and Relay
commands.


## Scenario: Phase 5 Relay Deployment Docs And NAT Smoke

### 1. Scope / Trigger

- Trigger: Phase 5 final child adds self-hosted Relay deployment assets,
  local Relay smoke automation, and a NAT-style mobile verification
  checklist.
- This scenario defines the transport-only local smoke baseline, Caddy reverse
  proxy, Relay health/info validation, NAT evidence, and deployment redaction
  rules. Relay is always transport-only and never permits Relay-side payload
  decryption or product-asset hosting.

### 2. Signatures

Deployment assets:

```text
deploy/relay/Dockerfile
deploy/relay/docker-compose.yml
deploy/relay/Caddyfile
deploy/relay/README.md
docs/smoke/relay-nat.md
scripts/smoke-relay-local.mjs
pnpm smoke:relay:local
```

Runtime checks:

```text
GET /health -> RelayHealthStatus
GET /api/info -> RelayServerInfo

RelayServerInfo.features.pcWebsocket = true
RelayServerInfo.features.httpPairBridge = true
RelayServerInfo.features.httpCommandBridge = true
RelayServerInfo.features has no staticRoomAssets field
RelayServerInfo has no webBuild field
```

Transport-only local smoke env vars:

```text
VIBEX_RELAY_BIND_ADDR
RELAY_PORT
```

### 3. Contracts

- `pnpm smoke:relay:local` builds and runs only `vibex-relay-server`. Root,
  asset-like, and extensionless navigation paths return 404; `/api/info`
  exposes no static-asset or Web-build capability.
- Release containers must not package the mobile runtime, generated mobile
  projects, provider home configs, local databases, auth tokens, pairing codes,
  or private keys.
- `apps/relay-server` source must compile with the Docker builder Rust version
  and the workspace `rust-version`; do not use newer Rust syntax there unless
  both the workspace and Dockerfile are updated together.
- Container deployments should use
  `VIBEX_RELAY_BIND_ADDR=0.0.0.0:9700`. `RELAY_PORT` remains a local shorthand
  that binds to `127.0.0.1:{port}` when `VIBEX_RELAY_BIND_ADDR` is unset.
- Compose publishes `127.0.0.1:9700` by default for local testing; operators
  must opt into broader host binding or HTTPS reverse proxy exposure.
- Caddy proxies `/health`, `/api/*`, and `/ws`. The PC Relay client uses `/ws`;
  mobile clients use `/api/info`,
  `/api/rooms/:room_id/pair`, and `/api/rooms/:room_id/command`.
- `/api/info` is the source for Relay bridge feature/limit checks. It is not PC
  business capability info and must not enable Agent/Git/terminal/Provider UI
  without an encrypted PC remote handshake.
- Relay limits are loaded from the documented `VIBEX_RELAY_*` environment
  variables, validated before bind, and surfaced as effective values by
  `/api/info`.
- NAT smoke evidence may capture Relay URL host, room id, connection state,
  active room/connection counts, feature flags, limits, structured error codes,
  screenshots, and reconnect notes.
- NAT smoke evidence must not capture auth tokens, pairing codes, private keys,
  decrypted payloads, raw ciphertext, nonce values, prompt bodies, file paths,
  terminal content, Git diffs, Provider setting details, or provider secrets.
- Zero-knowledge smoke checks use a disposable non-secret marker only to search
  Relay logs locally. The marker must be absent from Relay logs and should not
  be pasted into captured evidence.

### 4. Validation & Error Matrix

- Local Relay binary cannot start -> `pnpm smoke:relay:local` fails before
  health/info assertions.
- `/health` unreachable -> local smoke failure or deployment health failure.
- `/api/info` missing pair/command/WebSocket bridge features -> Relay mode is
  unsupported for NAT smoke.
- Relay `/api/info` reports a static-asset or Web-build field, or `/` returns
  HTML -> product-boundary regression.
- Public URL serves `/api/info` but not `/ws` -> Caddy/reverse-proxy
  misconfiguration; PC cannot maintain the outbound room connection.
- `activeRooms` stays `0` after PC Relay start -> PC settings, public URL, room
  id, firewall, TLS, or WebSocket proxy problem.
- Mobile connects to Relay but not PC capabilities -> encrypted pair,
  remote handshake, device auth proof, revocation, or permission problem; do
  not treat Relay `/api/info` as PC service info.
- Relay logs contain the disposable business marker or sensitive payload fields
  -> zero-knowledge regression.

### 5. Good/Base/Bad Cases

- Good: an operator deploys `vibex-relay-server` behind Caddy, the PC connects
  outbound to `/ws`, the installed mobile app pairs through `/api/rooms/:room_id/pair`,
  sends encrypted commands through `/api/rooms/:room_id/command`, and reconnect
  catch-up restores missed PC timeline state without Relay plaintext logs.
- Base: `pnpm smoke:relay:local` starts a local Relay, validates `/health`,
  validates `/api/info` bridge features and limits, and exits without requiring
  DNS, TLS, a PC desktop runtime, or a phone.
- Bad: deployment docs ask users to paste auth tokens into launch URLs, expose
  `9700` publicly without HTTPS guidance, use Relay room ids as authorization
  secrets, or add Relay log collection that stores decrypted business payloads.

### 6. Tests Required

- `pnpm smoke:relay:local`.
- `cargo check -p vibex-relay-server --all-targets`.
- `cargo test -p vibex-relay-server`.
- Run core protocol tests when Relay DTO docs or serialized contracts are touched.
- `git diff --check`.
- Full `pnpm check` before archiving unless an unrelated environmental blocker
  is documented.

### 7. Wrong vs Correct

#### Wrong

```text
Relay deployment guide
  -> expose http://server:9700 publicly
  -> put authToken and roomId in a QR URL
  -> collect full Relay request/response bodies for debugging
```

This treats transport metadata as authorization and leaks business/auth
material into deployment artifacts.

#### Correct

```text
Relay deployment guide
  -> expose HTTPS reverse proxy for /api/* and /ws
  -> prefill only Relay URL and room id where needed
  -> keep device proof in the installed mobile app
  -> collect only health/info/status counts and redacted structured errors
```

The Relay remains an opaque transport bridge. The PC remains authoritative for
business auth, permission, audit, and state.


## Scenario: Phase 4 Remote Foundation Envelope

### 1. Scope / Trigger

- Trigger: Phase 4 first child introduces `crates/remote`, remote protocol DTOs
  in `crates/core`, and a minimal Axum surface.
- This scenario establishes the shared envelope only. Pairing, auth, device
  permissions, audit logs, and Agent/file/Git/terminal/provider mutation APIs
  remain deferred to later child tasks.

### 2. Signatures

Core DTO source of truth:

```text
RemoteProtocolVersion
RemoteCapabilitySummary
RemoteHandshakeRequest
RemoteHandshakeResponse
RemoteRequestEnvelope
RemoteResponseEnvelope
RemoteLiveEventEnvelope
RemoteCatchUpRequest
RemoteCatchUpResponse
RemoteHealthStatus
RemoteServiceInfo
```

Initial Axum routes:

```text
GET /health -> RemoteHealthStatus
GET /api/info -> RemoteServiceInfo
GET /ws -> WebSocket RemoteRequestEnvelope / RemoteResponseEnvelope frames
```

### 3. Contracts

- Rust serde types in `crates/core/src/remote.rs` are the source of truth;
  frontend and native mobile clients consume them through `WebRemoteBackend`.
- `RemoteRequestEnvelope` and `RemoteResponseEnvelope` include protocol
  version, request id, optional correlation id, optional device id, operation,
  timestamp, and a temporary `unknown` JSON payload placeholder.
- `RemoteResponseEnvelope` preserves the request id and correlation id for both
  success and error responses.
- `RemoteLiveEventEnvelope` includes a monotonic sequence placeholder per
  channel so later reconnect work can use authoritative catch-up before live
  events.
- `crates/remote` defaults to disabled, loopback-only configuration. It must
  not start a public `0.0.0.0` listener before auth/pairing and permission
  enforcement exist.

### 4. Validation & Error Matrix

- Unsupported operation -> `capability/remote_unsupported_operation` in a
  structured `VibexError`.
- Invalid WebSocket JSON frame -> `validation/remote_invalid_envelope`.
- `/api/info` must report whether the service is enabled and whether a public
  listener is configured, so desktop wiring can prove safe defaults.

### 5. Good/Base/Bad Cases

- Good: a WebSocket handshake receives protocol version, server version, and
  capability metadata while preserving the caller's request/correlation ids.
- Base: `/health` and `/api/info` can be tested without binding a public network
  port.
- Bad: a remote Git/file/terminal/Agent mutation is accepted before pairing,
  auth, permissions, and audit records are implemented.

### 6. Tests Required

- Core serde round-trip for request/response envelope payloads and correlation
  id propagation.
- Remote route tests for `/health` and `/api/info`.
- WebSocket tests for handshake response, unsupported operation errors, and
  request id preservation.

## Scenario: Phase 4 Device Pairing Auth Permissions And Audit

### 1. Scope / Trigger

- Trigger: Phase 4 second child adds durable remote device trust, pairing/auth
  helpers, permission-level enforcement, and redacted audit records.
- This scenario builds the trust boundary for later Agent/File/Git/Terminal
  remote APIs. It must not expose those mutation APIs by itself.

### 2. Signatures

Core DTO source of truth:

```text
RemoteDevicePermissionLevel = read_only | approve_only | full_control
RemoteDeviceStatus = pending | active | revoked
RemoteDeviceSummary
RemoteDeviceDetail
RemotePairingCode
RemoteCreatePairingCodeRequest/Response
RemoteClaimPairingCodeRequest/Response
RemoteRevokeDeviceRequest
RemoteAuthProof
RemoteAuthContext
RemoteActionClass
RemoteAuditAction
RemoteAuditTargetKind
RemoteAuditOutcome
RemoteAuditRecord
RemoteAuditListRequest/Response
```

Remote service helpers:

```text
create_pairing_code(request) -> pairing metadata + plaintext code returned once
claim_pairing_code(request) -> device metadata + plaintext auth token returned once
authenticate(proof) -> RemoteAuthContext
authorize_action(auth_context, action_class) -> () | VibexError
revoke_device(request) -> RemoteDeviceDetail
```

### 3. Contracts

- Pairing code and auth token plaintext may be returned once to the caller but
  must not be stored in SQLite. Durable storage keeps hashes only.
- `RemoteAuthContext` is derived server-side from a known active device and a
  valid auth proof. Unknown, revoked, and invalid-token devices fail before
  permission checks.
- Permission classes are generic and provider-neutral. Later remote APIs must
  call the helper before performing side effects.
- `read_only` permits read classes only. `approve_only` permits reads plus
  permission resolution. `full_control` permits supported mutation classes.
- Audit summaries are redacted and must not contain pairing codes, auth tokens,
  terminal input, file contents, prompt bodies, provider credentials, or other
  secret material.

### 4. Validation & Error Matrix

- Unknown device -> `remote/remote_device_unknown`.
- Revoked device -> `remote/remote_device_revoked`.
- Invalid auth proof -> `remote/remote_auth_invalid`.
- Invalid pairing code -> `remote/remote_pairing_code_invalid`.
- Expired pairing code -> `remote/remote_pairing_code_expired`.
- Insufficient permission -> `permission/remote_permission_denied`.

### 5. Good/Base/Bad Cases

- Good: a local pairing code can be claimed once, creates an active named
  device, returns a one-time auth token, and future auth resolves to a typed
  context.
- Base: read-only devices can authenticate and read project/session classes but
  receive `remote_permission_denied` for Git/file/terminal mutation classes.
- Bad: plaintext pairing codes or auth tokens are stored in audit rows,
  diagnostics, or durable device/pairing tables.

### 6. Tests Required

- Core serde tests for device/auth/audit contracts.
- DB migration/repository tests for device, pairing code, and audit rows.
- Remote service tests for pair/claim/auth/revoke, invalid/expired code,
  permission denial, and audit redaction.

## Scenario: Phase 4 Remote Agent Sessions And Timeline Catch-Up

### 1. Scope / Trigger

- Trigger: Phase 4 third child connects `crates/remote` to
  `crates/agent::AgentManager` for authenticated Agent session APIs and
  reconnect-safe timeline catch-up.
- This scenario is limited to Agent sessions. File, Git, terminal, Provider
  settings, native mobile UI, Relay, and public listener behavior remain separate
  child tasks.

### 2. Signatures

Core DTO source of truth:

```text
RemoteAgentOperationKind =
  list_sessions | get_session | fetch_timeline | send_message |
  interrupt | resolve_permission | catch_up

RemoteAgentRequest =
  list_sessions(RemoteAgentSessionListRequest)
  get_session(RemoteAgentSessionDetailRequest)
  fetch_timeline(RemoteAgentTimelineFetchRequest)
  send_message(RemoteAgentSendMessageRequest)
  interrupt(RemoteAgentInterruptRequest)
  resolve_permission(RemoteAgentResolvePermissionRequest)
  catch_up(RemoteAgentCatchUpRequest)

RemoteAgentTimelineCursor { session_id, after_sequence }
RemoteAgentCatchUpResponse { events, next_cursors, compacted }
```

Remote route and WebSocket dispatch:

```text
POST /api/agent
  RemoteRequestEnvelope(operation = agent_session, payload = RemoteAgentRequest)
  -> RemoteResponseEnvelope(payload = typed remote Agent response | error)

GET /ws
  RemoteRequestEnvelope(operation = agent_session, payload = RemoteAgentRequest)
  -> RemoteResponseEnvelope(payload = typed remote Agent response | error)
```

Service dependency:

```text
crates/remote -> Arc<AgentManager> -> AgentManager::{list_sessions,
  get_session, fetch_timeline, send_message, interrupt, resolve_permission}
```

### 3. Contracts

- `crates/core/src/remote.rs` remains the protocol source of truth and owns every
  `RemoteAgent*` request/response type.
- Remote Agent request payloads carry `RemoteAuthProof`; auth tokens are used
  only for server-side authentication and must not be copied into audit
  summaries.
- Remote Agent responses reuse canonical `AgentSession`, `AgentSessionSummary`,
  `FetchTimelineRequest`, `TimelinePage`, `TimelineItem`,
  `SendAgentMessageRequest`, and `ResolvePermissionRequest` contracts instead
  of defining provider-specific remote variants.
- `RemoteAgentTimelineCursor.after_sequence` is the authoritative per-session
  timeline sequence. Catch-up calls `AgentManager::fetch_timeline` and wraps
  missed timeline items in `RemoteLiveEventEnvelope` on the `agent_session`
  channel.
- WebSocket Agent dispatch is request/response plus catch-up in this phase.
  Full push fanout from `AgentManager::subscribe()` can be added later without
  changing catch-up correctness.
- Desktop wiring may provide the remote router with `Arc<AgentManager>`, but
  the listener remains disabled/loopback-only unless an explicit future config
  enables it.

### 4. Validation & Error Matrix

- Missing Agent payload -> `validation/remote_agent_payload_missing`.
- Invalid Agent payload -> `validation/remote_agent_payload_invalid`.
- Agent support not wired into router -> `capability/remote_agent_sessions_unavailable`.
- Unknown/revoked/bad-token device -> existing remote auth errors from
  `RemoteTrustService`.
- `read_only` send/interrupt -> `permission/remote_permission_denied`.
- Mismatched permission resolution outer/inner request id or session id ->
  `validation/remote_permission_resolution_invalid`.
- Negative or otherwise invalid timeline sequence conversion ->
  `remote/remote_agent_timeline_sequence_invalid`.
- Missing session id -> `validation/session_not_found` from `AgentManager`.

### 5. Good/Base/Bad Cases

- Good: a read-only paired device can list Agent sessions, fetch a timeline,
  reconnect with `after_sequence`, and receive missed `agent_session` events
  before applying any later live stream.
- Base: an approve-only device can read sessions and resolve permission
  requests, and the server stamps the remote responder device id on the
  resolution before delegating to `AgentManager`.
- Bad: a read-only device sends an Agent message; the operation is denied
  before `AgentManager::send_message`, and the audit summary must not include
  the prompt body or auth token.

### 6. Tests Required

- Core serde/binding tests for tagged `RemoteAgentRequest` payloads and
  `RemoteAgentTimelineCursor`.
- Remote route tests for authenticated session list and catch-up response
  cursor advancement.
- Permission tests proving read-only denial for `send_message` or `interrupt`
  and redacted audit summaries.
- WebSocket tests proving `operation = agent_session` dispatch can perform
  authenticated catch-up and preserves request ids.
- Existing full checks: targeted Rust tests and `pnpm check`.

### 7. Wrong vs Correct

#### Wrong

```text
native mobile reconnect -> subscribe live events -> assume no frames were missed
```

This loses timeline items when the browser sleeps or the WebSocket reconnects.

#### Correct

```text
native mobile reconnect
  -> authenticate device
  -> POST /api/agent catch_up(session_id, after_sequence)
  -> apply authoritative timeline events
  -> then process later live events
```

Catch-up uses the PC-side `AgentManager` timeline as the source of truth.

## Scenario: Phase 4 Workspace File Git Terminal Remote APIs

### 1. Scope / Trigger

- Trigger: Phase 4 fourth child wires authenticated remote workbench APIs for
  workspace summaries, workspace-contained files, Git review/actions, and
  terminal snapshots/actions.
- The PC desktop runtime remains authoritative. This scenario does not add
  native mobile UI, Provider settings APIs, Relay infrastructure, or public listener
  behavior.

### 2. Signatures

Core DTO source of truth:

```text
RemoteWorkbenchOperationKind =
  list_workspaces | open_workspace |
  file_list_tree | file_read | file_search | file_write | file_delete |
  file_rename |
  git_status | git_diff | git_stage | git_unstage | git_revert |
  git_commit | git_history | git_commit_detail | git_blame |
  git_branch_list | git_branch_create | git_branch_checkout |
  git_remote_action |
  terminal_list | terminal_create | terminal_snapshot | terminal_write |
  terminal_resize | terminal_kill

RemoteWorkbenchRequest =
  tagged union carrying RemoteAuthProof plus canonical workspace/file/Git/
  terminal request DTOs
```

Remote route and WebSocket dispatch:

```text
POST /api/workbench
  RemoteRequestEnvelope(operation = workspace_file | git | terminal,
  payload = RemoteWorkbenchRequest)
  -> RemoteResponseEnvelope(payload = typed remote workbench response | error)

GET /ws
  same RemoteRequestEnvelope / RemoteResponseEnvelope dispatch path
```

Desktop wiring:

```text
apps/desktop -> build_router_with_agent_and_workbench(
  RemoteWorkbenchRuntime { db_path, terminals }
)
```

### 3. Contracts

- `crates/core/src/remote.rs` remains the protocol source of truth and owns every
  `RemoteWorkbench*`, `RemoteFile*`, `RemoteGit*`, and `RemoteTerminal*`
  request/response type.
- `crates/remote` authenticates every workbench request with
  `RemoteTrustService`, then delegates business behavior to existing
  `WorkspaceRepository`, `WorkspaceFileService`, `vibex_git`, and
  `TerminalManager` APIs.
- Read operations map to `RemoteActionClass::ReadProject`.
- File writes/deletes/renames map to `RemoteActionClass::MutateFile`.
- Git stage/unstage/revert/commit/branch/remote actions map to
  `RemoteActionClass::MutateGit`.
- Terminal create/write/resize/kill map to
  `RemoteActionClass::MutateTerminal`.
- `read_only` and `approve_only` devices may use read-only workbench APIs only.
  `full_control` is required for file/Git/terminal mutations.
- Mutating outcomes are audited with redacted summaries containing operation,
  workspace, path count, branch, or terminal id style metadata only. Audit rows
  must not store file contents, terminal input/output, full diffs/patches,
  prompts, pairing codes, auth tokens, provider credentials, or private remote
  URLs.
- Router capabilities set `supports_workspace_files`, `supports_git`, and
  `supports_terminal` only when a `RemoteWorkbenchRuntime` is wired.
- Default remote service behavior remains disabled/loopback-only unless a
  future explicit configuration enables a listener.

### 4. Validation & Error Matrix

- Missing workbench payload -> `validation/remote_workbench_payload_missing`.
- Invalid workbench payload -> `validation/remote_workbench_payload_invalid`.
- Workbench runtime not wired -> `capability/remote_workbench_unavailable`.
- Unknown/revoked/bad-token device -> existing remote auth errors from
  `RemoteTrustService`.
- `read_only` / `approve_only` file, Git, or terminal mutation ->
  `permission/remote_permission_denied`.
- Missing workspace -> `validation/workspace_not_found`.
- Filesystem containment, Git path validation, and terminal lifecycle errors
  must come from the existing service crates.

### 5. Good/Base/Bad Cases

- Good: a read-only paired browser can list workspaces, read a workspace file,
  inspect Git status/diff/history, and list or snapshot terminals.
- Base: a full-control paired device can write a file, stage Git paths, or write
  to a terminal, and the PC records a redacted audit row for the mutation
  outcome.
- Bad: a read-only device sends terminal input or file content; the operation is
  denied before the service side effect, and the audit summary does not contain
  that payload content.

### 6. Tests Required

- Core serde/binding tests for `RemoteWorkbenchRequest` tagged payloads.
- Remote route tests for authenticated file read/list style requests.
- Permission tests proving read-only denial for file, Git, and terminal
  mutations.
- Audit redaction tests proving mutation payload content and auth tokens are
  not persisted.
- Targeted Rust checks: `cargo test -p vibex-core remote`,
  `cargo test -p vibex-remote`, `cargo test -p vibex-fs`,
  `cargo test -p vibex-git`, `cargo test -p vibex-terminal`, and
  `cargo check -p vibex-desktop`.

### 7. Wrong vs Correct

#### Wrong

```text
POST /api/git/stage
  -> parse ad hoc auth
  -> call git stage
  -> write audit row with full patch or command payload
```

This creates a second protocol shape, bypasses the shared envelope/correlation
contract, and risks persisting sensitive workspace contents.

#### Correct

```text
RemoteRequestEnvelope(operation = git, payload = RemoteWorkbenchRequest::GitStage)
  -> authenticate RemoteAuthProof
  -> authorize RemoteActionClass::MutateGit
  -> call vibex_git::stage
  -> audit redacted operation/path-count outcome
  -> RemoteResponseEnvelope preserving request id and correlation id
```

Workbench APIs use the same remote envelope and permission model as Agent
remote APIs while delegating filesystem, Git, and terminal behavior to the
existing service crates.

## Scenario: Phase 4 Remote Provider Settings Safe Actions

### 1. Scope / Trigger

- Trigger: Phase 4 final child wires authenticated remote Provider settings
  summaries and safe Provider actions for native mobile.
- The PC runtime remains authoritative for Provider profiles, health, usage,
  failover, and injection preview. This scenario does not expose native config
  export apply/rollback, native import apply, Provider SDK calls from native mobile,
  Relay infrastructure, or public listener behavior.

### 2. Signatures

Core DTO source of truth:

```text
RemoteProviderOperationKind =
  list_profiles | preview_injection | list_health_summaries |
  run_health_probes | list_usage_summaries |
  list_failover_recommendations

RemoteProviderRequest =
  list_profiles(RemoteProviderProfileListRequest)
  preview_injection(RemoteProviderInjectionPreviewRequest)
  list_health_summaries(RemoteProviderHealthSummaryListRequest)
  run_health_probes(RemoteProviderRunHealthProbesRequest)
  list_usage_summaries(RemoteProviderUsageSummaryListRequest)
  list_failover_recommendations(RemoteProviderFailoverRecommendationListRequest)
```

Remote route and WebSocket dispatch:

```text
POST /api/provider
  RemoteRequestEnvelope(operation = provider_settings,
  payload = RemoteProviderRequest)
  -> RemoteResponseEnvelope(payload = typed remote Provider response | error)

GET /ws
  same RemoteRequestEnvelope / RemoteResponseEnvelope dispatch path
```

Desktop wiring:

```text
apps/desktop -> build_router_with_agent_and_workbench(
  RemoteWorkbenchRuntime { db_path, terminals }
)
  -> RemoteProviderRuntime { db_path }
  -> ProviderConfigService(db_path)
```

### 3. Contracts

- `crates/core/src/remote.rs` remains the protocol source of truth and owns every
  `RemoteProvider*` request/response type.
- `crates/remote` authenticates every Provider request with
  `RemoteTrustService`, then delegates Provider behavior to
  `ProviderConfigService`.
- Provider profile listing returns `ProviderProfileSummary`, not full
  `ProviderProfile`, so remote clients do not receive secret references or
  low-level native configuration details.
- Injection preview returns `ProviderInjectionPreview` but remote dispatch must
  force `persist = false`; read-only remote preview requests must not write
  preview records.
- Read operations map to `RemoteActionClass::ReadProviderSettings` and are
  allowed for `read_only`, `approve_only`, and `full_control` devices.
- Safe Provider mutations such as `run_health_probes` map to
  `RemoteActionClass::MutateProviderSettings`; `full_control` is required.
- Mutating Provider outcomes are audited with target kind `provider_settings`
  and redacted summaries containing operation names/counts only. Audit rows
  must not store Provider secrets, auth tokens, pairing codes, terminal input,
  native config contents, prompt bodies, or raw env values.
- Router capabilities set `supports_provider_settings` and include the
  `provider` live-event channel only when a `RemoteProviderRuntime` is wired.

### 4. Validation & Error Matrix

- Missing Provider payload -> `validation/remote_provider_payload_missing`.
- Invalid Provider payload -> `validation/remote_provider_payload_invalid`.
- Provider runtime not wired -> `capability/remote_provider_settings_unavailable`.
- Unknown/revoked/bad-token device -> existing remote auth errors from
  `RemoteTrustService`.
- `read_only` / `approve_only` safe mutation such as `run_health_probes` ->
  `permission/remote_permission_denied`.
- Missing Provider profile in preview/probe filters -> existing
  `ProviderConfigService` validation error such as `provider_profile_not_found`.

### 5. Good/Base/Bad Cases

- Good: a read-only paired browser can list Provider profile summaries, fetch a
  redacted injection preview, health summaries, usage summaries, and failover
  recommendations.
- Base: a full-control paired browser can run deterministic Provider health
  probes and receives refreshed summaries while the PC records a redacted audit
  row for the safe action.
- Bad: a read-only browser attempts to run health probes or native config
  export apply; the operation is denied or unavailable before any side effect,
  and no Provider secret/native file content is returned to native mobile.

### 6. Tests Required

- Core serde/binding tests for `RemoteProviderRequest` tagged payloads.
- Remote route tests for read-only profile list and redacted injection preview.
- Permission tests proving read-only denial and full-control success for
  `run_health_probes`.
- Audit redaction tests proving auth tokens and Provider secrets are not
  persisted in remote audit summaries.
- native mobile typecheck/build and screenshot tests for Provider settings capability,
  unsupported, loading, error, empty, and permission-gated safe-action states.

### 7. Wrong vs Correct

#### Wrong

```text
POST /api/provider/native-export-apply
  -> accept browser request
  -> write ~/.codex/config.toml
  -> return native config diff and secret env values
```

This bypasses the staged native export confirmation contract, exposes native
configuration detail to native mobile, and weakens remote device permissions.

#### Correct

```text
RemoteRequestEnvelope(
  operation = provider_settings,
  payload = RemoteProviderRequest::RunHealthProbes
)
  -> authenticate RemoteAuthProof
  -> authorize RemoteActionClass::MutateProviderSettings
  -> call ProviderConfigService::run_health_probes
  -> audit redacted operation/count outcome
  -> RemoteResponseEnvelope preserving request id and correlation id
```

Remote Provider settings use the same envelope, permission, and audit model as
Agent/workbench remote APIs while keeping native config writes out of scope.

## Scenario: Remote Seamless Runtime Selection

### 1. Scope / Trigger

- Trigger: Desktop and paired Remote clients must read and change the same
  provider-neutral Agent/authentication-source/model selection without adding a second
  switch-then-send protocol.
- The PC runtime owns Catalog assembly and durable desired/effective state.
  `vibex-remote` owns only authenticated transport and authorization.

### 2. Signatures

```text
RemoteAgentRequest::ListRuntimeOptions { auth }
  -> RemoteAgentRuntimeOptionsResponse { catalog }

RemoteAgentRequest::SetDesiredRuntime { auth, request }
  -> RemoteAgentSetDesiredRuntimeResponse { state }

RemoteAgentRequest::CancelRuntimeSwitch { auth, request }
  -> RemoteAgentCancelRuntimeSwitchResponse { state }

trait RemoteRuntimeOptionCatalogSource: Send + Sync {
  list_runtime_options() -> SessionRuntimeOptionCatalog
}

RemoteCapabilitySummary.supportsSeamlessRuntimeSelection: bool
RemoteCapabilitySummary.supportsAgentAccountAuth: bool
```

### 3. Contracts

- The Desktop composition root builds one `RuntimeOptionCatalogService` and
  injects it into both the Tauri command and `RemoteDispatcher`. Remote code
  must not rebuild Provider/model evidence or depend on an ACP adapter.
- The capability is true only when both runtime selection and the Catalog
  source are wired. It uses a serde default of `false`, so older service-info
  payloads fail closed.
- `ListRuntimeOptions.supportsAgentAccountAuth` is a client capability fence.
  When false/omitted, the server removes AgentAccount source summaries and
  options before serialization so old clients never see unknown tagged variants.
- `list_runtime_options` requires `ReadAgentSession`; `set_desired_runtime` and
  `cancel_runtime_switch` require `MutateAgentSession`. Authentication and
  authorization happen before any service call.
- Requests reuse `SetDesiredAgentSessionRuntimeRequest` and
  `CancelAgentSessionRuntimeSwitchRequest`, including session revision,
  selection revision, idempotency key, and exact pending switch id.
- Responses reuse the canonical redacted Catalog and selection state. They do
  not expose secrets, commands, base URLs, native session ids, process ids,
  binding history, adapter identity, or raw Provider payloads.
- Message sending remains `SendAgentMessageRequest(desiredRuntime,
  messageIdempotencyKey, ...)`; Remote must not wait for a switch before
  submitting a normal message.

### 4. Validation & Error Matrix

- Catalog source missing -> `remote_agent_runtime_catalog_unavailable`.
- Runtime selection service missing ->
  `remote_agent_runtime_selection_unavailable`.
- Read-only device lists the Catalog -> allowed.
- Read-only or approve-only device sets/cancels desired runtime ->
  `remote_permission_denied` before side effects.
- Stale session/selection revision -> canonical CAS conflict from
  `RuntimeSelectionService`; the client must refetch.
- Old service payload without the capability field -> unsupported, not an
  optimistic feature enable.

### 5. Good/Base/Bad Cases

- Good: Desktop and Remote receive the same ordered Catalog, a full-control
  Remote device sets desired state, and both windows converge through the
  authoritative revisions.
- Base: a read-only device renders the effective selection and Catalog but its
  selectors are disabled.
- Bad: Remote reconstructs models from Provider profiles or accepts a local
  desired value as committed effective state.

### 6. Tests Required

- Core serde tests assert tagged request names, canonical nested DTOs, and the
  backward capability default.
- Remote tests inject a deterministic Catalog, assert read authorization,
  assert mutation denial before dispatch, and verify serialized output omits
  auth tokens.
- Desktop wiring tests/build checks prove Tauri and Remote share the same
  Catalog source.
- Run `cargo test -p vibex-core -p vibex-remote -p vibex-desktop` and frontend
  typechecks after changing the contract.

### 7. Wrong vs Correct

#### Wrong

```text
Remote UI -> POST /switch-and-wait -> rebuild Provider models in Remote
          -> wait for effective -> send message
```

This duplicates Catalog ownership, bypasses durable queue ordering, and turns a
transient connection into correctness authority.

#### Correct

```text
Remote UI -> list_runtime_options (ReadAgentSession)
          -> set_desired_runtime (MutateAgentSession + CAS)
          -> send_message(desiredRuntime, stable idempotency key)
          -> poll authoritative selection/submission state
```

## Scenario: Native Mobile Remote Workbench And Agent Lifecycle

### 1. Scope / Trigger

- Trigger: native mobile exposes Files, Git, Terminal, Provider/Agent status,
  runtime selection, and Agent session create/rename/archive/delete against a
  paired desktop.
- The desktop runtime remains the only state authority. Native mobile must not
  open a local workspace, access its own filesystem or Git repository, own a
  PTY, launch an Agent, or read/write native Provider configuration directly.
- This scenario refines the earlier workbench, Provider, and seamless runtime
  contracts with the mobile interaction and concurrency rules required for a
  production client.

### 2. Signatures

```text
RemoteAgentRequest +=
  create_session(RemoteAgentCreateSessionRequest {
    auth, request: CreateAgentSessionRequest
  }) -> RemoteAgentCreateSessionResponse { session }
  rename_session(RemoteAgentRenameSessionRequest {
    auth, request: RenameAgentSessionRequest
  }) -> RemoteAgentRenameSessionResponse { session }
  archive_session(RemoteAgentSessionActionRequest { auth, session_id })
    -> RemoteAgentSessionActionResponse { completed }
  delete_session(RemoteAgentSessionActionRequest { auth, session_id })
    -> RemoteAgentSessionActionResponse { completed }

RemoteProviderRequest +=
  list_agent_summaries(RemoteAgentConfigSummaryListRequest {
    auth, include_disabled
  }) -> RemoteAgentConfigSummaryListResponse {
    agents: Vec<RemoteAgentConfigSummary>
  }

RemoteAgentConfigSummary {
  id, label, enabled, installed, configured, config_status,
  runtime_status, model_count, updated_at_ms
}

FileReadResponse { ..., content_revision }
FileWriteRequest { ..., expected_revision, encoding, line_ending }

RemoteMutationContract {
  idempotency_key, expected_revision, expected_generation
}
```

### 3. Contracts

- Session creation accepts only a workspace already published by the desktop.
  `crates/remote` canonicalizes the requested root, finds an active
  `WorkspaceRepository::list()` entry with both the same canonical root and
  `WorkspaceMode`, and replaces the client-supplied root with the stored
  authoritative root before calling `AgentManager::create_session`.
- Session create, rename, archive, and delete require
  `RemoteActionClass::MutateAgentSession` (`full_control`). Their audit rows
  contain only operation, stable target id, and success metadata; titles,
  workspace roots, auth tokens, and runtime configuration are excluded.
- File saves carry the revision returned by the last desktop read. A
  `file_external_revision_changed` conflict preserves both the unsaved mobile
  buffer and the newly read desktop version. The UI offers an explicit
  `Use desktop version` action; it must never silently overwrite either side.
- Git stage/unstage and commit use canonical backend mutations. Commit requires
  a visible confirmation step after the user enters the message.
- Terminal create/attach/input/resize/kill always target the desktop
  `TerminalManager`. Attach uses the current transport generation/session epoch
  and a sequence cursor; a generation mismatch or stream gap requires a reset
  snapshot. Killing a terminal requires confirmation, and confirmation is
  rejected if the selected terminal id changed meanwhile.
- Provider/Agent management returns redacted projections only.
  `RemoteAgentConfigSummary` is deliberately distinct from
  `AgentSnapshotEntry`; command lines, environment values, parameters, native
  paths, diagnostics, raw Provider records, and every `Secret` value are
  desktop-only and must not be added to a remote DTO or debug/audit payload.
- Provider health probes, runtime probe mutations, session lifecycle, runtime
  changes, file writes, Git mutations, and terminal mutations require
  `full_control`. Read-only devices may render published summaries and
  read-only workbench state but must not receive an enabled mutation control.
- Mobile management and runtime requests carry local monotonically increasing
  request generations. A completion is applied only when its generation and,
  for runtime calls, its session id still match. Refresh, workspace/session
  changes, and suspend invalidate older completions and clear only the busy
  state owned by the invalidated generation.
- Runtime selectors render unavailable Catalog options as disabled. Reasoning,
  mode, toggle, select, and optional string features expose `Default` by
  omitting the override. Whitespace-only string input removes an override;
  any non-empty input is preserved byte-for-byte and is never replaced by the
  Catalog display default. Applying a runtime sends the authoritative session
  and selection revisions with a fresh idempotency key.

### 4. Validation & Error Matrix

- Session root cannot be canonicalized, is not published, has a different
  `WorkspaceMode`, or is no longer active ->
  `validation/remote_agent_workspace_not_published` with no path diagnostic.
- `read_only` or `approve_only` attempts any lifecycle/workbench/Provider/
  runtime mutation -> `permission/remote_permission_denied` before side effects.
- File save revision differs from desktop ->
  `conflict/file_external_revision_changed`; preserve both versions for an
  explicit user choice.
- File conflict version was invalidated before reload ->
  `conflict/mobile_file_conflict_version_unavailable`.
- Terminal selection changes after close prompt ->
  `conflict/mobile_terminal_close_target_changed` and do not kill a terminal.
- Runtime Catalog option is unavailable -> it cannot be selected or applied.
- Runtime string override is longer than 256 bytes ->
  `mobile_runtime_feature_value_too_long` and do not submit.
- Stale runtime session/selection revisions -> canonical CAS conflict; refresh
  Catalog and selection state rather than treating the draft as committed.
- Async completion generation or session fence does not match -> discard the
  completion without changing the current surface, error, or authoritative
  state.

### 5. Good/Base/Bad Cases

- Good: a full-control device opens a published workspace, edits a revisioned
  file, stages and commits reviewed changes, operates a desktop terminal,
  checks Provider health, and switches the selected session runtime; all state
  converges from desktop responses and audit summaries remain redacted.
- Base: a read-only device browses files, Git state, terminal snapshots,
  redacted Agent/Provider summaries, and the runtime Catalog with mutation
  controls disabled.
- Bad: mobile creates a session for an arbitrary native path, serializes an
  Agent command/env record, applies a stale runtime response to a newly selected
  session, overwrites a conflicting file, or kills a terminal with one tap.

### 6. Tests Required

- `vibex-core`: serde round trips and stable operation tags for Agent lifecycle
  requests; `RemoteAgentConfigSummary` serialization proves private Agent
  fields are absent.
- `vibex-remote`: lifecycle permission and redacted audit coverage; published
  workspace canonical-root plus `WorkspaceMode` acceptance; unpublished,
  mismatched, and malicious-root rejection without path diagnostics.
- `vibex-remote-client`: lifecycle dispatch/capability mapping, mutation
  revision/generation propagation, and decoded Agent summaries remain the
  dedicated redacted DTO.
- `vibex-ui`: revision-conflict tests preserve local and server buffers and
  support explicit reload; shared workbench controllers retain their existing
  capability and stale-completion tests.
- `vibex-mobile`: native build/tests cover multiline editing, lifecycle
  confirmations, generation/session fences, runtime defaults/unavailable
  options, and terminal close confirmation.
- Required integration gates after contract changes: targeted package tests,
  no-dependency Clippy with warnings denied, `pnpm check:mobile-native`, graph
  and license checks, and the local Relay smoke test.

### 7. Wrong vs Correct

#### Wrong

```text
mobile path / secret-bearing AgentSnapshotEntry / local draft
  -> open local files or PTY
  -> mutate without full_control or confirmation
  -> accept whichever async response completes last
```

This makes mobile a second runtime authority, leaks desktop-only configuration,
and permits stale or destructive actions against the wrong resource.

#### Correct

```text
paired mobile request
  -> authenticate + authorize on desktop
  -> resolve a published workspace and canonical service DTO
  -> execute through DesktopRuntime-owned services
  -> return a redacted, revisioned response
  -> apply only when generation and resource fences still match
  -> require explicit resolution for conflicts and destructive actions
```

## Scenario: Remote Agent Default Account Authentication

### 1. Scope / Trigger

- Trigger: a paired mobile client needs the same redacted Agent default
  account state, login methods, operation progress, verification, model refresh,
  logout impact, and logout action as GPUI Desktop.
- `DesktopRuntime` remains the only credential/process authority. Remote and
  Relay carry typed requests and safe progress only; they never authenticate
  directly against an Agent or inspect its state home.

### 2. Signatures

```text
RemoteCapabilitySummary.supportsAgentAccountAuth: bool

RemoteAgentRequest =
  ListAuthContexts { auth }
  | ListAuthMethods { auth, agent_id }
  | AuthenticateContext { auth, request }
  | GetAuthenticationOperation { auth, operation_id }
  | CancelContextAuthentication { auth, request }
  | VerifyAuthContext { auth, request }
  | RefreshAuthModels { auth, request }
  | PreviewAuthLogout { auth, auth_context_id }
  | LogoutAuthContext { auth, request }

trait RemoteAgentAuthContextSource {
  list_auth_contexts() -> Vec<AgentAuthContext>
  list_auth_methods(agent_id) -> AgentAuthCatalog
  authenticate_context(request) -> AgentAuthContextAuthenticateResult
  get_authentication_operation(operation_id) -> AgentAuthenticationOperation
  cancel_authentication(request) -> AgentAuthContextMutationResult
  verify_context(request) -> AgentAuthContextMutationResult
  refresh_models(request) -> AgentAuthContextMutationResult
  logout_preview(auth_context_id) -> AgentAuthContextLogoutPreview
  logout(request) -> AgentAuthContextMutationResult
}
```

### 3. Contracts

- The service advertises `supportsAgentAccountAuth` only when an authoritative
  `RemoteAgentAuthContextSource` is wired. The capability defaults to false for
  old server payloads and clients must hide/disable account actions when false.
- Context and method listing require `ReadAgentSession`. Responses contain
  bounded ids, status, revision, account hint, advertised method metadata,
  action location, model descriptors, and timestamps only.
- Authenticate, operation-status query, cancel, verify, refresh models, logout
  preview, and logout all require `MutateAgentAuthentication`; only
  `full_control` devices have it. Operation status remains mutation-class
  because it can reveal a sensitive login workflow and is polled only by the
  device that controls that workflow.
- Every mutation reuses the canonical context request, including operation id,
  expected context revision, and confirmed affected-session count. Remote does
  not invent an account id, revision, model list, or logout impact.
- Host browser, device-code, host terminal, and remote-attachable terminal are
  explicit execution locations. A remote client renders the returned action;
  it does not assume a PC loopback OAuth callback can run in the phone browser.
- Authentication operation state is durable and queryable. Disconnecting the
  client does not cancel the host operation; cancellation is an explicit typed
  request with the same operation/context/revision fence.
- Each authentication mutation writes a bounded audit record with target kind
  `agent_authentication`, target context id, safe operation label, device id,
  request/correlation ids, and success/failure. Listing and status polling do
  not copy account payloads into audit details.
- `safe_remote_agent_auth_error` preserves a stable safe code/category and
  replaces diagnostic text. Responses/audit/logs must not contain token,
  cookie, OAuth state, device code, environment values, raw terminal output,
  native state-home path, command, process/native session id, or ACP payload.
- Runtime catalog negotiation is separate: a client must set
  `supportsAgentAccountAuth=true` on `ListRuntimeOptions` before account source
  variants are included. Older clients receive a Provider-only projection.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Account source is not wired | `remote_agent_account_auth_unavailable`; no partial local fallback. |
| Service/client capability is false | hide account actions; catalog response is Provider-only. |
| Unknown/revoked/bad-token device | normal remote authentication error before source call. |
| Read-only device lists contexts/methods | allowed with redacted DTOs. |
| Read-only/approve-only device invokes any account mutation/status/preview | `remote_permission_denied` before source call or audit success. |
| Full-control request has stale context revision | canonical `agent_auth_context_revision_conflict`; client refetches. |
| Logout confirmed count is stale | `agent_auth_context_in_use_changed`; request a new preview. |
| Backend error contains sensitive diagnostics | return the safe mapped error and bounded failed audit only. |
| Remote disconnects during login | host operation continues; client may query by operation id after reconnect. |

### 5. Good / Base / Bad Cases

- Good: a read-only phone lists “Codex default account” and its safe status but
  cannot start login or switch a session's auth source.
- Good: a full-control Web client starts a remote-attachable terminal login,
  reconnects, queries the same operation id, verifies the account, refreshes
  models, and receives the new context revision.
- Good: logout preview reports two Vibex session ids; logout succeeds only when
  the client echoes count two and an audit row records the safe action.
- Base: an older client omits the capability field and receives the same
  Provider-only catalog shape it already understands.
- Bad: return an OAuth URL/device code in durable operation JSON, let Relay
  launch the Agent, expose a raw terminal transcript, or allow approve-only
  devices to poll/manage authentication.

### 6. Tests Required

- Core serde tests cover every tagged request/response, capability default,
  safe Debug output, and absence of auth tokens in serialized operation queries.
- Remote tests cover read-only context/method listing, full-control mutations,
  denial before dispatch for lower permissions, and the dedicated audit target.
- Compatibility tests assert `ListRuntimeOptions` filters both account source
  summaries and account options when the client capability is false.
- Error tests inject secret-shaped diagnostics and assert response, tracing,
  and audit rows contain only safe codes/labels.
- Reconnect tests start an operation, drop the client transport, query it by id,
  then cancel or complete it without duplicate host authentication.
- Run `cargo test -p vibex-core -p vibex-remote -p vibex-remote-client` and the
  native mobile typecheck after protocol changes.

### 7. Wrong vs Correct

#### Wrong

```text
mobile -> Relay -> read ~/.codex/auth.json
mobile -> call Agent OAuth endpoint directly
Relay -> cache token and model entitlement
```

#### Correct

```text
remote typed request + device proof
  -> authorize on DesktopRuntime
  -> execute/query host AgentAuthContextService
  -> return redacted context/operation/action DTO
  -> audit bounded mutation outcome
  -> keep Relay as ciphertext transport only
```

## Scenario: Native Mobile Pairing, Auto Transport, And Bounded Streams

### 1. Scope / Trigger

- Trigger: `crates/vibex-remote-client` pairing/transport code is changed, or
  the native mobile client wires the shared GPUI Backend facade to
  v2 Direct/Relay routes.
- This scenario covers the Rust client boundary: entry-bound one-time pairing,
  Direct-first Auto transport, typed WebRemoteBackend mapping, authoritative
  cursor recovery, and Terminal/File binary flow. Product Views remain outside
  the transport adapter.

### 2. Signatures

```text
PairingEntryHint { schema_version, kind, origin?, transport? }
PairingEntryHintKind = mobile_app | origin | untrusted_custom_scheme
select_pairing_claim_route(RemotePairingOffer, PairingEntryHint)
  -> PairingClaimRoute::Direct { claim_base_url, transport }
   | PairingClaimRoute::Relay(RemotePairingCandidate)
claim_pairing_offer(...) / claim_pairing_offer_via_relay(...)
remote_http_client() -> BackendResult<reqwest::Client>
remote_http_client_for_config(RemoteClientConfig {
  base_url, pinned_tls_certificate_der?, ...
}) -> BackendResult<reqwest::Client>
android_websocket_connector() -> BackendResult<tokio_tungstenite::Connector>

DirectWebSocketTransport::probe() -> RemoteGatewayInfo
DirectWebSocketTransport::probe_direct_candidates(Vec<DirectCandidate>)
  -> Vec<CandidateProbeResult>
DirectWebSocketTransport::{connect, reconnect, request, subscribe, attach}
DirectWebSocketTransport::next_domain_event()
DirectWebSocketTransport::next_binary_event_for(stream_id?)
AutoRemoteTransport::{connect,reconnect,request,subscribe,attach,active_route}
DirectCandidate { url, label, priority, tls_certificate_der? }
MobileRemoteRouteBundle {
  local_network?: { url, tls_certificate_der }, direct_candidates, relay
}

WebRemoteBackend::facade() -> BackendFacade
WebRemoteBackend::resolve_unknown_mutation(RequestId) -> MessageSubmissionState
BackendFacade::{capabilities, replace_capabilities}

DomainSyncEngine::{observe, observe_invalidation, apply_replay_page, apply_snapshot,
  reset_for_reconnect, reset_for_session_epoch}
RemoteEventV2.sequence = connection-scoped channel/domain sequence
TimelineLiveEvent.sequence = authoritative per-session timeline sequence in payload
timeline_domain_event(RemoteLiveEventEnvelope, generation, &mut domain_sequence)
  -> RemoteEventV2
ChunkedFileReceiver::push(FileChunkDescriptor, &[u8])
TerminalBinaryBuffer::{push_frame, take_batch, require_reset}
CredentialStore / ClientIdentityStore
```

### 3. Contracts

- Parse and validate the full server-issued offer before using its candidates.
  A `mobile_app` hint carries exactly one eligible transport and no origin; an
  `origin` hint carries exactly one normalized development-host origin and no
  transport. Either form may select only a Direct, Tailnet, or self-hosted Relay
  route already in the offer. Untrusted schemes, unmatched routes, and ambiguous
  origins fail closed. When several server-owned candidates share the mobile
  hint's transport, claim through the first valid candidate in offer order, then
  retain every validated candidate in the committed Auto route bundle. Never use
  hint data itself as a request URL.
- One confirmation chooses and invokes exactly one claim route. Do not fall back
  to another route after a timeout, write error, malformed response, or other
  failure that could occur after dispatch because the one-time offer may already
  be consumed. A deliberate retry starts from a new offer.
- Validate an active returned device, the provisional public identity, non-empty
  grant, server id/pin, and the complete route bundle before exporting the
  credential to the host. The returned projection contains no challenge or claim
  nonce. Long-lived private/grant material must stay out of `Debug` and public
  runtime snapshots.
- Direct candidate probing performs only bounded `GET /api/v2/info`; it does
  not claim a pairing offer, issue a ticket, or rotate a device grant. Candidate
  count, probe response bytes, and probe time are bounded. Selection is
  deterministic by measured latency then candidate priority.
- Product mobile connections require HTTPS/WSS and reject URL credentials,
  query strings, and fragments. HTTP/WS is accepted only for an explicit
  loopback development exception. The client checks the paired server id and
  static identity key before ticket/WS use.
- Zero-config pairing stores its resolved loopback/private/link-local numeric
  HTTPS origin separately as `localNetwork`, together with the full encrypted-
  transcript-bound certificate DER. It never stores or resolves the offer's
  `vibex-lan.local` placeholder. Credentials written before this field existed
  deserialize with `localNetwork = None` and retain their previous routes.
- A local-network candidate is priority zero and carries its certificate pin
  into every `/api/v2/info`, ticket, and WebSocket connection config. Pinned
  HTTP uses a no-proxy/no-redirect client with only the stored certificate as a
  root and a hostname exception scoped to that store. Pinned WSS requires the
  exact leaf, no intermediates, a valid signature/time chain to that single root,
  and normal TLS handshake signatures. The pin never applies to other Direct or
  Tailnet candidates, which retain their normal WebPKI policy.
- HTTP requests to `localhost` or loopback/private/link-local numeric Direct and
  Relay URLs bypass environment/system proxies and disable redirects; public
  URLs retain the platform proxy policy. URL classification happens before
  building the client and never mutates process proxy variables. Local smoke
  health probes use the same explicit no-proxy boundary.
- Android product HTTP and WebSocket routes use the same bundled Mozilla
  WebPKI roots. The HTTP client installs them with `tls_certs_only`; the WSS
  client supplies an explicit rustls connector backed by the same root set.
  This avoids an Android platform-verifier failure that can map a valid public
  chain with CRL metadata but no OCSP responder to `Revoked`. Hostname, chain,
  signature, validity-window, and trusted-root checks remain mandatory;
  accepting invalid certificates is forbidden. Non-Android clients retain
  their existing platform-root behavior.
- A connection obtains a single-use WS ticket, sends v2 hello first, proves
  the paired X25519 device identity over the ticket challenge and hello
  transcript, and verifies the ephemeral X25519/HKDF/HMAC session confirmation.
  Session keys and ephemeral keys stay in memory.
- `AutoRemoteTransport` receives every validated Direct/Tailnet candidate and the
  optional self-hosted Relay candidate from the committed credential. It prefers
  healthy Direct routes, can fall back to Relay for ordinary transport failure,
  preserves authoritative cursors across handoff, and exposes only the typed
  `ActiveRemoteRoute`. Revoked, authentication-required, incompatible, and
  identity-mismatch outcomes are terminal and must not trigger route reselection.
- `WebRemoteBackend` is the only adapter visible to shared Controllers. It
  delegates all seven Backend domains to typed v2 DTOs, passes idempotency and
  revision/CAS metadata, filters capabilities by server features and device
  permissions, and never exposes wire envelopes to Views.
- `BackendFacade` clones share one synchronized capability snapshot. Every
  successful handshake/reconnect replaces that snapshot from the latest
  `RemoteServerInfoV2`; it must not union with the previous snapshot. Feature or
  permission removals therefore disappear from already-cloned facades before
  controllers refresh their capability-gated actions.
- RPC timeout is request-local. A timeout, write failure, or socket loss marks a
  mutation as unknown and registers an operation-specific result query; the
  client never retries a prompt or other mutation automatically. A caller must
  query the authoritative result before any deliberate retry.
- Domain cursors advance only through contiguous generation/sequence events.
  Gaps, retention misses, queue overflow, reconnect, or session-epoch changes
  emit `Lagged`/`Resync`, rewind local cursors where necessary, and require an
  authoritative snapshot/catch-up before live application resumes. Replay pages
  update authority but are not re-injected into the wire event queue.
- Agent live events have two independent sequence spaces. The outer
  `RemoteEventV2.sequence` is contiguous across every `agent_session` event sent
  on one connection, including events from different sessions. The serialized
  `TimelineLiveEvent.sequence` inside `payload` remains the authoritative
  per-session timeline cursor. Gateway fanout must assign the outer domain
  sequence separately and must never copy a session timeline sequence into it;
  otherwise an established session whose next item is greater than one, or two
  interleaved sessions, creates a false domain gap and repeated mobile refetches.
- Domain and binary consumers use separate bounded queues. Binary consumers may
  select a stream id so Terminal frames cannot be stolen by another transfer.
  A domain queue overflow publishes one explicit resync marker instead of
  presenting later events as a false contiguous stream.
- `file`, `git`, `provider`, and `device` events are projection invalidations,
  not append-only business streams. Gaps or bursts on one of these channels
  coalesce to the newest invalidation for that channel without pausing Agent or
  Terminal domains. The bounded event queue reserves at most one invalidation
  slot per projection channel and preserves those slots when a business-stream
  overflow emits its explicit resync marker.
- Terminal frames preserve raw bytes and validate stream id, generation,
  sequence, checksum, bounded buffering, and rebuild/reset conditions. File
  chunks validate transfer id, sequence, offset, per-chunk SHA-256, total size,
  maximum size, cancellation, and final size before committing the sink; the
  receiver streams into a sink rather than buffering the complete file.
- Durable credential stores keep grant metadata and the long-lived device
  identity separate. Debug/serde projections never expose auth tokens or
  private keys, and no session key, pairing challenge, prompt, file content, or
  Terminal output is persisted.
- HTTP JSON bodies are consumed incrementally and rejected once the bounded
  response limit is exceeded; a large `Content-Length` fails before body read.
  WebSocket callbacks feed a bounded channel and may force reconnect/resync when
  a slow consumer exhausts capacity.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Entry hint schema/kind/origin/transport is invalid or uses an untrusted custom scheme | `remote_pairing_entry_hint_incompatible`, `remote_pairing_entry_hint_invalid`, or `remote_pairing_entry_untrusted`; send no claim. |
| Mobile transport matches no offered route, or entry origin matches zero or multiple offered routes | `remote_pairing_entry_route_mismatch` or `_route_ambiguous`; send no claim. |
| One-time claim has a possibly post-dispatch failure | Return the typed claim error; do not replay across Direct/Relay. |
| Claim response device/public key/grant or route bundle is invalid | `remote_pairing_claim_response_invalid` or route validation error; export no credential. |
| Candidate list empty/over limit | `remote_candidate_count_invalid`; no probe beyond the bound. |
| Direct URL is insecure outside explicit loopback, contains credentials/query/fragment, or has invalid backoff/queue limits | Structured local validation error; no network request. |
| Pinned route is not HTTPS/WSS, uses a hostname/public IP, or has malformed/oversized DER | `remote_pinned_tls_route_invalid` / `remote_tls_certificate_invalid`; make no request. |
| Pinned HTTP/WSS presents a certificate other than the stored certificate | TLS transport failure; never retry with hostname or certificate checks disabled. |
| A local numeric Direct/Relay URL is configured while environment proxies are present | Use a proxy-free HTTP client; do not send local pairing, probe, or ticket requests to the proxy. |
| Stored credentials omit `localNetwork` | Load it as `None` and continue using the existing Direct/Relay bundle. |
| Android receives a valid public chain with no OCSP responder | Verify both HTTP and WSS through the bundled WebPKI roots; do not publish a false `Revoked` state. |
| Android certificate hostname, chain, signature, validity window, or root trust is invalid | Return the typed HTTP/WS transport failure and remain offline; never bypass certificate verification. |
| `/api/v2/info` identity or v2 range mismatch | `remote_server_identity_mismatch` / `remote_protocol_incompatible`; do not issue a WS ticket. |
| Reconnect removes an enabled feature or permission | Replace the shared facade snapshot and remove the operation; never retain stale UI authority. |
| Pairing claim challenge/nonce/device key is missing or insecure URL is used | `remote_pairing_request_invalid` / `remote_secure_context_required`; challenge is not retained. |
| WS ticket is missing, expired, reused, or server confirmation mismatches | Reject authentication and keep the socket out of `online`. |
| RPC times out while the socket remains usable | `remote_rpc_timeout`; keep socket state separate and require idempotency result query for mutations. |
| Socket closes with an in-flight mutation | `remote_rpc_result_unknown`; mark the mutation unknown and never replay it automatically. |
| Event generation/sequence gaps, queue overflow, or reconnect rewind | `BackendEvent::Lagged` with authoritative refetch metadata; do not deliver a later event as contiguous. |
| Two Agent sessions publish local timeline sequences such as `19` then `3` | Send outer `agent_session` domain sequences `1` then `2` on a fresh connection while preserving `19` and `3` inside their typed payloads. |
| Projection invalidation burst or out-of-order invalidation | Keep one newest `ProjectionInvalidated` event for that channel and leave strict Agent sequence handling active. |
| Binary stream id/generation/sequence/checksum/offset is invalid | Reject the frame, request reset/rebuild, and write no unvalidated bytes. |
| File chunk exceeds chunk/transfer limit or final size does not match | Reject before sink finish; invoke cancellation/cleanup and preserve no partial success claim. |
| Stored credential JSON is malformed or private material appears in Debug | Return storage validation failure; redaction tests fail closed. |
| Auto route reports revoked/authentication-required/incompatible/identity mismatch | Publish the matching terminal state; do not probe or reconnect another route. |
| HTTP response exceeds the JSON byte budget | `remote_http_payload_too_large`; do not allocate the full response. |

### 5. Good / Base / Bad Cases

- Good: a mobile Tailnet entry selects the first valid Tailnet candidate in
  server offer order,
  performs one claim, commits a credential containing all routes, and Auto later
  uses the same paired device identity across Direct/Relay handoff.
- Good: Auto probes LAN and Tailnet candidates in parallel, chooses the healthy
  low-latency path, then uses the same paired device identity for the ticket and
  v2 handshake while exposing the selected active route.
- Good: with no Tailnet, published Direct route, or Relay, the local candidate
  passes pinned HTTPS probe, obtains a WS ticket, reaches v2 Online over pinned
  WSS, and completes a heartbeat.
- Good: an Android client claims and attaches over a publicly trusted Tailnet
  HTTPS/WSS endpoint whose leaf has CRL metadata but no OCSP responder; both
  transports validate the same chain and neither reports `Revoked`.
- Good: a mobile suspend/reconnect or bounded queue overflow produces a lagged
  event and authoritative timeline/runtime refetch; an uncertain Agent send is
  resolved by `GetMessageSubmission` before any retry.
- Good: reconnecting from a server with `device_pairing` to one that advertises
  only `device_management` removes pairing from every existing facade clone.
- Good: 100 file invalidations coalesce into one authoritative Files refetch
  while a contiguous Agent event remains deliverable.
- Good: a running background session and the selected completed session can
  interleave timeline events without changing the selected timeline's loading
  state; the transport domain cursor stays contiguous while each payload keeps
  its own session cursor.
- Good: a Terminal stream with an evicted frame emits `reset_required`, while a
  large file is written chunk-by-chunk into a sink and the final total/checksum
  is checked before commit.
- Base: a loopback HTTP fixture is allowed only when the development host
  explicitly sets the exception; product mobile state remains HTTPS/WSS-only.
- Base: non-Android clients continue using their existing platform trust path.
- Base: a credential created before `localNetwork` existed still connects through
  its prior Direct/Relay candidates.
- Bad: use the entry hint as a URL, try Direct and Relay claims in sequence, probe
  by consuming the offer, put the grant in a URL, treat an RPC timeout as socket
  death, resend a prompt after reconnect, advance a cursor on a dropped event,
  share one binary queue between Terminal and File, or use `Response::bytes()`
  before enforcing the HTTP limit.
- Bad: merge reconnect capabilities into the old snapshot, or let a projection
  invalidation gap pause unrelated sequenced domains.
- Bad: assign `RemoteEventV2.sequence = TimelineLiveEvent.sequence`; timeline
  sequences are local to a session and cannot order a multi-session channel.
- Bad: fix an Android availability failure with
  `danger_accept_invalid_certs`, or configure WebPKI roots for HTTP while
  leaving WSS on the failing platform verifier.
- Bad: discard the local route after claim, resolve `vibex-lan.local` later,
  require Relay when the local probe is healthy, or share the pinned trust store
  with unrelated Direct candidates.

### 6. Tests Required

- `cargo test -p vibex-remote-client --locked` covers secure URL policy, exact
  Direct/Tailnet/Relay entry selection, mismatch/ambiguity/custom-scheme
  rejection, one-route claim smoke, candidate selection, control waiter cleanup,
  bounded domain/binary queues, stream filtering, cursor gap/rewind/reconnect
  behavior, credential redaction, Terminal rebuild, and File chunk cases.
- `local_lan_smoke` must configure no Tailnet, published Direct route, or Relay;
  perform discovery bootstrap, encrypted approval/claim, pinned HTTPS probe,
  pinned WSS v2 Online, and heartbeat. Mobile storage tests must deserialize an
  otherwise valid older credential with no `localNetwork` field.
- Direct and Relay loopback smoke probes must construct no-proxy clients so a
  developer or CI environment proxy cannot turn a healthy local endpoint into a
  false `remote_candidates_unreachable` failure.
- Regression coverage must keep
  `shared_facade_tracks_capability_additions_and_removals_after_reconnect`,
  `projection_invalidation_gaps_coalesce_without_pausing_other_domains`, and
  `projection_invalidation_burst_coalesces_without_pausing_agent_events` green.
- `cargo test -p vibex-remote timeline_live_events_use_a_connection_scoped_domain_sequence
  --locked` must prove interleaved session payload sequences remain unchanged
  while their outer domain sequences are contiguous.
- `cargo check -p vibex-mobile --locked` proves the native mobile transport,
  bounded WebSocket channel, storage traits, and shared Backend graph remain
  native-platform-safe.
- Direct smoke must cover `/api/v2/info` probe without offer consumption,
  pairing claim, device identity, WS ticket, v2 crypto confirmation, subscribe,
  heartbeat, disconnect, and reconnect. Relay smoke covers E2EE claim, handoff,
  revoke, and reconnect without cross-route claim replay. A TLS/WSS fixture is
  required before a production LAN/Tailnet release claim; loopback HTTP is
  development evidence only.
- `pnpm check:mobile-native` must validate the native mobile contract after
  target-reachable remote changes. Its self-test must reject a missing Android
  HTTP root override, missing WSS connector, or disabled certificate
  verification. Run workspace Rust checks and `git diff --check` before commit.
- Android device qualification must claim a real publicly trusted HTTPS offer,
  establish its WSS session, and confirm logs contain neither a false
  certificate `Revoked` result nor a process crash.

### 7. Wrong vs Correct

#### Wrong

```rust
let claim_url = options.entry_hint.origin.unwrap();
for route in [claim_url, offer.relay_candidate.url] {
    if claim_pairing_offer(route, request.clone()).await.is_ok() { break; }
}
let bytes = response.bytes().await?;
if bytes.len() > MAX_JSON_BYTES { return Err(too_large()); }
let client = reqwest::Client::new(); // Android platform verifier on product routes.
let socket = tokio_tungstenite::connect_async(request).await?;
let local = offer.direct_candidates[0].url; // Persist vibex-lan.local.
let event_sequence = timeline_event.sequence; // Wrong sequence space.
// A slow WebSocket consumer advances the cursor before the event is retained.
send_message_again_after_timeout(request).await?;
facade_capabilities.extend(reconnected_server_capabilities);
```

#### Correct

```rust
let route = select_pairing_claim_route(&offer, &entry_hint)?;
let response = claim_exactly_once(route, request).await?;
let info = http_json(remote_http_client()?.get(info_url)).await?;
let socket = connect_async_tls_with_config(
    request,
    None,
    false,
    Some(android_websocket_connector()?),
).await?;
let local = DirectCandidate {
    url: resolved_numeric_lan_origin,
    tls_certificate_der: Some(encrypted_transcript_certificate),
    priority: 0,
    ..candidate
};
validate_claim_response(&offer, &provisional_identity, &response)?;
let event = timeline_domain_event(timeline_event, generation, &mut domain_sequence);
let body = read_chunks_with_limit(response, MAX_JSON_BYTES).await?;
let decision = if is_projection_invalidation(&event) {
    sync.observe_invalidation(event.clone())
} else {
    sync.observe(event.clone())
};
bounded_domain_queue.push_or_resync(event, decision);
facade.replace_capabilities(remote_capabilities(Some(&server_info)));
if mutation_is_unknown {
    query_authoritative_submission(idempotency_key).await?;
}
```

The transport enforces byte/queue bounds before publishing, and mutation
recovery is an explicit authoritative query rather than a best-effort replay.

## Reconnect Semantics

Clients must not rely on live event delivery for correctness. After reconnect:

1. Client authenticates and presents last known sequence per session/channel.
2. Server returns authoritative missed events or a compacted timeline slice.
3. Client subscribes to live events only after catch-up completes.

This applies to Agent timelines, terminal summaries, Git changes, and permission
state.

### Relay Session Replacement And Terminal States

- The PC Relay runtime keeps one active `RelaySession` per device peer. When a
  new `RelayHandshakeHello` arrives for a peer that already has a session, the
  old session and its outbound forwarder are dropped before the new session is
  installed. A stale forwarder must not seal a notification or binary frame
  with an old session id, room route, counter, or permission context.
- `AutoRemoteTransport` may reselect Direct or Relay after an ordinary socket
  loss, timeout, or recoverable close. `DeviceRevoked`, `AuthenticationRequired`,
  and `UnsupportedVersion` are terminal outcomes: publish the control/close
  event, expose `Revoked` or `Incompatible`, and do not start Direct-to-Relay
  fallback or reconnect attempts.
- A terminal close must not be hidden behind queued `ResyncRequired` or other
  stale domain controls. The bounded domain queue is cleared before the
  terminal event is delivered, and subsequent operations fail with the stable
  terminal error code.

#### Validation Matrix

| Condition | Required result |
| --- | --- |
| Same device peer sends a new Relay hello | Retire the previous session/forwarder before installing the new one. |
| Old session emits an asynchronous outbound frame after replacement | Drop it; do not route or encrypt it for the new peer session. |
| Relay/Remote close is `device_revoked` or `authentication_required` | State becomes `Revoked`; no automatic route reselection. |
| Relay/Remote close is `unsupported_version` | State becomes `Incompatible`; no automatic route reselection. |
| Terminal close arrives while resync controls are queued | Clear stale domain controls and deliver the terminal close first. |

#### Tests Required

- Relay v2 smoke must cover Direct -> Relay handoff, device revoke, and a
  reconnect attempt while an older session's outbound task is still draining.
- Assert that revoke remains `Revoked` across repeated `next_event()` calls and
  that no new route is selected after the terminal close.
- Assert that a same-peer replacement cannot deliver a frame from the retired
  session and that the serialized frame retains only the current route/session
  metadata.

## Relay Server Boundaries

The Relay server may provide:

- `/health`
- `/api/info`
- `/ws` for PC room connection
- `/api/rooms/:room_id/pair`
- `/api/rooms/:room_id/command`
- Room TTL, heartbeat, connection limits, and rate limits

The Relay server must not decrypt business payloads, inspect file paths, inspect
terminal output, inspect Agent messages, or make authorization decisions beyond
room-level transport rules. It must not host product application assets;
non-API product/static routes return 404.

## Large Payloads

Large files, screenshots, and logs should use chunked transfer. Chunks must be
authenticated, ordered, resumable where practical, and scoped to an authorized
request id.

## Audit Logs

Audit remote actions that can affect local state:

- Pairing and device revocation.
- Permission approvals and denials.
- Agent message sends and interrupts.
- File writes/deletes/moves.
- Git operations.
- Terminal input and process kill operations.
- Provider config export or dynamic switch.

Audit logs must avoid storing secrets and should reference timeline or command
ids instead of duplicating sensitive payloads.

## Anti-Patterns

- Do not implement a separate mobile-only API shape.
- Do not let Relay room ids act as authorization secrets by themselves.
- Do not send plaintext business payloads through Relay.
- Do not restore Relay/Desktop static hosting for the mobile runtime.
- Do not skip timeline catch-up after reconnect.

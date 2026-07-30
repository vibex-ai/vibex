# Vibex Architecture Baseline

> **Authority**: this document is the cross-platform UI and remote architecture
> baseline for Vibex. Implementation status is decided by code, tests, and the
> evidence files listed in [Evidence Map](#evidence-map) — not by this document.

Read this before designing anything that spans desktop, Web, mobile, or the
remote transport. Layer-specific rules live in
[backend](../backend/index.md) and [frontend](../frontend/index.md); this file
owns the decisions those layers must not contradict.

## Scope

- One GPUI design system, one set of domain components, one Local/Remote backend
  contract, one versioned remote protocol, and several viewport-driven shells.
- `apps/desktop` is the only visual, interaction, and information-architecture
  source. Web and mobile derive from it; they never define a parallel design.
- The PC `DesktopRuntime` is the only authoritative state owner. Web and mobile
  are network clients.

### Non-Goals

- Do not compile `apps/desktop` as a whole to WASM. Shared UI lives in
  `crates/vibex-ui`; native platform work stays in `apps/desktop`.
- Do not run Agent processes, Git repositories, PTY, or project filesystems on
  mobile.
- Do not scale the desktop three-pane workbench down to phone width. Compact is
  a re-layout, not a shrink.
- Do not build a second design system for touch.
- Do not let Relay become a second business database or state authority.
- V1 ships Direct and user self-hosted Relay only. There is no official Vibex
  Relay, no Vibex account system, and no public multi-tenant Relay operation.
  Protocol extension points may exist, but no default official endpoint ships
  and nothing may retroactively depend on one.

## System Architecture

```text
                         ┌──────────────────────────┐
                         │      DesktopRuntime      │
                         │ Agent/File/Git/PTY/Config │
                         │   only authoritative state │
                         └────────────┬─────────────┘
                                      │
                    ┌─────────────────┴─────────────────┐
             NativeBackend                        RemoteGateway
                    │                                   │
          GPUI Desktop client               versioned Vibex remote protocol
                                                        │
                                          ┌─────────────┴─────────────┐
                                  Direct WebSocket             Relay WebSocket
                                  LAN / private network        user self-hosted E2EE
                                          └─────────────┬─────────────┘
                                               WebRemoteBackend
                                    ┌───────────────────┴───────────────────┐
                             GPUI-WASM Web                        GPUI-WASM + Capacitor
                         Wide / Medium / Compact                     Compact first
```

Boundaries:

- GPUI views and controllers depend only on the backend capability traits.
- Desktop reaches `DesktopRuntime` in process through `NativeBackend`.
- Web and mobile reach `RemoteGateway` through `WebRemoteBackend`.
- `DesktopRuntime` decides permissions, data versions, and mutation results.
  Clients cannot bypass it.
- Direct and Relay are transports only. They must not produce two RPC surfaces
  or two state models.

Capabilities are split by domain instead of one large trait
(`crates/vibex-backend/src`): `AgentBackend`, `WorkspaceBackend`, `FileBackend`,
`GitBackend`, `TerminalBackend`, `ManagementBackend`, `DeviceBackend`.

UI uses a capability snapshot to decide which affordances to show. The server
must still re-check every permission; a capability snapshot is not a security
boundary.

## GPUI Desktop Is The Only Design Source

New Web and mobile surfaces inherit directly from the desktop assets:

- `apps/desktop/src/app.rs` — workbench structure and product hierarchy.
- `apps/desktop/src/code_workbench.rs` — file, edit, Preview, Git, Agent flows.
- `apps/desktop/src/management.rs` — Management Center semantics.
- `apps/desktop/src/theme.rs` — theme wiring.
- `apps/desktop/src/terminal_surface.rs` — Terminal visuals and interaction.
- `apps/desktop/assets/icons` — brand, Agent, file-type, and action icons.
- `crates/vibex-ui/src` — shared tokens, primitives, controllers, and shells.

Design token ownership sits in the shared layer: `crates/vibex-ui/theme/tokens.json`
is the source and `scripts/generate-tokens.mjs` produces
`crates/vibex-ui/src/generated_tokens.rs`. Neither GPUI Desktop nor GPUI-WASM
may reintroduce a CSS file as the authoritative token source.

Style rules that apply to every platform:

- One semantic meaning uses one colour, icon, name, and state expression
  everywhere.
- Agent timeline, diff, file tree, Terminal, and approval cards are shared
  domain components.
- Compact may change arrangement and density. It may not switch to a different
  card style or brand language.
- No hover-only discovery of a primary action. Touch surfaces get an explicit
  button or long-press menu.
- Desktop keyboard shortcuts stay. Touch adds discoverable entry points.
- Dialogs, popovers, and menus may become sheets on narrow widths, but content
  and behaviour semantics stay the same.

## Adaptive Shells

`crates/vibex-ui/src/shell.rs` owns the resolution. Breakpoints are driven by
content minimums and viewport size, never by User-Agent:

| Shell | Width | Form |
| --- | --- | --- |
| `WideShell` | ≥ `WIDE_MIN_WIDTH` (1100) | Desktop workbench, panels side by side |
| `MediumShell` | ≥ `MEDIUM_MIN_WIDTH` (760) | One primary plus one auxiliary surface, rest in drawers |
| `CompactShell` | < 760 | Single task stack, bottom navigation, sheets and full-screen sub-pages |

`ABSOLUTE_MIN_WIDTH` is 360 and `ABSOLUTE_MIN_HEIGHT` is 620. A narrow desktop
window legitimately enters `CompactShell`; a tablet or landscape phone
legitimately enters `MediumShell`.

Structure conversion:

| Desktop | Medium | Compact |
| --- | --- | --- |
| Left session/file tree | Collapsible sidebar | Separate list page or full-screen drawer |
| Central Agent/editor | Stays primary | The single primary page |
| Right Preview/detail | Overlay or drawer | Full-screen secondary page |
| Multi-pane Git diff | Resizable split | Single-column unified diff |
| Dense top toolbar | Primary actions kept | Primary actions plus a More sheet |
| Popover / dialog | In-window overlay | Bottom or full-screen sheet |
| Hover actions | Also click-discoverable | Explicit button, swipe, or long press |
| Keyboard shortcuts | Full support | On-screen buttons and a Terminal helper key row |

Mobile navigation is two levels:

```text
Global:       Sessions / Management Center / Settings
In a session: Agent / Files / Changes / Terminal
```

The Management Center keeps the desktop name and domain model. Session-scoped
tools and global configuration must not be mixed into one bar.

## Platform Positioning

| Platform | Shells | Backend | Role |
| --- | --- | --- | --- |
| GPUI Desktop | Wide/Medium, Compact when narrow | `NativeBackend` | Full local workbench and authoritative runtime |
| GPUI-WASM Web (`apps/web`) | All, chosen by viewport | `WebRemoteBackend` | Browser remote workbench; desktop experience on wide screens |
| Capacitor mobile (`apps/mobile`) | Compact first, Medium in landscape | `WebRemoteBackend` | Pocket remote console with reduced density |

"Desktop Web or mobile Web" is not a deploy-time choice. The same artifact picks
a shell from available width. `apps/mobile` packages only `apps/web/dist` under
the `dev.vibex.remote` application identity.

## Mobile V1 Scope

In scope:

- **Agent and approvals** — list, create, open, resume sessions; live timeline;
  send, continue, interrupt; explicit running / needs-input / done / failed
  state; approvals as a high-priority sheet showing action, target, risk, and
  allow/deny; push straight into the session and the approval item. An approval
  must not be a passive timeline card and must not require the app to be open.
- **Files** — tree, path navigation, search; text, Markdown, image, and Git diff
  viewing; create and edit UTF-8 text files up to 1 MiB; save carries a
  revision and a conflict must never silently overwrite; explicit dirty,
  saving, saved, conflict, and disconnected states.
- **Git review** — status and diff, stage and unstage, commit message editing,
  commit behind a second confirmation.
- **Terminal** — list, create, attach, switch, close; live ANSI output, input,
  resize, reconnect recovery; Esc/Ctrl/Tab/arrow helper key row; input and recent
  output stay visible under the soft keyboard; explicit connection quality,
  read-only, and rebuild state. Mobile Terminal is a remote PTY control surface;
  it never creates a local shell.
- **Management Center** — Agent/Provider availability, Provider profile and
  model view and switch, Provider health check, Relay status, paired devices
  with permissions, last-seen, and revoke.

Out of scope for V1:

- Binary editing, batch operations, directory copy, file delete or rename, and
  project-scale refactoring.
- Git discard/revert, branch create/switch/delete, fetch/pull/push, remotes,
  worktrees, and history rewriting.
- Provider secret editing or export, MCP/Skills/Prompts import, Hooks,
  Scheduled, Automation, Backup/Restore, and bulk Provider CRUD.

Every exclusion is enforced by server-side device capability and permission
checks. Hiding a button is not enforcement.

## GPUI-WASM Platform Constraints

| Capability | Constraint | Requirement |
| --- | --- | --- |
| WebGPU | No Canvas2D fallback if init fails | Real-device gates for desktop browsers and Android/iOS WebView; see `docs/platform/wasm-browser-gate.md` |
| Native popup | Anchored popups unavailable | Use in-window GPUI popover/sheet |
| File picker | Web path selection incomplete | Upload bytes through the DOM/Capacitor bridge |
| Clipboard read | Synchronous read unavailable | Rely on paste events and the platform bridge |
| Credential storage | GPUI credential API unavailable | Abstract Web Storage and Capacitor secure storage |
| Keyboard layout | Mapper is US-centric | Test non-US layouts and shortcuts explicitly |
| IME | Candidate-window placement incomplete | CJK input, soft keyboard, and composition need real-device acceptance |
| Accessibility | Canvas accessibility adapter incomplete | Separate accessibility plan and release standard |
| Fetch body | Body is fully buffered | Terminal and large files must not use plain buffered fetch |
| DOM embedding | Canvas cannot host an iframe | Web Preview needs an overlay bridge or external open |

The default dispatcher is single-threaded: it lowers COOP/COEP and Capacitor
WebView compatibility cost. Enabling the multi-threaded dispatcher requires
SharedArrayBuffer, Atomics, and cross-origin isolation response headers, and
should be justified by Terminal, timeline, and large-file performance data.

The application entry must own the `ApplicationHandle`. Keeping the application
alive by leaking it is a demo pattern, not product code.

## Crate Boundaries

```text
crates/vibex-ui              shared design system, views, controllers, shells, pure UI state
crates/vibex-backend         per-domain backend traits, capability and error model
crates/vibex-remote-client   WebRemoteBackend, protocol state machine, Direct/Relay transport
crates/vibex-terminal-ui     portable terminal emulator, frame model, GPUI surface
crates/desktop-model         platform-neutral projections (timeline, preview tree, diff, UI settings)
crates/desktop-runtime       typed facade, bootstrap, subscriptions, lifecycle
apps/desktop                 NativeBackend, native platform bridges, DesktopRuntime startup
apps/web                     wasm-bindgen entry, Web bridge, static assets
apps/mobile                  Capacitor shell, push, secure storage, deep link, lifecycle bridge
```

The platform bridge handles only what the host must do: safe area, soft
keyboard, foreground/background lifecycle, secure storage, push notification and
deep link, camera scanning, file selection, share and download, and opening
system URLs. Pages, navigation, and domain components must never fall back to a
TypeScript or React implementation.

## Connectivity

`WebRemoteBackend` targets one `RemoteTransport`:

```text
RemoteTransport
  ├── DirectWebSocketTransport
  └── RelayE2eeTransport → user self-hosted Relay
```

| Network shape | Vibex class | Typical entry | Third-party service |
| --- | --- | --- | --- |
| Same LAN | Direct | PC LAN address or same-origin WebUI | No |
| Tailscale / Headscale / WireGuard / ZeroTier | Direct | Tailnet IP, MagicDNS, or HTTPS serve | Only the mesh tool the user chose |
| User self-hosted Vibex Relay | Relay | User-configured WSS/HTTPS endpoint | Self-deployed |

- Private mesh networks stay classified as Direct even when the mesh internally
  relays packets.
- Relay is an outbound connection from the PC. It does not require an inbound
  public port on the PC.
- Relay sees routing metadata and ciphertext only. It never reads code,
  conversations, Terminal data, or configuration.
- Direct and Relay share the same handshake, RPC, events, error codes, and
  consistency semantics. Frame carriage may differ; business logic may not fork.
- The Management Center offers Auto, Direct Only, and Relay Only, plus a
  self-hosted Relay endpoint.

In Auto mode a pairing QR may carry several Direct candidates and one Relay
candidate. The client probes reachable addresses concurrently or in short
intervals and prefers a healthy low-latency Direct path, falling back to Relay.
Reachability probing must not consume the pairing offer: only the selected
transport performs the single claim, retries with the same device nonce are
idempotent, and a claim from a different device must be rejected. Switching
transport keeps the same device identity and `DesktopRuntime` permissions.

### HTTP And WebSocket Layering

| Purpose | Protocol |
| --- | --- |
| HTML, GPUI-WASM, fonts, icons, manifest | HTTPS |
| Agent, Git, File, approval RPC and live events | WSS |
| Terminal and file streams | WSS binary frames |
| health, pairing bootstrap, controlled download | A small HTTPS API |

Production uses HTTPS plus WSS. `http://localhost` and `ws://localhost` are
development-only. `http://192.168.x.x` must not be a production entry point:
WebGPU, PWA, camera, and some storage APIs require a secure context; an HTTPS
page blocks cleartext `ws://` as mixed content; and an online WebUI reaching a
private address additionally hits CORS and Private Network Access limits.

Recommended access shapes:

- LAN WebUI — the PC `RemoteGateway` serves the `apps/web` bundle and WSS from
  the same origin.
- Private mesh WebUI — expose the loopback `RemoteGateway` as a mesh-internal
  HTTPS/WSS address through the mesh tool's reverse proxy.
- Public WebUI — a trusted HTTPS static site loads GPUI-WASM and connects over
  E2EE Relay.
- Capacitor app — GPUI-WASM assets ship inside the app, never downloaded from
  the PC; the network side still uses Direct WSS or E2EE Relay.

HTTPS/WSS protects the transport only. Relay paths must keep application-layer
E2EE on top so neither the TLS terminator nor Relay can read business content.

### Mesh Relay Versus Vibex Relay

These are different layers and must never be configured as each other:

- A mesh relay (for example Tailscale DERP) relays WireGuard packets at the
  network layer.
- A Vibex Relay is an application-layer service that understands rooms, the
  pairing bridge, and the Vibex transport envelope.

Prefer keeping `RemoteGateway` on loopback and exposing it through the mesh
tool's own serve/proxy feature rather than binding to every interface. Joining a
mesh only grants network reachability; it never grants Agent, file, Git, or
Terminal permission. Vibex pairing is still required on every network path.

### User Self-Hosted Relay

In-repo deliverables:

- `apps/relay-server` — the `vibex-relay-server` service.
- `crates/relay` — E2EE frames, counters, and replay checks.
- `crates/desktop-runtime/src/relay.rs` — the PC-side Relay client.
- `deploy/relay/docker-compose.yml` and `deploy/relay/Caddyfile` — deployment.
- `scripts/smoke-relay-local.mjs` and `docs/smoke/relay-nat.md` — local and
  physical-network verification.

The desktop Management Center takes a Relay HTTPS origin. The client verifies
protocol version and features from `/api/info` before establishing a transport.

Relay stays zero-knowledge: a room id is routing metadata, not an authorization
secret. Device auth, permissions, revoke, and audit remain in the PC
`DesktopRuntime`.

The current implementation targets personal and small self-hosted use, not a
public multi-tenant service: room state is in memory, operational limits are
mostly code defaults, and tenant auth, dynamic quotas, and horizontal scaling
are not implemented.

## Unified Remote Protocol

The wire contract lives in `docs/remote/protocol-v2.md`; shared DTOs live in
`crates/core/src/remote.rs`. The rules below are the parts that must not drift.

### Handshake

```text
client → hello { client_id, client_type, app_version, protocol_version,
                 device_id, capabilities }
server → server_info { server_id, desktop_version, protocol_range,
                       enabled_features, device_permissions, session_epoch }
```

Capability gating and explicit version-incompatibility errors are required. An
older client must never see an unknown enum and crash.

### Frame Types

- JSON control — hello, ping/pong, subscribe, attach, detach.
- JSON RPC — `request_id`, a timeout class, and structured errors.
- JSON event — Agent, Git, File, Provider, device, and runtime updates.
- Binary Terminal — raw PTY bytes, terminal id, sequence.
- Binary File — chunked upload/download, checksum, offset, cancel.

Application-level ping/pong provides one liveness signal for browsers and
WebViews. An RPC timeout does not by itself mean the socket is closed.

### Consistency And Reconnect

Every mutable domain needs a monotonic sequence or cursor, or an explicit
generation, plus:

- live events for latency and authoritative fetch for correctness;
- bounded paged catch-up for the disconnected interval;
- gap detection and de-duplication;
- an idempotency key per mutation;
- file revisions and compare-and-swap for configuration.

Recovery order:

1. The client reconnects with its last server/session epoch and per-domain
   cursors.
2. The server confirms the cursors are still inside the retention window.
3. It replays pages when possible, or returns `resync_required`.
4. The client refetches authoritatively, then resumes live subscriptions.
5. Unconfirmed mutations are resolved by querying the idempotency key rather
   than blindly resending.

### Terminal Data Plane

Terminal traffic is raw bytes, never re-encoded through UTF-8 strings:

```text
terminal binary frame
  opcode
  terminal_slot / id
  sequence
  raw bytes | rows+cols | snapshot metadata
```

The client stores the last sequence and reconnects with
`attach(after_sequence)`. If the ring buffer still holds the range, the server
replays incrementally; if it expired, it returns `rebuild_required` plus the
retained snapshot. Polling a full terminal string on a fixed interval is not an
acceptable substitute for the live binary stream.

## Pairing, Device Permissions, And Security

### Pairing Model

1. On first `RemoteGateway` enable, the PC generates a persistent identity key
   protected by `0600` or OS secure storage.
2. In the desktop Management Center the user picks "pair a new device" and the
   permission scope to pre-grant; the desktop generates a short-lived pairing
   offer.
3. The offer carries server id, desktop public key, candidate Direct/Relay
   endpoints, protocol version, a one-time challenge, and an expiry.
4. The QR or deep link carries the trust anchor in a URL fragment or an
   equivalent local mechanism so a Web server request or log cannot capture it.
5. The phone generates a persistent device identity used for the device list and
   revoke, plus an ephemeral key per connection for an independent session key.
6. After the offer is atomically claimed, the PC stores the device grant and the
   QR becomes invalid immediately.
7. Both Direct and Relay then authenticate mutually using the desktop identity,
   the device identity, and the ephemeral keys.

A QR must never contain a long-lived bearer token, a private key, a Provider
secret, or workspace information. The one-time challenge is itself a short-lived
sensitive capability: single use, short expiry, and kept out of server access
logs, screenshots, and diagnostics.

### Scan-To-Connect Flow

```text
Management Center → pair new device → choose permissions
  → show a 60–120s one-time QR
  → phone camera or Vibex app scans
  → Vibex app opens
  → tries Direct / mesh / Relay automatically
  → completes E2EE handshake and device registration
  → lands in Sessions
```

The QR should be an HTTPS URL a Universal Link or App Link can intercept, so an
installed app is invoked directly and a missing app lands on the HTTPS WebUI or
install guidance. The fragment is not sent to the hosting site; the client
should clear it from the address bar and history after parsing.

The pairing offer carries at least:

```text
format_version, protocol_version, server_id, server_identity_public_key,
offer_id, one_time_challenge, expires_at, direct_candidates[],
relay_candidate?, granted_permission_scope
```

The offer must be signed by the desktop identity, or `offer_id` must be only an
index into an authoritative desktop-side grant record. The server must not trust
permission fields echoed by a client: the permission summary in the QR is for
display and integrity checking, and editing it cannot escalate privilege.

Because the desktop user explicitly started pairing and chose a scope, that is
the authorization act; scanning does not force a second confirmation, only a
"new device connected" notice. A high-security policy may optionally require the
desktop to confirm the device name and fingerprint before activating the grant.

A revoked device, a reset desktop identity, or an invalid grant requires a new
scan. Moving between Direct and Relay must not.

### E2EE And Replay Protection

- A distinct session id and derived keys per connection.
- An independent monotonic counter or strict nonce tracking per direction.
- Immediate rejection of duplicates, out-of-window reordering, and nonce reuse.
- A handshake transcript bound to protocol version, endpoint, and both
  identities.
- Key rotation and proactive disconnect of live connections after a revoke.
- Mature protocols and audited cryptography libraries instead of hand-assembled
  constructions.

### Server-Side Defenses

- Listen on loopback by default; enabling LAN access is an explicit action.
- Validate Host, Origin, and CORS to prevent DNS rebinding.
- Direct must use a controlled TLS/same-origin scheme or an authenticated
  encrypted application-layer connection.
- Browsers cannot set an arbitrary `Authorization` header on a WebSocket. Use a
  one-time WS ticket, an HttpOnly cookie, or a controlled
  `Sec-WebSocket-Protocol`. Long-lived tokens never go in a URL.
- Check permissions per operation on the server. UI capability is not a security
  boundary.
- Canonicalize file paths and enforce workspace policy and symlink checks.
- Each device has its own permissions, last-seen, audit trail, and one-click
  revoke.
- Relay stores no plaintext. Offline push may retain only minimal
  non-decryptable notification material.

## Cutover State

The former React/Tauri desktop tree, `packages/ui`, the Tauri command adapter,
the React WebUI, and the legacy Capacitor wrapper are retired. They exist only
in Git and release history. They are not workspace members, rollback source
paths, or implementation templates.

- New Web/mobile code must not import, copy, or run old React UI, Tailwind or
  shadcn composition, old TypeScript transport, or old CSS.
- `apps/mobile` packages only the current `apps/web/dist`.
- GPUI tokens are not generated from a legacy CSS file.
- Product rollback uses a published release artifact plus a compatible server
  and data backup. It never restores a second buildable UI source path on
  `main`.

Any retained document or scenario that still speaks of React, Tauri, Zustand,
TanStack Query, or localStorage migration is pre-cutover evidence unless it
names a current GPUI replacement.

## Acceptance Criteria

Section ids below are stable: `scripts/check-cross-platform-release-gate.mjs`
and `docs/release/cross-platform-release-gate.json` map release gates to them.
Renumbering these headings breaks release traceability.

### 16.1 Design Consistency (设计一致性)

- Desktop, wide Web, and phone use the same GPUI tokens, icons, and domain
  components.
- The visual baseline for new UI comes only from GPUI Desktop.
- Wide Web is explainably consistent with GPUI Desktop at the same viewport.
- Compact is re-layout and reduced density, not a second brand or component
  style.
- 360×800, 390×844, 768×1024, 1200×800, and 1440×900 all have no unreachable
  action.
- No hover-only primary action; touch target size, safe area, and soft-keyboard
  behaviour pass real-device acceptance.

### 16.2 Mobile Core Loop (移动核心闭环)

1. The desktop shows a short-lived QR; after scanning, the phone connects to the
   PC with no manually entered address or token, choosing Direct or Relay
   automatically.
2. View and create sessions, send an Agent message, and receive a live timeline.
3. Receive, understand, and resolve an approval request.
4. View files and diffs, and save a light text edit safely.
5. status/diff, stage/unstage, and commit behind a second confirmation.
6. Create or attach a Terminal, exchange live input/output, and recover after a
   disconnect.
7. View Agent/Provider, Relay, and device state.

### 16.3 Network And Consistency (网络与一致性)

- Direct and Relay use the same client API and protocol.
- LAN, private-mesh Direct, and user self-hosted Relay each have repeatable
  connection acceptance. An official Relay is explicitly outside the matrix.
- When a mesh cannot connect directly and internally falls back to its own
  relay, Vibex still reports Direct semantics.
- Self-hosted Relay `/health`, `/api/info`, protocol-incompatible, and
  unreachable states all produce explicit feedback.
- Wi-Fi/cellular switches, app foreground/background, and PC sleep-resume all
  reconnect correctly.
- A cursor gap triggers catch-up or authoritative resync and never silently
  drops events.
- An unconfirmed mutation is not executed twice because of a reconnect.
- With several clients connected, file revision, Git, and Terminal state stay
  explainable.
- When the PC is offline the UI states plainly that operations cannot run; Relay
  does not pretend the workload is still available.

### 16.4 Security (安全)

- Relay can neither read nor modify business content.
- A QR contains no long-lived token and no private data.
- A pairing offer cannot be reused after success, expiry, or cancellation, and a
  duplicate claim must fail.
- Replay, tampering, a wrong device identity, and a revoked device are all
  rejected.
- Host/Origin/DNS-rebinding, path traversal, and symlink escape have automated
  tests.
- Git commit, approvals, and Terminal writes all leave a device audit record.

## Risks

| Risk | Impact | Handling |
| --- | --- | --- |
| GPUI Web platform is still evolving | API or behaviour instability | Pin revisions; spike on real devices first |
| WebGPU and WebView differences | Some phones cannot start | Maintain a device matrix and an explicit unsupported page |
| Canvas IME and accessibility | CJK input or assistive tech limited | Treat as a release gate, not late cleanup |
| Shared-view extraction regresses desktop | Core product degradation | `NativeBackend` first, then desktop visual regression |
| Scaling the desktop layout to phone | Poor usability | Shell-level re-layout with touch and keyboard design |
| Hand-rolled Relay/E2EE | Security and reliability risk | Mature protocols and audited crypto libraries |
| QR or deep-link leakage | Unauthorized device claims first | Short-lived, single-use, fragment-carried, locally cleared, audited |
| Treating mesh reachability as authorization | Other devices on the private network control Vibex | Device pairing enforced on every path |
| Self-hosted Relay misconfiguration | Cleartext entry, disconnects, version mismatch | HTTPS/WSS, `/api/info`, deployment smoke, explicit diagnostics |
| Personal Relay used as a public service | Insufficient quota, isolation, scaling | State the personal-scale boundary; harden separately |
| Concurrent multi-client mutation | Duplicate execution or overwrite | Revision, CAS, idempotency, audit |

Open engineering questions that data — not this document — must settle: the
minimum supported browser, Android WebView, and iOS versions; the final
breakpoints and the Terminal performance budget on low-end devices; when the
in-memory Relay needs persistence or horizontal scaling; the priority of PDF,
Office, and embedded Web Preview; and whether the 1 MiB mobile text-edit limit
needs to adapt per device.

## Evidence Map

All paths are in-repo. This document must not cite absolute local paths or
third-party working copies.

| Area | Source |
| --- | --- |
| GPUI Desktop | `apps/desktop` |
| Desktop theme wiring | `apps/desktop/src/theme.rs` |
| Desktop responsive wiring | `apps/desktop/src/responsive.rs` |
| Shared shells and design system | `crates/vibex-ui/src` |
| Design tokens | `crates/vibex-ui/theme/tokens.json`, `scripts/generate-tokens.mjs` |
| Backend capability traits | `crates/vibex-backend/src` |
| Remote client | `crates/vibex-remote-client/src` |
| Platform-neutral projections | `crates/desktop-model` |
| Desktop runtime | `crates/desktop-runtime` |
| Remote DTOs | `crates/core/src/remote.rs` |
| Remote router | `crates/remote/src/lib.rs` |
| Remote wire contract | `docs/remote/protocol-v2.md` |
| Relay protocol and crypto | `crates/relay/src/lib.rs` |
| Relay server | `apps/relay-server` |
| Desktop Relay client | `crates/desktop-runtime/src/relay.rs` |
| Self-hosted Relay deployment | `deploy/relay` |
| Relay local smoke | `scripts/smoke-relay-local.mjs` |
| Relay NAT/mobile smoke | `docs/smoke/relay-nat.md` |
| Remote LAN smoke | `docs/smoke/remote-lan.md` |
| Terminal | `crates/terminal/src/lib.rs`, `crates/vibex-terminal-ui` |
| Browser support gate | `docs/platform/wasm-browser-gate.md`, `docs/platform/support-matrix.md` |
| Migration contract | `docs/migration/wasm-ui-contract.md` |
| Release gate | `docs/release/cross-platform-release-gate.json`, `scripts/check-cross-platform-release-gate.mjs` |

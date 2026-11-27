# Architecture Baseline

This document is the executable architecture contract for the current Vibex
products. It describes source-backed behavior, not a migration plan or a
historical client.

## Product Surfaces

Vibex has three clients and two transport services around one authoritative
runtime core:

| Surface | Source | Responsibility |
| --- | --- | --- |
| Native desktop | `apps/desktop` | Full GPUI workbench over the authoritative `DesktopRuntime` |
| Headless server | `apps/server` | The same `DesktopRuntime` as a daemon (`vibex-server`) for cloud deployment |
| Native mobile | `apps/mobile-nsh` | iOS/Android Native Shell client: platform views over the same remote projections |
| Self-hosted Relay | `apps/relay-server` | Encrypted frame transport only |

The Native Shell client replaced the GPUI mobile client, which was removed from
the tree. It is a client of the same authority and the same projections and
owns no state. Anyone still running the removed build gets this one as an
in-place update: it keeps that client's application id, bundle ids and signing
key, and imports its data on first launch.

## Authority Invariant

The **runtime** is the only state authority. The runtime is the
`DesktopRuntime` core process regardless of its frontend: a desktop app on
the user's PC, or a headless `vibex-server` on a cloud host. Clients —
desktop UI, mobile, any future web surface — never start an Agent, open a
local workspace, own a PTY, mutate local Git, or store provider
configuration. They request typed operations over Remote v2 and render
authoritative responses.

This is a deliberate extension of the original "desktop is the only state
authority" rule: the cloud server occupies the same authority seat the
desktop occupies on a PC, and a connected desktop becomes a client exactly
like the phone. One runtime owns one Vibex home and one database; the
deployment is single-user by design. Relay stays transport-only and never
becomes a second authority or database.

## Ownership Invariants

1. The runtime process (`DesktopRuntime`, desktop or headless) owns Agent
   lifecycle, session timelines, files, Git, PTY, provider configuration,
   device permissions, audit records, and durable local state. `apps/server`
   is the headless entry point of that same core: it consumes
   `DesktopRuntimeConfig::headless_from_environment` and adds a daemon
   lifecycle (signals, CLI) but no second state model.
2. `crates/core` owns serialized domain ids, DTOs, errors, capabilities, and
   protocol contracts. It has no GPUI, database, filesystem, or provider
   implementation dependency.
3. `crates/desktop-model` owns framework-neutral timeline/session projections and
   reducers. It does not open sockets or call runtime services.
4. `crates/vibex-backend` owns the provider-neutral capability facade. Native and
   remote adapters implement the same domain traits.
5. `crates/vibex-ui` owns semantic tokens, portable component models, workflow
   controllers, and shell composition. Views do not own transport or authority.
6. `crates/vibex-remote-client` owns pairing, reconnect, synchronization, and
   Direct/Tailnet/Relay transport. Relay never becomes a second database.
7. `crates/vibex-nsh-ui` owns the Native Shell UI layer (tokens, styled controls,
   screens). It depends on `nsh-core` and `nsh-base` only, never on GPUI, and it
   holds no transport or authority: the app's host seams supply both.
8. `crates/vibex-mobile-pairing` owns the phone's pairing core: entry
   classification, credential claims, the credential schema and the on-device
   store. It is platform-neutral (no GPUI, no nsh) and shared by the mobile
   clients; it never becomes an authority, only a client that claims a grant.

## Session Presentation Contract

Desktop is the visual and information-architecture source for Agent sessions.
Mobile projects that same GUI contract into a compact composition:

- user messages and Agent Markdown are rendered as timeline content;
- tool/process details are explicit, bounded, and collapsible;
- permission requests are prominent cards with approve/deny actions;
- elicitation requests remain typed GUI forms with explicit submit/decline;
- the composer supports send, stop, and continue according to authoritative
  session state;
- an empty desktop session list exposes typed new-session creation using a
  desktop-owned workspace and published Agent runtime;
- reconnect and loading states remain visible and never fabricate local history.

Mobile may use a session drawer, edge swipe, sheets, reduced density, and a
restrained dark palette. It must not turn the Agent session page into a
terminal-first workflow, copy a separate domain component family, or silently
drop approval/error states.

## Native Platform Boundary

### Native Shell (nsh) mobile client

`apps/mobile-nsh` is a Rust app for Native Shell (`nsh`): `nsh.json` names this
crate's root component, and `nsh` mounts each platform's own views (UIKit on
iOS, Android views on Android) over the engine. The UI layer is
`crates/vibex-nsh-ui`; the app crate holds only the root, the host seams and the
theme generation.

- The palette is generated into `apps/mobile-nsh/theme.json` from
  `crates/vibex-ui/theme/tokens.json`, so both clients read one token source.
- Native behavior is the point of the move: text input, selection, scrolling,
  navigation and the keyboard are the platform's. Screens keep the phone's
  information architecture, copy and spacing ladder, not pixel-identical
  rendering.
- The app stays a client: camera, clipboard, discovery and the remote transport
  arrive through the host seams, never as local authority. Attachments are the
  exception that proves it: `nsh`'s own picker (`pick_images`, `pick_files`)
  hands the app bytes, which travel to the authoritative runtime as `data:`
  URLs rather than being read from a phone path.
- The paired workspace keeps the conversation mounted between full-width
  Sessions and Workbench pages. Page and tab selection are local presentation
  state. Disclosing a page does not replace the active session or its draft.
  Native modals get back before the enclosing page; vertical scroll remains
  independent of horizontal page drags.
- `SidebarOrganizationView` and `FileTreeProjection` drive the sidebar and file
  tree. The UI receives display rows and sends intent; sidebar mutations use
  the holder's revision, and directory reads are scoped to the current
  workspace and load generation.
- Files are edited with `nsh`'s `Editor` (highlighting in the language the
  runtime reports) and written with a revision guard; Git status, staging,
  commit, revert, commit detail and branch checkout call the same backend
  contracts the desktop workbench uses.
- The platform names the phone for pairing (`NshHost.nativeSetDeviceName` /
  `vibex_mobile_nsh_set_device_name`), and packaging is
  `apps/mobile-nsh/scripts/build-android-host.sh` and `build-ios-host.sh`, with
  `package-ios-unsigned-ipa.sh` producing the unsigned device IPA.
- The UI layer is written to be reusable if the desktop moves to `nsh`'s GPUI
  backend; it must not assume a phone-only composition.

### Host boundary

`apps/mobile-nsh` is the only mobile client. The Java and Swift hosts are thin
adapters over what the runtime cannot reach, and everything they own is a seam
the app calls back through:

- Android enters through `com.longbridge.nsh.MainActivity`, a `singleTask`
  Activity that hosts the `nsh` view. It owns the camera capture and the QR
  scanner, the connection foreground service, LAN discovery, the share intake
  and the APK install hand-off; each one reports back through a
  `NshHost.native*` method. Every class that declares one of those methods loads
  `libnsh.so` itself, because a restored scanner Activity can run before the main
  Activity.
- iOS enters through `AppDelegate`, which mounts the host view, routes
  `vibex://share`, registers the quick action and schedules local notifications.
  `HostBridge` keeps the connection through a `beginBackgroundTask` window; iOS
  has no foreground service.
- Both hosts keep the released application identity so a phone still running the
  removed client takes this one as an update. `apps/mobile-nsh/scripts/check-upgrade.mjs`
  pins the ids, the versionCode scheme, the iOS build number and the legacy file
  names; changing one is a release decision, not a rename.
- `nsh` reports safe areas and keyboard insets to the app as root geometry. The
  hosts do not compute content rectangles, and no device-specific notch or
  keyboard constant belongs in a screen.

The checked-in project definitions live under `apps/mobile-nsh/android` and
`apps/mobile-nsh/ios` (generated from `ios/project.yml`). Built libraries,
Xcode projects and APKs are build outputs and are never source dependencies.

## Remote Data Flow

```text
DesktopRuntime
  -> RemoteGateway (typed v2 HTTP/WebSocket API)
  -> Direct/Tailnet or encrypted self-hosted Relay
  -> AutoRemoteTransport
  -> WebRemoteBackend
  -> AgentWorkflowController / shared UI projections
  -> native mobile platform views (nsh)
```

Zero-configuration LAN pairing is a separate bootstrap path before that flow:

```text
Mobile DNS-SD discovery
  -> temporary Desktop HTTP listener
  -> DirectionalV2 application-encrypted request/status/claim
  -> numeric LAN address + encrypted-session-bound TLS certificate
  -> persistent Desktop pinned-TLS LAN Gateway
  -> normal Remote Data Flow above (Tailnet/Relay remain optional)
```

The temporary plaintext listener remains pairing-only, does not expose the
RemoteGateway business router, and is never persisted as a candidate. The
long-term LAN path is a separate HTTPS/WSS Gateway whose certificate is pinned
by the encrypted pairing transcript. Desktop persists and restores this local
network listener independently of Direct/Tailnet/Relay publication. Desktop UI
presents discovery as a separate pairing entry with its own permission,
lifetime, SAS approval, and stop state; the three publication method rows do
not each receive a nearby-device control.

Pairing is one-time and server-issued. The mobile app validates the offer,
claims exactly one route, persists the minimum credential bundle in its sandbox,
and keeps session/private keys in memory. Direct candidates and Relay routes are
validated before use; authentication, protocol, permission, and identity failures
are terminal rather than automatic route-reselection triggers.

The server timeline and sequence cursors are authoritative. On reconnect, gap,
queue overflow, or session-epoch change, the client refetches before applying
new live events. Unknown mutations are resolved by an idempotency query and are
never replayed blindly.

## Cloud Deployment Flow

`vibex-server` reuses the identical gateway, so the remote flow is the same
with the cloud runtime in the authority seat:

```text
vibex-server (DesktopRuntime, Headless mode)
  -> RemoteGateway (identical /ws/v2 + /api/v2 API)
  -> HTTPS/WSS (public mode: trusted proxy TLS or server certificate)
  -> AutoRemoteTransport
  -> WebRemoteBackend
  -> desktop UI / mobile GPUI clients
```

Headless pairing replaces the desktop SAS approval surface: the server prints
a single-use numeric pairing code (only its SHA-256 hash is stored), the
client submits it in the bounded JSON body of
`POST /api/v2/pairing/code/claim`, and the claim issues exactly one device
grant bound to a client-generated identity key. The code never travels in a
URL, the route is rate-limited, and reuse/expiry returns structured errors
with an audit record. Security posture for Public mode is enforced at
configuration validation, not at request time: the gateway refuses to boot
without trusted TLS, an explicit non-loopback host allowlist, and forwarded
headers only behind a trusted proxy. Deployment artifacts live in
`deploy/server/` (Docker, compose, Caddy, systemd) and the protocol contract
in `docs/remote/protocol-v2.md`.

The desktop workbench participates in both seats. Natively it fills the
authority role through `NativeBackend`; paired with a headless runtime it
drives the identical facade through `WebRemoteBackend` (the "Remote Runtime"
settings page, credential store under the desktop home). Every Config Center
section — Agents, provider settings, MCP/Skills/Prompts/Hooks, scheduled tasks,
automation and recovery (diagnostic export plus database
backup) — runs through that facade, so the remote runtime serves it; the
recovery artifacts themselves are written where the runtime data lives and the
client displays the reported path. Agent account authentication,
including the legacy per-Agent sign-in and its stored environment credentials,
travels the same way.

Operations that genuinely belong to the machine in front of the user stay
local and degrade with explicit messages instead of silently failing: the
desktop's own self-update, remote-access connectivity/pairing setup together
with the paired-device management that shares its dialog, and reading a stored
provider credential back into a form (a paired runtime never returns stored
secrets, so the field opens empty and a typed value replaces
it). Live updates arrive through the same `BackendEvent` pump mapped onto the
desktop event pipeline.

Session creation, managed-worktree lifecycle, the usage view, and the live
per-session token snapshot also resolve on the authority: a reserved session id
and a deferred runtime materialization travel in the create request, the
worktree lifecycle and its destructive preflights ride the gateway's Git
operations, and both usage reads are served by the runtime's usage service.
Nothing in that list needs the desktop shell, so the headless seat answers
exactly like a desktop authority.

Operations that do need the desktop shell stay client-side: local CLI-history
scan/import, the client's own storage usage and cleanup, and the Relay
status card.

Terminals follow the same rule through a transport seam: `TerminalTransport`
has an in-process implementation over `TerminalManager` and a remote one over
the gateway's terminal operations, and the workbench, composer and Agent
sign-in surfaces render whichever authority owns the PTY.

Three composer/Agent conveniences resolve on the authority too, because each
depends on state that only the authority owns: composer command discovery
(the Agent catalogue, `@` file references and `$` Skills) is one
`discover_composer_commands` read, Agent-owned model catalogues are discovered
by the bridge that lives with the Agent, and the legacy profile-scoped Agent
sign-out releases credentials where they are stored.

Command execution follows the same rule: a resolved composer command is one
`execute_composer_command` mutation, so a paired client never runs a command
against a runtime it does not own.

The live per-session token-usage snapshot and the aggregated usage view are
served by the runtime's usage service over Remote v2, so the indicators update
against a paired runtime exactly as they do locally.

Provider credentials belong to the runtime database. In cloud mode the
operator's own host stores them, which is the accepted single-user threat
model; multi-tenant hosting remains out of scope.

## Layout And Design

The shared token source is `crates/vibex-ui/theme/tokens.json`; generated Rust
constants are derived and checked. Desktop owns Wide/Medium/Compact information
architecture. Mobile uses the Compact composition as its primary surface and
borrows Medium behavior only where the viewport permits it.

The mobile visual baseline uses a restrained dark treatment: near-black
background, subtle elevated surfaces, small radii, compact typography, clear
secondary text, and explicit icon actions. This is a visual treatment only;
all Vibex session labels, timeline states, approval semantics, and remote
workflows remain desktop-compatible.

Touch actions must be explicit and usable without hover. Fixed-format controls
have stable dimensions. Loading, empty, streaming, failure, reconnecting,
approval-pending, and destructive-confirmation states are all first-class.

## Dependency Direction

```text
apps/desktop    -> NativeBackend -> typed domain services -> crates/core
apps/mobile-nsh -> remote facade -> RemoteGateway -> DesktopRuntime
Relay           -> encrypted frames only
```

No shared UI module may import a runtime composition root, database, socket,
provider SDK, or platform host. No mobile module may call a desktop service
directly or introduce a second authority.

## Security And Privacy

- Native mobile credential files are written atomically with restrictive Unix
  permissions where supported; malformed or mismatched records are discarded.
- Auth tokens, private keys, pairing links, prompt bodies, file contents, and
  terminal bytes are redacted from `Debug`, logs, and evidence.
- RemoteGateway validates Host/HTTP2 authority and HTTP(S) Origin boundaries and
  rejects secrets in URLs. Published LAN mode requires a trusted HTTPS/WSS proxy;
  the local-only Gateway requires an encrypted-pairing-bound certificate pin and
  a loopback/private/link-local numeric address.
- The only non-HTTPS LAN exception is the bounded zero-configuration pairing
  listener. After its plaintext hello, every request, status, offer challenge,
  claim, and grant is protected by a DirectionalV2 application-encrypted
  session bound to the Desktop X25519 identity and a fresh mobile ephemeral key.
  Mobile bypasses proxies for this listener, which disappears on every terminal
  lifecycle.
- The long-term local Gateway uses a stable certificate derived from the Desktop
  identity, exposes the normal typed v2 business router over TLS only, and is
  accepted by mobile only through the exact stored certificate. Disabling
  hostname checks without a single-certificate trust store is forbidden.
- Relay forwards encrypted payloads and has no provider, workspace, or Agent
  authorization logic.

## Release Packaging Check

- Linux AppImage work must identify libraries that belong to the host graphics
  ABI and validate the generated artifact on a different graphics stack. Follow
  the [Linux AppImage Host Graphics ABI](../backend/quality-guidelines.md#scenario-linux-appimage-host-graphics-abi)
  contract.

## Required Gates

```bash
pnpm check:licenses
pnpm check:mobile-native
cargo test -p vibex-ui --locked
cargo test -p vibex-mobile-nsh --locked
pnpm smoke:relay:local
```

Android and iOS builds are separately qualified on hosts with their SDKs and
devices. A host-side Rust check does not claim physical rendering or signing.

# Vibex Mobile on Native Shell

The Vibex mobile client, rebuilt on [Native Shell](https://nsh.dev) (`nsh`).
It replaces `apps/mobile`'s GPUI view layer screen by screen; the rest of
`apps/mobile` keeps shipping until the last screen moves.

## Why

`apps/mobile` draws its whole surface with GPUI (`gpui-pre-mobile`), including
text input, selection, scrolling and navigation. On a phone that means the app
reimplements what the platform already does, and the result feels foreign:
caret and selection handles, the keyboard, scroll physics and the back gesture
are drawn by us instead of by iOS and Android.

`nsh` mounts each platform's own views — UIKit on iOS, Android views on
Android — over a Rust engine. The same Rust crate runs on both, and the
platform owns text, input, selection, scrolling, accessibility and navigation.
That is the reason for the move; it is not a dependency swap.

The desktop stays on GPUI (`gpui-fast`). `nsh`'s GPUI backend draws with the
same crate, so the desktop can embed these screens later if it moves, but it
does not today.

## Layout

| Path | What |
| --- | --- |
| `apps/mobile-nsh` | The `nsh` app: `nsh.json`, the root component, the pairing service, host seams, theme generation |
| `crates/vibex-nsh-ui` | The shared UI layer: theme, styled controls, screens |
| `crates/vibex-mobile-pairing` | Platform-neutral pairing: entries, claims, credentials, the on-device store |
| `theme.json` | The palette, generated from `crates/vibex-ui/theme/tokens.json` |

`crates/vibex-nsh-ui` depends only on `nsh-core` and `nsh-base`; it does not
link GPUI. The screens are written to be reusable on the desktop when it
moves, so they take the platform's behavior instead of drawing their own.

## What is there today

The pairing page, which is the phone's front door: the wordmark, the saved
runtimes list, the three ways in (scan the QR code, find a runtime on the same
network, paste a link) and the manual and nearby sheets.

The page is backed by a real pairing service:

- entries (a `vibex://` link, a server connection string, or a code plus the
  server address) are classified and claimed on a background task through
  `crates/vibex-mobile-pairing`;
- a claimed credential is stored in `pairing.json` in the app's data directory
  (owner-only, written atomically) and appears in the saved list, which probes
  each runtime's reachability;
- paste reads the system clipboard through `nsh`.

The camera and local-network discovery are still host seams: they say so on
the page instead of pretending to work. The host integration fills them in
with the scanner activity and the discovery bridge.

### The session shell

A successful claim or a tapped saved runtime opens the session shell:

- the drawer lists the runtime's sessions (`list_sessions`) with their Agent,
  age and state, and opening one reads its timeline;
- the transcript is `vibex-desktop-model`'s projection over the fetched page,
  so a row means the same thing it means on the desktop: user bubbles, Agent
  prose, reasoning, tool cards, notices and errors;
- the composer sends through the session's own runtime selection and reads the
  transcript back (`send_message` then `fetch_timeline`);
- the runtime's event stream is followed for as long as the shell runs: a live
  timeline item is applied to the same projection, so a streamed reply grows
  in place, and session state changes update the drawer;
- Agent messages render Markdown through `nsh`'s `TextView`: tables, code
  blocks, lists and links natively, with a streaming reply's open syntax
  repaired while it arrives;
- a pending permission is a card above the composer: the title says what is
  asked, the lines say what it would do, and every answer the runtime allows
  is a button. Answering sends the resolution as a human reply and re-reads
  the transcript.

Question forms (elicitation), attachments, the composer's commands and
@-mentions land with their own passes. The shell's tests mount it over a
fixture; the live and answer paths are compile-verified and use the same
projection and backend types the desktop tests cover.

## Run it

Install the `nsh` CLI and use a Rust toolchain (see
[nsh.dev/docs/installation](https://nsh.dev/docs/installation)):

```sh
cd apps/mobile-nsh
nsh run --target ios        # iOS simulator
nsh run --target android    # emulator or device
nsh run --target catalyst   # Mac Catalyst window
```

The iOS/Android hosts come from `nsh`'s generated runners for a Rust app.
Shipping in the existing Kotlin/Swift shells (which own notifications, the
foreground service, the scanner and sharing) is the embedding path: the
platform crates mount this crate's root in an `AppView`/`NshView`, and the
host keeps its native integrations. That wiring lands with the transport
migration; see "Next" below.

## Tests

```sh
cargo test -p vibex-mobile-nsh
```

Two layers:

- **Theme sync**: `theme.json` matches the token source, and `nsh`'s real theme
  parser reads every token (including translucent and custom ones).
- **Headless flows**: the root is mounted with `nsh`'s headless driver, with no
  simulator: the pairing page shows its three entries, the link sheet opens and
  its connect action stays disabled until there is something to claim, a bare
  code without an address is refused with the copy that says what is missing,
  and the nearby sheet opens. The session shell's flows are covered too: the
  drawer lists sessions, switching one swaps the transcript, a streaming
  Markdown reply renders, the composer appends what was sent and clears, and a
  permission card is answered and leaves. Each migrated screen adds the flows
  its GPUI pair has.

`cargo test --workspace` and `cargo clippy --workspace -- -D warnings` in CI
cover the new crates as they do the rest of the workspace.

## Embedding in the existing shells

The `nsh` CLI's generated runners do not carry Vibex's native integrations.
The app therefore mounts into the Kotlin and Swift shells that already own the
notifications, the foreground service, the camera and the discovery bridge.

**Android** (Kotlin; the symbol names match this class):

```kotlin
package ai.vibex.mobile.nsh

object NshHost {
    init { System.loadLibrary("vibex_mobile_nsh") }
    external fun nativeRunRoot(handle: Long)
    external fun nativeScanResult(value: String)
    external fun nativeDiscoveryEvent(value: String)
}

val view = com.longbridge.nsh.NshView(this)
container.addView(view, LayoutParams.MATCH_PARENT)
val handle = view.javaClass.getDeclaredField("handle")
    .apply { isAccessible = true }
    .getLong(view)
NshHost.nativeRunRoot(handle)
```

The `handle` field is the `NshView`'s native handle that nsh documents for
custom Rust hosts; reading it is the only package-private access involved.

**iOS** (Swift, with the symbols declared in the bridging header):

```swift
let app = vibex_mobile_nsh_attach(container)
vibex_mobile_nsh_scan_result(payload)          // a scanned pairing link
vibex_mobile_nsh_discovery_event(json)         // one discovery event
vibex_mobile_nsh_detach(app)                   // when the shell goes away
```

The camera and the network browser stay in the host. Their observations come
back through the two event calls; discovery uses this JSON contract:

```json
{"kind":"candidate","key":"desktop-1","name":"Studio"}
{"kind":"removed","key":"desktop-1"}
{"kind":"permission_denied"}
{"kind":"failed","message":"…"}
```

Still missing from the bridge: the host has no way to be *asked* to open the
camera or start discovery, so the page's Scan and Find entries still say they
are not wired. That outbound half (a callback registered by the shell at
attach) is the next host step, together with the platform device name.

## Theme

`theme.json` is generated; do not hand-edit it. After a token change in
`crates/vibex-ui/theme/tokens.json`:

```sh
cargo run -p vibex-mobile-nsh --bin gen-theme
```

`cargo test -p vibex-mobile-nsh` fails when the checked-in file is stale.

The phone's spacing, radius and type ladder live in `crates/vibex-nsh-ui/src/theme.rs`
and come from `apps/mobile/src/theme.rs`. The palette is shared with the
desktop through the token source.

## Parity contract

The migration keeps the phone's product decisions while the platform owns
behavior:

- **Kept 1:1** — information architecture, page structure, copy, palette,
  spacing rhythm, icons, states and what every action does.
- **The platform owns** — text metrics and line breaking, scroll physics, the
  keyboard, selection menus, system sheets and navigation transitions,
  accessibility. These will look native, not pixel-identical; that is the
  point of the move.
- **Reimplemented** — sheet drag feel, toasts and motion curves keep their
  rhythm, not their exact frames.

A feature is migrated only when every entry point it has today is reachable and
does the same thing. Screens are compared against screenshots and recordings of
the shipping GPUI client, taken before the screen moves.

## Next

1. Question forms (elicitation), attachments, and the composer's commands and
   @-mentions.
2. The outbound half of the host bridge (asking the shell for the camera and
   discovery), the platform device name and the release pipeline.
3. Workbench: files, Git and the terminal (nsh's `Editor`, `DiffView` and
   `Terminal`, the latter able to drive the `alacritty_terminal` state the
   client already keeps).

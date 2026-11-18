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
| `apps/mobile-nsh` | The `nsh` app: `nsh.json`, the root component, host seams, theme generation |
| `crates/vibex-nsh-ui` | The shared UI layer: theme, styled controls, screens |
| `theme.json` | The palette, generated from `crates/vibex-ui/theme/tokens.json` |

`crates/vibex-nsh-ui` depends only on `nsh-core` and `nsh-base`; it does not
link GPUI. The screens are written to be reusable on the desktop when it
moves, so they take the platform's behavior instead of drawing their own.

## What is there today

The pairing page, which is the phone's front door: the wordmark, the saved
runtimes list, the three ways in (scan the QR code, find a runtime on the same
network, paste a link) and the manual and nearby sheets. The actions are host
seams: they log today, and the transport integration fills them in next.

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
  its connect action stays disabled until there is something to claim, and the
  nearby sheet opens. Each migrated screen adds the flows its GPUI pair has.

`cargo test --workspace` and `cargo clippy --workspace -- -D warnings` in CI
cover the new crates as they do the rest of the workspace.

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

1. Pairing transport: camera, clipboard, LAN discovery and `vibex-remote-client`.
2. The session shell: sidebar, timeline, composer.
3. Workbench: files, Git and the terminal (nsh's `Editor`, `DiffView` and
   `Terminal`, the latter able to drive the `alacritty_terminal` state the
   client already keeps).
4. The Kotlin/Swift host integration and the release pipeline.

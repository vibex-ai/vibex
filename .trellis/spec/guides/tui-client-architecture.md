# TUI Client Architecture

The character-grid client (`crates/vibex-tui`, binaries `vibex`,
`vibex-server tui`, `vibex-desktop tui`) is a **client**, not a second runtime.
These rules are enforced by tests in `crates/vibex-tui/tests/contracts.rs`; a
change that breaks one of them is a design change, not a lint fix.

## 1. Positioning

`crates/vibex-tui` is the fourth client alongside the desktop app, the native
mobile app, and the remote web client. It consumes the same domain traits and
the same workflow controllers, so it cannot develop its own interpretation of a
session, an approval, or a capability grant.

```text
desktop GUI ─┐
             ├─► DesktopRuntime (sole authority) ─► RemoteGateway ─► remote clients
vibex-server ┘          ▲                                              (desktop-remote / mobile / TUI)
                        └── NativeBackend / BackendFacade ──► in-process clients
```

### Hard dependency boundary

| Allowed | Forbidden |
| --- | --- |
| `vibex-core`, `vibex-backend` (traits only), `vibex-desktop-model`, `vibex-ui` (`default-features = false`), `vibex-markdown` (`default-features = false`), `vibex-terminal-ui`, `ratatui`, `crossterm` | `gpui`, `gpui-component`, `vibex-desktop-runtime`, `vibex-db`, `vibex-agent-acp`, `vibex-browser`, `vibex-content` |

The TUI library never starts a runtime and never decides which runtime to talk
to. Both are the composition root's job:

| Root | File |
| --- | --- |
| Standalone client | `apps/tui/src/seat.rs` |
| `vibex-server tui` | `apps/server/src/main.rs` |
| `vibex-desktop tui` | `apps/desktop/src/main.rs` |

## 2. Seats

Exactly one runtime may own a home (`DesktopHomeLock`, flock-exclusive), so a
client is in one of two seats:

| Seat | Reachable when | Consequences |
| --- | --- | --- |
| **Authority** | the home lock is free | the client owns the runtime; native steering, client-local recovery and self-update are available |
| **Remote** | another process owns the home, or an explicit link/code was given | the client is a network peer; steering degrades to interrupt + resend; recovery runs on the authority |

Seat selection is visible in the status bar. When the home is locked and the
runtime is not accepting local clients, the client must print the three ways out
(enable Remote Access → Direct, quit the desktop app, or `vibex connect`) rather
than a bare connection error.

### Local bootstrap trust model

The local-machine bootstrap mints a device credential for the runtime that owns
the home. Its trust anchor is **the filesystem**, not the loopback interface:

1. the runtime identity public key is read from
   `<home>/relay/desktop-identity.json`;
2. a pairing code is minted against the same database the running runtime uses
   (safe because the database runs in WAL mode with a busy timeout, and
   `vibex-server pairing-code` already does exactly this);
3. the claim pins the identity public key, so a different local process cannot
   impersonate the runtime;
4. the credential is written to `<home>/local-clients/tui-credential.json` with
   mode 0600 at creation.

Plain HTTP is only acceptable as an explicit, loopback, bootstrap fallback.
Pinned TLS is preferred whenever the runtime advertises a certificate, and a
pinned route requires HTTPS on a numeric local address — a hostname cannot carry
a pin.

## 3. Threading and event model

One process, two execution domains:

```text
main thread                          tokio worker
  crossterm events                     RPCs through BackendFacade
  pure intent → effect reduction       BackendEventSubscription read loop
  dirty-frame scheduling               terminal frame pump
  mpsc try_recv drain            ◄──── results as AppMessage
```

Rules:

* the main thread never `.await`s;
* the first frame does not wait for I/O — the skeleton is painted first;
* a write does not trigger a full reload; incremental `apply_*` is used;
* a stale result is dropped, never applied (each request carries a generation);
* `BackendEvent::Lagged` forces an authoritative refetch — the client never
  advances a cursor across a gap and never auto-resends a prompt after a
  reconnect.

## 4. Rendering

The transcript is the performance-critical surface and follows four rules:

1. a block caches its rendered lines **and** its height;
2. only dirty blocks are re-measured;
3. a streaming update rewrites exactly one block;
4. blocks outside the viewport are estimated, never rendered.

Measured contract: with no input and no events the loop produces **zero frames**.
Folding happens *before* wrapping, so a collapsed block never pays for the lines
it will not show.

Colour degrades `truecolor → ansi256 → 16 → none`, resolved from
`NO_COLOR` > `VIBEX_TUI_COLOR` > detection > truecolor. Colour is never the sole
carrier of meaning. Icon and border glyphs degrade to ASCII when the locale is
not UTF-8. All width arithmetic goes through `unicode-width`; `chars().count()`
is never a column count.

## 5. Interaction

* One binding table per scope drives **dispatch, the key bar and `?` help**
  simultaneously. They cannot drift.
* `label == None` marks a hidden alias: dispatchable, never advertised.
* Bindings are data, so `~/.vibex/tui-keys.toml` remaps them without a rebuild.
  A malformed file produces warnings and the affected binding keeps its default.
* Uppercase letters fold into lowercase on the event path, so a
  shift-only binding is unreachable. Use a modifier or a function key.
* `Esc` only walks back one level; it never cancels a running turn. `Ctrl+C`
  owns clear-draft / interrupt / quit-confirm.
* Every action has a keyboard path. The mouse is an enhancement only.
* Every page renders through the shared page frame; no page hand-rolls chrome.

## 6. Security

| Rule | Requirement |
| --- | --- |
| R1 | No API key, token, private key or stored provider secret is ever written to TUI configuration, logs, diagnostics or scrollback exports. A one-time pairing code is shown once, on a dedicated surface, and never persisted. |
| R2 | Stored credentials are never rendered back. The credential form is masked, explains that the value replaces the stored one, and clears the value after submission. |
| R3 | Destructive actions carry an audit row on the authority; the client only references ids. |
| R4 | Every mutation carries an `idempotency_key`; a conflict is surfaced as "already executed with different content", never swallowed. |
| R5 | Screen contents and terminal bytes are as sensitive as a prompt: never logged at `info`. |
| R6 | Permission levels (ReadOnly / ApproveOnly / FullControl) only *hide*; authorisation is the server's `remote_permission_denied`. The current level is always visible. |
| R7 | The client never opens the runtime database, except for the documented local bootstrap path. |
| R8 | Non-loopback transport is HTTPS/WSS. Pinned TLS is preferred; plain HTTP only on loopback with an explicit opt-in. |

## 7. Testing

| Layer | What it proves |
| --- | --- |
| Reducer unit tests | the intent → effect mapping is a pure function; key sequences drive state |
| `TestBackend` render tests | layout degrades correctly at 80×24 / 100×30 / 120×40 / 200×50, CJK wraps, colour-less mode still reads |
| Contract tests | dependency boundary, key tables, locale coverage, no secret-shaped copy, docs exist per page |

`cargo test -p vibex-tui` runs all three.

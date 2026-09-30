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
| `vibex-core`, `vibex-backend` (traits only), `vibex-desktop-model`, `vibex-ui` (`default-features = false`), `vibex-markdown` (`default-features = false`), `vibex-terminal-ui`, `ratatui`, `crossterm`, `rustix` (Unix only, descriptor plumbing) | `gpui`, `gpui-component`, `vibex-desktop-runtime`, `vibex-db`, `vibex-agent-acp`, `vibex-browser`, `vibex-content` |

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

### Console ownership

Owning the alternate screen means owning `stderr` too. An authority seat boots
`DesktopRuntime` **in this process**, and the runtime reports startup stages
from background tasks that finish after the first frame is painted; a byte
written there wraps at the last column and scrolls the grid, leaving log residue
under the interface. So the client diverts the process's `stderr` into a spill
file for the duration of the session (`crates/vibex-tui/src/console.rs`), and
hands the terminal's descriptor back on exit.

Rules:

* the diversion is installed **before** `EnterAlternateScreen`, never after;
* it is released by the same `TerminalGuard` that restores the screen, and by
  the panic hook, so a panic message stays visible;
* diverted output is named on exit, never dropped silently — an empty spill
  prints nothing;
* `VIBEX_TUI_LOG` chooses the spill file; the default is
  `$TMPDIR/vibex-tui-<pid>.log`;
* the PTY layer asserts that a byte written while the interface owns the screen
  never reaches the terminal, which is the measured form of this rule.

## 4. Screen composition

The screen is **one vertical stack of full-width bands**, computed by
`crate::layout::compute`:

```text
outer padding (1 row top/bottom, 2 columns each side)
  status band          1 row, always
  [gap] [tasks]        only when background work exists
  [gap] [todo]         only when the session has steps
  [gap]
  transcript           fills; Min(5) rows, the only flexible band
  [gap] [queue]        only when messages are held
  [gap] turn status    while running, waiting, or reporting a live session
  [gap] [banner]       transient messages
  [gap] [dock]         the running-work panel, while it is open
  prompt gap           1 row
  prompt               borders + one blank row + the draft
  shortcut band        1 row, always last
outer padding
```

Rules that make this work:

* **No permanently-boxed side pane.** Every border costs two columns or two rows
  the content does not get; on a fixed grid that is the whole budget. Navigation
  is a full-screen view or an overlay instead.
* **The transcript has no frame.** Its structure is the per-block rail, which is
  inside the content rather than around it.
* **Optional bands collapse to zero height**, never to a smaller size, and the
  frame skips their renderers entirely. `SHORT_TERMINAL_ROWS` drops the banner,
  tasks, todo and dock bands before it touches the transcript or the composer.
* **A band that duplicates another yields to it.** While the dock is open it
  lists the plan and the held queue, so `band_request` gives those two bands
  zero height rather than printing the same rows twice on one screen.
* **`layout::compute` is pure data.** The composition is asserted at every
  terminal size without rendering anything.
* **The renderer owns the transcript's wrap width.** It configures the
  `Transcript` with the band it is about to draw into; a width derived from a
  pane layout instead (a sidebar and a details column that the band stack does
  not have) silently wraps prose into a fraction of the screen. The public
  `view::transcript_width` mirrors `compute`, and a test asserts the two agree
  at several sizes.
* **Session-view bands only exist on session pages.** `band_request` gives the
  plan, queue, turn-status and dock bands zero height when the page has no
  session context, so the session list cannot wear the active session's chrome.
* **A list column is measured in cells, never in characters.** Padding a row
  with `{:<width$}` counts characters, so a double-width title overflows its
  column and drags everything after it out of alignment; the session list
  measures its state column once per frame and pads by display width.
* The gutter is taken from the transcript's right edge and only when the
  transcript is at least `MIN_TRANSCRIPT_FOR_GUTTER` wide; below that the two
  columns go back to the prose.
* **The frame publishes what the mouse needs, nothing more.** `FrameRegions`
  carries the transcript rect, a `ListRegion` (rect, scope, row count, first
  line), the composer's text rows, the queue band, the dock, the turn rail's
  ticks, the shortcut band's hints with their intents, the banner and a modal's
  close control. A band that can disappear without a repaint clearing the field
  (the dock) is reset to `None` by its renderer's caller, so a closed panel
  cannot keep a stale hit rect. Every one of them is written by the renderer that drew it, because
  only that code knows where the band landed; `run.rs` does nothing but
  hit-test. A click on a hint runs the intent its key would run, so the mouse
  cannot grow a second, divergent command set.
* **The banner row has one owner.** `BannerPriority` orders the claimants
  (transient < tip < mode < warning); `App::set_banner` refuses to displace a
  higher-priority message, and `refresh_banner` re-derives the row from the
  current conditions once per loop so a message that is no longer true
  withdraws itself. A producer that only assigned the field would let the last
  writer win and the row flicker between unrelated messages.
* **Surfaces inside a band reserve rows rather than overlay.** The pinned prompt
  header (`MAX_STICKY_ROWS` + a gap) and the transcript search bar (a rule plus
  the bar) are subtracted from the transcript band before the viewport is
  positioned, and the renderer publishes what is left as
  `FrameRegions::scrollback`. That rect is the only coordinate space the mouse
  layer knows: a pointer maps to a display line through it, so chrome can never
  be mistaken for content.

## 5. Visual language

Structure is carried by four devices, in this order of importance:

1. **The rail.** Every transcript block owns a one-column bar down its whole
   height, coloured by block role (`ThemeRole::accent_*`, selected through
   `TuiTheme::rail`). It is a filled cell, not a drawn glyph, and it is
   *background*-coloured — a foreground-coloured space is invisible.
2. **The turn rail.** One tick per turn in the gutter, positioned by
   conversation order, with chevrons to jump a turn at a time. It maps the
   session, not the buffer.
3. **Layered surfaces.** `surface` / `surface_raised` / `surface_highlight` step
   away from `background`, so a plane change is visible without a border.
4. **A three-step grey scale.** `gray_dim` for punctuation and chrome, `gray`
   for muted body, `gray_bright` for secondary labels. One grey cannot do three
   jobs without everything competing.

Rules that follow:

* A block is `rail | pad(2) | content | pad(1)`, and the rail covers the block's
  status and attribution rows too, so the block reads as one object.
* A run of three or more collapsed work items folds into its first member
  (`MIN_GROUP_RUN`). An expanded or failed item breaks the run.
* The current block is marked with a pointer in the rail plus a lifted header.
  Never a full-width reversed row: it is the heaviest emphasis a terminal has.
* Focus is expressed as a fade toward the canvas (`TuiTheme::fade`), not as a
  colour switch, so a blurred pane stays recognisable.
* Colour is never the only carrier. When `has_color()` is false the rail becomes
  a drawn glyph and bands disappear.
* **Everything structural comes from `crate::glyphs`**, which declares each
  glyph's fallback and the width invariant it must keep. The prompt arrow is
  always two columns and every spinner frame always one, so a degradation never
  shifts the layout.
* **A band that reports progress derives it from the transcript.**
  `App::todo_progress` reads the last plan-shaped block's `Status: title` lines,
  so the progress bar cannot disagree with the rows the reader can scroll to.
  A counter with no runtime source (background tasks) stays at zero rather than
  guessing.
* **The pinned prompt header is chrome, not content.** Only a user message
  pins; an expanded one does not (it is already fully visible inline). It
  shrinks one row per row scrolled past down to `min(full_height,
  MAX_STICKY_ROWS)`, and the next prompt pushes it off from the bottom rather
  than overlapping it, so the transcript below is never hidden. It is decided
  against a conservatively small viewport so two frames cannot disagree.

### Surfaces that must not regress

| Surface | Contract |
| --- | --- |
| Status band | location on the left, status segments right-aligned as a group joined by ` │ `. A left-aligned list pushes the state off the edge exactly when a narrow terminal makes it worth reading. |
| Turn status | spinner + activity on the left, elapsed and tokens right-aligned. Present whenever a turn is running or the session is alive; idle has its own slower pulse so a connected session does not look busy. |
| Composer | one blank row above the draft; the bottom border *is* the info line, using ` · `; the mode prefix says what the draft will do. |
| Completion | a drawer above the composer: two full-width rules, no corners, count on the top rule, selection marker is the composer's own arrow. |
| Shortcut band | `KEYS:LABEL` joined by a rule; keys bright, labels dim; leading hints survive a narrow terminal. |

## 6. Rendering

The transcript is the performance-critical surface and follows four rules:

1. a block caches its rendered lines **and** its height;
2. only dirty blocks are re-measured;
3. a streaming update rewrites exactly one block;
4. blocks outside the viewport are estimated, never rendered.

Measured contract: with no input and no events the loop produces **zero frames**.
Folding happens *before* wrapping, so a collapsed block never pays for the lines
it will not show.

**Markdown is interpreted, never echoed, and its styling survives wrapping.**
The renderer never prints the syntax it parsed: headings carry emphasis (the top
two levels are underlined as well, so the hierarchy survives a terminal whose
CJK font has no bold face) instead of `#` markers, inline code is a background
run instead of backticks, and links print their destination only when the label
does not already say it. Wrapping is style-preserving: `wrap_text` decides where
lines break, and each visual line is matched back onto the styled runs
character-by-character (a break consumes the space it broke on, so offsets are
not reliable) — re-applying only the first span's style to a wrapped line erases
every emphasis, code and link on it. Content whose spacing *is* the layout —
fenced code, diffs, table rows and rules — takes a preformatted path that
hard-wraps by cell and never collapses runs of spaces, and a table is a closed
box (`┌┬┐ ├┼┤ └┴┘`) whose columns are padded by display width, so a double-width
cell cannot push the next `│` out of line.

Colour degrades `truecolor → ansi256 → 16 → none`, resolved from
`NO_COLOR` > `VIBEX_TUI_COLOR` > detection > truecolor. Colour is never the sole
carrier of meaning. Icon and border glyphs degrade to ASCII when the locale is
not UTF-8. All width arithmetic goes through `unicode-width`; `chars().count()`
is never a column count.

## 7. Interaction

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
* **Every popup renders through `crate::modal`.** One chrome — border, title on
  the top rule, `[✗]`, inner padding, bottom-aligned centered footer hints — and
  one sizing preset per weight class (`palette`, `prompt`, `picker`, `large`,
  `document`, `card`). A surface picks a preset and supplies content; a popup
  that draws its own box is the drift this prevents. `ModalSizing::compact`
  returns the margins on a small terminal, and a terminal too small for the
  minimum clears to the title rather than drawing a broken box.
* **A text-entry state consumes keys before the binding table.** The composer,
  the list filter, the transcript search bar and the settings filter/editor are
  text fields: printable keys edit them, and only `Esc`/`Enter`/arrows are
  commands. Their state lives on `App` and the transitions are reducer methods,
  so they stay testable without a terminal. A modifier chord is never text:
  every printable-key arm excludes `Ctrl` and `Alt`, so a binding such as
  `Alt+B` reaches the table instead of typing a `b`.
* **A paste over the threshold collapses into a chip.** `PASTE_CHIP_LINES` /
  `PASTE_CHIP_BYTES` decide; the buffer's text holds the label and the original
  bytes ride in a `Chip`. A chip is atomic — the cursor steps over it, one
  `Backspace` removes it, and any edit that reaches into one dissolves it into
  literal text so no range can point at the wrong bytes. `take_expanded` puts
  the bytes back on the way out; `text()` shows only the label. Image chips are
  the same object with a different payload: `[Image #N]` is a display label only,
  `expanded_text` drops it, and `take_with_attachments` returns text and pictures
  together because the message is one value. Labels are numbered monotonically
  per draft and never recycled, `IMAGE_CAP` bounds a prompt, and clipboard bytes
  travel as a shared `Arc` so the undo snapshots do not copy the pixels.
* **Clipboard reading is the worker's job, never the reducer's.** `Effect::
  ReadClipboardImage` runs on `spawn_blocking`, shells out to the desktop's
  clipboard tool with a deadline and a kill, and answers with an `AppMessage`.
  The client still *writes* the clipboard only over OSC 52; reading is an
  enhancement that degrades to "name a file instead" when no tool exists.
* **A selection in the draft is byte offsets plus a sticky anchor.** `Shift`
  motions extend it, `Alt+A`/`Alt+C` select and copy it, a mouse press-drag
  selects it, and every mutation consumes it first, so typing replaces it.
  `ComposerBuffer::selection` widens to whole chips: a half-selected chip label
  would otherwise let a cut leave a marker that no longer parses.
* **The key editor refuses a chord another action owns.** Dispatch takes the
  first match in table order, so a duplicate would disable the loser silently.
  The editor names the owner instead, `d` restores a row from `DEFAULT_BINDINGS`
  (rebuilding so collapsed aliases come back) and `s` writes the same grammar
  `Keymap::load` parses. The file remains hand-editable: it is one
  `action_id = "Chord"` line per override.
* **Client-owned arrangement is persisted client-side, keyed by id.** The
  sidebar's pins, manual order and folded groups live in `SidebarState` and are
  written to `~/.vibex/tui-sidebar.json`; the path is an `AppOptions` field so a
  test or a preview can run without touching a home directory. Order is
  reconciled against the *visible* rows, and pinned rows always sort first, so a
  manual move across that boundary is refused with a message instead of
  appearing to do nothing.
* **A draft's mode is derived from its text, not tracked beside it.**
  `sync_composer_mode` reads the first character (`? ` = history search), so an
  undo, a recalled history entry or a paste cannot leave the prefix describing a
  mode the draft is not in.
* **The composer keeps an undo history and one kill buffer.**
  `ComposerBuffer` snapshots after each mutation (`MAX_UNDO`), coalescing
  consecutive typing into one step and breaking the batch on any cursor move —
  undo must remove what was just typed, never move text out from under a cursor
  the reader placed deliberately. Word motions use the `Small` class rule
  (alphanumerics and `_`, punctuation, whitespace), while `Ctrl+W` stays
  whitespace-delimited because that is what a shell does.
* **A sub-mode owns `Esc` before the screen does.** Closing a selection
  highlight, a search bar, a settings chooser or filter happens in `go_back`
  before page navigation, in that order of innermost first. `Esc` means "undo
  the thing I am in the middle of" and only then "go back".
* **Search is a smart-case regular expression** (`crate::search`): lowercase
  folds case, any uppercase letter makes it exact, an invalid pattern matches
  nothing and says `bad pattern`, zero-width matches are skipped. Matches are
  highlighted by splitting the *rendered* line, so markdown that renders away
  cannot shift a match.
* **History pages in both directions, with the cursor as the contract.**
  `FetchTimelineRequest` carries `after_sequence` or `before_sequence` (never
  both; `after` wins), and the response's `has_older` is the only thing that
  authorises another backward request. The controller owns the lifecycle in a
  ticket: it refuses while a page is in flight or older history is exhausted,
  validates that every item of a backward page is older than the cursor — serde
  ignores unknown fields, so a backend that predates the cursor would otherwise
  answer with the newest page — and turns paging off permanently when that
  happens rather than retrying per gesture. A prepend trims from the *newest*
  end at the item budget, because the oldest items are what was just fetched.
* **A prepend anchors the viewport by block id, not by offset.** `ScrollState`
  counts display lines from the top, so inserting a page above the viewport
  moves everything the reader was looking at. `sync_transcript` only re-anchors
  when a prepend actually happened, and it anchors to the block id that was on
  the first visible line.
* **A mouse text selection copies through OSC 52** — the same route as every
  other copy, so no clipboard crate enters the dependency graph. A selection is
  `(display line, column)` in the published transcript rect, extracted through
  `Transcript::plain_lines` (not the viewport) so a drag can exceed the screen,
  and painted by grapheme so a wide glyph is never split.

## 8. Security

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

## 9. Testing

| Layer | What it proves |
| --- | --- |
| Reducer unit tests | the intent → effect mapping is a pure function; key sequences drive state |
| `TestBackend` render tests | layout degrades correctly at 80×24 / 100×30 / 120×40 / 200×50, CJK wraps, colour-less mode still reads |
| Contract tests | dependency boundary, key tables, locale coverage, no secret-shaped copy, docs exist per page |
| PTY end-to-end | the real binary enters raw mode, paints a first frame, writes zero bytes when idle, keeps in-process diagnostics out of the terminal, restores the terminal on exit, survives a resize storm |

`cargo test -p vibex-tui` runs the first three. The PTY layer needs the harness
entry point, so it runs as
`cargo test -p vibex-tui --features pty-harness --test pty`; `pnpm check:rust`
covers both.

The idle assertion is load-bearing: it is the measured form of "no animation and
no events means no frames". A startup notice legitimately repaints while it is
visible, so the test settles first and only then measures.

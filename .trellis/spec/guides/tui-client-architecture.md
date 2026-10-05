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

Draft, request, editor, and transcript ownership follow the
[TUI Session State contract](../frontend/tui-session-state.md). A navigation
change does not transfer an in-flight operation to the new page.

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
* **A message carries the session's own runtime selection.** A send's
  `desired_runtime` is authoritative, so filling it with the client's preferred
  catalogue entry moves the session onto another Agent as a side effect of
  typing into it — the next desktop attach then finds its Agent changed, and the
  abandoned runtime refuses to be resumed. The worker reads the session's durable
  selection (`AgentBackend::runtime_selection`) and only falls back to the
  catalogue — filtered to the Agent the session already records — for a session
  that predates runtime-selection state. The same state rides in the session
  snapshot, so the client can name the Agent and model a message will actually
  go to instead of guessing from `options.first()`.
* **A runtime switch is a compare-and-set, never a blind write.**
  `SetDesiredAgentSessionRuntimeRequest` carries the expected session and
  selection revisions; the durable state is read first and a stale expectation is
  refused. Sending zero revisions is a request that only succeeds on a session
  that has never moved.

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
  never reaches the terminal, which is the measured form of this rule;
* **the keyboard protocol is pushed with the screen and popped with it.**
  `DISAMBIGUATE_ESCAPE_CODES` (plus `REPORT_ALTERNATE_KEYS`, so the first flag
  does not cost shifted text) is what lets a terminal report `Shift+Enter` as
  something other than `Enter`; push and pop are paired, so the next program in
  the pane reads the keys it expects. The push is **optimistic, not probed**:
  the query that would detect support travels through the same input queue the
  interface is about to read, so asking first stalls startup on every terminal
  that answers the companion device-attributes query but not this one, and the
  answer would only change the wording of the key bar.

## 4. Screen composition

The screen is **one vertical stack of full-width bands**, computed by
`crate::layout::compute`:

```text
outer padding (1 row top/bottom, 2 columns each side) — given back first
  status band          1 row while the terminal can hold it
  [gap] [tasks]        only when background work exists
  [gap] [todo]         only when the session has steps
  [gap]
  transcript           fills; Min(5) rows, Min(1) when the terminal is shorter
  [gap] [queue]        only when messages are held
  [gap] turn status    while running, waiting, or reporting a live session
  [gap] [banner]       transient messages
  [gap] [dock]         the running-work panel, while it is open
  prompt gap           1 row while there is room for it
  prompt               padded draft surface + separate runtime info line; shrinks last
  shortcut band        1 row, last — only where the page is navigated, not written on
outer padding
```

Rules that make this work:

* **No permanently-boxed side pane.** Every border costs two columns or two rows
  the content does not get; on a fixed grid that is the whole budget. Navigation
  is a full-screen view or an overlay instead.
* **The transcript has no frame.** Its structure is the glyphs in its own text —
  the prompt mark, the work bullet, the heading colour — and a blank row between
  blocks, rather than a border around them or a rail beside them.
* **Optional bands collapse to zero height**, never to a smaller size, and the
  frame skips their renderers entirely. `SHORT_TERMINAL_ROWS` drops the banner,
  tasks, todo and dock bands before it touches the transcript or the composer.
* **There is no minimum terminal size.** `Chrome::shed` gives rows back as the
  terminal shortens, least useful first: the outer padding, the status row, the
  blank rows that separate the bands, the queue and turn-status bands, the
  shortcut band, and only then the transcript's floor — down to a single line.
  The composer is the last band to shrink, so every size paints the conversation
  and the place to type; a size notice is not something the client has. The
  render tests drive every page down to 1×1 and `run.rs` has no size branch.
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
  The composing page is that case from the other side: it is a session page for
  the *composer* (`is_composing_page`), yet it has no session of its own, so
  `band_request` gates those bands on `page_owns_session` as well — a running
  turn, its elapsed clock, its plan and its held queue belong to the session the
  reader is leaving, not to the page writing a new one.
* **The composer is sized in wrapped rows, not newlines.** A draft that is one
  logical line can be several rows on screen, so `band_request` cannot know the
  prompt's height on its own: `render` computes the frame once to learn the
  band's width, then `composer_height` measures `display_row_count(width)` —
  the same `wrap_source_text` the renderer paints with — and computes the frame
  again. The band is capped at `MAX_COMPOSER_DRAFT_ROWS`; past that the box
  scrolls under the caret and republishes the rows it scrolled as
  `FrameRegions::composer_scroll`, which is what a click adds back to map a
  visible row onto the row the draft wrapped to.
* **The key legend belongs to the pages that are navigated, not written on.**
  `band_request` gives the shortcut band zero height on a session page
  (`is_session_page`: the session view and the composing page), so those pages
  spend their last row on the draft; everything the band would advertise is a
  `?` away, and the composer's own info line names the runtime the band used to
  caption. The session list and the management pages keep it.
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

1. **The left margin.** Two columns, the first of which carries the selection
   pointer. It is a margin, not a bar: a per-block column of colour costs every
   row a column of text, and once a block is one line tall the bars of adjacent
   blocks read as one striped edge rather than as one bar per block. A block is
   identified by what it says — the prompt mark on the reader's own words, the
   bullet on a work item, the colour of the text — not by a stripe beside it.
2. **The turn rail.** One tick per turn in the gutter, in conversation order: a
   count of the session as much as a map of it, with the turn the viewport
   starts on drawn heavier. More turns than rows window the ticks around the
   active one. A transcript with no turns draws nothing — a scrollbar in those
   columns is counted as turns, which is the mistake the rail exists to avoid.
3. **Layered surfaces.** `surface` / `surface_raised` / `surface_highlight` step
   away from `background`, so a plane change is visible without a border.
4. **A three-step grey scale.** `gray_dim` for punctuation and chrome, `gray`
   for muted body, `gray_bright` for secondary labels. One grey cannot do three
   jobs without everything competing.

Rules that follow:

* A block is `margin(2) | content | pad(1)`, and the margin is empty except for
  the pointer: a marker that took a column of its own would shift the block's
  text one column to the right of every other block's.
* **The transcript is dense by default, and a row has to earn its height.**
  A work item or a notice is *one* row: its action and the single detail that
  identifies it (`is_dense_row`), with its body behind the fold and in the block
  detail overlay. Messages keep no "You"/"Agent" header — the prompt mark says
  who is speaking and the reader's own words carry a `❯` on their first line.
  A kind label is never printed when the title already says it. Two dense rows
  are separated by nothing; everything else keeps the gap that makes it an
  object. Plan updates and approval resolutions are not drawn at all: the todo
  band and the dock own the plan, and the request row already shows the outcome.
  Every density decision is mirrored in `estimate_height`, because a renderer
  and an estimator that disagree about shape make scrolling jump.
* A run of three or more collapsed rows with the same kind, action title, turn
  and runtime folds into its first member (`MIN_GROUP_RUN`), which reports `+N`
  before its summary. Expanded and failed rows break the run. Opening the head
  reveals every member; keyboard navigation skips members while folded.
* The current block is marked with a pointer in the margin plus a lifted header.
  Never a full-width reversed row: it is the heaviest emphasis a terminal has.
* Focus is expressed as a fade toward the canvas (`TuiTheme::fade`), not as a
  colour switch, so a blurred pane stays recognisable.
* Colour is never the only carrier. Structure is drawn with glyphs — the prompt
  mark, the work bullet, the disclosure, the heading text itself — so a theme
  without colour loses emphasis, not meaning.
* **Everything structural comes from `crate::glyphs`**, which declares each
  glyph's fallback and the width invariant it must keep. The prompt arrow is
  always two columns and every spinner frame always one, so a degradation never
  shifts the layout.
* **A frame's click regions are per-frame.** A rect describes where something
  was when the frame drew it, so the lists the frame rewrites — the turn ticks
  and the shortcut hints — are cleared at the start of every frame. A list that
  only grows leaks memory and lets a click land on a row that has moved or gone.
* **A scroll offset is clamped to the transcript, not to itself.** The bottom is
  the last row of content at the last row of the viewport, and it is measured
  against the band the frame actually drew — the bands above and below take
  their rows first. A reader who scrolls past it would be scrolling into blank
  space and could keep going forever, since an offset has no ceiling of its own;
  reaching the bottom resumes following the tail, because that is what scrolling
  to the newest line asks for. The keyboard and the wheel share one clamped
  path, and a step starts from the row the frame is actually showing: following
  the tail is a *position*, so the state's offset is kept on it — a step from
  the offset the state was last dragged to threw the reader to the top of the
  session, a different turn, on the first scroll after opening it.
* **A turn the runtime has finished cannot still be streaming.** The state-free
  row projection cannot know that, so a provider that streams its answer as
  deltas and never sends a final message leaves a row marked `streaming` for the
  rest of the session — and a client that draws a spinner from that flag says
  "running" over a finished answer, forever. `transcript_rows` settles it once
  for every client: only the last turn can be live, and only while the session
  state (or an accepted send) says so.
* **A band that reports progress derives it from the projection, not from what
  is drawn.** `App::todo_progress` reads the last plan-shaped *timeline row*'s
  `Status: title` lines. Reading the transcript instead would tie a progress bar
  to whether the transcript happens to draw that row — and it deliberately does
  not draw a plan update. A counter with no runtime source (background tasks)
  stays at zero rather than guessing.
* **The pinned prompt header is chrome, not content.** Only a user message
  pins; an expanded one does not (it is already fully visible inline). It
  shrinks one row per row scrolled past down to `min(full_height,
  MAX_STICKY_ROWS)`, and the next prompt pushes it off from the bottom rather
  than overlapping it, so the transcript below is never hidden. It is decided
  against a conservatively small viewport so two frames cannot disagree.

### Surfaces that must not regress

| Surface | Contract |
| --- | --- |
| Status band | location on the left, status segments right-aligned as a group joined by ` │ `. A left-aligned list pushes the state off the edge exactly when a narrow terminal makes it worth reading. The location is a secondary label, never a status hue, and a path that does not fit folds its ancestors to initials (`/h/p/c/p/c/node_modules`) before the head is dropped, keeping the component that names the directory whole. On the prompt the band's last segment is a control: the session list's label and the chord that answers there, published as a click region so the mouse runs the same intent the key does. |
| Turn status | spinner + activity on the left, elapsed and tokens right-aligned. Present whenever a turn is running or the session is alive; idle remains static so a connected session does not look busy. |
| Composer | padded raised surface without a title or border; an empty box names the draft's vocabulary (`/ commands @ files $ skills`) in the reader's language, and runtime and run options sit on a separate line below it. |
| Completion | a drawer above the composer: two full-width rules, no corners, count on the top rule, selection marker is the composer's own arrow. |
| Shortcut band | drawn only on the pages that are navigated rather than written on (the session list and management); keys and localized labels separated by middots; prioritize send/newline or expand/details for the current focus, then runtime switching and navigation. Use the keymap for chords and click actions. |

## 6. Rendering

The transcript is the performance-critical surface and follows four rules:

1. a block caches its rendered lines **and** its height;
2. only dirty blocks are re-measured;
3. a streaming update rewrites exactly one block;
4. blocks outside the viewport are estimated, never rendered.

Measured contract: with no input and no events the loop produces **zero frames**.
Folding happens *before* wrapping, so a collapsed block never pays for the lines
it will not show.

**A literal is coloured by what it is, not by the fact that it is code.** One
colour for every code span is what makes a technical paragraph read as a single
grey block: `cargo run -p vibex-tui`, `crates/vibex-tui/src/view.rs` and
`0.1.0-rc.7` are three different kinds of fact, and a reader scanning for the
version should not have to read every path to find it. `classify_literal` sorts
a span into a count or version, a path, file or glob, and everything else, and
the palette carries a hue for each — plus one for a token that is neither prose
nor program text, which is where a keycap and an inline formula land. The
classes are deliberately coarse and the rules deliberately conservative: a
version has to *start* with a digit, so `deepseek-v4.1-flash` stays a name, and
a file name has to look like one. A test asserts the hues stay distinct in every
shipped theme, because a class the reader cannot tell apart is a class that is
not there.

**Markdown is coloured by syntax role, and the role decides the colour.** Body
prose is `gray_bright`, one step below `foreground`. A heading level takes its
own hue from the theme's chart ladder — three chromatic steps for levels 1–3,
then the greys, because a document that nests deeper than three levels is
outlining — which is what makes an outline legible at a glance and survives a
terminal whose CJK face has no bold cut. Literals take their class's colour from the syntax
palette with no background of their own; links their own accent; and everything
that is punctuation rather than content — bullets, ordered markers, task boxes,
quote bars, thematic breaks, table borders — one muted step. Emphasis is weight
and slant only: colour keeps meaning "this is a different kind of thing".
`MarkdownPalette` is resolved once per theme, alongside the syntax palette the
catalogue ships as JSON, so no render (and no streaming delta) pays for a parse.
A single-colour theme still has to spread what it has across the roles that
carry meaning, and a test asserts the ladder and the roles stay distinct in
every shipped theme.

**Markdown is interpreted, never echoed, and its styling survives wrapping.**
The renderer never prints the syntax it parsed: headings carry their level's
colour and weight instead of `#` markers, inline code is a coloured run instead
of backticks, and links print their destination only when the label does not
already say it. Wrapping is style-preserving: `wrap_text` decides where
lines break, and each visual line is matched back onto the styled runs
character-by-character (a break consumes the space it broke on, so offsets are
not reliable) — re-applying only the first span's style to a wrapped line erases
every emphasis, code and link on it. Content whose spacing *is* the layout —
fenced code, diffs, table rows and rules — takes a preformatted path that
hard-wraps by cell and never collapses runs of spaces, and a table is a closed
box (`┌┬┐ ├┼┤ └┴┘`) whose columns are padded by display width, so a double-width
cell cannot push the next `│` out of line. Table cells wrap within their columns;
column budgets include all padding and separators. When columns cannot fit,
render records vertically rather than discard cell content.

**A streamed answer is rendered once, and never from its first character
again.** Re-parsing the whole document on every delta is quadratic in the length
of the answer, which is felt exactly when the answer is worth reading.
`StreamingMarkdown` keeps a *frozen prefix* — source bytes that text arriving
later cannot reinterpret — and re-renders only what follows it. A freeze point
is a blank line that ends a top-level block: a paragraph, a heading, a closed
fence. Nothing inside a list, a quote, a table, an indented line or an unclosed
fence qualifies, because the text after it can still change how those lines
read. The rows are appended to one buffer and the stale tail is truncated, so a
delta costs the size of the unfrozen tail rather than a copy of the answer. The
transcript keeps one renderer per streaming block and drops it when the answer
finishes, which is what makes the last frame of a stream and a reload of the
same session the same render — a test asserts the streamed rows equal a whole
render at every chunk boundary. There is no caret glyph in the streamed text:
nothing is appended to mark the live block, so nothing shifts and nothing
flickers; the turn band's spinner is what says the Agent is still working.

**A streamed block is re-rendered at push time, and laid out at frame time.**
The delta arrives on the worker's message, the renderer advances then, and the
transcript invalidates only that block's height and rows; the frame composes
whatever is current. Deltas are drained in batches and frames are capped at one
per `FRAME_INTERVAL`, so a burst of tokens costs one repaint rather than one per
token, and an unchanged frame writes nothing.

**A running thought is the one body drawn without being opened, and it is drawn
in a fixed window on its tail.** Reasoning is a dense row, so its body normally
sits behind the fold; while the Agent is still on it, the row shows up to
`STREAMING_WINDOW_LINES` rows of the newest content instead, with a `…` row once
older rows have left the top — the newest line pushes the oldest off rather than
growing the block. The window's marker column carries an animated rail, one cell
per row and the header included, so the bar is exactly as tall as the range it
stands for; its crest travels down on the chrome animation clock, so the window
is dropped from the render cache when the phase moves and re-drawn without a
re-measure. A terminal that cannot blend colours draws the rail flat — the bar
still marks the window. `set_animation_phase` is told the clock by the one
function that draws the transcript, and a transcript with nothing streaming
never advances it.

The runtime does not close a reasoning stream: a row that once streamed keeps
that flag for the rest of the turn. The window is therefore opened only on the
block at the **end** of the transcript — the thought the Agent is on now — and
folds to one row the moment any other row follows it, which is also what makes a
tool call end the window without the provider saying so. That block is not
eligible for a dense run either, because folding it away would hide the window
the reader is watching. `estimate_height` bounds a live window by its own cap:
markdown reflows the body, so the estimate is an upper bound rather than an
equality, and an unmeasured window must never be estimated shorter than it
draws.

**The composer wraps its draft so byte offsets survive.** `wrap_text` produces
*rendered* lines: a run of whitespace collapses to one space and a broken token
is re-joined with a space of the wrapper's own, so a line is no longer the slice
at its `source_start`. Everything painted onto a composer row — the caret, the
selection, a chip's label — is addressed by a byte offset in the draft, and a
collapsed run shifted those offsets until they landed inside a multi-byte
character and the slice panicked. The composer therefore wraps with
`wrap_source_text`, which keeps every line a verbatim slice of the draft (an
ASCII control character is drawn as a space, one byte for one byte, so a tab
cannot move the caret either), and `floor_boundary` backs an offset down to a
character boundary so any future mismatch costs a column instead of the process.
A grapheme wider than the whole line overflows it rather than disappearing.

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
  shift-only binding is unreachable. Use a modifier or a function key. A shifted
  key that arrives as its *unshifted* code plus `Shift` — what a terminal
  implementing the protocol's disambiguation without its alternate keys sends —
  is folded into the character it produced at the same door, so no reader
  downstream has to know which form its terminal chose.
* **A key the legacy encoding cannot express gets a chord it can.** `Enter` and
  `Shift+Enter` are the same carriage return until the terminal has been asked
  for its keyboard protocol, so the newline action is bound twice: `Shift+Enter`
  where the protocol answers, and `Ctrl+J` — a line feed, a byte of its own in
  every terminal — everywhere else. Help lists both; the send key stays `Enter`.
* `Esc` only walks back one level; it never cancels a running turn. `Ctrl+C`
  owns clear-draft / interrupt / quit-confirm.
* **The composer is not a room without a door.** `Esc` in the composer returns
  to the session list in a single press — `go_back` skips its focus hop on a
  session page — and leaves the draft where it was, so stepping out costs
  nothing and stepping back in resumes typing. A selection highlight and an open
  completion drawer are dismissed first; clearing the draft is `Ctrl+C`, never a
  second `Esc`.
* **A chord a text field does not use stays global.** The composer scope wins
  over `Scope::Global`, so binding a completion key to `Ctrl+P` silently killed
  the command palette inside every session. Completion navigation lives on
  `Alt+↑`/`Alt+↓` (and the arrow keys while the drawer is open), and `Ctrl+P`,
  `Ctrl+Q`, `Ctrl+G` and `Esc` resolve to their global intents with the composer
  focused.
* **The runtime switcher is visible where the reader is.** The composer's info
  line names the session's Agent and its `provider/model` plus the key that moves
  them (`Ctrl+G`); the picker opens with the cursor on the session's current
  choice and marks it, refuses an option the catalogue says is unavailable, and
  submits a compare-and-set switch. A backend that cannot switch runtimes gets a
  toast instead of an overlay whose Enter does nothing.
* **An Agent is more than its model, so the switcher carries its run options.**
  What the chosen Agent publishes — thinking depth, conversation mode, then its
  session features — is read through the shared `RuntimeCascadeProjection`
  rather than re-derived here, so all three clients agree on what an Agent
  offers. It is the switcher's *second view*, not rows appended under the
  catalogue: the catalogue is as long as the machine has models, and the run
  options belong past the bottom of a fifty-row list only in the sense that
  they are hidden there. `Tab`/`Shift+Tab` swap `RuntimePickerView::Choices` and
  `RuntimePickerView::Options`, `selected` indexes the rows of whichever view is
  up (`App::runtime_picker_row_count()`), the options view opens on a caption
  naming whose options they are, and a view with nothing in it says so rather
  than swallowing the key. On the composing page `Tab` first takes the row the
  cursor is on, because there is no live session to move: the reader who has
  just picked an Agent gets *that* Agent's options rather than the ones belonging
  to the entry the page started on, and the page's choice becomes the row the
  cursor was on. A row opens a value list whose first entry is the
  Agent's own default (`On`/`Off` for a switch, free text for a string feature,
  which the prompt overlay collects); `Esc` steps out one view at a time — value
  list, run options, catalogue, closed — and the value list carries the option
  with it rather than an index, so a catalogue read that lands while it is open
  cannot move a value onto another option. Every apply re-checks the value
  against the catalogue and re-uses `Effect::SwitchRuntime` — the composing page
  keeps it in `new_session_runtime` for the session it creates — and the
  composer's info line names the depth and mode in effect beside the runtime,
  because the question "what will this message be sent through" is answered
  there.
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
* **A guard protects the arithmetic beside it, not just the branch.**
  `bool::then_some(value)` and `map_or(value, …)` evaluate `value` whatever the
  condition says, so a subtraction inside one runs on coordinates the check just
  rejected. The mouse is where this bites: a terminal reports motion for the
  whole window, most of which is outside every band, and a pointer above the
  queue band subtracted past zero and killed the client. Containment checks wrap
  the arithmetic — `then(|| …)`, `if … && rect_contains(…)`, `saturating_sub` —
  and a test walks the corners of every band to keep it that way.
* **The session list is a page of sections, and it says what it holds before
  the reader reads a row.** The view gets the whole screen, and spends its first
  two lines on the page: the location with the chord that moves it, one chip per
  state family the list holds (`◆ 1 waiting  ▶ 1 running  ◇ 5 idle`) right
  aligned, then `+ New session` as a click target with the grouping mode and its
  key on the right. Chips are counted through the filter *and* through folded
  headings — a heading that hides its sessions still holds them — and a family
  with nothing in it is not drawn as a zero. Neither hint row is truncated: the
  location gives way to the chips, and a right-hand hint that does not fit whole
  is dropped, because half a key is a key the reader cannot use.
* **A list row carries the same facts as the desktop's.** The session list
  renders the shared projection's rows (order, pins, folders) under a heading
  per group — disclosure, name, the count of sessions in its subtree, and a rule
  to the edge — and adds what the desktop's sidebar row has. A session row is
  two lines: the state mark, the title, the Agent's name after a middot, the
  unread dot, pin and auto-continue marks, and a coarse right-aligned age on the
  first; what the session last did on the second, in the dim step, with
  `Pending:` in the attention colour for a session waiting on the reader. The
  echo is the client's own reading of events it already receives
  (`note_session_echo`: the Agent's messages, tool calls, commands, file
  operations and errors — never a delta, never the reader's own message),
  pruned to the sessions the runtime still lists, falling back to the workspace
  root. Unread is likewise the client's own notion — a final `AgentMessage` for
  a session the reader was not looking at (`note_activity`), cleared by
  `open_session` — because the runtime does not track "read"; the authority's
  own unread set is folded in beside it. Marks are one column at every glyph
  tier and their shape carries the meaning, or a reader who cannot tell the
  colours apart loses the state: `★` pinned (ASCII `*`), `●` unread, the state's
  own `▶`/`✗`/`◇`/`◆`/`▤`, and `↻`/`↻N` for auto-continue. The state is *only* a
  mark: the words were removed once the shapes were learned, because they cost
  the title a third of the row and said nothing the mark did not.
* **Rows are not all one line tall, so a click is measured against the row it
  landed in.** The frame publishes each drawn row's height and the index of the
  row at the top of the list in its `ListRegion`, and `list_row_at` walks those
  heights; a click on a session's second line selects that session rather than
  the one below it. The list still scrolls by row (ratatui's `ListState`), so
  the two never disagree about which row is where.
* **Auto-continue is the desktop's rule, not a second one.** A session the
  reader switched on continues itself when a turn stops without an answer, and
  "without an answer" is read the way the desktop reads it: the shared
  `agent_session_turn_requires_continuation` decides, and
  `latest_timeline_turn_ended_normally` over the session's timeline — asked for
  with a probe when the session is not open — supplies the answer. The same
  five-second countdown precedes the send, the same per-turn bookkeeping stops
  a second send for one turn, a session waiting on the reader is never
  continued, and a turn the reader stopped suspends the continuation instead of
  restarting it. A send in flight holds the countdown at zero rather than
  interleaving two turns. The countdown is drawn wherever the reader is: the
  row's `↻N` on the list, and the composer's info line inside the session, which
  is where the Desktop puts the same seconds. The preference rides the sidebar arrangement's
  auto-continue fields, so it is the same preference on every surface; a
  suspension this client makes is local, because the protocol has no pause
  change — only the enable/disable that clears one.
* **The list draws the authority's arrangement, and edits it there.** Folders,
  the order of projects and sessions, the pinned band, collapsed headings and
  unread marks are the Desktop's: `SidebarBackend` reads them as a
  `RemoteSidebarOrganizationSnapshot` (the remote service when a Desktop is
  attached, the persisted UI state when this process owns the runtime) and the
  rows are walked from it with the same `sidebar_root_items` /
  `sidebar_project_items_for_workspace` / `sort_sidebar_sessions` helpers the
  Desktop and the phone use. So `p`, a heading's collapse and a reorder are sent
  back as `MutateSidebarOrganization` with the rendered revision, and the answer
  replaces the tree; a refusal says so and re-reads. The revision is the
  content's fingerprint, so an answer must carry the fingerprint of the tree it
  actually holds — the shell that owns the runtime re-reads after saving rather
  than answering with the view it applied, or the client's next echo is refused
  and every second keystroke looks like a race. An arrangement that
  arranges nothing (no folders, no order, no pins) is refused as a source — it
  carries no information, and adopting it would replace recency with an
  arbitrary id order — and this client keeps its own fallback projection, as it
  does when the capability is unsupported. Two arrangements are never mixed:
  while one is loaded, the local `tui-sidebar.json` state is not what the list
  draws.
* **A running turn is what the spinner animates on.** `is_animating` is the
  single question the loop asks before repainting on its own, and it answers
  yes while a turn reads as running (`turn_reads_running`), not only while
  text streams: the quiet stretches — runtime startup, thinking, a slow tool —
  are exactly when a held frame reads as a frozen client. A question waiting on
  the reader and the composing page's mark animate too; everything else holds
  still, and that is what keeps an idle session at zero frames (the PTY
  `an_idle_interface_writes_nothing` contract).
* **Work records have the same layout while running and after completion.**
  `is_dense_row` always hides the body until explicit expansion. Reasoning
  shows its label; a tool shows a recognized action, never raw JSON or XML as a
  fallback. Complete input followed by output can still supply the action;
  unknown or incomplete payloads remain available in details. Runtime attribution
  also lives in details. Failure stays visible and breaks grouping. Keep height
  estimates and cached neighbour gaps in sync with the rendered shape.
* **Expansion belongs to the reader.** `Transcript::set_blocks` retains it by
  stable block ID while content updates. Group changes invalidate measurements
  and cached rows. The rendered group head supplies the mouse hit target;
  clicking it and the expand key run the same intent.
* **A stream must recover from missing events.** `App::refresh_timeline` starts
  a generation-scoped session load from sequence zero when a timeline gap is
  reported. An in-flight load prevents duplicate requests. Applying the snapshot
  updates the projection without navigating, transferring a draft or resending
  a message. Worker-result batches have a time budget so a burst cannot keep the
  next frame waiting for the queue to empty.
* **A send is projected until the runtime echoes it.** The client does not own
  the timeline: the reader's own message comes back a round trip later, so it is
  drawn locally as the row it will become (`PendingSend::row`) and the session
  reads as running (`turn_reads_running`) from Enter rather than from the
  runtime's answer. The projection is confirmed against the *timeline* — a
  newer item with the same text and attachments — and otherwise withdrawn on
  refusal or after the timeout. Its row identity is a serial, not a clock: the
  transcript diffs by id. The turn clock is derived (`sync_turn_clock`) rather
  than set by events, so no path leaves the client repainting after a turn
  stopped, and nothing drains into the gap the projection covers.
* **The send queue is per session, and drains by session.** A held message
  carries the id it was written for: switching sessions must not release it,
  hide it, or send it anywhere else, and it goes out when *its* session's turn
  ends — including while the reader is looking at another one. The band, the
  dock's queue rows and every queue key read the open session's subset
  (`queued_for_active`), because a message held elsewhere is not this reader's
  to see or act on here. `session_is_running` consults the open session first
  and the session list second: the sessions the reader is *not* looking at are
  exactly the case the queue has to get right.
* **Sending from the composing page lands in the session view at once.** The
  session it is sent into does not exist for a round trip, so the message is
  projected with no session id (`PendingSend.session_id: None`), the view is
  emptied of the session the reader came from (`enter_creating_session` clears
  the selection, the active session and the timeline model), and the page
  becomes the session view — otherwise the reader watches a logo and concludes
  nothing happened. The projection is drawn while no session is open, is
  stamped with the id when the runtime answers, and a failed creation withdraws
  it and puts the draft back on the page. The answer opens the session the way
  the list does (`App::open_session_effects`: the snapshot, which carries the
  session's own runtime selection, and the catalogue), because a creation answer
  names the session and not the runtime it was made with: without that read the
  composer fell back to the catalogue's first entry and the switcher offered to
  move the brand-new session onto it.
* **A key does what the surface advertising it says.** The composing page's
  line names `Ctrl+W` as "change the workspace", so that key opens the directory
  picker there — the old `SwitchWorkspace` listed workspaces into state nothing
  drew *and* navigated away from the page, which is a gesture with no visible
  result. A picker answers the page that opened it rather than the flow it was
  first written for.
* **A page answers for itself before the session behind it does.** The
  composing page keeps the client's session *selected* (leaving it must return
  there), so anything that asks "which session?" answers wrongly on that page.
  The runtime picker is the case that bit: it took the session-switch path and
  switched the session the reader was leaving — failing with "selected Agent
  runtime configuration is unavailable" — while `new_session_runtime` stayed
  empty and the session was created on the catalogue's first entry. Decide by
  the page that is *shown* (the picker-target rule below), not by whether a
  session happens to be selected, and keep the choice on the page until
  `CreateSession` carries it.
  The reading side is the same rule, or the page lies about what it will make:
  the Agent and model the page and the composer's info line name, the entry the
  picker opens on and marks as current, and the run options a view lists all
  come from `App::page_runtime_selection()` — the page's own choice, or the
  entry a creation with no choice falls back to (`default_runtime_selection`
  mirrors the worker's own rule: first available, else first published) — and
  `Effect::CreateSession` carries that same selection instead of leaving the
  runtime to reach for a default of its own. The rule is one predicate,
  `App::page_owns_session()`: every session-scoped read that describes the page
  — the turn and its clock, the queue, the plan, the dock, the approval count,
  the steer key, the transcript's animation — answers empty while it is false.
  Two exceptions are deliberate. The clock *keeps counting* behind the page
  (`sync_turn_clock` asks the session, not the page) so a reader who steps out
  and back finds the turn's real elapsed time; and the view that waits for a
  session being created is not the composing page, so a send in flight still
  reads as running there.
* **The runtime a page names is the runtime it creates with.** A creation is
  not allowed to land on an Agent the page never named: the page answers with
  the catalogue's fallback entry when it has no choice of its own, and a send
  from the composing page on a backend that *can* publish a catalogue reads it
  instead of handing the choice to the runtime's default — the draft waits, and
  the reader presses `Enter` again once the Agent has a name. Only a backend
  that publishes no catalogue at all keeps the runtime's fallback, because there
  is nothing there for the page to name.
* **A runtime choice moves the session the page is showing, and nothing else.**
  `Ctrl+G` is a global key, so its *target* has to come from the page rather
  than from "is a session selected?": the client keeps a session selected behind
  the list and behind the composing page, and moving that unseen session is how
  a codex session turned into the Agent the reader had picked for the session
  they were about to write. `App::page_shows_session()` is the predicate — a
  session page with a session selected — and `App::page_runtime_selection()` is
  its reading side: a page that shows one answers with that session's durable
  selection, and a page that shows none answers with the choice the page holds,
  or the entry a creation with no choice falls back to. `apply_runtime_selection`
  and `apply_run_option` follow it, so a choice made on the list or while writing
  a new session becomes the *next* session's Agent (`runtime_next_session` says
  so in the picker's title and in the toast) and the open session is untouched;
  a choice made on a session's page switches only that session and is never
  carried into the next session.
* **A list key acts on the row the cursor is on.** The list shows rows, and the
  session the client has open behind it is not always the one the reader pointed
  at: rename, fork, archive and delete all resolve their target through
  `App::list_session_target()`, and the prompt and the confirm re-read it — the
  cursor cannot move while an overlay is up — so a key can never change a
  session nobody selected. `Enter` already worked this way; a heading row owns no
  session and the action is inert there.
* **A new session is a page, not a dialog — and it is the page the client opens
  on.** The reader who asks for one asked to write, so the gesture lands on a
  page that hands them the composer and names what the message will be sent
  through — Agent, model, workspace — with the keys that change them, and never
  the session behind it. The session is created by *sending*: the title comes
  from the message, and a runtime chosen on the page travels into
  `Effect::CreateSession` rather than needing a session to exist first. The page
  is a session page for the *composer* (`is_composing_page`), so the composer
  owns the keyboard. It is also where `App::new` starts: a reader who types
  `vibex` in a directory came to write in it, so the page names that directory
  as the workspace (`App::starting_workspace`) and offers the sessions that
  already exist from a clickable corner entry in the status band. The entry
  names the chord that answers where it is drawn — `Ctrl+L` while the composer
  holds the keyboard, the global `1` otherwise — because the plain digits are
  characters in a draft. `Esc` steps between the prompt and the list; from a
  session it is still session → list → prompt.
* **The page's choices belong to the page until a creation consumes them.**
  Entering the composing page keeps the Agent and the directory already chosen
  for it: `Ctrl+G` and `Ctrl+W` are global, so a reader who picked either from
  the list picked it for the session they are about to write, and resetting the
  choice on the way in (`begin_new_session`) is how their choice went nowhere.
  A creation is what consumes both — `enter_creating_session` clears the
  directory, and the Agent the creation carried is what the view that waits for
  it names — so the page a reader comes back to afterwards names the directory of
  the session they are in rather than one chosen for a session that already
  exists. The page *shows* what it holds (the hero's runtime and workspace rows),
  so a kept choice is never a silent one. The page also reads the session list
  on the way in even though it draws none of it: the countdown, the unread mark
  and "is this session's turn running" are derived from that state, and none of
  it may be wrong because the reader started on the prompt.
* **The waiting mark is the only chrome that animates by itself.** The landing
  mark's sweep is a greeting rather than a heartbeat:
  `chrome_animating` gates both the tick period and
  `advance_transcript_animation`, and the greeting ends after one whole pass
  (`LANDING_SWEEP_FRAMES`), so a session that is merely open — and a prompt left
  waiting — costs zero frames. A terminal that
  cannot blend colours gets the mark at full strength rather than a sweep it
  cannot show.
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
* **Clipboard reading is the worker's job, never the reducer's.**
  `Effect::ReadClipboardImage` and `Effect::ReadClipboard` run on
  `spawn_blocking`, shell out to the desktop's clipboard tool with a deadline
  and a kill, and answer with an `AppMessage`. The read is *type-aware*: the
  owner lists what it offers and the bytes come back under the type it offers,
  because asking for PNG and nothing else finds nothing on a clipboard holding a
  JPEG. The client still *writes* the clipboard only over OSC 52; reading is an
  enhancement that degrades to "name a file instead" when no tool exists.
* **An attachment travels with its place in the message.** The chip label is
  dropped from the text, so the offset — `inline_text_offset`, in UTF-16 units
  of the *sent* text — is the only thing that says where the picture was: a
  reader that does not get one appends the picture at the end of the paragraph.
  The URI has to resolve for the client that draws it as well as for the runtime
  that reads it: an absolute `file://` URI for a file, and for clipboard bytes a
  file written beside the message when this client *is* the authority — the
  desktop can only draw a path — with the data URL kept for a remote seat, where
  no file of ours is reachable and the runtime materialises the bytes itself.
  `outgoing()` computes the offsets from the chips in one walk, so an expanded
  paste in front of an image moves it, and the queue carries the pairs so a held
  message does not lose them. **A projection row carries only text**, so every
  surface puts the attachment back itself: the desktop as an inline chip beside
  the words, the terminal as a bracketed placeholder at the offset
  (`attachment::with_attachments`), which is what the reader's own message must
  read as — the optimistic `PendingSend` row included, since it is built to be
  the row the echo becomes.
* **One paste route, whoever pasted.** The terminal's bracketed paste and the
  client's own clipboard reader both end in `App::insert_pasted_text`: a picture
  named as a path becomes an attachment — quoted, `file://`, `~`-relative and
  backslash-escaped spellings all count, several pictures attach together, and a
  path inside a sentence stays a sentence — and anything else becomes draft
  text. `Ctrl+V` is bound to it in the composer scope: a terminal
  in bracketed-paste mode never sends the key, so the binding only fires where
  the terminal forwards it — and there it is the only way to paste a picture.
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
* **What the settings surface changes is persisted, per appearance where the
  choice is.** The look, the language, the standing workspace and the palette's
  recents live in `InterfacePreferences` and are written to
  `~/.vibex/tui-interface.json` (the path an `AppOptions` field, like the
  arrangement's, so a test writes nothing into a home). `apply_setting_value` is
  the only writer and the only saver, so a preview that `Esc` reverts also
  reverts what was written. The theme is two slots — light and dark — because a
  palette is authored for one appearance: switching appearances reads the other
  slot instead of overwriting it, and the row shows the *resolved* id rather
  than a stored one that may belong to the other appearance. Every stored value
  is an optional string, and a value this build does not know loads as no
  choice, so a newer build's file is not an error in an older one. Resolution
  order is the composition root's option, then the environment (`VIBEX_THEME`,
  `VIBEX_TUI_ICONS`), then the file, then detection: a value named for one run
  outranks the standing one and is not written back over it, while the reader's
  remembered language outranks `LANG`, which is a guess about the terminal
  rather than a demand.
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
| Tiny-terminal render tests | every page paints a whole frame down to 1×1; a small terminal keeps a line of transcript and a composer that shows the draft |
| Contract tests | dependency boundary, key tables, locale coverage, no secret-shaped copy, docs exist per page |
| PTY end-to-end | the real binary enters raw mode, paints a first frame, writes zero bytes when idle, keeps in-process diagnostics out of the terminal, restores the terminal on exit, survives a resize storm |

`cargo test -p vibex-tui` runs the first four. The PTY layer needs the harness
entry point, so it runs as
`cargo test -p vibex-tui --features pty-harness --test pty`; `pnpm check:rust`
covers both.

The idle assertion is load-bearing: it is the measured form of "no animation and
no events means no frames". A startup notice legitimately repaints while it is
visible, and the landing mark sweeps one pass as a greeting, so the test settles
until the process has been quiet and only then measures.

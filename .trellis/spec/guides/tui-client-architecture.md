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
| `vibex-core`, `vibex-backend` (traits only), `vibex-desktop-model`, `vibex-ui` (`default-features = false`), `vibex-terminal-ui`, `ratatui`, `crossterm`, `rustix` (Unix only, descriptor plumbing) | `gpui`, `gpui-component`, `vibex-desktop-runtime`, `vibex-db`, `vibex-agent-acp`, `vibex-browser`, `vibex-content` |

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
runtime is not accepting local clients, the client must print the endpoints it
tried and the three ways out (turn on Settings → General → Vibex TUI, quit the
desktop app, or `vibex connect`) rather than a bare connection error.

### Local bootstrap trust model

The local-machine bootstrap mints a device credential for the runtime that owns
the home. Its trust anchor is **the filesystem**, not the loopback interface:

1. the runtime identity public key is read from
   `<home>/relay/desktop-identity.json`;
2. the certificate the runtime terminates TLS with is derived from that same
   identity, so a pinned route needs no operator-supplied link;
3. a pairing code is minted against the same database the running runtime uses
   (safe because the database runs in WAL mode with a busy timeout, and
   `vibex-server pairing-code` already does exactly this);
4. the claim pins the identity public key, so a different local process cannot
   impersonate the runtime;
5. the credential is written to `<home>/local-clients/tui-credential.json` with
   mode 0600 at creation.

A desktop serves same-machine clients on its pinned-certificate Gateway narrowed
to the loopback address, and that listener is **not** part of Remote Access: it
is on by default and switched from Settings → General → Vibex TUI. The Direct
listener is dialled only as a fallback for a desktop that predates the
local-client listener.

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
* **A settled turn reaches a remote client as a re-read, never an event.** The
  authority publishes a session's state change as a sidebar invalidation, so
  the reads that invalidation triggers are the only copy of the state a remote
  client gets. The root list therefore settles the open session's own copy
  (`reconcile_open_session`, under the recency rule a pushed update obeys), and
  a branch page settles the worker it names through
  `App::sync_open_session_from_tree`, because a worker is absent from the root
  list. A client that stored the rows alone drew a finished turn as running —
  spinner, elapsed clock and the `Steer` placeholder included — until the
  reader re-opened the session by hand.
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
* **A page with two halves says so with a tab strip.** The configuration
  centre's modules and the devices page's pairing and trust halves are peers
  under one title, so the page leads with pills and one rule beneath them, the
  active pill filled, and the arrow keys — or a click, through
  `FrameRegions::device_tabs` — move between them. The strip costs two rows and
  never replaces the content: each half keeps its own rows, its own cursor and
  its own footer line, and a panel opened in one half is closed by leaving it.
* **A row is absent when the runtime would refuse the verb.** The devices page
  lists a way to pair only when it can work here — a phone always scans, a local
  network can only be searched where the runtime advertises itself, and a second
  desktop can only be handed a code by a headless runtime. A disabled row would
  teach the reader to distrust the page; an absent one sends them to the way that
  works. `Esc` closes an open pairing panel before it becomes a walk back to the
  page the reader came from, and the per-row verbs (`a` allow, `r` refuse) are
  bound for the whole scope but advertised only while they have a subject.
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
  line), the controls a list row draws for itself (`RowActionRegion`: rect, the
  row it acts on, its intent), the composer's text rows, the queue band, the
  dock, the turn rail's ticks, the shortcut band's hints with their intents, the
  banner and a modal's close control. Every one of them is written by the
  renderer that drew it, because only that code knows where the band landed, and
  every one of them is dropped by `FrameRegions::begin_frame` with the frame
  that drew it, so a closed panel, a modal that has left or a row that has
  scrolled away cannot keep a stale hit rect; `run.rs` does nothing but
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
   count of the session as much as a map of it, with every turn the viewport
   shows lit and the turn the viewport starts on drawn heavier. The lit window
   is the reader's place, not a cursor position: a screen holding three turns
   lights three ticks. More turns than rows window the ticks around the active
   one. A transcript with no turns draws nothing — a scrollbar in those columns
   is counted as turns, which is the mistake the rail exists to avoid.
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
* A run of three or more collapsed activities with the same turn and runtime
  folds into its first member (`MIN_GROUP_RUN`). Typed activities use
  `vibex_ui::timeline::ActivitySummary` to report every action category in a
  mixed run; legacy rows still require the same kind and title and report
  `+N`. Expanded, failed, approval-pending, and typed running operations break
  the run. Opening the head reveals every member; keyboard navigation skips
  members while folded. See [Timeline Presentation](../frontend/timeline-presentation.md)
  for shared labels, details, and cache contracts.
* **A tool call reaches the transcript as one row.** An Agent narrates it as a
  run of timeline items — the same call id with a growing payload — and the rows
  are folded by the shared projection before the transcript sees them, so the
  newest update replaces the row's text in place. The transcript must not draw
  one block per update: that turns a single call into a column of half-parsed
  JSON, inflates the `+N` run summary, and hands every duplicate the same block
  id, which breaks selection and click targets keyed by it.
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
  was when the frame drew it, so `FrameRegions::begin_frame` drops all of them:
  the turn ticks and shortcut hints, the list, the banner, the composer's box and
  the modal's close control. One that outlives its control lets a click land on a
  row that has moved or gone — or answer a surface that is no longer drawn.
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
  state (or an accepted send) says so. That settlement reaches the screen only
  when the projection is rebuilt, and the state can move through a *read* rather
  than an event — the session list an invalidated sidebar re-reads, an ownership
  tree answer — which no rebuild follows. `sync_transcript_liveness` is what
  closes that: the loop compares the state the transcript was built from on
  every pass and rebuilds when it moved, exactly as `sync_turn_clock` derives
  the clock. The page that skipped it kept the finished turn's rows streaming,
  and the turn line — which reads a streaming row as work in flight — went on
  spinning and counting time under an answer that had already landed.
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
| Status band | location on the left, status segments right-aligned as a group joined by ` │ `. A left-aligned list pushes the state off the edge exactly when a narrow terminal makes it worth reading. The location is a secondary label, never a status hue, and a path that does not fit folds its ancestors to initials (`/h/p/c/p/c/node_modules`) before the head is dropped, keeping the component that names the directory whole. On both session pages (`Page::is_session_page`) the band's last segment is a control: the session list's label and the chord that answers there — `Ctrl+L` while the composer holds the keyboard, the global `1` otherwise — published as a click region so the mouse runs the same intent the key does. |
| Turn status | the activity mark and what the Agent is doing on the left, elapsed, tool count and rate after it as middot-joined measurements. Present whenever a turn is running or the session is alive; idle remains static so a connected session does not look busy. The running mark is the desktop sidebar's — the ring of the Braille cell with a highlight walking it, one step per animation tick (`glyphs::ACTIVITY_RING` and `activity_ring_frames`) — and it wears the theme's primary, or green while the session continues itself. One session has one running mark, so the band and the desktop's session row may not disagree about what the turn will do next. The label beside the mark wears the desktop's live-label shimmer: a band crossing the words, one lap per `shimmer::SWEEP_MILLIS` (the desktop's 1200 ms, in frames of this client's animation clock), its half-width `shimmer::SPREAD` of the label's own width, and its ink the label's grey walked toward `roles::border` — the canvas's opposite, on the landing mark's rule that a light brightens what it crosses. The label is truncated to the columns left after the mark before it is lit, so a lap completes over what is drawn. Only the running arm shimmers: a band waiting on the reader is not work in flight. **Motion** off, `NO_COLOR`, and a label that is not running all draw the flat span, and a palette that cannot blend carries the band in `BOLD` instead of in a ramp. |
| Composer | padded raised surface without a title or border; an empty box names the draft's vocabulary (`/ commands @ files $ skills`) in the reader's language, and the runtime a message will go through — Agent, the account it runs on, and model — with the run options in effect sit on a separate line below it. A surface that marked only two of the three names would not say which entry the reader picked. |
| Completion | a drawer above the composer: two full-width rules, no corners, count on the top rule, selection marker is the composer's own arrow. `/` and `@` open the desktop popup's panels — commands + quick phrases, files + Agent delegation — under a header row that names them and carries the trigger's sigil; `←`/`→` and a click on a name switch panels locally, because one discovery answer carries every list. At either edge the key is not consumed, so the caret keeps its movement inside the query and the drawer stays open. A paneled drawer stays drawn with an empty-state row when its panel has nothing to offer, since closing it would take the panels with it; `$` keeps its single Skills panel and the plain arrows, and a single panel with no matches still closes the drawer. |
| Shortcut band | drawn only on the pages that are navigated rather than written on (the session list and management); keys and localized labels separated by middots; prioritize send/newline or expand/details for the current focus, then runtime switching and navigation. Use the keymap for chords and click actions. |
| Devices page | two halves plus a third the runtime may not have. The pairing rows are the runtime's to offer — a row it would refuse is absent, never disabled — and the local-network row additionally requires a reachable **Direct** route, which is the precondition the window itself checks. An offer carries *every* published route, not one: the panel names the route the code and link carry and lets `[`/`]` move between them (local, no round trip), and only draws the choice when more than one route exists. Closing the panel or leaving the pairing half **cancels the offer on the runtime**, exactly as it takes the LAN advertisement down: a code the reader walked away from is still claimable until the runtime is told. An open offer is polled, so a claim is reported rather than counted down to expiry. The **Remote access** half is read-only by design — it reports which publishing methods are up, degraded, or off, and what an operator has to do — because switching one on moves a Tailscale Serve config, binds a port, or enrolls a relay, and those belong to whoever is sitting at the machine. It is gated on `DevicePairingPublications`, which is advertised independently of `DevicePairing` so a runtime with *no* published route can still answer the read that explains itself. |

## 6. Rendering

The transcript is the performance-critical surface and follows four rules:

1. a block caches its rendered lines **and** its height;
2. only dirty blocks are re-measured;
3. a streaming update rewrites exactly one block;
4. blocks outside the viewport are estimated, never rendered.

Measured contract: with no input and no events the loop produces **zero frames**.
Folding happens *before* wrapping, so a collapsed block never pays for the lines
it will not show.

**A spinner's clock is the page's, not the turn's.** `App::chrome_animating` and
`App::is_animating` answer for every mark drawn from `animation_phase` that is on
screen: the session list's running rows, a record being saved in the
configuration centre and the request row on the devices page, and the dock's own
rows for the work a finished turn left behind. Each is asked of the state its
surface draws from — the ring's states are one definition the row and the clock
share — so a mark cannot stand on one frame under a clock that thinks it has
nothing to draw, which is the one reading a spinner must never give. Each is
false when its surface is not the page in front of the reader: that is what keeps
the zero frames above honest rather than a client that repaints for a panel
nobody has open.

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

**Reasoning follows the desktop's preferences and live-turn projection.**
The TUI defaults to Latest at bottom, default expansion off, and Window. A
reasoning row opens only when the reader expands it or default expansion is on;
Window mode then clips a body that is still arriving to its newest rows, while
Full mode and a settled thought draw the whole body. In timeline retains
historical reasoning; Latest at bottom follows the current turn's live status.
See [Timeline Presentation](../frontend/timeline-presentation.md) for the
setting fields, state retention, and disclosure contracts.

A live window shows up to `STREAMING_WINDOW_LINES` rows of newest content. Every
row of it is the thought's own text: the live header and the rail say the
thought is unfinished, so no row is spent on a `…` mark when older rows have
left the top. Its marker column carries an animated rail including the header.
The rail follows the chrome animation clock, so a phase change invalidates
rendered rows without re-measuring. A terminal that cannot blend colours draws
the rail flat.

The six-row count comes from `vibex_ui::timeline::REASONING_WINDOW_LINES`, shared
with desktop and mobile. Keep the terminal's incremental Markdown renderer,
animated rail, and follow/scroll controls when aligning activity
presentation with the GUI clients.

Provider reasoning flags can remain stale after another activity starts. Use
the shared conversation projection to settle superseded thoughts, and only
window the trailing live thought. A following tool or a completed turn removes
the non-final bottom indicator; Timeline mode keeps the settled row. An open
thought stays outside activity groups. `estimate_height` bounds a live window
by its own cap: an unmeasured window must never be estimated shorter than it
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
  owns clear-draft / interrupt / quit-confirm. Only the page that draws the
  transcript may interrupt its turn; on the session list, in settings, and on
  every other page the selected session is behind the page, so the press never
  stops it. **The pages a reader opened are a history, not a hierarchy.**
  `navigate_to` remembers the page it leaves — and
  the pane that owned the keyboard on it — in `App::page_history`, and `go_back`
  returns there, so a reader who opens Usage (`3`) from the composing page comes
  back to that draft rather than to a session list they never opened. Walking
  back is not an open: it never pushes, an entry naming the page already shown is
  skipped rather than spending a press on no movement, and the history is capped
  at `PAGE_HISTORY_LIMIT`. With nothing to walk back to, the two halves of where
  the client starts still step between each other — the list and the prompt — and
  every other page falls back to the list.
* **The composer is not a room without a door.** `Esc` in the composer returns,
  in a single press, to the page the session was opened from — the session list
  when the reader came from it — because `go_back` skips its focus hop on a
  session page, and leaves the draft where it was, so stepping out costs nothing
  and stepping back in resumes typing. The focus a page was left with comes back
  with it, so a panel opened from a draft lands in that draft's composer. A
  selection highlight and an open completion drawer are dismissed first; clearing
  the draft is `Ctrl+C`, never a second `Esc`.
* **A chord a text field does not use stays global.** The composer scope wins
  over `Scope::Global`, so binding a completion key to `Ctrl+P` silently killed
  the command palette inside every session. Completion navigation lives on
  `Alt+↑`/`Alt+↓` (and the arrow keys while the drawer is open), and `Ctrl+P`,
  `Ctrl+Q`, `Ctrl+G` and `Esc` resolve to their global intents with the composer
  focused.
* **Agent setup is visible where the reader is.** The composer's info line names
  the session's Agent and its `provider/model` plus the key that moves them
  (`Ctrl+G`); the picker opens with the cursor on the session's current choice
  and marks it, refuses an option the catalogue says is unavailable, and submits
  a compare-and-set switch. A backend that cannot switch runtimes gets a toast
  instead of an overlay whose Enter does nothing. The catalogue opens as a *list
  of Agents*: every group but the one in use is folded, because the catalogue is
  as long as the machine has models and the reader who opened the surface asked
  "what am I on"; `←`/`→` (and `Enter` on a heading) open and close the rest. The
  groups are published in the order the Desktop lists Agents in — the runtime's
  catalogue carries the shell's own sort strategy, manual drag and usage counts —
  so every client draws the same list.
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
  than swallowing the key.

  The run options a view lists are those of the row the reader is on, held in
  `RuntimePickerState::working` for as long as the picker is up: `Tab` seeds it
  from the catalogue row under the cursor without choosing it, and `Enter` on a
  row chooses it and moves straight to its options, so picking an Agent and
  saying how it runs are one visit rather than two. The pinned recent rows are
  the deliberate exception: a recent row *is* the reader's own answer read back
  to them — the Agent, the model and the run options they chose together — so
  `Enter` on one applies it and closes the picker, and `Tab` is what opens it
  for editing. An entry with nothing to tune closes the picker instead of
  opening an empty view.

  A row opens a value list whose first entry is the Agent's own default
  (`On`/`Off` for a switch, free text for a string feature, which the prompt
  overlay collects); `Esc` steps out one view at a time — value list, run
  options, catalogue, closed — and the value list carries the option with it
  rather than an index, so a catalogue read that lands while it is open cannot
  move a value onto another option. **A row that changes is a row that is sent**:
  there is no staged draft and no apply chord, because a setting that only lived
  in the client would be a lie about the session it names. Every change re-checks
  the value against the catalogue and re-uses `Effect::SwitchRuntime` — the
  composing page keeps it in `new_session_runtime` for the session it creates —
  and the composer's info line names the depth and mode in effect beside the
  Agent, because the question "what will this message be sent through" is
  answered there.
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
* **The session list is a page of sections, and it is the page.** Nothing draws
  a frame around it: `session_page` insets the band by one column and the header,
  the actions and the rows all paint straight onto it, so the two columns and two
  rows a border would take belong to the list. The view spends its first two
  lines on the page, each with a row of air around it: the name — `sessions_title`
  reads "Session list" — with one chip per state family the list holds
  (`◆ 1 waiting  ▶ 1 running  ◇ 5 idle`) right aligned, then `+ New session` as a
  click target with the control that answers the page on the right: the workspace
  the next session opens in, as a bracket hint with its key, also a click target.
  A control answers the pointer before the click does — `App::hovered_hint` is
  what the button reads to light its own surface. The location is deliberately
  *not* repeated here: the status band above owns it, and the same path in two
  adjacent rows reads as a broken page. Chips are counted through the filter
  *and* through folded headings — a heading that hides its sessions still holds
  them — and a family with nothing in it is not drawn as a zero. The action on
  the left is never the one that goes; a right-hand hint that does not fit whole
  is dropped rather than truncated, because half a key is a key the reader cannot
  use. A heading's rule comes from `glyphs::heading_rule`, so a legacy console
  gets a hyphen rather than a box-drawing character it cannot draw.
* **The tree is the list: there is no flat run to switch to.** The grouping
  toggle and the flat projection it chose between are gone, so `sidebar_view`
  always renders the arrangement's headings and the persisted arrangement no
  longer carries a `grouped` flag. A heading is a *control*: it closes with its
  count past the rule (`▾ vibex ────────────────── 3`, the rule in `gray_dim` and
  the count one step brighter) and keeps a row of air above and below it, so a
  click on it folds it on the *first* press — a tree folds on one click, and
  asking for two is how a reader concludes the list is inert. A session keeps the
  two-step contract, because opening one leaves the page. `App::sidebar_row_is_heading`
  is what the mouse layer asks before it decides which contract applies.
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
  pruned to the sessions the runtime still lists. A row with nothing to report
  is *one* line: the workspace is the heading it already sits under, so the row
  never repeats its directory, and a session blocked on the reader says so even
  with no echo to hang it on — that is the one thing on the row to act on. Every
  item keeps a row of air under it, because rows that touch are read as one
  block. Unread is likewise the client's own notion — a final `AgentMessage` for
  a session the reader was not looking at (`note_activity`), cleared by
  `open_session` — because the runtime does not track "read"; the authority's
  own unread set is folded in beside it. Marks are one column at every glyph
  tier and their shape carries the meaning, or a reader who cannot tell the
  colours apart loses the state: `★` pinned (ASCII `*`), `●` unread, the state's
  own `▶`/`✗`/`◇`/`◆`/`▤`, and `↻`/`↻N` for auto-continue. The state is *only* a
  mark: the words were removed once the shapes were learned, because they cost
  the title a third of the row and said nothing the mark did not.
* **The list's clock answers for the rows it draws.** The ring on a running row
  is the only thing on the page that moves, and the session wearing it is usually
  not the one the list has open — a reader scouting the list while another
  session works has to see that work turning. `sessions::session_state_turns_ring`
  is the one reading of which states wear the ring: the row draws its frame from
  it and `App::chrome_animating`/`App::is_animating` step the phase from it, so
  the row and the clock cannot drift apart by a state. The answer is read from
  the session set rather than from the rows the frame built — the window, the
  folds and the filter belong to the frame's own projection — so a folded row
  keeps the clock running rather than stopping a ring under a running one.
* **A row's own controls act on their row, not on the cursor.** Rename and
  delete are drawn at the right end of a session row while the reader is on it —
  the selected row, or the row under the pointer — *in the age's place*, flush
  with the edge the age ends at, and the age is not drawn under them: one column
  cannot answer "when" and "act on this" at once, and the desktop's own row
  hides the age behind the same pair. Every other row gives those columns back
  to its title, which is what keeps the list from becoming a field of buttons.
  Each control *is* the chord that runs it (`R`/`D`, from
  `Keymap::chord_for_in`), in the spelling the key bar and help page use: a
  terminal has no tooltip to hover, so a mark that did not name the key would
  only send the reader to the help page to decode it, and the same lookup means
  a rebind renames the button with no second table to keep in step. The row
  reserves whichever of the age and the controls is wider *before*
  the reader arrives, so a title does not reflow when the pointer lands on it,
  and the pointer resting on a control lights that one alone — the delete in
  `danger`, the rename in the accent — for the width of its whole target. Each is
  published as a `RowActionRegion` carrying its rect *and the row it belongs
  to*: the pointer is what chose the target, so the click moves the cursor to
  that row before running the intent — an intent that resolves its session
  through `App::list_session_target()` would otherwise rename or delete the row
  the cursor happened to be on. The keyboard is still the primary path and the
  same two actions: `r` and `d`, both in the binding table and the palette.
  A control nobody can see is not published — a row the window has scrolled past
  and a pair a narrow terminal has pushed out of the band are dropped rather
  than left as rects a click could land on. An intent with no binding has
  nothing to spell, so it gets no control: an unlabelled target would be a
  button whose only way to explain itself is a click.
* **Depth is drawn, for headings and for the sessions under them.** A heading
  indents two cells per level, and so does a session row: a project heading is
  the leftmost thing in its band, its folders and sessions start a step in, and
  a folder's own sessions a step in from the folder. A session row that ignored
  its depth would start left of the folder that holds it, which is what makes an
  arrangement's tree unreadable — the rows would look like one flat list with
  headings scattered through it. The indent is part of the title's budget (and
  of the published control columns), not padding painted over the row.
* **A delegated session is a row, one indent in from the session that owns it.**
  The runtime's root list deliberately omits a child a delegated task created,
  so the list is built from two reads: the root sessions it always had, and the
  ownership tree (`children::ChildSessions`, fed by `Effect::LoadSessionTree` →
  `AgentBackend::session_tree`). A session the tree says has children carries a
  disclosure before its title — a control that folds its branch on one press
  (`z`, or a click on the mark) and stays a row that opens on Enter — and its
  children hang under it with `delegated: true`, the `↳` mark in the column a
  root row spends on its selection bar, and the same state mark, age and echo
  every other row has. The tree is walked to whatever depth the runtime
  published, a filter reaches inside a folded branch the way it reaches inside a
  folder, and a heading's count includes the children it holds. Folding is the
  client's own display choice and rides `tui-sidebar.json`
  (`SidebarArrangement::collapsed_child_ids`), never the authority's
  arrangement: the desktop persists the same fact in its own UI state, and the
  two must not overwrite each other. A runtime with no ownership read says so
  once (`ChildSessions::mark_unsupported`) and the list goes on drawing exactly
  what it drew before. Pin and move refuse a child, because a branch follows the
  session that owns it; opening, renaming, archiving and deleting work by the
  same intents as any other row, and the Agent label, echo and unread marks are
  kept for it too.
* **The ownership tree is walked from both ends.** A session's own page reads
  its branch on open, so the dock names the sessions it delegated — a
  `DockSection::Children` section whose rows open that session — and the status
  band draws the way back up for a child: `▴ <main session>` with the `Alt+Left`
  chord, published as a click target like the Sessions entry. `Alt+Right` is the
  other direction: it opens the single child, or lands the dock cursor on the
  first of several, or becomes the read that finds out when the branch has not
  been read yet. Both chords stay bound for the whole scope — a key that comes
  and goes with the state is a key the reader cannot learn — but no surface
  *names* one with no subject: `App::ownership_key_applies` drops `Sub-sessions`
  from the key bar, the help page and the shortcut cheatsheet on a session that
  delegated nothing, and `Main session` on a root, the way the devices page
  already drops its verbs. The detail card carries the same fact as a `Main`
  field, so all three surfaces answer "where did this session come from" from
  one reading of the ownership edges.
* **The list is a window over lines, not a selection over rows.** A workspace can
  hold hundreds of sessions and a row can carry a detail card, so the page lays
  every row out, measures them in lines, and draws the window it is scrolled to
  (`App::session_scroll`, with `session_band_rows` published by the frame that
  drew it). The window follows the cursor while the cursor is what moves — a
  step, a click, a filter, all of which go through `set_selection` — and the
  window's own gestures hand it over: `PageUp`/`PageDown`, half pages, `Home`/
  `End` and the wheel move `session_scroll` and leave the selection alone
  (`session_scroll_manual` is what tells the two apart). The offset is clamped to
  the last page and snapped down to a row's first line, because a window that
  starts mid-row shows half a row at the top. Rendering the window by hand — a
  `Paragraph` of the visible lines rather than ratatui's `List` — is what makes
  this possible: `List` scrolls by item and cannot show the inside of a row
  taller than its area, which a session with an open detail card is.
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
  auto-continue fields, so it is the same preference on every surface; the
  shell-wide default (`autoContinueDefaultEnabled`) turns a project on before
  any per-project or per-session decision is consulted, so a project created
  after the switch was flipped continues its sessions here too. A suspension
  this client makes is local, because the protocol has no pause change — only
  the enable/disable that clears one.
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
* **An act on a session is published to the authority that owns it.** The TUI
  and the Desktop are two windows on one runtime, so an act one of them performs
  must not be invisible to the other. An act travels on the channel that fits
  it — the per-session marks ride `RemoteSidebarOrganizationMutation`, a row
  that appears or disappears rides the session streams:
  - *The unread mark, in both directions.* "Unread" is the authority's memory,
    and reading is an act of whichever window has the session on screen, so the
    mark is a value those windows agree on rather than a one-way notification.
    Both moves are one mutation, `SetSessionsUnread { session_ids, unread }`,
    because they are the same fact seen from two seats: a client that opens a
    session sends `unread: false` (the Desktop stops badging what was read
    there), and a client that draws a dot for a finished answer on a session
    nobody is looking at sends `unread: true` (the Desktop draws the same dot).
    One boolean rather than two mutations is deliberate: a mark is a value, not
    an event, so two clients racing to read and to be notified resolve by intent
    rather than by arrival order. A mark that already holds is an
    `AlreadyApplied` no-op, which is what keeps a finished turn from costing a
    write per turn. The unread set is stored on its own
    (`App::authority_unread_sessions`) rather than read out of the arrangement
    view: an arrangement that arranges nothing is deliberately dropped, and the
    marks have to survive that. A mark is not an edit of the tree, so it carries
    **no expected revision** — a rearrangement on the Desktop between the read
    and the mark must not refuse it or make a read look like a failed tree edit.
    The client also keeps its own optimistic set (`App::unread_sessions`), drawn
    the instant a completion arrives so the dot does not wait on the wire. The
    two sets are ORed for display (`session_is_unread`), so **the optimistic set
    must be reconciled down as well as up**: when a snapshot drops an id the
    authority previously held, that id is removed from both sets
    (`apply_sidebar_organization`). Replacing only the authority's copy is what
    left the TUI dot glowing after the Desktop opened the session and cleared the
    shared mark. An id the authority *never* held is left alone — that is a mark
    whose write is still in flight, not a stale one.
  - *Deleting a session.* A deletion deletes the row, so there is no surviving
    snapshot to broadcast. The manager publishes the removed ids on its own
    channel (`subscribe_session_removals`), the runtime fans them out as
    `DesktopEvent::SessionsRemoved` / `BackendEvent::SessionsRemoved`, and a
    client drops the rows, the open page and its own per-session state
    (`forget_removed_sessions`). A remote client learns the same news from the
    sidebar invalidation the runtime publishes with it and re-reads the list. The
    ids are published only after the deletion commits: a client told earlier would
    drop a row that then survived. A client that owns a Desktop drives the deletion
    through the same tombstone the local delete uses, so no read racing the
    deletion can rebuild the row.
  - *Archiving a session.* An archive keeps the row in the database but takes it
    off the sidebar, so it rides the ordinary session-update snapshot: the
    manager publishes the archived session on `subscribe_session_updates`, and the
    runtime fans it out as `DesktopEvent::SessionUpdated` beside the sidebar
    invalidation a remote client re-reads for. The authoritative root list omits
    an archived row by construction (`archived_at_ms IS NULL`), so a client that
    owns a Desktop — the seat that receives the snapshot — must apply the same
    rule to it. A live snapshot is the one row source that is not that list: one
    the sidebar accepts unconditionally is how a session the TUI archived stayed
    drawn on the Desktop with an "Archived" badge instead of leaving the way the
    reader's own archive takes it off. The in-process client therefore drops a
    snapshot that is not `deleted_at_ms.is_none() && archived_at_ms.is_none()` —
    through the same row teardown the optimistic removal uses — before it can be
    added or parked among the delegated rows, and a snapshot with the archive
    cleared is free to put the row back, exactly as the restore is.
  - *Creating a session.* A creation adds a row no client's list has yet, and the
    caller's reply reaches nobody else, so the manager publishes the committed
    snapshot on the session-update stream (`subscribe_session_updates`) — the
    same stream a rename, an archive or a turn boundary rides. The runtime fans
    it out as `DesktopEvent::SessionUpdated` and publishes the sidebar
    invalidation beside it, which is how a remote client learns to re-read its
    own. An in-process client answers a session it cannot place by marking the
    root page stale **and waking the ownership pass** (`delegated_refresh_wake`):
    the pass polls, and a creation that waited out the poll interval arrived
    visibly late in a sidebar the reader was watching. A session an owner is
    about to link under itself — a delegation, or a session an Agent creates — is
    the exception: creation does not announce it, and the owner announces it with
    `announce_created_session` once the ownership edge is committed. A child
    announced before that edge exists has nothing to place it with, and every
    client drawing a session list reads it as a navigation root. A client whose
    own creation reply races the announcement still ends up with a navigation row
    rather than a delegated one.
    - *The ownership page is not that list.* A tree page's registry describes the
      nodes on the page, and a node the page returns **without a parent edge** is
      an ownership root — exactly the set the root list holds — so it is never
      filed among the delegated rows. Filing it hid a row from the sidebar and,
      because a page can describe a creation the moment its row commits while the
      announcement waits out the event batch, registered the session the
      announcement was about: the announcement then read as news about a row the
      workbench already knew, so no list was ever reloaded for the row it was
      missing. A root the workbench has not met is instead answered like an
      announcement it cannot place — the root page is marked stale and the pass
      woken — and the row arrives from the root list, which is the only
      projection that adds one. A row a mutation in flight has taken off the
      sidebar is not a reason to re-read anything: the read filters it back out
      until the mutation settles.
    - *A gap in the fan-out is the same news.* A creation rides the fan-out
      stream, and no later event names the row it announced, so a receiver that
      lagged past it lost the only notice the sidebar would have drawn: a lagged
      fan-out re-reads the root list (`load_agent_overview`) instead of trusting
      some unrelated action to. A remote seat needs that read for the sidebar
      invalidation itself — a paired runtime announces a creation that way and
      through no other event — so the invalidation is recognized where the
      backend event is still visible, before the mapping to event types that have
      no twin for it drops it.
* **A running turn is what the spinner animates on.** `is_animating` is the
  single question the loop asks before repainting on its own, and it answers
  yes while a turn reads as running (`turn_reads_running`), not only while
  text streams: the quiet stretches — runtime startup, thinking, a slow tool —
  are exactly when a held frame reads as a frozen client. A question waiting on
  the reader and the composing page's mark animate too; everything else holds
  still, and that is what keeps an idle session at zero frames (the PTY
  `an_idle_interface_writes_nothing` contract).
* **Work records have the same layout while running and after completion.**
  Tool bodies stay behind their disclosure. Reasoning uses the preferences
  described above. A tool shows a recognized action, never raw JSON or XML as a
  fallback. Complete input followed by output can still supply the action;
  unknown or incomplete payloads remain available in details. Runtime attribution
  also lives in details. Failure stays visible and breaks grouping. Keep height
  estimates and cached neighbour gaps in sync with the rendered shape.
* **Expansion belongs to the reader.** `Transcript::set_blocks` retains it by
  stable block ID while content updates. A group retains its summary while
  exposing child headers, whose details expand independently. Closing the
  summary closes all child details. Thin connectors join both levels, and
  reversible disclosure motion respects the Motion preference. Group changes
  invalidate measurements and cached rows; pointer and keyboard actions share
  the visible header's intent. Keep search, copy, and selection consistent with
  [Timeline Presentation](../frontend/timeline-presentation.md).
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
  the Agent and model the page and the composer's info line name, and the entry
  the picker opens on and marks as current, all come from
  `App::page_runtime_selection()` — the page's own choice, or the entry a
  creation with no choice falls back to (`default_runtime_selection` mirrors the
  worker's own rule: first available, else first published). The run options a
  view lists come from `App::picker_selection()` instead: the row the reader is
  editing while the picker is up, and that same page selection otherwise, because
  the page's copy of a choice the picker just made catches up a round trip later.
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
  is nothing there for the page to name. A read that has not answered is not an
  answer either: `App::runtime_options_reading` keeps the Agent setup row saying
  it is *loading* rather than calling an unanswered read unavailable, and the
  reply — a catalogue, an empty one, or a failure — is what replaces it.
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
  belongs to both session pages — the prompt and a session's own page — and is
  drawn last so it holds the corner; it names the chord that answers where the
  keyboard is: `Ctrl+L` while the composer holds it, the global `1` once the
  reader has moved to the transcript, because the plain digits are characters in
  a draft. `Esc` returns to the page this one was opened from — a session opened
  from the list comes back to the list — while the prompt and the list still step
  between each other when nothing was opened before them.
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
* **Animation completion still needs a repaint.** The landing mark alternates
  moving passes with quiet intervals; composer labels animate only when their
  text changes. Before suppressing quiet repaints, paint the final resting mark
  or settled text. Keep text transitions scoped to the editor that owns them,
  including existing sessions. See the
  [TUI Session State contract](../frontend/tui-session-state.md) for scheduling,
  overlay, and composer-target rules and the required render assertions.
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
  `Effect::ReadClipboard` — one variant, carrying `ClipboardWanted` so the
  gesture that asked travels with the answer — runs on `spawn_blocking`, shells
  out to the desktop's clipboard tool with a deadline and a kill, and answers
  with an `AppMessage::Pasted` that names the gesture again. The read is
  *type-aware*: the owner lists what it offers and the bytes come back under the
  type it offers, because asking for PNG and nothing else finds nothing on a
  clipboard holding a JPEG. The client still *writes* the clipboard only over
  OSC 52.
* **An empty host clipboard is not yet "nothing to paste": the terminal may be
  holding it.** A client on the far side of an `ssh` link cannot read the
  reader's clipboard from the host — the screenshot is on the machine the reader
  is sitting at — so the loop asks the terminal itself, over kitty's OSC 5522.
  `terminal_clipboard::Probe` is that conversation and the loop drives it,
  because the answer arrives on the input queue `crossterm` reads and would
  otherwise deliver as keystrokes: while a probe is in flight the loop reads no
  keys and reads the queue at the byte level itself, which is the one moment the
  interface does not echo typing. The three steps are ordered by what the reader
  has to be asked about: the capability query (a DECRQM report nobody is asked
  about) so an unsupporting terminal is only ever waited on once per run, the
  type list (which a terminal must serve without a prompt) so a clipboard with
  nothing the gesture can use never causes one, and only then the read itself,
  which a terminal may confirm. `Ctrl+C` is handed back to the interface, noise
  in the queue is skipped rather than allowed to end the answer, and the
  answer's packets are reassembled per media type with the picture preferred. `TerminalClipboard::Refused` is its own outcome: a terminal
  that will not hand the clipboard over is reported as that, never as an empty
  clipboard, and everything degrades to "name a file instead".
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
  choice is.** The look, the language, reasoning preferences, the standing
  workspace and the palette's recents live in `InterfacePreferences` and are written to
  `~/.vibex/tui-interface.json` (the path an `AppOptions` field, like the
  arrangement's, so a test writes nothing into a home). `apply_setting_value` is
  the only writer and the only saver, so a preview that `Esc` reverts also
  reverts what was written. The theme is two slots — light and dark — because a
  palette is authored for one appearance: switching appearances reads the other
  slot instead of overwriting it, and the row shows the *resolved* id rather
  than a stored one that may belong to the other appearance. Choice values are
  optional strings and toggles are optional booleans. An absent or unknown
  choice resolves to its default, so a newer build's file is not an error in an
  older one. Resolution
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

## 8. Notifications

`crate::notify` is the out-of-band layer: it tells the reader about a session the
terminal is not showing. It paints nothing — every byte and every process it
produces leaves *after* the frame, through one door in the run loop — and it has
exactly two channels:

| channel | carrier | reaches |
| --- | --- | --- |
| terminal escapes | `BEL`, `OSC 2` / `9` / `777` / `99` / `1337` | the reader's own terminal, across `ssh` |
| desktop popup | `notify-send` (Linux/BSD) or `osascript` (macOS) | the host the client runs on, never a remote seat |

* **The escape is the only channel that survives `ssh`, and detection picks it,
  not a probe.** The client writes the bytes and the reader's terminal (or the
  multiplexer in front of it) interprets them. Asking the terminal what it
  supports would put a query in the input queue the interface is about to read,
  and the answer changes only the spelling of a sequence an unknown terminal
  ignores. `TerminalProfile::detect_from` is the table (`TERM_PROGRAM`, `TERM`,
  `KITTY_WINDOW_ID`, `WT_SESSION`, `VTE_VERSION`, …) and it is a unit test; an
  unrecognised terminal gets `OSC 2`, the title.
* **Detection is only as good as the environment, so a terminal host states its
  own identity.** The same table decides whether an Agent's mark is a picture,
  and the reader's environment describes the terminal the *host* sits in. A
  Vibex terminal tab is a cell emulator with no pixel layer, but it starts with
  the desktop's environment — kitty's `TERM`, a window id — so a client inside
  it concluded it could place images and every mark landed as blank cells. The
  emulator therefore publishes what it can draw
  (`vibex_terminal_ui::child_environment`) and the PTY host applies it when it
  spawns: `TERM=xterm-256color`, `COLORTERM=truecolor`, `VIBEX_TUI_IMAGE=off`,
  and the outer terminal's identity names cleared. The declaration belongs to
  the emulator rather than the host, because the grid is what a child writes
  into and a second host would otherwise reintroduce the bug. The fallback is
  not a degraded picture: the Agent's name stands in the same cells.
* **A multiplexer is respected.** tmux and screen swallow OSCs they do not
  implement, so inside one the default is the bell (tmux always forwards it and
  flags the window) and a configured OSC is wrapped in tmux's passthrough
  envelope. Without `allow-passthrough on` the envelope is discarded, which
  degrades to "no notification" rather than to stray text on the screen.
* **The desktop popup is fenced twice.** It is the one thing here that starts a
  process, so it never fires unless the reader turned it on *and* the seat is
  local: `SSH_CONNECTION` / `SSH_TTY` / `SSH_CLIENT` mark a remote seat, where a
  bubble would be drawn on the cloud host and the client would report success.
  The child is reaped on its own thread, and a missing program is a state the
  settings row states rather than a failure to retry.
* **One event is one reminder: the channels are routed, not stacked.** A reader
  who is *at* the terminal is told by the terminal channel; a reader who has
  switched away is told by the popup, which reaches across applications and
  carries words. That is the rule the desktop already applies to its own window,
  and it is what stops a bell from ringing beside a bubble. When no popup can be
  drawn — a remote seat, no program, the switch off, or a desktop on the other
  end — the escape carries the news in both focus states, because it is the only
  channel that survives `ssh`. Focus that was never reported counts as "at the
  terminal", so a terminal without DECSET 1004 keeps its escape and simply never
  raises a bubble. `Notifier::popup_carries` is the whole rule.
* **A desktop on the other end takes the heavy channels.** When the seat resolved
  `RemoteServerKind::Desktop`, that desktop shell announces the same events to
  the same reader, and no routing inside this process can see it.
  `NotifyPreferences::light_when_desktop` (default on) is the answer: the
  terminal channel is pinned to the title and the popup is left to the desktop.
  An `Off` transport stays `Off` — the policy is "lighter", never "louder than
  what I asked for" — and the settings rows say which of the two is speaking
  rather than naming a sequence that will not be written. `App::set_runtime_kind`
  is the one writer of the fact, because the app and the notifier each keep a
  copy of it.
* **The channel that carries a message also lays it out.** `NotificationMessage`
  keeps the Agent, the sentence and the session title apart: a terminal sequence
  gets them on one line (a control character inside an OSC payload can end it),
  while a popup gets `Vibex · <agent>` as its summary and the sentence with the
  session title on the next line. The parts are sanitized before they are placed,
  so the only line breaks in a body are the ones the layout put there. AppleScript
  has no escape for a newline inside a literal, so `SystemDelivery::OsaScript`
  joins one literal per line with `linefeed`.
* **A notification is news only when the reader is not already reading it.**
  `accept` is the one policy: the layer must be on, at least one channel must be
  live, the kind must be switched on, and the reader must not be looking at that
  session's own page with the terminal focused. Focus comes from DECSET 1004
  (tmux needs `focus-events on`); a terminal that never reports focus is assumed
  to be focused, so an unsupported terminal does not turn every failure into a
  bell.
* **The dedup key is the runtime's `(session, source_event_id)`.** The live
  stream, the polling fallback and the remote seat's notification intent all
  spell the same pair, so one failure is announced once; the ledger is bounded
  and consumed at `accept`, so a suppressed event does not silence a later
  duplicate.
* **The payload carries no Agent prose.** A terminal may log an OSC payload and
  a title is read by anyone glancing at the screen, so a notification is the
  kind plus the Agent's name, localized; the session title rides only when the
  reader opts in (`includeSessionTitle`). Every field is sanitized: control
  characters cannot close the sequence, whitespace collapses, and the length is
  bounded.
* **The settings page is a second door onto the same file.** The Notifications
  section of the settings surface (master switch, the three kinds, the transport
  chooser with an "automatic" value, the desktop popup, the light-reminders
  switch, the title, and a test button) edits the same `NotifyPreferences` the
  hand-written `~/.vibex/tui-notify.json` holds, and `apply_setting_value` is the
  only writer, so the page and the file cannot disagree. An unknown transport id
  in the file falls back to detection rather than to "off".
* **The path is the composition root's, like every other file.** `AppOptions`
  carries `notify_path`; `None` reads and writes nothing, which is what a test
  wants — a test that flipped a switch would otherwise edit the home of whoever
  ran it. `App::notification_preferences_path()` is what `lib.rs::run` names.
* **The test button goes through every live channel.** It skips the "is the
  reader looking" policy (a test is asked for, not news) but not the master
  switch, and its answer is a toast that says whether anything was sent — the
  point of the button is to make a terminal's or a desktop's silence
  attributable. A real event is routed to one channel; the test is the one moment
  the reader asked to see all of them, and the row that offers it names them.

## 9. Security

| Rule | Requirement |
| --- | --- |
| R1 | No API key, token, private key or stored provider secret is ever written to TUI configuration, logs, diagnostics or scrollback exports. A one-time pairing code is shown once, on the pairing panel that minted it, and never persisted. Leaving that panel — or the pairing half of the page — does not merely stop drawing it: the client asks the runtime to cancel the offer, because an offer the reader walked away from is still claimable until the runtime is told. A status read answers with the offer's summary and never with the code. |
| R2 | Stored credentials are never rendered back. The credential form is masked, explains that the value replaces the stored one, and clears the value after submission. |
| R3 | Destructive actions carry an audit row on the authority; the client only references ids. |
| R4 | Every mutation carries an `idempotency_key`; a conflict is surfaced as "already executed with different content", never swallowed. |
| R5 | Screen contents and terminal bytes are as sensitive as a prompt: never logged at `info`. |
| R6 | Permission levels (ReadOnly / ApproveOnly / FullControl) only *hide*; authorisation is the server's `remote_permission_denied`. The current level is always visible. |
| R7 | The client never opens the runtime database, except for the documented local bootstrap path. |
| R8 | Non-loopback transport is HTTPS/WSS. Pinned TLS is preferred; plain HTTP only on loopback with an explicit opt-in. |

## 10. Testing

| Layer | What it proves |
| --- | --- |
| Reducer unit tests | the intent → effect mapping is a pure function; key sequences drive state |
| `TestBackend` render tests | layout degrades correctly at 80×24 / 100×30 / 120×40 / 200×50, CJK wraps, colour-less mode still reads |
| Tiny-terminal render tests | every page paints a whole frame down to 1×1; a small terminal keeps a line of transcript and a composer that shows the draft |
| Contract tests | dependency boundary, key tables, locale coverage, no secret-shaped copy, docs exist per page |
| PTY end-to-end | the real binary enters raw mode, paints a first frame, writes zero bytes when idle, keeps in-process diagnostics out of the terminal, restores the terminal on exit, survives a resize storm, and takes a picture from a terminal that answers the clipboard protocol — and gives the input queue back when the terminal stays silent or refuses |

`cargo test -p vibex-tui` runs the first four. The PTY layer needs the harness
entry point, so it runs as
`cargo test -p vibex-tui --features pty-harness --test pty`; `pnpm check:rust`
covers both.

The idle assertion is load-bearing: it is the measured form of "no animation and
no events means no frames". A startup notice legitimately repaints while it is
visible, and the landing mark sweeps one pass as a greeting, so the test settles
until the process has been quiet and only then measures.

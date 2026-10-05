# Vibex TUI

The character-grid client for a Vibex runtime. It is the same client family as
the desktop and mobile apps: it renders authoritative state owned by a
`DesktopRuntime` and never opens the database, the browser runtime, or the
media stack itself.

```bash
vibex                      # attach to (or start) the runtime for this home
vibex connect vibex://…    # pair with a runtime and attach to it
vibex status               # report which mode this home would use
vibex status --home <dir>  # ... for another home, without attaching
vibex --home <dir>         # run against another home
vibex-server tui           # run the client from the server binary
vibex-desktop tui          # run the client from the desktop binary (Linux)
```

`vibex status` never attaches, so it is the first thing to run when something
is wrong — it prints the home, the flavour, the mode, the endpoint and the
reason, and exits zero even when the real run would fail.

### Homes and flavours

The leaf of the home path decides which runtime flavour the client starts:

| Home ends with | Flavour | Local listener |
| --- | --- | --- |
| `desktop-preview`, `desktop-rc`, `desktop-stable` | that desktop channel | the desktop Direct port, 1428 |
| anything else | server | 8765, or `VIBEX_BIND_ADDR` |

The flavour has to come from the path because the runtime refuses to start a
channel in a home that does not match it. `--home /tmp/scratch` is therefore a
throwaway server-style runtime, which is the quickest way to try the client
without touching a real home.

Local attachment never trusts the loopback interface on its own: the client
reads the runtime's identity public key from the home's own
`relay/desktop-identity.json` and compares it with what the runtime presents.
Plain HTTP is used only for a loopback route whose runtime does not terminate
TLS, and only with the explicit development opt-in.

## Packaging

The standalone client ships inside the desktop packages as a second, non-main
binary so a shell user never installs anything extra:

| Platform | How the client is reached |
| --- | --- |
| Linux | `/usr/bin/vibex` from the `.deb` / AppImage |
| macOS | `vibex` next to the `.app` bundle, symlinked onto `PATH` by the installer |
| Windows | `vibex.exe` from the NSIS installer; the GUI binary uses the `windows` subsystem and cannot host a console |
| Containers | `docker exec -it <container> vibex-server tui` |

## Modes

A client attaches in one of two modes, decided once before the interface starts.
The status band names the mode, and *Settings → Connection* shows the same word:

| Mode | When | Notes |
| --- | --- | --- |
| **Local mode** | nothing else owns the home | this process starts and owns the runtime; native steering is available |
| **Remote mode** | another runtime owns the home, or a link was given | the client is a network peer; steering degrades to interrupt + resend |

If the home is locked and the runtime is not accepting local clients, the client
prints three ways out: enable *Settings → Remote Access → Direct*, quit the
desktop app, or connect to another runtime with `vibex connect`.

## Screen composition

One vertical stack of full-width bands — status, transcript, turn status,
composer, and a shortcut bar on the pages that are navigated rather than written
on. There are no permanently-boxed side panes: every border costs two columns or
two rows that the content does not get, and on a fixed grid that is the whole
budget. The transcript is what is being read, so it takes the full width, and
message surfaces and expandable work rows give it structure.

Navigation that would otherwise be a permanent column is a full-screen view
(Sessions, Management) or an overlay, which is also the only way a session
row can show a title, workspace, state and age without truncating all four.

The transcript wraps to the band it is drawn in — the renderer states that
width every frame — so a wide terminal is filled rather than showing a narrow
column with an empty half beside it.

Optional bands (background tasks, queued messages, banners, the dock) collapse
to zero height rather than to a smaller size, and a short terminal drops them
before it touches the transcript or the composer. They belong to the session
view, so the session list does not inherit the active session's plan or turn
line: a global page keeps its own chrome.

There is no minimum terminal size. Below the size that holds the whole stack,
the outer padding, the status row and the blank rows between bands go too, then
the running-turn and queue bands, and finally the transcript keeps one line
while the composer shrinks to its own smallest form. Every size paints a whole
frame of the interface; none of them paints a notice about being small.

## Visual language

Your messages use a raised background and a prompt marker. Continuation lines
align with the first line's text, and Markdown punctuation in your input remains
literal. A message that scrolls out of view stays pinned above the transcript.
Agent replies render Markdown as text arrives, including wrapped code and tables.

Tool calls and reasoning use compact disclosure rows (`▸` / `▾`). Their layout
stays the same while running and after completion. Click a row or press `e` to
expand its details; raw payloads and reasoning paragraphs stay behind that fold.
Three or more consecutive records of the same action can share one row with a
`+N` count. Expanding the group reveals all members. Different actions, turns,
runtimes and failures remain distinct, and updates preserve your expansion choices.

**Layered surfaces.** Backgrounds step away from the canvas, so a tool body or a
raised row reads as a distinct plane rather than as more text on the same
canvas. The grey scale has three steps — dim for punctuation and chrome, medium
for muted body, bright for secondary labels — because one grey cannot do three
jobs without everything competing.

**The turn rail.** One tick per turn in the right gutter, in conversation order
rather than by scroll proportion, so it is a count of the session as much as a
map of it: a reader can see how many turns there are and which one the viewport
starts on. When there are more turns than rows the ticks window around the
active one. A session with no turns draws nothing there — a scrollbar in the
same columns would be counted as turns, which is the one thing the rail must not
do.

**Focus.** A pointer marks the selected transcript block. The prompt marker and
terminal caret show where typing lands. `Tab` switches between the transcript
and composer; `Up` in an empty composer enters the transcript at its last row.

**One chrome, many modals.** Every popup — the command palette, the Agent setup
picker, an approval card, a diff view, a confirmation — is drawn through one
chrome: the same border, the same title on the top rule, the same `[✗]` in the
same corner, the same inner padding, and a footer whose key hints are
bottom-aligned and centered. A modal declares how important it is (a palette, a
picker, a card, a document) and the geometry follows; on a compact terminal the
margins are given back to the content. Nothing draws its own box, so nothing
drifts.

**The transcript is a reading surface, not a buffer.** Three things follow from
that. A user prompt that has scrolled off the top stays pinned above the
viewport, shrinking toward its truncated height and pushed off by the next
prompt rather than overlapping it. A search (`/`) is a smart-case regular
expression whose matches are inverted in place, with the counter on the right of
the bar. And a drag selects text across lines, copying it over OSC 52 on
release — the same escape sequence the rest of the client uses, so it works over
SSH and inside tmux.

**One owner per row.** The banner row above the composer has four kinds of
claimant — a warning about the connection, a reminder about the composer's mode,
a tip, a transient note — and they are ordered. A tip never displaces a warning,
and a banner whose condition has gone withdraws itself on the next frame rather
than waiting to be dismissed. Clicking it dismisses it early.

**The mouse is a second path, never the only one.** Every click has a key
equivalent, and the frame publishes the rectangles that make the click possible:
the transcript, a list's rows, the queue, the composer, the turn rail's ticks,
the shortcut band's hints, the banner and a modal's close control. A click on a
hint runs the intent its key would have run; a click on a turn tick scrolls to
that turn; a click in the composer puts the cursor on the cell that was clicked.

**Glyphs have fallbacks.** Terminals disagree about more than colour: the
conservative Windows console does no font fallback, so a glyph outside its
raster font is tofu. Every chrome glyph declares an ASCII fallback and the width
it must keep, so a degradation never shifts the layout.

### Turn status

The row above the composer is where the interface reports what is happening:

```text
⠹ read src/net/upload.rs                                     1m12s ⇣12K
○ Idle                                                        1m12s ⇣12K
◆ 2 Approvals
```

It sits between the transcript and the composer because it must never scroll
away. Idle remains static. Running turns animate even while waiting for a tool or runtime startup.

### Composer

The draft sits on a padded background without a frame or repeated session title.
Its height follows wrapped text, and long drafts scroll to keep the caret visible.
An empty box names the draft's own vocabulary — `/ commands @ files $ skills`,
in the reader's language — because that is the one place a reader looks when
there is nothing to read. Below the box, one line names the Agent, model and run
options.

A paste of four or more lines (or one over 10 KB) collapses into a chip —
`[Pasted: 42 lines]` — instead of burying the draft. The chip is one object:
the cursor steps over it, one `Backspace` removes it, and pasting the same bytes
again expands it in place rather than adding a second copy. What is sent is the
original bytes, not the label.

`Alt+I` attaches an image: the client asks the desktop clipboard for one and
falls back to asking for a path when there is none. The clipboard decides what
kind of image it holds — a screenshot is a PNG, a picture copied out of a
browser is usually a JPEG — so the offered types are listed first and the bytes
are read back under the type the owner offers. The reader is whichever tool the
desktop provides: `wl-paste` on Wayland, `xclip` under X11, `pngpaste` or the
system script host on macOS, PowerShell on Windows. Each has a deadline, and a
machine with none of them has no clipboard images rather than an error.

`Ctrl+V` pastes: an image on the clipboard becomes an attachment, text goes into
the draft, and an empty clipboard says so. Terminals that paste by themselves
never send this key — that is what the terminal's own paste is for — so the
binding exists for the ones that forward it, where the picture a clipboard holds
would otherwise paste nothing at all. A terminal paste that names an existing
image file attaches it too, rather than typing the path into the prompt: quoted,
`file://`-prefixed, `~`-relative and backslash-escaped paths all count, several
pictures in one paste attach together, and a path mentioned inside a sentence
stays a sentence.

The image becomes a second kind of chip, `[Image #1]`, numbered monotonically
for the draft and capped at ten per prompt. The info line under the draft
reports how many are attached.

The label is stripped from the message text — the Agent is not told about a
placeholder it cannot see — and the picture travels as an attachment that names
both the bytes and its place: `inline_text_offset` is where the chip sat in the
text, in UTF-16 units, which is what puts the picture back between the words
rather than at the end of the paragraph on every other client. A file the
runtime can read is sent as an absolute `file://` URI, the form the desktop
writes itself; a clipboard image in local mode is written beside the
message and sent the same way, because a path is the only form the desktop can
draw. In remote mode, where no file of ours is reachable, the bytes travel as
a data URL that the runtime materialises on its own host. A message pulled back
out of the queue puts its chips where they were, not stacked at the end.

The info line under the draft is right-aligned: the runtime a message will go
through, the Agent and model it names, and the key that switches it sit against
the box's far corner, where a reader looks for them, and the rule on the left is
the line the eye follows into the prompt. Attached images, a waiting approval
and the multi-line hint ride the same line, and a terminal too narrow for all of
it loses the hints before it loses the runtime.

Scrolling stops at the bottom of the session: the last line of the transcript
rests on the last row of the band, and reaching it resumes following the tail.
The wheel and the keyboard share that ceiling, so neither can walk the viewport
into blank space, and a scroll starts from the row the frame is actually showing
— following the tail is a position, not the absence of one, so the first scroll
after opening a session is a step rather than a leap to the top.

A draft can also be selected. `Shift` plus the motion keys extends the
selection, `Alt+A` takes the whole draft and `Alt+C` copies the selected part;
the mouse works the same way — press, drag, release copies — and typing over a
selection replaces it. A selection that touches a collapsed chip selects the
whole chip, so a cut can never leave half a `[Pasted: …]` marker behind.

Typing `? ` turns the composer into a filter over the messages you have sent;
`↑↓` walks the matches and `Enter` recalls one into the draft. `Up` on an empty
draft still steps through history one entry at a time.

Opening a session puts the caret in the composer, and a click anywhere in the
box takes the keyboard (a click on a text row also places the caret). The
terminal's own cursor is drawn on the draft, so where typing will land is
visible rather than inferred.

Editing is readline-shaped, because that is the muscle memory a terminal user
already has:

| Keys | Effect |
| --- | --- |
| `Ctrl+Z` / `Alt+Z` | undo / redo, one step per word typed, paste or kill |
| `Ctrl+K` / `Ctrl+U` | kill to the end / start of the line |
| `Ctrl+W`, `Alt+Backspace` | kill the word before the cursor |
| `Alt+D` | kill the word after the cursor |
| `Ctrl+Y` | yank the last killed text back |
| `Alt+B` / `Alt+F`, `Ctrl+←` / `Ctrl+→` | move by word |
| `↑` / `↓` | move through draft lines; `↑` in an empty draft enters the timeline |

There is one kill buffer rather than a ring: a ring is a second thing to learn
for a case that a terminal rarely reaches. Undo coalesces consecutive typing
into one step and breaks the batch on a cursor move, so undo removes what was
just typed rather than moving text out from under a cursor placed on purpose.

### Running work

Tools and reasoning keep the same compact rows while running and after completion.
Tool rows show a recognized command, path or query; reasoning rows show a label.
Click the header or press `e` to reveal the complete content. `Enter` opens the
selected block's details. Expanded content stays open while new events arrive.
Consecutive identical actions may group with a `+N` count; failed records always
remain visible. Runtime attribution is available with expanded details.

Agent replies stream in full. If events are missed, the client fetches authoritative
history before resuming updates, without sending your message again.

### Markdown

Agent messages are markdown, and the interface interprets it rather than echoing
it. Headings are bold (the top two levels also underlined, so the hierarchy
survives a terminal whose CJK font has no bold face) with no `#` markers; inline
code is a background run rather than a pair of backticks; links show their
destination only when the label does not already say it; lists get markers and
task boxes; a table is a closed box whose columns are measured in cells, so a
double-width character cannot push the border out of line; fenced code keeps its
own spacing and is coloured by a small per-language palette.

Four roles carry the hierarchy, so a message is never one flat colour: body
prose sits one step below `foreground` (`gray_bright`), headings and emphasis
take the brightest `foreground`, literals — inline code, commands, paths — use
the theme's literal colour (amber in the default theme), and link labels get
their own role derived from the catalogue's chart series (cyan) rather than
`accent`, which several themes resolve to plain foreground and would leave a
link looking like underlined prose. Chrome — block headers, markers, rules,
the URL beside a link — stays in the grey steps and never competes with the
text it labels.

Styling survives line breaks: the wrapper stays the authority on where lines
break, and each visual line is matched back onto the styled runs, so emphasis or
code that lands across a wrap keeps its colour. Content whose spacing *is* the
layout — code and diffs — keeps its spacing. Table cells wrap inside their
columns; very narrow tables become vertical records so cell contents remain readable.

### Status bar

The workspace appears on the left. Connection warnings, context usage and the
current mode appear on the right. A ready connection does not label a running
turn as complete; the turn status row reports whether work is active. On the
session pages the group's last segment is a control rather than a status: the
clickable **Sessions** entry, which names the chord that answers where the
keyboard is.

The location is chrome, not a status: it is drawn in the bright grey a secondary
label gets, so colour on that row means something happened rather than where the
reader is. A path too long for its third of the row folds its folders to their
initials first — `/h/p/c/p/c/node_modules` — and only drops the head behind an
ellipsis when even that will not fit, because the tail is the part that names
the directory.

### Composer modes

The prefix says what the draft will do, in the place the reader is already
looking:

| Prefix | Mode | Sends as |
| --- | --- | --- |
| `❯` | normal | a message to the Agent |
| `!` | shell | a shell command |
| `?` | history search | a query over sent messages |

### Completion

Typing `/`, `@` or `$` opens a drawer above the composer: two full-width rules,
no corners, the match count on the top rule, and the composer's own arrow as the
selection marker so the highlighted row lines up with the text being typed.

### Shortcut band

Keys and labels are separated by middots. The row belongs to the pages that are
navigated — the session list and the management pages — and is not drawn on the
pages a message is written on: a session page spends that row on the draft
instead, and everything the band would advertise is a `?` away. Where it is
drawn it prioritizes the actions available at the current focus: send and
newline while composing, expand, details and reply while browsing the
transcript. Agent setup remains available through `Ctrl+G`. Hints use the
current keymap, and clicking a hint runs the same action as its key.

## Pages

### Sessions

The list is a page of sections, and it is the page: nothing is drawn around it,
so the rows and their headings own the columns and rows a border would have
taken. Its first two lines are the page's own, each with a row of air around it.
The first names the page — `Session list` — and counts what it holds, one chip per
state (`◆ 1 waiting  ▶ 1 running  ◇ 5 idle`) on the right; the location stays in
the status band above rather than being printed twice in two rows. The second is
the one action a session list needs — `+ New session` on the left, which carries
its own surface while the pointer rests on it — with the control that answers the
page on the right: where the next session opens, as a bracket hint with its key
(`[Workspace Ctrl+W]`). The chips are counted through the filter and through
folded headings, so a collapsed group still says how much work it holds, and
every control is a click target. The action is never the one that goes; a
right-hand hint that does not fit whole is dropped rather than truncated, because
half a key is a key the reader cannot use.

The list itself is a window over every row, measured in lines rather than in
rows: a workspace can hold hundreds of sessions and a row can carry a detail
card, so nothing fits the page at once. The window follows the cursor while the
cursor is what moves — a step, a click, a filter — and the page keys, `Home` and
`End`, and the wheel move the window instead, leaving the cursor where the reader
put it. The window never starts mid-row: a half row at the top reads as a torn
frame.

Under them come the sections the reader arranged — the desktop's folders,
projects and order — each with a heading that closes with its count past the rule:
`▾ vibex ──────────────────────────── 3`. The rule is in the dim step, the count
one step brighter, and the heading keeps a row of air above and below it, so a
stack of rows reads as groups rather than as one run. There is no flat list to
switch to: the tree is the list. A single click on a heading folds it — it is a
control, not a row to open — while a session keeps the two-step contract, because
opening one leaves the page. `/` filters, and a search opens closed folders rather
than hiding what it matched.

A session with something to report is two lines, and keeps a row of air under it.
The first carries the mark, the title, the Agent answering it after a middot, the
marks it has earned and a coarse right-aligned age:

```
▶ fix the flaky test · Claude Code                                          3m
  Ran cargo test --workspace and it passed
```

The second line says what the session last did, in the dim step; a session
blocked on the reader says so instead — `Pending:` in front of what it said, or
the blocked-until-you-answer line when this client has not seen it say anything —
because that is the one thing on the row to act on. The line is the client's own
reading of the events it already receives: the Agent's messages, tool calls,
commands, file operations and errors, never a message still arriving and never the
reader's own words, which are usually the title anyway. A session with nothing to
report is one line: the workspace is the heading it already sits under, so the row
does not repeat its directory. Rows are not all the same height, so the frame
publishes how many lines each drawn row takes and the mouse maps a click through
that rather than through the line number.

- **The Agent's name** follows the title: "who is answering" is the first thing a
  reader scanning several sessions wants, and a name is one word where a coloured
  initial was a guess.
- **The unread dot** (`●`) appears on a session whose answer finished while the
  reader was looking elsewhere — the client's own notion, cleared by opening it.
  The dot and the pin are different shapes, not only different colours: a
  monochrome terminal has to tell them apart.
- **The pin** (`★`, or `*` on a terminal without box drawing) marks a session
  the authority has hoisted above the rest. It sits beside the age rather than
  leading the row: the leading columns belong to the state and the title.
- **State** is one mark, not a word: `▶` running, `✗` failed, `◇` idle, `◆`
  waiting on the reader, `▤` archived. The shapes are a small vocabulary the
  reader learns once, and spelling them out cost the title a third of the row
  for information the mark already carried. The shape carries the meaning on
  its own; the colour says it a second time, and the header's chips count them.
- **Auto-continue** is a mark of its own beside the age: `↻` when the session
  will continue itself, `↻3` while it counts down. `t` works it: it stops a
  countdown, resumes a suspended session, or switches auto-continue on or off —
  each of those is a control the desktop has.
- **The time** is coarse — `now`, `3m`, `5h`, `2d` — because a list separates
  "a moment ago" from "a while ago", and a timestamp to the second is a column
  of noise.

Order and folders are the desktop's, because they are the desktop's to own: the
list draws the arrangement the authority publishes — its folders (nested, with
whatever is folded), the order of projects and sessions, the pinned band, and
the unread marks the desktop has already cleared. `Enter` opens, `n` starts a
new one, `r` renames, `f` forks, `a` archives, `d` deletes, and `Ctrl+A`
includes archived sessions. Nesting is drawn rather than implied: a project is
the leftmost thing in its band, its folders and sessions start a step in, and a
folder's own sessions a step in from the folder. The row the reader is on — the
cursor's, or the one under the pointer — carries its two controls where the age
is: `R` renames and `D` deletes. The control *is* the chord the key bar and the
`?` help name — there is no tooltip to hover in a terminal, so a mark that did
not name the key would only be a puzzle — which also means a rebind renames the
button. The age is not drawn under them — one column does not answer "when" and
"act on this" at once — and it comes back when the reader moves on. A click on
either control runs the same action as the key.

### Auto-continue

A session with auto-continue on continues itself when a turn stops without an
answer: the Agent errored, or went idle without producing a final message. The
rule is the desktop's — the same predicate decides whether a turn needs
continuing, the same reading of the timeline decides whether it ended normally,
and the same five-second countdown precedes the send — so the two surfaces
agree about which sessions are running themselves.

The countdown is the reader's escape hatch: a message that appears by itself
with no warning is indistinguishable from a runaway Agent, so the row shows
`↻5`, `↻4`, … — and inside the session it rides the composer's info line, where
the desktop puts its own Continue button — while `t` stops it. Stopping a turn (`Esc` on a running session)
suspends auto-continue for that session rather than letting it restart the turn
the reader just cancelled; sending a message resumes it; a continuation that
has already gone out is not sent twice for the same turn. A session waiting on
the reader (`◆`) is never continued: answering a question is the reader's job.

The preference itself travels in the sidebar arrangement's auto-continue
fields, so switching a session on here switches it on in the desktop, and a
project whose default is on continues its sessions here too. With no authority
to write to (no runtime arrangement loaded), the switch is this client's alone
for the run.

`p` pins the selected session, `Enter` on a heading folds it, and
`Alt+↑`/`Alt+↓` move a session through the order; all three are sent to the
authority, which owns the tree and answers with it, so the desktop shows the
change too and the next desktop-side edit shows up here (the runtime publishes a
sidebar invalidation, and the client re-reads). The answer carries the revision
of the tree it now holds, not the one from before the change: that revision is
what the client echoes with its next change, so a stale one would make every
second keystroke look like a race. Pinned rows always sort first,
so a move across that line is refused with a message rather than silently doing
nothing. When the authority refuses a change — a tree that moved on, a move it
will not make — the refusal is reported and the tree is re-read.

When no arrangement is available — no runtime yet, a runtime with no desktop
attached, or a reader who has never arranged anything — the list falls back to
projecting the sessions themselves: pinned first, then recency, grouped by
workspace and project. That fallback arrangement (pins, order, folded headings)
is the reader's own preference, so it is written to `~/.vibex/tui-sidebar.json`
beside the key file; a client with nowhere to write keeps it in memory for the
run. Session creation picks a workspace through
the authority's own directory browser — `Ctrl+W` (or `b` on the session list)
opens the picker over the runtime's listing, `u` climbs out of a directory,
`Enter` chooses — so a remote client chooses a path that exists where the Agent
runs, rather than typing one from memory. The picker answers the page that
opened it: a reader writing a new session stays on that page with the directory
chosen, and one who opened it from the list stays on the list.

### New session

The page the client *opens* on: running `vibex` in a directory puts that
directory's prompt in front of the reader, because that is what they came to do.
The workspace is the directory the client was started in — the answer they
already gave by choosing where to run the command — and the runtime is the one
last used here, so both are named and ready before a key is pressed. The
sessions that already exist are one gesture away: the status band's top-right
corner holds a clickable **Sessions** entry naming the chord that answers where
the reader is (`Ctrl+L` while the composer holds the keyboard, the global `1`
otherwise). `n` opens the same page from the list.

`n` does not ask a question: it opens a page. The reader who asked for a session
asked to *write*, so the page hands them the composer with the mark above it and
the Agent the message will go through named under that — the Agent and model,
the workspace, and the keys that change both (`Ctrl+G` for Agent setup, `Ctrl+W`
for the directory). The draft's vocabulary is not repeated on the page: the
empty composer under it spells out `/ commands @ files $ skills` itself, and the
box is read every time it is written in where the hero is read once.

The session is created by *sending*, not by answering a dialog: the title comes
from the message, which is where a title comes from anyway. Sending leaves the
page at once — waiting there for the runtime to make the session reads as
nothing having happened — so the reader lands in the session view with their
message already on its timeline and the turn already running. The view is
emptied of the session they came from first: it is a *new* session they are
about to be in, and its history is not the old one's. If the creation fails, the
draft goes back into the composer and the reader goes back to the page. A runtime chosen on
the page is the one the session is born with — created with it, rather than
moved to it afterwards — and `Esc` leaves the page with the words still in the
box, so the gesture is repeatable.

The page names what it will create, at every step: the Agent and model on the
page, on the composer's line and in the switcher's caption are read from the
page's own choice, never from the session the reader came from, and that same
choice is what the creation carries. `Ctrl+G` opens Agent setup: the catalogue
opens as a list of Agents with only the one in use unfolded, published in the
order the Desktop arranges them in (its sort strategy, its manual drag and the
usage it counts), so `↑↓` walk the Agents, the pinned recent rows and — once a
group is opened with `→` or `Enter` — the accounts and models under them. The
row in effect is marked, and the pinned rows carry what a heading would have
said: the Agent they belong to. How the row under the cursor would run is the
line beneath the list, which names the values themselves without repeating the
run-option view's labels.
`Enter` on a row chooses it *and* moves to its run options, so picking an Agent
and saying how it runs are one visit rather than two; `Tab` opens the run
options of the row under the cursor without choosing it. Either way the run
options are sent as they are set — thinking depth, conversation mode, then the
session features that Agent advertises — with no apply step to remember. A page
that has chosen nothing names — and creates with — the catalogue's first
available entry.

The mark is drawn from characters — block glyphs where the terminal has them,
two rows of ASCII where it does not — and a light sweeps across it while the
page waits. It is the client's only animation that is not a turn's spinner, it
is a greeting rather than a heartbeat — one pass, then the mark rests — and it
stops the moment the reader leaves. A prompt left open therefore costs no
frames, which is the idle contract the PTY layer measures.

`Enter` puts the message in the transcript immediately. The runtime owns the
timeline, so its own copy of the reader's message is a round trip away — and a
client that waits for it looks like one that dropped the message. The send is
projected locally as the row it will become, and the turn line reads running
from the moment Enter is pressed rather than from the moment the runtime
answers. The projection is dropped as soon as the echo lands (the reader sees
one message, never two), when the send is refused, or after ninety seconds —
whichever comes first. Nothing else is released into that gap: a held message
waits for the turn the runtime reports, not for the one the client hopes for.

A message written while a turn is running is *held*, not dropped: it waits in
the queue band above the composer until the turn ends, and then goes out on its
own. The queue belongs to the session it was written for — leaving for another
session and coming back finds it exactly where it was, the band shows only the
open session's rows, and a message is never released into a session it was not
written for. A message released while the reader is elsewhere announces itself,
because that is the only way they can learn it went.

`e` opens a detail card under the selected row — id, workspace, state, agent,
model when it is known, the timestamps, and the message and turn counts for the
open session. `c` closes every open card and `y` copies the selected session's
details.

### Agent

The transcript is a block list with a streaming tail. `e` folds the selected
block, `F2` folds them all, `Ctrl+E` toggles reasoning, `y` copies a block body
and `Ctrl+Y` its metadata. `Enter` opens a block's details. The composer takes
`/` commands, `@` files and `$` skills, `Enter` sends, `Shift+Enter` breaks the
line — `Ctrl+J` does the same on a terminal that cannot report `Shift+Enter`,
see [Terminal ownership](#terminal-ownership) — `Ctrl+O` hands the draft to
`$EDITOR`, and `Ctrl+S` steers a running turn. `Ctrl+C` clears the draft, then
interrupts, then offers to quit. The status band's corner keeps the prompt's
**Sessions** entry, so another session is one gesture away without leaving the
draft behind.

`/` on the transcript opens a search: a regular expression, case-insensitive
until it contains an uppercase letter, highlighted in place. `Enter` keeps the
matches and `n` / `p` step through them, wrapping at the ends. Invalid patterns
say so rather than silently matching nothing.

Opening a session hydrates a window rather than the whole archive, so scrolling
to the top of the transcript (`PageUp`, `Ctrl+U`, `Home`, the wheel) fetches the
next older page, and `u` asks for one explicitly. The page is prepended above
the viewport and the reader keeps their place: the block they were looking at
stays on the first visible line. A backend that predates the cursor answers with
the newest page instead; the client detects that and stops asking rather than
prepending the wrong end of the conversation.

A drag with the mouse selects text over as many lines as it covers, scrolling at
the edges, and copies on release. A double click takes the whole word, so
`src/net/upload.rs` arrives in one piece. `Esc` dismisses the highlight; `y`
copies the selection again if the clipboard was clobbered.

Pending approvals appear as a card: `a` allows, `d` denies, `Ctrl+A` allows for
the rest of the session, and a digit answers with that specific advertised
option. Elicitation forms are filled field by field and submitted with `s`.

### Management

Agents, Providers, MCP servers, Skills, Prompts and Devices.
Every row shows its availability: an operation the authority does not offer, or
that this device's grant does not permit, is visible and explains itself rather
than disappearing.

Stored credentials are never displayed. Setting one opens a masked field that
explains the value is sent to the runtime and cleared locally.

### Devices

Pairing issues a one-time code with a scannable link, shows it once, and never
writes it to logs or configuration. Devices can be renamed and revoked, and the
remote-protocol audit trail is readable here.

### Usage

Per-session and aggregate token counts. Vibex records tokens only — there is no
cost, price or currency in the data model, so the page does not invent one.

### Settings

Theme and its dark/light mode (20 shipped themes), language (`en`, `zh-CN`,
`zh-TW`), icon set, the default workspace for new sessions, and the
key-binding file. The page is one surface with four modes rather than four screens:

| Mode | Entered by | What it does |
| --- | --- | --- |
| Browse | default | `↑↓` moves, `Enter` opens the row, `Space` toggles, `d` resets |
| Filter | `/` | Typing narrows the rows; `Enter` keeps the query, `Esc` clears it |
| Picking | `Enter` on a choice | `↑↓` previews the value live, `Enter` keeps it, `Esc` puts the old one back |
| Editing | `Enter` on a text row | Type the value; `Enter` saves, `Esc` discards |

A reset asks first and then restores the shipped default.

Every value the page changes is remembered for the next run: the appearance,
the theme, the icon set, the language and the default workspace are written to
`~/.vibex/tui-interface.json`, beside the arrangement and the key file. The
theme is kept per appearance — a light palette and a dark one are two choices,
so switching to light and back finds the dark one where it was. The command
palette's **Recent** section is the same file: the commands the reader reached
for last lead the list next time. A client with nowhere to write keeps the
values for the run.

Precedence is deliberate: a value named for this run — `--theme`, `VIBEX_THEME`
or `VIBEX_TUI_ICONS` — wins over the remembered one and does not rewrite it,
the reader's remembered choice wins over what the process locale or the
terminal suggests, and the shipped default answers last.

`Enter` on the Key bindings row opens the editor: every binding, grouped by
scope, with the same `/` filter. `Enter` on a row captures the next chord,
`d` puts that row back on its shipped chord and `s` writes
`~/.vibex/tui-keys.toml` (also reloaded by `F9`). A chord another action already
owns is refused and the owner is named — dispatch takes the first match in the
table, so accepting it would silently disable the other action. Rows moved off
their default carry a marker, and the title shows whether there are unsaved
changes. The interface reports which lines of the file it could not use instead
of failing to start.

### First run

With an empty session list the surface is a short ordered guide: connect to the
runtime, choose where the Agent works, start a session, write the first message.
The workspace step is already answered when the client was started in a
directory — that directory is the workspace — so the guide opens on the session
step and the earlier ones are shown as done. Each step's state is derived from
what the client already knows, so nothing is persisted and nothing has to be
dismissed — the guide retires by itself once all four are done, and comes back
into an empty home where it is useful again.

### Help

`?` opens the shortcuts cheatsheet: every binding, grouped by category (Global,
Transcript, Composer, Modals, Workbench, Management, Panels). `/` filters it,
`←`/`→` or `Enter` folds the category the cursor is on, and the highlighted
binding explains itself on the line below. It is generated from the same binding
tables that dispatch the keys, so a hint cannot describe a key that does
nothing.

### Command palette

`Ctrl+P` opens a fuzzy-scored list, grouped the same way as the cheatsheet.
Commands you have run are lifted into a `Recent` section on the next open, and
they break ties among equally good matches, so the palette converges on the way
you actually work.

### Queue

A message written while a turn is running is held rather than interleaved with
work already in flight. The queue band shows what is waiting, and `Alt+↑↓`
picks a row, `Alt+E` pulls it back into the draft, `Alt+X` drops it, `Alt+J/K`
reorders it and `Alt+Enter` interrupts the turn and sends it now. The queue
drains itself when the turn ends, and a held message carries its images with it
— "send this later" sends what was composed.

### Dock

`Alt+D` opens a panel directly above the composer that answers "is anything
still running?" without leaving the draft:

```text
◈ Running   Alt+J/K move · Alt+G open · Alt+H hide done · Alt+D close
  ▾ Agents 1
    ⠹ reviewer   reviewing the parser change
  ▾ Plan 1/3
    ✓ read the design
    ⠹ write the band
  ▾ Held 1
    #1 fix the flake
```

Sections are `Agents` (delegated children, one row each), `Plan` (the current
plan's steps) and `Held` (the queue). The list is derived from the transcript
the reader can already see, so it cannot disagree with the page behind it: a
plan step comes from the structured plan item when the runtime publishes one and
from the plan block when it does not. `j`/`k` or `↑`/`↓` move, `Alt+G` jumps to
the row's block — or takes a held message back into the draft — `Alt+H` hides
finished work, and `Enter` on a heading folds a section. While the dock is open
it replaces the plan and queue bands rather than saying the same thing twice,
and `Esc` closes it.

## Environment

| Variable | Meaning |
| --- | --- |
| `VIBEX_HOME` | runtime home (default `~/.vibex/<channel>`) |
| `VIBEX_CHANNEL` | `stable`, `rc`, or `preview` |
| `VIBEX_THEME` | theme id, for this run (outranks the remembered choice) |
| `VIBEX_TUI_COLOR` | `truecolor`, `ansi256`, `16`, `none` |
| `VIBEX_TUI_ICONS` | `auto`, `emoji`, `ascii` |
| `VIBEX_TUI_KEYS` | key-remap file (default `<home>/tui-keys.toml`) |
| `VIBEX_TUI_INTERFACE` | interface-settings file (default `<home>/tui-interface.json`) |
| `VIBEX_TUI_LOG` | spill file for process diagnostics (default `$TMPDIR/vibex-tui-<pid>.log`) |
| `NO_COLOR` | disable colour entirely |

## Terminal ownership

The client draws on the alternate screen, so it owns the terminal for as long as
it runs — including `stderr`. Local mode boots the runtime *in this
process*, and the runtime reports startup stages from background tasks that
finish after the first frame is painted. A line like that landing in a frame
wraps at the last column and scrolls the grid, which is the log residue that
must never appear under the interface.

So writes aimed at `stderr` are diverted into a spill file while the client owns
the screen, and the terminal's own `stderr` is handed back on exit. Nothing is
dropped silently: the client names the file when it exits, and the file is
`VIBEX_TUI_LOG` when that is set. Remote mode writes nothing at all, so no
notice is printed and no file is named.

Ownership covers the keyboard too. On entering, the client pushes the terminal's
keyboard protocol — disambiguate escape codes plus alternate keys — and pops it
on the way out. Until a terminal has been asked for it, `Shift+Enter` is not a
key it can report: the same carriage return arrives for `Enter` and
`Shift+Enter`, which is why the newline chord needed the push. A terminal that
ignores it keeps its legacy keys, where `Ctrl+J` — a line feed, which every
terminal sends as a byte of its own — is the newline chord, and where a terminal
can be configured to send it for `Shift+Enter` itself.

The flags are pushed optimistically rather than probed. The query for them
travels through the same input queue the interface is about to read, so asking
first would cost a stalled startup on every terminal that answers the companion
device-attributes query but not this one — and the answer would change nothing
but the wording of the key bar.

## Degradation

Every one of these has a defined behaviour rather than a broken screen:

| Condition | Behaviour |
| --- | --- |
| stdout is not a TTY | no raw mode; a clear message and a non-zero exit |
| terminal below 60×16 | the bands around the conversation are given back; the transcript and the composer stay, and the frame is never refused |
| in-process diagnostics | diverted to a spill file, named on exit; never drawn into a frame |
| `NO_COLOR` | glyphs and indentation carry the structure; colour is never the only signal |
| non-UTF-8 locale | ASCII borders and markers |
| sixteen colours only | the page keeps the terminal's own background; the palette carries ink, accents and rules, and no plane is painted |
| no keyboard protocol | `Shift+Enter` arrives as `Enter`; `Ctrl+J` breaks the line, and the help panel lists it |
| disconnected | a banner, mutations disabled, the last known state marked stale |
| read-only device | actions are visible, disabled, and say which permission they need |
| very long conversation | the transcript is capped and the oldest blocks are dropped |

The colour tier is chosen once, at startup: `NO_COLOR` first, then an explicit
`VIBEX_TUI_COLOR`, then `COLORTERM`, then `TERM`. A `TERM` that names a 24-bit
terminal — `xterm-kitty`, `xterm-ghostty`, `wezterm`, `foot`, `contour`,
`alacritty`, or a `-direct` entry — is believed on its own, because `COLORTERM`
is not always there to be read: `sshd` forwards only the names it is configured
to accept, so the terminal's own name is what survives a link.

## Previewing

A fixture session renders to stdout, which is how the layout is reviewed:

```bash
cargo run -p vibex-tui --example preview -- 150 44
cargo run -p vibex-tui --example preview -- 100 30 --light
cargo run -p vibex-tui --example preview -- 150 44 --ansi    # real SGR codes
cargo run -p vibex-tui --example preview -- 150 44 --no-color
cargo run -p vibex-tui --example preview -- 120 34 --settings
cargo run -p vibex-tui --example preview -- 120 34 --welcome
```

## Testing

```bash
cargo test -p vibex-tui
```

Five layers:

| Layer | Command | What it proves |
| --- | --- | --- |
| Reducer | `cargo test -p vibex-tui --lib` | the intent → effect mapping is a pure function |
| Render | `cargo test -p vibex-tui --test render` | layout degrades at 80×24 / 100×30 / 120×40 / 200×50, CJK wraps, colour-less mode still reads |
| Tiny terminal | `cargo test -p vibex-tui --test tiny_terminal` | every page draws a whole frame down to 1×1, and a small terminal keeps a line of transcript and a composer that shows the draft |
| Contract | `cargo test -p vibex-tui --test contracts` | dependency boundary, key tables, locale coverage, no secret-shaped copy |
| PTY | `cargo test -p vibex-tui --features pty-harness --test pty` | the real binary enters raw mode, paints a first frame, writes **zero bytes when idle**, keeps in-process diagnostics out of the terminal, restores the terminal on exit, and survives a resize storm |

The PTY layer is the only one that can catch a failure outside the renderer.
The idle test is the `idle_cost` contract from the design report: after startup
settles, a client with nothing to do must not touch the terminal at all.

`pnpm check:rust` runs all four.

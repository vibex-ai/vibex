# Vibex TUI

The character-grid client for a Vibex runtime. It is the same client family as
the desktop and mobile apps: it renders authoritative state owned by a
`DesktopRuntime` and never opens the database, the browser runtime, or the
media stack itself.

```bash
vibex                      # attach to (or start) the runtime for this home
vibex connect vibex://…    # pair with a runtime and attach to it
vibex status               # report which seat this home would use
vibex status --home <dir>  # ... for another home, without attaching
vibex --home <dir>         # run against another home
vibex-server tui           # run the client from the server binary
vibex-desktop tui          # run the client from the desktop binary (Linux)
```

`vibex status` never attaches, so it is the first thing to run when something
is wrong — it prints the home, the flavour, the seat, the endpoint and the
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

## Seats

A client attaches in one of two ways, decided once before the interface starts:

| Seat | When | Notes |
| --- | --- | --- |
| **Authority** | nothing else owns the home | this process starts and owns the runtime; native steering is available |
| **Remote** | another runtime owns the home, or a link was given | the client is a network peer; steering degrades to interrupt + resend |

If the home is locked and the runtime is not accepting local clients, the client
prints three ways out: enable *Settings → Remote Access → Direct*, quit the
desktop app, or connect to another runtime with `vibex connect`.

## Visual language

Three ideas carry the interface.

**The rail.** Every transcript block owns a one-column colour bar down its whole
height. It gives each block a visible left edge, so a long tool output stays one
object instead of dissolving into the previous one, and it lets a session be
scanned by colour before a word is read:

| Rail | Block |
| --- | --- |
| accent | your message |
| foreground | the Agent's reply |
| violet | reasoning |
| blue | tool call, command, file change, search |
| grey | system notice, todo, git |
| amber | approval, question, retry |
| red | error |
| green | resolution |

The rail is a *filled cell* rather than a drawn line: it is exactly one column
wide in every font and needs no box-drawing glyph. Because a filled cell is
carried entirely by colour, a terminal without colour falls back to a drawn
`│`, which keeps the structure and lets the colour go.

Work items — tool calls, commands, reasoning — also carry a `⏺` bullet, and a
run of three or more collapsed ones folds into its first member with a
`╶╶ N more` summary. A session produces work items in bursts, and showing all of
them at full height buries the sentences they are evidence for.

**Layered surfaces.** Backgrounds step away from the canvas, so a tool body or a
raised row reads as a distinct plane rather than as more text on the same
canvas. The grey scale has three steps — dim for punctuation and chrome, medium
for muted body, bright for secondary labels — because one grey cannot do three
jobs without everything competing.

**Focus is a fade, not a switch.** Panes that do not have the keyboard keep
their colour and drop in weight, and the composer's rail goes from the accent to
dim grey. That makes "where will my keystrokes go" answerable at a glance.

### Composer

```text
╭─ <session title> ─────────────────────────────╮
│ ❯ the draft so far                             │
╰─ <agent> · <model> · <running>     multiline ▏╯
```

The bottom border is an info line rather than a rule: a terminal has no room for
chrome that only carries status, and a divider that also informs is free.

### Status bar

Identity on the left, context in the centre, appearance on the right. Splitting
it into zones is what stops the bar from becoming one left-aligned sentence
whose tail is the first thing a narrow terminal eats. The centre is the
context-window readout — `8.5K / 1.0M`, with the colour moving through the
usage thresholds — so the answer is available without reading the number.

### Key bar

Keys are drawn bold and bright, labels dim: the key is what the reader is
looking for and the label only confirms it. The leading hints survive a narrow
terminal, so shrinking the window degrades the bar from the least important end.

## Pages

### Sessions

The sidebar groups sessions by workspace and project. `/` filters, `Enter`
opens, `n` creates, `r` renames, `f` forks, `a` archives, `Ctrl+X` deletes, and
`Ctrl+A` includes archived sessions. Session creation picks a workspace through
the authority's own directory browser, so a remote client chooses a path that
exists where the Agent runs.

### Agent

The transcript is a block list with a streaming tail. `e` folds the selected
block, `F2` folds them all, `Ctrl+E` toggles reasoning, `y` copies a block body
and `Ctrl+Y` its metadata. `Enter` opens a block's details. The composer takes
`/` commands, `@` files and `$` skills, `Enter` sends, `Shift+Enter` breaks the
line, `Ctrl+O` hands the draft to `$EDITOR`, and `Ctrl+S` steers a running turn.
`Ctrl+C` clears the draft, then interrupts, then offers to quit.

Pending approvals appear as a card: `a` allows, `d` denies, `Ctrl+A` allows for
the rest of the session, and a digit answers with that specific advertised
option. Elicitation forms are filled field by field and submitted with `s`.

### Files

A read-only view of the workspace tree. `Enter` opens a file, `e` hands it to
`$EDITOR`. The TUI is not an editor: `Ctrl+O` and `e` are the only write paths,
and both go through a program you chose.

### Changes

Git status, diffs, staging (`a`), unstaging (`u`), committing (`c`) and the
worktree menu (`w`). Worktree lifecycle actions run their preflight first and
show the result before touching anything.

### Management

Agents, Providers, MCP servers, Skills, Prompts, Hooks, Devices and Recovery.
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

### Recovery

Diagnostics export and database backup create / inspect / restore. All of these
run on the authoritative runtime; restoring is destructive and requires typing
the backup id. The capability gate marks them as needing full control on a
paired device.

### Settings

Theme (20 shipped themes), language (`en`, `zh-CN`, `zh-TW`), icon set, and the
key-binding file. `F9` reloads `~/.vibex/tui-keys.toml`; the interface reports
which lines it could not use instead of failing to start.

### Help

`?` opens contextual help for whatever has focus. It is generated from the same
binding tables that dispatch the keys, so a hint cannot describe a key that does
nothing.

## Environment

| Variable | Meaning |
| --- | --- |
| `VIBEX_HOME` | runtime home (default `~/.vibex/<channel>`) |
| `VIBEX_CHANNEL` | `stable`, `rc`, or `preview` |
| `VIBEX_THEME` | theme id |
| `VIBEX_TUI_COLOR` | `truecolor`, `ansi256`, `16`, `none` |
| `VIBEX_TUI_ICONS` | `auto`, `emoji`, `ascii` |
| `VIBEX_TUI_KEYS` | key-remap file (default `<home>/tui-keys.toml`) |
| `VIBEX_TUI_LOG` | spill file for process diagnostics (default `$TMPDIR/vibex-tui-<pid>.log`) |
| `NO_COLOR` | disable colour entirely |

## Terminal ownership

The client draws on the alternate screen, so it owns the terminal for as long as
it runs — including `stderr`. An authority seat boots the runtime *in this
process*, and the runtime reports startup stages from background tasks that
finish after the first frame is painted. A line like that landing in a frame
wraps at the last column and scrolls the grid, which is the log residue that
must never appear under the interface.

So writes aimed at `stderr` are diverted into a spill file while the client owns
the screen, and the terminal's own `stderr` is handed back on exit. Nothing is
dropped silently: the client names the file when it exits, and the file is
`VIBEX_TUI_LOG` when that is set. A remote seat writes nothing at all, so no
notice is printed and no file is named.

## Degradation

Every one of these has a defined behaviour rather than a broken screen:

| Condition | Behaviour |
| --- | --- |
| stdout is not a TTY | no raw mode; a clear message and a non-zero exit |
| terminal below 60×16 | a size notice, not a half-rendered frame |
| in-process diagnostics | diverted to a spill file, named on exit; never drawn into a frame |
| `NO_COLOR` | glyphs and indentation carry the structure; colour is never the only signal |
| non-UTF-8 locale | ASCII borders and markers |
| disconnected | a banner, mutations disabled, the last known state marked stale |
| read-only device | actions are visible, disabled, and say which permission they need |
| very long conversation | the transcript is capped and the oldest blocks are dropped |

## Previewing

A fixture session renders to stdout, which is how the layout is reviewed:

```bash
cargo run -p vibex-tui --example preview -- 150 44
cargo run -p vibex-tui --example preview -- 100 30 --light
cargo run -p vibex-tui --example preview -- 150 44 --ansi    # real SGR codes
cargo run -p vibex-tui --example preview -- 150 44 --no-color
```

## Testing

```bash
cargo test -p vibex-tui
```

Four layers:

| Layer | Command | What it proves |
| --- | --- | --- |
| Reducer | `cargo test -p vibex-tui --lib` | the intent → effect mapping is a pure function |
| Render | `cargo test -p vibex-tui --test render` | layout degrades at 80×24 / 100×30 / 120×40 / 200×50, CJK wraps, colour-less mode still reads |
| Contract | `cargo test -p vibex-tui --test contracts` | dependency boundary, key tables, locale coverage, no secret-shaped copy |
| PTY | `cargo test -p vibex-tui --features pty-harness --test pty` | the real binary enters raw mode, paints a first frame, writes **zero bytes when idle**, keeps in-process diagnostics out of the terminal, restores the terminal on exit, and survives a resize storm |

The PTY layer is the only one that can catch a failure outside the renderer.
The idle test is the `idle_cost` contract from the design report: after startup
settles, a client with nothing to do must not touch the terminal at all.

`pnpm check:rust` runs all four.

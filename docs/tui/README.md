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
| `NO_COLOR` | disable colour entirely |

## Degradation

Every one of these has a defined behaviour rather than a broken screen:

| Condition | Behaviour |
| --- | --- |
| stdout is not a TTY | no raw mode; a clear message and a non-zero exit |
| terminal below 60×16 | a size notice, not a half-rendered frame |
| `NO_COLOR` | glyphs and indentation carry the structure; colour is never the only signal |
| non-UTF-8 locale | ASCII borders and markers |
| disconnected | a banner, mutations disabled, the last known state marked stale |
| read-only device | actions are visible, disabled, and say which permission they need |
| very long conversation | the transcript is capped and the oldest blocks are dropped |

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
| PTY | `cargo test -p vibex-tui --features pty-harness --test pty` | the real binary enters raw mode, paints a first frame, writes **zero bytes when idle**, restores the terminal on exit, and survives a resize storm |

The PTY layer is the only one that can catch a failure outside the renderer.
The idle test is the `idle_cost` contract from the design report: after startup
settles, a client with nothing to do must not touch the terminal at all.

`pnpm check:rust` runs all four.

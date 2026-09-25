# Embedded browser

The embedded browser is a **tool panel** — the same rank as the terminal and the
document preview — backed by the system Chrome/Chromium/Edge over the Chrome
DevTools Protocol. It is not an application shell and it is not a WebUI product.
Keep that wording in docs, comments and UI copy; the phrasing is what keeps the
architecture from drifting.

## Ownership

The runtime owns everything that has authority:

| Owned by the runtime (`crates/browser`, assembled by `crates/desktop-runtime`) | Owned by the client |
| --- | --- |
| Browser process, isolated `user-data-dir`, CDP connection | Frame decoding and painting |
| Tab / ref / generation state | Input capture and coordinate conversion |
| Screencast frame slots and the ack credit | Address bar, dialog cards, file-chooser cards |
| Domain policy, downloads, dialogs, file choosers | The preview-panel tab that hosts the surface |
| The redacted operation ledger and its persistence | |

A client never holds a CDP connection. It reaches the browser through
`BrowserBackend` (native: in-process service; remote: Remote v2), which mirrors
the `TerminalBackend` seam.

## Two channels, one target

- **Humans** watch `Page.startScreencast` frames rendered as GPUI textures.
- **Agents** read `Accessibility.getFullAXTree`, pruned to a bounded element
  list.
- **Diagnostics** (`Runtime.consoleAPICalled`, `Runtime.exceptionThrown`,
  `Log.entryAdded`, failed `Network` responses) are always on, because for a
  coding agent they are usually the most useful signal on the page.

Both channels attach to the same flattened CDP session per tab, so what the
agent acts on and what the user sees cannot diverge.

## Invariants that are easy to break

1. **Every replacement frame releases the previous texture.**
   `RenderImage::new` mints a new `ImageId` and the sprite atlas has no
   eviction, so a per-frame image without a matching `Window::drop_image` grows
   GPU memory without bound. `BrowserSurface` parks the outgoing image in
   `pending_drop` and releases it at the top of the next paint, because
   `drop_image` needs a `&mut Window`. Any new per-frame image path needs the
   same treatment.

2. **Frames use latest-value semantics, never a queue.** A frame that arrives
   while the consumer is busy replaces the one waiting; `dropped_frames` reports
   the gap. Queueing decoded frames costs hundreds of megabytes per second of
   backlog.

3. **`Page.screencastFrameAck` is credit-based.** The ack is sent after a
   consumer takes a frame, so Chrome's encoder follows the consumer's rate. A
   hidden panel therefore costs nothing, and an immediate ack would let Chrome
   run unbounded.

4. **Input coordinates are viewport CSS pixels and never include the scroll
   offset.** The panel scales by `frame pixels ÷ drawn size` only. Adding
   `scrollOffsetX/Y` makes every click and highlight drift.

5. **Refs are generation-scoped.** `r{generation}-{index}` is invalid after any
   observation, navigation or rerender in the same tab. A stale ref is refused
   with an explanation telling the model to observe again — never resolved
   against the new element list.

6. **A failed load does not move the tab's URL.** Chrome navigates to
   `chrome-error://chromewebdata/` when a page does not load;
   `is_chrome_error_page` keeps that internal URL out of `tab.url`, so the
   address bar and the Agent keep seeing what was actually requested. The error
   page still renders as a frame and the failure stays in the network
   diagnostics — a page that silently reported the error URL as its address is
   how a broken launch-time proxy went unnoticed.

7. **Every CDP command carries a deadline.** The DevTools channel is known never
   to settle when the page is wedged. `BROWSER_CDP_COMMAND_TIMEOUT_MS` is the
   default; do not add a command without one.

8. **Chrome must be spawned by the runtime.** A stdio MCP sidecar is spawned and
   owned by a third-party agent CLI, so the runtime has no child handle for it
   and cannot clean up anything it starts. `shutdown_inner` stops the browser
   next to the terminals.

9. **Downloads default to denied and file choosers are intercepted.** A page
   never chooses a write path and never opens a native dialog that does not
   exist in headless mode.

10. **The browser service is polled inside the Tokio runtime, always.**
   `BrowserService` is a Tokio citizen: it spawns Chrome, opens async pipes,
   arms a deadline on every CDP command and `tokio::spawn`s frame acks. The
   panel, however, drives it from GPUI's executor, which has no Tokio context —
   so `LocalBrowserTransport` installs the runtime for the duration of each poll
   (`runtime_context`, `LocalBrowserTransport::run`). Skipping that seam for one
   call panics on the first click of the browser entry with
   `there is no reactor running, must be called from the context of a Tokio 1.x
   runtime`, raised from tokio's pidfd reaper inside `Command::spawn`. A new
   panel-side call must go through the transport (or `run`), never straight at
   the service, and `BrowserFrameStream::next` needs it too because the frame
   stream is awaited outside the transport.

## Security rules

- **Isolated profile, always.** `--user-data-dir` points under the runtime data
  directory, never at the user's real profile and never inside the workspace.
  Chrome 136 and later refuse remote debugging against the default profile
  anyway.
- **`--remote-debugging-pipe` wherever it is available**, so no TCP port is
  opened. The loopback port fallback exists only for platforms where the pipe
  transport is not implemented and is read back from `DevToolsActivePort`.
- **Never `--no-sandbox`.** In a container, configure user namespaces or seccomp
  instead.
- **Loopback is not blanket-trusted.** Only origins the runtime positively
  identified as this workspace's development server skip approval; every other
  loopback and private-network target prompts, because the browser runs on the
  runtime host and can reach services that are not exposed at all.
- **The child's proxy environment is decided at launch, not inherited.** Chrome
  parses `all_proxy` as an HTTP proxy even when it names a SOCKS server, so a
  shell exporting `all_proxy=socks5://127.0.0.1:7891` (Clash-style) makes every
  navigation fail with `ERR_EMPTY_RESPONSE`. `browser_proxy_for` therefore drops
  `all_proxy` when `http_proxy` and `https_proxy` both exist — those are parsed
  correctly and take precedence — and otherwise hands the same URL to Chrome as
  `--proxy-server`, where the scheme is honored. `no_proxy` stays in the
  environment; only Chrome's built-in loopback bypass covers the translated
  flag. Any new launch variable needs the same kind of review: the panel
  inherits whatever shell started the workbench, and a broken proxy looks
  exactly like a broken page.
- **`browser_upload` and `browser_preview_open` resolve symbolic links and
  confine the result to the agent's authorized roots.** Local previews are
  served as a `data:` URL so the page never learns an absolute path.
- **Page content is untrusted data.** Every tool description carries
  `BROWSER_UNTRUSTED_CONTENT_NOTICE`, and `browser_extract` wraps its result in
  explicit content delimiters.
- **No blocking notice stands between the user and the browser.** A first
  attempt parked `open_browser` behind a modal risk card; the card rendered
  without its buttons, and because the overlay was deliberately not closable the
  panel became unusable. `BROWSER_RISK_DISCLAIMER_VERSION` and
  `has_acknowledged_risk_disclaimer` exist but nothing shows a notice, and
  `BrowserUnavailableReason::DisclaimerPending` stays reserved vocabulary. If a
  notice is wanted again it must be **non-blocking** — an inline banner in the
  panel, or a dialog built with an explicit footer like the RC-import card — and
  it must always have a way out that does not depend on a button rendering.
  Nothing that a user cannot dismiss may gate a feature.
- **Auth challenges and permission prompts are declined by Chrome, not by us.**
  Headless has no UI for either, and Chrome cancels both on its own: a 401 page
  settles and `navigator.geolocation` resolves to a denial. `Fetch` is
  deliberately **not** enabled to make that explicit — `Fetch.authRequired` only
  fires for requests the patterns match, and enabling it without patterns pauses
  every request (a first attempt hung `Page.navigate` until its deadline). The
  live transport test pins both behaviours so a Chrome change fails a test
  instead of freezing the panel.

## Audit and redaction

`BrowserActionRecord.summary` is redacted before it reaches the ledger:
URLs lose credentials, query strings and fragments; form values never appear.
`browser_recording_*` is the one place raw values are kept, in memory only,
because an exported test is useless without them — and the tool description says
so out loud. The persisted `browser_audit_records` table stores no page content,
form value, cookie or screenshot. `Debug` for frames, observations, tabs,
sessions and network entries prints metadata only.

## MCP delivery

The runtime exposes browser tools over a loopback Streamable HTTP endpoint
(`POST /mcp`, `Authorization: Bearer <session token>`) and, for agents that
cannot use HTTP MCP, through a stateless stdio sidecar that forwards the same
JSON-RPC to that endpoint. One handler serves both, so behaviour cannot drift.

- Every request must pass the loopback `Host` and `Origin` checks, which is what
  blocks DNS rebinding.
- Bearer tokens are `btok_<session id>_<mac>` with
  `mac = SHA256(global secret ‖ 0x00 ‖ session id)`. The session id is in the
  clear so the endpoint can find the session; the MAC is what authenticates it,
  and a token minted for one session cannot be replayed for another.
- A built-in server id must be checked against the set
  (`vibex_core::is_builtin_mcp_server_id`), never against one constant: a second
  built-in server compared against a single id is treated as a user server and
  dropped on every profile that has not enabled the optional MCP flag.
- A built-in server may be described more than once to offer a preferred
  transport and a fallback. `wire_mcp_servers` keeps the first entry whose
  transport the agent supports and drops later entries with the same id;
  forwarding both would register the server twice.
- Some agents (`grok`, `cursor`, `hermes`, `pi`, `factory-droid`) never receive
  wire MCP servers at all. Report them as unavailable rather than listing tools
  they can never call.

## Panel wiring (the parts that are easy to leave dangling)

The runtime already owns the browser; the panel is a subscriber. Three wires
have been dropped once and must stay:

1. **Every runtime event the panel renders has a consumer.** `DialogOpened` and
   `FileChooserOpened` are the page blocking on a human, `DialogClosed` is the
   page unblocking itself, `SessionChanged` is the execution source moving under
   the panel, `Availability` is the browser appearing or vanishing, and
   `DevServerDetected` is the offer to open a server the terminal just printed.
   `apply_browser_event` sends each to the surface that owns the tab; a variant
   that falls into `_ => {}` is a feature that silently does not exist.
2. **A hand-over needs a hand-back.** Human input pauses the Agent on that tab
   (`execution_source = User`, `tab.aborted`), and only
   `resume_agent_operations` re-arms it — the panel offers that button and says
   the Agent must observe again. Hover alone must never take over: the panel
   forwards every pointer move, so `input_takes_over` excludes `MouseMove`.
3. **Detection lives in the runtime, not in the UI.** `observe_terminal_output`
   is fed by the PTY reader through `TerminalManager::set_output_observer`, so a
   URL printed on a terminal tab nobody is watching still counts, and an origin
   joins the allow-list only after `probe_candidate` answers.

## The browser shell the screencast cannot supply

Chrome's own popups and dialogs are browser UI; a screencast carries only the
page. Each one needs a panel-side answer:

| Missing surface | What the panel does |
| --- | --- |
| JS dialogs (`alert`/`confirm`/`prompt`/`beforeunload`) | card from `DialogOpened`, answered through `handle_dialog` |
| File chooser | card from `FileChooserOpened`; the Agent attaches files with `browser_upload`, the human cancels through `resolve_file_chooser` |
| `<select>` popup | `probe_select_hint` on hover, then the panel's own list; `select_menu_at` / `choose_select_option` apply the choice and dispatch `input` + `change` |
| Clipboard | Ctrl/Cmd+C reads the selection with `selection_text` and writes the system clipboard; Ctrl/Cmd+V types the system clipboard into the page with `Input.insertText`; Shift/Alt combinations stay with the page |
| Downloads, permissions, HTTP auth | denied by policy or by Chrome itself (see Security rules) |

The `<select>` probe runs on hover rather than on click on purpose: a click that
waited for a round trip would reach the page after its own release, and Chrome
would never synthesize the click.

## Approvals

Domain approval reuses `PermissionRiskCategory::Network` and the existing
permission card; no new risk variant is introduced, because that would force
edits to the exhaustive risk-label matches on desktop and mobile.

Two facts the browser layer has to handle itself:

- The runtime keeps **no permission policy store**, so "always allow for this
  session" is remembered by the browser layer.
- Pending permissions have **no global expiry**. The browser card therefore
  carries its own TTL and denies on timeout.

## Tool tiers

| Tier | Audience | Content |
| --- | --- | --- |
| Coarse | weak models | one call does a whole job (`browser_open_and_read`, `browser_click_by_name`) |
| Fine | strong models | full observe/find/click/fill/press surface |
| Visual | multimodal models | fine plus screenshots and visual regression |

Extend `AGENTS_ACCEPTING_IMAGE_TOOL_RESULTS` only after confirming an agent
forwards image content from a tool result, and say so in the commit.

## Testing

`crates/browser` carries the unit tests for the parts that fail silently or
dangerously: reference lifecycle, URL classification and path authorization,
CDP framing, tool tiering, redaction and Playwright mapping. The GPUI surface
covers coordinate conversion and decode layout. `apps/desktop` exposes an
`EmbeddedBrowserContractProbe` (`--probe`) asserting that degradation stays
explicit, the tier ladder is monotonic, the frame drop is present, and audited
records carry no page content.

`browser_transport::tests` guards the executor seam described in invariant 10:
one test polls runtime-bound work (a process spawn plus a timer) from a thread
with no Tokio context, and one opens a real session and tab through
`LocalBrowserTransport` from such a thread. The second skips itself when no
system browser is installed — that is the environment where the whole feature is
explicitly unavailable — and is the regression test for the first-click crash.

`process::tests` pins the launch-time proxy decision (invariant-adjacent, see the
Security rules): `all_proxy` alone or beside one scheme variable becomes a
`--proxy-server` flag, `all_proxy` beside both scheme variables is dropped, and
an http-only environment is passed through untouched. The decision is a pure
function of a lookup closure, so the tests never touch the process environment
that other tests share.

The same live test also covers the shell fallbacks, because each of them is only
real against a browser: the page's own selection comes back through
`selection_text`; a rendered `<select>` is found by point, its options read, a
choice applied and read back; a 401 settles the tab instead of hanging it; and
geolocation resolves to a denial. `browser_surface::tests` holds the pure parts —
which keystrokes mean copy and paste, how close a click must be to a probed
point, dialog cards belonging to their own tab. `management::tests` and
`desktop-runtime` pin the per-Agent delivery copy, and `desktop-model` pins that
an old UI-state file reads as "notice not accepted".

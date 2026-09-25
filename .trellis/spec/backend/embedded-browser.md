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

6. **Every CDP command carries a deadline.** The DevTools channel is known never
   to settle when the page is wedged. `BROWSER_CDP_COMMAND_TIMEOUT_MS` is the
   default; do not add a command without one.

7. **Chrome must be spawned by the runtime.** A stdio MCP sidecar is spawned and
   owned by a third-party agent CLI, so the runtime has no child handle for it
   and cannot clean up anything it starts. `shutdown_inner` stops the browser
   next to the terminals.

8. **Downloads default to denied and file choosers are intercepted.** A page
   never chooses a write path and never opens a native dialog that does not
   exist in headless mode.

9. **The browser service is polled inside the Tokio runtime, always.**
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
- **`browser_upload` and `browser_preview_open` resolve symbolic links and
  confine the result to the agent's authorized roots.** Local previews are
  served as a `data:` URL so the page never learns an absolute path.
- **Page content is untrusted data.** Every tool description carries
  `BROWSER_UNTRUSTED_CONTENT_NOTICE`, and `browser_extract` wraps its result in
  explicit content delimiters.

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

`browser_transport::tests` guards the executor seam described in invariant 9:
one test polls runtime-bound work (a process spawn plus a timer) from a thread
with no Tokio context, and one opens a real session and tab through
`LocalBrowserTransport` from such a thread. The second skips itself when no
system browser is installed — that is the environment where the whole feature is
explicitly unavailable — and is the regression test for the first-click crash.

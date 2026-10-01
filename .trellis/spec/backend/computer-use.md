# Computer use

Computer use lets an Agent read and drive the **desktop of the machine the
runtime runs on**: applications the runtime did not start, windows the user may
be looking at, and input that can reach the whole session. It is a **tool
panel** capability of the same rank as the terminal and the embedded browser,
and it is not a remote-desktop product. Keep that wording in docs, comments and
UI copy; the phrasing is what keeps the architecture from drifting.

> The reason this feature carries more rules than any other panel: it is the one
> place an Agent acts on the user's **real accounts** on a desktop the user may
> be using **at the same time**. A wrong click is not a failed tool call; it can
> be a message that was sent, a purchase that completed, or a file that is gone.

## Ownership

| Owned by the runtime (`crates/computer`, assembled by `crates/desktop-runtime`) | Owned by the client |
| --- | --- |
| The engine helper process and its single-owner lock | Frame decoding and painting |
| Accessibility observations, element references and tree digests | The panel's controls and its own ledger rendering |
| The risk model, approvals and the grant store | Nothing that decides whether an action may run |
| The redacted action ledger and its persistence | |
| The emergency stop and the disconnect contract | |

A client never holds an engine connection. A local panel reaches the service
through the runtime; a paired client reaches approvals through the existing
permission card, and the live desktop hop for a remote runtime is an explicit
degradation (`ComputerUnavailableReason::RemoteRuntimeUnsupported`), not a
silently empty panel.

## The engine boundary

The runtime does not implement desktop automation. It drives one **engine**
through a helper process:

- the helper is spawned **by the runtime**, never by a gateway, relay or sidecar
  — the process that starts the engine is the process the operating system
  attaches its screen and accessibility grants to;
- the two speak newline-delimited JSON over the child's standard streams, with
  an authenticated handshake before anything else;
- the helper holds a **single-owner lock file**; a second helper against the
  same runtime home refuses to start rather than injecting input beside the
  first;
- an unauthenticated helper exits after thirty seconds, and a helper whose
  parent disappears releases every held key and exits;
- **the helper does not run as root** while a desktop user session exists. That
  combination is refused, because a privileged process typing into a user's
  session is the Linux analogue of the Windows Session 0 isolation problem;
- a **headless macOS runtime** does not start it either: on that platform the
  grants belong to the application in the launch chain, so a headless process
  would hold none of them and the feature would look broken rather than
  unavailable.

The engine's own tool list is **not** passed through to an Agent. The tool
surface, its descriptions and its result shape are Vibex's, so replacing the
engine never changes what a model sees or what an approval card says.

## The tool surface

Nine tools, deliberately not a god tool:

```
computer_list_apps       → canonical identities, with the id to address them by
computer_launch_app      → start an installed application (approved once per session)
computer_get_app_state   → accessibility tree + element references (+ screenshot by tier)
computer_click           → element reference first, coordinates as a fallback
computer_type_text       → semantic write when an element is named, typing otherwise
computer_set_value       → semantic write with read-back verification
computer_press_key       → a key or a chord
computer_scroll          →
computer_permissions     → what the OS actually granted
```

Launching is a tool rather than a favour the reader does by hand because a
stopped application has **no window to observe and no addressable launcher**: on
Wayland the shell's launcher is a layer-shell surface the engine cannot list, and
the desktop's own launcher may need a keyboard chord the compositor refuses. The
risk model already classified it (`LaunchApp`, approved once and remembered for
the session), so the surface was the only thing missing. The result is
`verified` **only** when the engine reported a live process for it; a launcher
that returned has not proven the window exists, and the tool's description tells
the model to observe before acting.

Three rules live in the descriptions themselves, because tool descriptions
travel with the tool while a context handover does not:

- an element reference is **short-lived** — observe again after every action;
- a valid reference is never inferred from an element count;
- an action that fails in the background is **not** retried in the foreground
  automatically: the human is asked.

### Element references

`c{generation}-{index}`, where the generation belongs to one application's
observation. An action validates the reference against the cached observation,
then re-reads the tree digest:

- a reference from an earlier generation is refused with
  `computer_element_reference_stale`;
- a tree whose digest moved since the observation is refused with
  `computer_tree_changed`, and the fresh observation is cached so the model's
  next call can use it.

A stale reference is never resolved against the new element list. "The click
landed on whatever moved into that position" is the failure this prevents.

A third refusal exists and is kept apart: `computer_element_not_addressable`.
An engine that cannot prove which window a tree belongs to will either issue no
element handles at all or keep no snapshot for the ones it does issue — on Linux
that is a window with no accessibility tree, which leaves the driver holding an
X11 property fallback it refuses to act on. Observing again cannot help there,
so it must not be reported as a stale reference: that is exactly how a model is
sent into an observe/act/fail loop. The policy stays the same — a window the
engine will not prove is a window it will not act on.

## Verification

Every action returns verification metadata, and the class contract is that
**missing metadata means unverified, never success**:

| Field | Meaning |
| --- | --- |
| `verified` | the engine asserted the effect (a semantic write with a read-back, an action with a post-check) |
| `unverified(<reason>)` | the action was dispatched; whether the UI changed is not established |
| `failed` | the engine refused or the call failed |

The reasons are part of the vocabulary: `synthetic_input`,
`clipboard_paste`, `accessibility_action_unasserted`, `value_mismatch`,
`window_changed`, `readback_unsupported`, `provider_unavailable`,
`foreground_escalation` and `missing_metadata`.

A tool result that is unverified says so in words as well as in the field, and
tells the model to observe again rather than report a success.

## The risk model

Risk is classified by the **runtime's own reading** of the canonical application
identity, the accessibility element and the delivery mode — never from a
model-supplied string. The model's `app` selector is resolved against the
engine's own list and replaced by the canonical target before anything is acted
on. An ambiguous name is an error, not a coin flip.

| Class | Default | Approval | Remembered |
| --- | --- | --- | --- |
| Credential application or secure text field | **hard deny** | none — no card | never |
| Vibex's own window or process | **hard deny** | none | never |
| Destructive control label | approval | once | never |
| File deletion or externalising | approval | once | never |
| Clipboard read | approval | once | never |
| Foreground takeover | approval | once | never |
| Concurrent human activity | approval (a conflict question) | once | never |
| Quitting an application | approval | once | never |
| Launching an application (`computer_launch_app`, `vibex computer launch`) | approval | session | yes |
| Clipboard write | approval | session | yes |
| Ordinary semantic action | allowed | none | — |

Three rules hold this together:

1. **"Remember for the session" is only for reversible, non-destructive
   actions.** A destructive action is never batched, however many approvals the
   user has already given.
2. **The strictest applicable class wins.** A click that is both destructive and
   foreground is approved once, even though neither field alone asked for that.
3. **A grant is scoped to the application, not to the Agent.** "Allow clicks in
   this application" is a meaningful decision; "allow this Agent to click
   anything" is not.

Approvals reuse the existing permission card and the existing risk categories
(`Command`, `FileReadSensitive`, `FileWrite`, `FileDeleteOrMove`, `CustomTool`),
so no new variant forces edits to the exhaustive risk-label matches on desktop
and mobile. The card carries the canonical target, the action class and the
reason; for destructive and foreground actions it also carries a screenshot of
the target window, captured for the card and never returned as tool content.

The computer layer carries its **own approval TTL**: pending permissions have no
global expiry in this runtime, so an unanswered card would hold an agent tool
call for the whole prompt budget.

## The emergency stop

One press reaches a terminal state, in this order:

1. new calls are refused with `computer_stopped_by_user`;
2. queued input is dropped — an input that had not started never lands;
3. every held key and mouse button is released;
4. the helper stops accepting input;
5. the ledger records the stop and the actions that ran;
6. **a human must re-enable it**; the Agent cannot undo a stop.

Step 3 is the one that matters most: a stuck modifier key makes the user's whole
machine unusable, which is the most severe secondary failure this feature can
cause. It is also why the emergency stop and the disconnect contract share the
same release path.

## The disconnect contract

For a runtime whose controlling client can go away:

| | Contract |
| --- | --- |
| Soft threshold | new actions are refused with `computer_paused_offline`; **every pending approval is void** — a later answer would describe a situation that no longer exists |
| Hard threshold | queued input is cleared and held keys are released, through the emergency stop's path |
| Agent session | paused, not terminated: its computer calls fail with `paused_offline` and resume when the client returns |
| Helper | stays alive but input-incapable; restarting it would re-run the platform permission handshake |
| Recovery | requires a human confirmation, exactly like the stop |
| Local mode | the thresholds are not triggered, but the code path is the same one |

The soft threshold is below the hard one by design: a pause is recoverable, a
released key is not, and the gap absorbs network jitter.

## Delivery paths

| Path | Who | Enforcement |
| --- | --- | --- |
| MCP tool | every Agent that receives a built-in server | full: policy, approval, ledger |
| CLI + skill | an Agent that receives no MCP server at all | full at the runtime; the host sees a shell command |
| Native agent feature | an Agent whose own product carries computer use | **none** — Vibex only probes it read-only |
| Unavailable | an Agent that can neither receive MCP nor run commands | stated as such in the UI |

The CLI path is honest about its weakness: the Agent runs a command, so the host
approves "run this command" rather than one desktop action. Its environment
(`VIBEX_COMPUTER_MCP_ENDPOINT`, `VIBEX_COMPUTER_MCP_TOKEN`, `VIBEX_HOME`) is
minted per session and injected into the Agent's own process environment; the
token is the same session-scoped bearer token the MCP endpoint verifies. The
skill document says all of this out loud, including the granularity difference.

The native path is **never written**: Vibex does not touch another product's
computer-use configuration key. Writing a security default there would silently
override a capability the user enabled themselves.

## Tool tiers

| Tier | Audience | Content |
| --- | --- | --- |
| Structured | every Agent not confirmed to forward image content | accessibility tree, references, semantic actions |
| Visual | Agents confirmed to forward image content from a tool result | structured plus the `include_screenshot` parameter |

`include_screenshot` is **withheld** from the structured-tier schema rather than
advertised and ignored: a tool that looks available and returns nothing wastes
the model's rounds. The list is a conservative whitelist — an Agent joins it
only after it has been observed to forward the image to its model — and the UI
copy follows the list rather than hard-coding a number.

## Platform claims

| Platform | Claim |
| --- | --- |
| macOS | supported; accessibility and screen-recording permission required, and screen recording needs an application restart on recent versions |
| Windows | supported; background input fails more often (occluded windows, some toolkits) and those cases return an explicit background-unavailable result that needs a foreground escalation; elevated and UWP targets are out of scope |
| Linux X11 | supported: accessibility actions, unfocused input injection, window capture |
| Linux Wayland | **graded**: standard Wayland has no protocol for raw input to an occluded surface, so those calls return background-unavailable and need a foreground takeover; setting another window's geometry is refused; native Wayland windows are only visible through the driver's experimental backend, which Vibex measures and turns on by itself when the session has them (see "The Wayland backend") |

Do not say "supports Linux" without the display server. A deployment checklist
that must hold on every desktop environment:

1. a real desktop session exists (not merely an X server);
2. the accessibility bus answers (`org.a11y.Bus`), not merely a process;
3. the helper runs as the desktop user, never as root.

`--computer-doctor` answers all three plus the engine and permission state, and
maps each failure onto a named `ComputerUnavailableReason` instead of a boolean.

## The driver protocol, as the engine actually speaks it

Verified against `cua-driver 0.31.0` on Linux; the same shape holds on macOS and
Windows. The engine is **a daemon plus a client**, and the difference matters at
three points:

| Question | Command | Needs the daemon |
| --- | --- | --- |
| what can it do | `list-tools` — one `name: description` line per tool | no |
| is this machine ready | `doctor --json` — probes with `label`/`status`/`message` | no |
| is it running | `status` — prints `Cua Driver daemon is running` | no |
| do something | `call <tool> '<json>'` | **yes** |
| start it | `serve --socket <path>` | — |

Three consequences the implementation lives with:

1. **A tool call needs a running daemon, so the helper starts one.** It runs
   `serve` as its own child with `kill_on_drop`, after the driver reports no
   daemon, and retries the call once. The process that spawns the daemon is the
   process the operating system attaches the screen and accessibility grants to,
   so this is also why the daemon is not left to the user's shell. If a daemon is
   already running under the driver's own default socket, it is reused and none
   is started.
2. **A missing daemon is not a rejected action.** The client maps the driver's
   `daemon is not running` to its own `computer_driver_daemon_missing` before the
   generic rejection branch, because the caller has to start one and retry rather
   than report a failure to the model.
3. **Readiness is asked in the driver's vocabulary.** `doctor --json` reports the
   probes this platform actually depends on — display server, X11 connection,
   accessibility bus — and the adapter maps a failing probe to a named
   `ComputerUnavailableReason` (`NoDesktopSession`, `AccessibilityBridgeMissing`)
   instead of paraphrasing a log. `list-tools` and `doctor` are what tell a real
   driver apart from a file with the right name: both are read-only, need no
   daemon, and touch no desktop.

The engine's permission model is macOS-specific: `permissions status --json`
answers with the daemon's own TCC identity, so it is read there and reported as
not-required elsewhere. The probe never prompts — a permission dialog belongs to
a click in the settings, not to a startup path.

### The argument mapping, which is not this crate's vocabulary

Vibex's tool vocabulary and the driver's are **different languages**, and the
driver enforces the difference: every one of its tool schemas is
`additionalProperties: false`, so a key it does not know is refused before
anything touches the desktop. Sending Vibex's own words (`app`,
`element_index`, `delta_x`) therefore fails *every* call with the same opaque
error, which is exactly the failure this table exists to prevent.

| Vibex asks for | The adapter sends |
| --- | --- |
| an application | `pid` — resolved from `list_apps`, never from a name |
| a window | the **integer** `window_id`; a pid alone does not resolve on Wayland |
| an element | `element_token` — the driver's opaque per-snapshot handle, mapped from the index the service uses |
| a coordinate click | `pid` + `x`/`y` + `coordinate_frame: "desktop"` (the driver's default frame is window-local pixels) |
| a chord | `hotkey` with one `keys` array, not `press_key` plus `modifiers` |
| a scroll | `direction` + `amount` (1–50), derived from the delta pair: the dominant axis is the gesture |
| `launch_app` | `launch_path` from the directory, falling back to `name` |
| `kill_app` | `pid` — and it resolves a pid even when the application has no window left |
| an observation | `get_window_state` with `pid` + `window_id`; the driver's `get_accessibility_tree` is a *desktop* snapshot and takes no application |
| a screenshot | `get_window_state` with `include_accessibility_tree: false`, or `get_desktop_state` for the whole display |

Four properties of the engine's replies are load-bearing:

1. **A refusal is a document, not an empty failure — and it is written to
   stdout.** Argument problems arrive as
   `{"refusal": {"code", "message"}, "status": "refused"}`; capability problems
   as a bare `{"code", "detail", "escalation"}`. Both come with a non-zero exit
   *and empty stderr*, so an adapter that reads only stderr turns "unknown
   argument `app`" into "the helper failed". The adapter parses both envelopes,
   keeps the engine's own message on the error, and maps
   `background_unavailable` / `wm_chord_unavailable` onto
   `computer_background_unavailable` with the engine's escalation as the
   recovery hint — that hint is what tells the model to retry in the
   foreground, which is the documented Wayland path.
2. **A non-zero exit is not always a failure.** The engine exits non-zero for a
   *partial* answer too: a capture-only `get_window_state` whose pixels could
   not be attributed to the window (Wayland's `surface_identity_unproven`)
   returns `screenshot_error` and exit 1. A refusal and a capability failure are
   distinguishable — they carry `refusal` or a top-level `code` — so the
   adapter accepts a bare document as an answer for the screenshot paths, and a
   tree that arrived without its pixels is marked degraded with the engine's own
   reason rather than silently returning no image.
3. **Element handles belong to one snapshot.** The driver's `element_token` is
   stale as soon as the next `get_window_state` of the same window replaces its
   snapshot, and the service validates a reference by re-observing. The
   index→token map therefore lives in the adapter, is replaced (never merged) by
   each observation, and is bounded in size. Nothing above the seam sees a
   driver handle.
4. **An empty tree needs a reason.** A reply with no elements and no
   `degraded`/`degraded_reason` is an error (`computer_accessibility_bridge_missing`),
   never an empty window. Trees the driver recovers through the X11 property
   fallback set `degraded`, and the observation carries that through.

`release_all_keys` is a deliberate no-op against this engine: it has no
release-all primitive, and every input tool Vibex can reach is an atomic
press-and-release (the only held-input pair, `mouse_button_down` /
`mouse_button_up`, is not on this crate's tool surface). The helper's exit path
and the emergency stop still call it, and it truthfully reports that there is
nothing held.

## Settings and installation

The settings page is the product's front door for this feature. Nothing about
setup requires a terminal, and nothing happens on its own. It is built from the
same rows and groups as every other settings page — no wizard, no modal — and it
says four things:

- **The switch.** One row turns the feature on and off, and the change is live:
  turning it on starts the helper and the endpoint, turning it off releases the
  desktop and stops them. The runtime is told what the user chose rather than
  the other way round, and `VIBEX_COMPUTER_USE=1` only *seeds* the switch once
  so an administrator does not have to find it.
- **The driver.** Its row carries the state of this machine and the two actions
  that change it: Install and Check again. Installing runs a fixed, documented
  installer for the platform, only after the button is pressed, and never from
  anything a model wrote.

  Its sentence names exactly one state, and the states are kept apart because
  they send the reader to different buttons:

  | State | Sentence | Install |
  | --- | --- | --- |
  | never probed | not checked on this machine yet | enabled |
  | no executable found | no driver on this machine yet | enabled, primary |
  | found but silent | a driver file was found but did not answer as a driver, with its path | enabled |
  | present, runtime refuses (no desktop session, root, unsupported platform, engine did not start) | driver found, but *that reason*, with its path and detail | disabled |
  | present, OS permission missing or restart-pending | the permission is in place or still needed, with the path | disabled |
  | present, everything in place, switch off | ready at *path* | disabled |
  | running | running with *path* | disabled, labelled Installed |

  **A driver that is present is never reported as missing**, even when the
  runtime refuses for another reason: that sentence is what a reader acts on,
  and a second install is the wrong action for every state below the third row.
  Detection asks the driver for its own tool list, which is how a file with the
  right name is told apart from a working engine.

  A window paired with a runtime on **another machine** says so and offers
  neither button: the driver belongs to the machine that owns the desktop.
- **The system permissions.** A read-only line with the three states the
  operating system reports, because Vibex cannot consent on the user's behalf
  and a permission that needs a restart must not look like a denial.
- **Approval, calls and this platform.** The approval policy (allow / ask /
  deny), the switch that permits operating Vibex's own windows, the call
  timeout, and one row per capability family so a platform that cannot do
  something says so next to the switch that would have used it. On Wayland that
  group also carries the window row described below.

Three limits on the approval policy are deliberate and are stated on the page:

1. `Allow` removes the per-action card for approvable classes; it never reaches
   the classes that are refused outright. A password manager and a secure field
   are refused in every mode.
2. Destructive clicks and foreground takeovers keep their per-action card even
   under `Allow`. They are irreversible and user-visible; a global preference is
   not consent for a specific irreversible act.
3. Operating Vibex's own windows has its own switch rather than riding on the
   policy, because the failure it prevents — an Agent driving the window it is
   running in — is a feedback loop, not an over-permissioned action.

The call timeout is bounded to the offered choices and snaps up, so a
hand-edited settings file cannot shorten every action to nothing or set it to
unlimited.

### The Wayland backend

On a Wayland session the driver sees X11 windows by default and native Wayland
windows only through its experimental backend, so a session can look empty while
several applications are plainly open. Making the reader find an environment
variable to fix that is the wrong shape for a settings page, so Vibex measures
and decides:

- **The measurement is a comparison.** The configured view (what the live daemon
  already sees) against a short-lived probe daemon started with the opt-in on a
  **private socket**, plus the compositor's own `wayland_backend` health check.
  A session whose windows appear either way needs nothing; a compositor that does
  not advertise the wlroots manager globals can never offer them, and the page
  says that rather than promising a switch will help. The probe daemon is killed
  by dropping it, so the live daemon is never disturbed.
- **The row offers Automatic, On and Off.** Automatic is the default and the
  store holds no value for it; On and Off are the reader's own decision.
- **Precedence is the reader's before the machine's**: the environment variable
  (how someone exports the opt-in by hand, and what a host with no settings page
  uses), then the stored setting, then the measurement. An unmeasured session is
  not evidence of native windows, so it stays off.
- **The answer travels through the helper's environment**, because the helper is
  what spawns the driver daemon and the daemon is what reads the variable. A
  change to it rebuilds the helper for the same reason a timeout change does.
- **A daemon whose backend does not match the setting is stopped first.** The
  engine reuses whatever daemon already owns its socket, and a daemon outlives
  the helper that started it — it is spawned with kill-on-drop, but a helper
  that is killed never runs its destructors, so the daemon is reparented and
  keeps running. Without this, turning the backend on would be silently ignored:
  the new helper would reuse the old daemon and every native Wayland window
  would stay invisible while the settings said otherwise. The same check runs in
  reverse, so turning it off is not defeated by a daemon left running with it.
- **The resolved value is written back into the runtime settings**, so the
  capability statement the page shows describes the helper that is actually
  running instead of an environment variable nobody set.
- The measurement is cached per driver check; *Check again* drops it, so a window
  that opened since the last look can change the answer.

**Seeing windows is not driving them.** Both halves have to hold, and on a
compositor the driver has no input backend for, only the first one does:

- On Hyprland the pinned engine routes foreground input through its own
  compositor plugin and **refuses to downgrade** to XTest/libei
  (`production Hyprland input plugin is unavailable`; `no fallback or downgrade
  is permitted`). Background delivery is refused for the opposite reason: a
  virtual pointer injects at the compositor's focus, so it cannot address an
  occluded window. The driver's own `health_report` still reports every check as
  passing — `virtual-pointer=true` is about a global that exists, not about
  input the engine will deliver — so the settings page must not promise input on
  the strength of that report. Until the plugin exists for the installed
  compositor, computer use on such a session is read-only, and the honest answer
  to a model is the driver's own refusal, not a retry.
- A window whose accessibility tree the engine cannot prove (a terminal, a
  custom-drawn toolkit, anything without AT-SPI on Linux) is one it will not act
  on: it issues no handles, or keeps no snapshot for the ones it issues. That
  surfaces as `computer_element_not_addressable` — never as a stale reference —
  and an Agent that keeps observing will keep getting the same answer.

## Degradation vocabulary

`NoDesktopSession`, `AccessibilityBridgeMissing`, `PlatformUnsupported`,
`EngineMissing`, `PermissionPending`, `PermissionRestartRequired`,
`RunningAsRoot`, `RemoteRuntimeUnsupported`, `FeatureDisabled`.

An empty accessibility tree **without** a degradation reason is treated as an
engine error, never as "this window has no controls". Those two readings demand
opposite responses from a model.

## Audit and redaction

The ledger records the action kind, a redacted summary, the canonical target,
the risk class, the delivery mode, the verification state and the source. It
stores **no** typed text, clipboard contents, accessibility tree bodies, window
titles beyond the canonical application label, or screenshots. `Debug` for
frames, observations and screenshots prints metadata only.

Screenshots written for the CLI path live in a `0700` directory, are `0600`,
lose their inline base64, and expire after a day.

## Loop detection

The service keeps a bounded history of recent actions and sampled screen
fingerprints, and reports the stuck shapes it knows: a short repeated pattern,
the identical action three times, a frozen screen, a pointer-only stretch,
typing with no visible change, and an observe/act cycle over an unchanged
screen.

It **never interrupts**. The same shape can describe a form being filled field
by field, and cancelling a converging long action is worse than letting a stuck
one run one more round. The warning goes to the model and to the ledger; the
human's stop button is the only thing that ends a loop.

## Testing

`crates/computer` carries the unit tests for the parts that fail silently or
dangerously: the risk model, the reference lifecycle, the stop and disconnect
contracts, the helper protocol and its lifecycle, the loop heuristics, the
screenshot budget and the target-resolution rules. `crates/desktop-runtime`
pins the spawn policy, the audit projection and the delivery matrix, and
`crates/db` pins that the ledger round-trips and that an expired grant is not
live.

`apps/desktop --probe` exposes `computerUseContract`, which asserts — with no
desktop present — that degradation is named, credentials are refused, no
destructive class can be remembered, missing verification reads as unverified,
a stale reference is refused, screenshots are tier-gated and an audit row
carries no payload. `pnpm smoke:computer` runs it.

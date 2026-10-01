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

Eight tools, deliberately not a god tool:

```
computer_list_apps       → canonical identities, with the id to address them by
computer_get_app_state   → accessibility tree + element references (+ screenshot by tier)
computer_click           → element reference first, coordinates as a fallback
computer_type_text       → semantic write when an element is named, typing otherwise
computer_set_value       → semantic write with read-back verification
computer_press_key       → a key or a chord
computer_scroll          →
computer_permissions     → what the OS actually granted
```

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
| Launching an application | approval | session | yes |
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
| Linux Wayland | **graded**: standard Wayland has no protocol for raw input to an occluded surface, so those calls return background-unavailable and need a foreground takeover; setting another window's geometry is refused; the experimental path requires an explicit opt-in |

Do not say "supports Linux" without the display server. A deployment checklist
that must hold on every desktop environment:

1. a real desktop session exists (not merely an X server);
2. the accessibility bus answers (`org.a11y.Bus`), not merely a process;
3. the helper runs as the desktop user, never as root.

`--computer-doctor` answers all three plus the engine and permission state, and
maps each failure onto a named `ComputerUnavailableReason` instead of a boolean.

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
  something says so next to the switch that would have used it.

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

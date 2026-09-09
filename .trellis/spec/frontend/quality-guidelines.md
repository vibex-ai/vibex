# Frontend Quality Guidelines

Frontend quality means native desktop and the installed mobile runtime operate
against the same Vibex domain contract while presenting the right amount of
control for each form factor.

Validation today: [Architecture Baseline](../guides/architecture-baseline.md),
the `pnpm check` quality gates, `check:mobile-native`, and `cargo test` suites in
the owning crates. The capture-based evidence gate system was removed; scenario
sections below record behavioral contracts only, not gate workflows.

Historical React/Tauri migration scenarios were removed together with their
commands and artifacts and must not be restored. New UI work must cover shared
GPUI components, platform dependency isolation, Wide on native desktop, the
compact native mobile client, native input, and the
NativeBackend/WebRemoteBackend boundary.

## Review Checklist

- UI renders Vibex timeline and capability types, not raw provider SDK payloads.
- Wide screens follow the GPUI Desktop workbench model; Medium/Compact recompose the
  same domain components.
- The native mobile client remains a remote client and never runs a local
  Agent/Git/PTY/filesystem.
- Permission, Plan, Tool call, Diff, and command cards are collapsible.
- Destructive remote actions require clear confirmation.
- Dark mode is implemented for every new screen.
- Loading, empty, streaming, error, reconnecting, and permission-pending states
  are handled.
- Components are keyboard accessible where interaction exists.

## Responsive Requirements

Desktop:
- Left navigation, central workspace, right rail.
- Collapsible panels.
- Split panes and tabs.
- Integrated editor, Git, terminal, and Agent views.
- Management panels rendered inside the central work area must size against the
  available panel width, not just viewport breakpoints. In the three-column
  workbench, `xl:grid-cols-*` can still leave a form column too narrow when the
  right rail is open; keep dense forms stacked or wait until `2xl`/a proven
  container width before enabling secondary columns.

Native mobile:
- Single-column list-to-detail flows.
- Bottom or thumb-accessible action bars.
- Compact timeline cards.
- Permission approvals optimized for quick review.
- GUI Agent timeline content remains primary; terminal data is shown only in the
  shared terminal surface when the session exposes it.

## Test Expectations

Add tests or story coverage for:

- Timeline card rendering by event kind.
- Permission approval and denial flows.
- Reconnect state and timeline catch-up UI.
- Provider injection preview redaction.
- Git diff and destructive action confirmation.
- Mobile layout for session detail, Git change, terminal, and Provider switch.
- Dark mode snapshots or visual checks for major screens.

## Scenario: Native Mobile GPUI Contract

### 1. Scope / Trigger

- Trigger: changing `apps/mobile`, shared Agent timeline projections, native
  pairing/storage/input code, or the vendored mobile platform integration.
- The contract covers Android and iOS source structure and the GUI session
  behavior. It does not require a platform SDK on every development machine.

### 2. Signatures

```text
cargo check -p vibex-mobile --locked
cargo ndk -t arm64-v8a check -p vibex-mobile --locked
cargo test -p vibex-mobile --locked
node scripts/check-mobile-native.mjs
node scripts/check-mobile-native.mjs --self-test
pnpm build:mobile:android
pnpm build:mobile:ios
```

### 3. Contracts

- `apps/mobile` is a Rust GPUI crate. Android enters through NativeActivity and
  `gpui_android`; iOS exports the Rust entry point and uses `gpui_ios`' UIKit
  loop. No DOM, browser host, or downloaded application bundle is part of the
  product.
- The mobile view consumes `AgentWorkflowController` and the shared desktop
  timeline projection. User bubbles, Agent Markdown, process/tool details,
  approval cards, and the composer remain GUI components; a terminal cannot be
  substituted for the session page.
- The mobile treatment is visual and ergonomic: restrained dark surfaces,
  compact spacing, small radii, clear secondary text, full-page side navigation,
  and thumb-safe explicit actions. Vibex labels, state semantics, and approval
  behavior remain authoritative.
- Pairing, route selection, reconnect, and credential storage stay outside View
  rendering. Credentials are validated, atomically persisted, and redacted from
  diagnostics; the PC remains the state authority.
- Native fonts are loaded before opening the first window. IBM Plex Sans is the
  interface family and the reviewed WenQuanYi payload is the CJK fallback; both
  remain registered in the license gate.
- `Window::insets()` is the only safe-area/IME geometry source. GPUI refreshes
  the window when platform insets change, and the root composition applies the
  effective top/right/bottom/left padding.
- Native text fields explicitly move GPUI focus on touch and request the soft
  keyboard. Pairing honors the transport encoded in the trusted
  `vibex://open/<transport>` entry instead of silently preferring another route.
- Android text fields expose a real focusable `View` and non-null
  `InputConnection` to the IME. Calling `showSoftInput` against NativeActivity's
  DecorView is not text-input integration. The Java editor is a platform adapter;
  the focused GPUI input handler remains the document owner and synchronizes text
  and selection back to that editor.
- Android `TextWatcher` and selection offsets cross the JNI boundary as UTF-16
  ranges and are queued onto the GPUI thread before they reach an input handler.
  Stale ranges are clamped before slicing, and shaped placeholder offsets must
  never become document offsets when the document is empty.
- Android platform initialization accepts a replacement `AndroidApp` when an
  Activity is recreated in the same process and clears pending IME events for the
  previous Activity. A process-global one-shot Activity handle is invalid for the
  NativeActivity lifecycle.
- The session page is the center of a three-page horizontal composition. A
  right-moving finger opens the project/session page from the left; a
  left-moving finger opens the workspace-tools page from the right. Both pages
  follow the pointer continuously after a horizontal threshold, cover the full
  safe-area-adjusted viewport when open, close with the inverse gesture, and
  settle after roughly 25% travel or a small final delta toward the destination,
  with symmetric hysteresis for opening and closing and a faster close animation.
  Reversing that transition requires a materially stronger final-direction delta,
  so release-adjacent direction jitter cannot override the travel decision. A tap
  and clearly vertical pan remain available to the underlying control or scroller.
- Files, Git, Terminal, Providers, and Runtime live in the right workspace-tools
  page and remain selectable through its internal tabs. Do not duplicate these
  launchers in a strip on the center session page. The center-page swipe must
  reach the left project/session page independently, while the top-left menu
  button remains as an explicit fallback entry.
- Native touch scrolling is a platform contract, not a view-level acceleration
  workaround. Android and iOS emit the full per-event finger translation, use a
  bounded recent velocity window, and continue a moving release with time-based
  deceleration. A stationary hold suppresses momentum and a new touch stops any
  active deceleration immediately.
- A variable-height mobile timeline gives every unmeasured Turn a non-zero
  initial height hint. The first frame must expose the complete estimated scroll
  extent before off-screen Turns are measured; intrinsic measurements replace
  hints as rows render. Virtualizing the rows without this initial extent is not
  sufficient because the first upward gesture can otherwise be clamped to the
  small measured tail.
- The left project/session page and timeline own independent persistent scroll
  handles. A wheel or touch-pan routed inside the open page is consumed there so
  the timeline does not move behind it; this event isolation is separate from
  platform drag and momentum fidelity, and both behaviors must be verified.
- A full-page overlay that appears during an active horizontal pan must preserve
  the same `ScrollWheelEvent` hit chain. Use `block_mouse_except_scroll` (or a
  persistent gesture host) for the overlay; `occlude` removes the center-page
  capture host from later move/end events and leaves the page at its first
  partial offset. The overlay must consume scroll events that are not claimed
  by the horizontal drawer gesture so the page underneath remains isolated.
- A mobile client can create a session through the shared typed backend using
  an available runtime and desktop-owned workspace. Pending ACP elicitation
  forms render as explicit text/number/boolean/single/multi controls and resolve
  through `AgentWorkflowController`; they are never silently omitted.
- Every interactive control has a stable size and an explicit pressed/disabled/
  busy state. Loading, empty, streaming, reconnecting, error, and pending
  approval states must be renderable without layout jumps.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Old mobile runtime tree or host metadata returns | `check-mobile-native` fails. |
| Mobile view bypasses the shared controller or renders a terminal-only session | Review fails; no platform build claim. |
| Pairing record is malformed, mismatched, or not redacted | Storage validation fails closed and the record is cleared. |
| Stored connection cannot reach Desktop | Keep the credential and show Retry plus explicit Disconnect; do not force re-pairing. |
| No session exists | Offer typed session creation after runtime/workspace catalogs load; never make the composer a silent no-op. |
| Agent requests elicitation input | Render and validate the compact form, or show a typed unsupported-field error; never drop the request. |
| Safe-area or IME inset changes | Refresh and recompute root padding from `Window::insets()` without hard-coded device geometry. |
| Android input is focused | `dumpsys input_method` identifies the bridge editor as `mServedView`, reports a non-null `mServedInputConnection`, and exposes text input rather than the DecorView fallback. |
| Placeholder is clicked while its document is empty | Resolve the selection to document offset zero; never slice with the shaped placeholder's byte offset. |
| Android Activity is recreated while its process survives | Replace the platform Activity handle, discard stale IME events, and start normally without a one-shot initialization panic. |
| A moving Android touch receives `ACTION_UP` | Apply the final pointer delta, end the direct pan, then continue with bounded time-based momentum from Android's monotonic event timestamps. |
| A touch is held still before release or a new touch begins during momentum | Do not fling after the hold; cancel existing momentum immediately when the new touch lands. |
| A variable-height timeline opens at the bottom before off-screen Turns are measured | Its estimated maximum offset already covers every Turn, so the first upward gesture is not clamped to the measured tail. |
| A horizontal pan starts on the center session page | Right-moving fingers reveal the left project/session page; left-moving fingers reveal the right workspace-tools page, one physical pixel per reported pointer pixel. |
| A side page is inserted before the current touch ends | Keep the root capture host in the scroll hit chain and deliver every later move/end event to the same gesture owner; never leave the first partial offset as a settled state. |
| A side page reaches its open state after an inset or viewport-width change | Resolve its travel from the current safe-area-adjusted viewport width and cover the center page completely; do not retain a fixed drawer width. |
| The project/session page is open and receives a vertical scroll | Move only that page's list; the timeline offset remains unchanged. |
| Approval or composer action is unavailable while the server is busy | The control is disabled with an explicit busy state; no duplicate mutation is sent. |
| Timeline generation/sequence is stale | Ignore the result and request authoritative refresh. |

### 5. Good/Base/Bad Cases

- Good: from the session page, drag right to reveal Sessions or left to reveal
  workspace tools; either page tracks one-for-one, covers the viewport, and
  closes with the inverse gesture.
- Good: the top-left menu button opens the same full-width Sessions page as the
  right-moving gesture.
- Base: a slow horizontal drag remains linear and settles after roughly 25% of
  the distance toward the next page; a vertical pan continues scrolling the
  timeline or active side page.
- Bad: make the left page edge-only or button-only, remove the explicit menu
  fallback, keep a fixed-width floating drawer, leave the five workspace
  launchers on the session page, or let a side page and the timeline scroll
  together.
- Good: a moving touch preserves the final release delta and decelerates smoothly
  while its drawer or timeline remains the scroll target.
- Good: once a side page appears during a drag, later move and end events still
  reach the drawer owner and the page settles at a full endpoint.
- Base: a stationary hold does not reuse an older movement sample to create a
  fling.
- Bad: an occluding overlay cuts off the active gesture after its first move, the
  platform emits only raw move deltas with no momentum, the view scales those
  deltas to compensate, or unmeasured virtual Turns contribute zero height to the
  first-frame scroll range.

### 6. Tests Required

- `cargo test -p vibex-mobile --locked` covers storage permissions/atomicity and
  malformed-file removal, secret-redacted `Debug`, UTF-8/UTF-16 IME editing,
  empty-placeholder hit testing, stale selection clamping, both main-page swipe
  directions, inverse side-page closing, terminal/cancelled gesture recovery,
  the menu-button fallback, rendered move/end delivery after an overlay appears,
  full-viewport travel, snap decisions, Markdown block projection, and route
  bundle validation.
- `cargo test -p vibex-ui --locked` covers the shared controller, timeline
  projection, approval surfaces, and compact shell semantics.
- `node scripts/check-mobile-native.mjs --self-test` covers the negative source
  contract, native entry points, vendored platform dependencies, GUI session
  markers, and forbidden legacy paths.
- Android-target compilation must include `gpui_android`; a host-only mobile
  check cannot validate code behind `cfg(target_os = "android")`. Platform unit
  tests assert event-time velocity, fling bounds, and frame-rate-independent
  momentum decay. Mobile GPUI tests assert a non-zero first-frame timeline
  extent and drawer/timeline scroll isolation.
- Android and iOS device validation must separately exercise touch/keyboard,
  full-page navigation in both directions, safe-area/IME changes, timeline
  streaming, approval and elicitation resolution, new-session creation,
  send/stop/continue, reconnect, bundled Latin/CJK text, and credential redaction
  before a release claim.
- Android device validation must also inspect the served editor/InputConnection,
  perform insert/delete/cursor/insert editing, and recreate the Activity at least
  twice in the same process while checking native and Java crash logs.

### 7. Wrong vs Correct

```text
Wrong: copy a terminal screen into mobile, keep a second session reducer, or
       load a remote page at runtime.
Correct: native GPUI -> shared AgentWorkflowController -> authoritative desktop
         timeline -> compact GUI cards and composer.

Wrong: stop drawer event propagation and call the scrolling issue fixed while
       Android still ends every pan at `ACTION_UP` and unmeasured Turns have no
       scroll extent.
Correct: isolate each scroll surface, preserve direct finger deltas, implement
         platform momentum, and seed virtual Turn heights before first paint.

Wrong: center session page -> button-only or edge-only fixed drawer;
       center session page -> persistent Files/Git/Terminal/Providers/Runtime strip.
Correct: menu fallback + full-page Sessions <- right swipe - center session -
         left swipe -> full-page workspace tools with internal tabs.
```

## Scenario: Native Mobile QR Pairing Entry

### 1. Scope / Trigger

- Trigger: changing the first-run mobile screen, native camera integration,
  pairing result delivery, or the one-time desktop pairing claim.

### 2. Signatures

```text
scanner::launch() -> BackendResult<()>
scanner::subscribe() -> UnboundedReceiver<String>
Android nativeOnPairingQrScanned(value: String) -> ()
iOS vibex_mobile_pairing_qr_scanned(value: *const char) -> ()
claim_pairing_link(link: String) -> BackendResult<MobileCredentialBundle>
Android Activity static initializer -> System.loadLibrary("vibex_mobile")
initialize_android_tls(AndroidApp) -> application Context registered before GPUI
```

### 3. Contracts

- An unpaired mobile client opens on a QR pairing surface. The primary action
  launches the platform camera scanner; the screen must not expose a text field
  for pasting the pairing link or secret fragment.
- Android uses a non-exported camera Activity and iOS uses a full-screen native
  camera presentation. Both accept only full `vibex://open/<transport>#/pair/...`
  entries and leave unrelated QR codes unhandled.
- Every Android Activity that declares a Java-to-Rust native method explicitly
  loads `libvibex_mobile.so` in a static initializer. NativeActivity manifest
  metadata loads the entry library but does not associate Java JNI callbacks
  with the application ClassLoader. The scanner may be restored directly after
  process death, so it cannot rely on `GpuiNativeActivity` loading first.
- The Android package locates the `rustls-platform-verifier` AAR through locked
  Cargo metadata and initializes it with the application Context before GPUI or
  remote networking starts. Missing JVM support is a packaging failure, not a
  reason to weaken TLS verification.
- Native callbacks enqueue the opaque link without logging or persisting it.
  GPUI consumes the result once, then delegates parsing, expiry, selected-route,
  identity, claim, and credential validation to the existing pairing owner.
- Camera permissions are declared by each platform host. Permission denial or a
  missing camera returns to the pairing screen and never falls back to manual
  secret entry.
- A successful claim persists only the validated credential bundle through
  `CredentialStorage`; session/private transport keys retain their existing
  in-memory and redaction boundaries.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Camera permission denied or camera unavailable | Show a native camera error, dismiss the scanner, and keep the QR pairing screen available. |
| QR value does not start with `vibex://open/` | Keep scanning; do not forward or log the value. |
| Pairing offer is malformed, expired, or route-invalid | Existing structured pairing error is shown on the QR pairing screen; no credential is saved. |
| Native result callback fires more than once | Consume one pending value and let the pairing busy guard prevent duplicate claims. |
| An Android Activity declares a native callback without loading `vibex_mobile` | `check-mobile-native` fails; never ship the resulting `UnsatisfiedLinkError` crash path. |
| Android TLS verifier AAR or application-Context initialization is missing | Native contract/build check fails before device qualification; do not fall back to accepting invalid certificates. |
| Pairing claim and credential validation succeed | Atomically save the bundle and enter the existing connecting flow. |

### 5. Good / Base / Bad Cases

- Good: scan a Desktop-generated Direct, Tailnet, or self-hosted Relay QR code;
  the encoded transport reaches `claim_pairing_link` unchanged and pairing uses
  that route.
- Good: after process death Android restores the scanner Activity first, loads
  the Rust library, delivers one JNI result, and enters the paired interface.
- Base: cancel the native scanner and remain on the QR pairing screen with no
  state or credential mutation.
- Bad: render `TextInput::new("Paste pairing link", ...)`, log a scanned value,
  or parse and claim the offer independently in Android/iOS host code.
- Bad: rely only on `android.app.lib_name`, or assume the main NativeActivity
  always runs before a secondary Activity invokes its JNI callback.

### 6. Tests Required

- `cargo test -p vibex-mobile --locked` asserts pending scan results are consumed
  once and retains pairing route, credential validation, storage, and redaction
  coverage.
- `node scripts/check-mobile-native.mjs` asserts Android/iOS camera declarations,
  native result bridges, explicit Android library loading, TLS runtime
  packaging/initialization, the GPUI scanner queue, and absence of
  `pairing_input`; its self-test must reject each missing Android bridge.
- Android/iOS platform builds compile their CameraX/ML Kit and AVFoundation
  implementations. Device qualification covers allow, deny, cancel, unrelated
  QR, expired offer, and one successful pairing for every supported route.

### 7. Wrong vs Correct

```text
Wrong: manifest library metadata -> restored scanner Activity -> unresolved JNI callback crash.
Correct: Activity System.loadLibrary -> one-shot GPUI queue -> claim_pairing_link -> validated credential storage.
```


## Scenario: Native File Dialog Backend Isolation

### 1. Scope / Trigger

- Trigger: GPUI adds or changes a native file/directory chooser or the Linux
  tray stack.

### 2. Signatures

```toml
# GPUI uses a distinct rfd release so Cargo cannot merge its Portal features
# with GTK3 features pulled in by any other workspace consumer.
rfd = { version = "0.17.2", default-features = false,
        features = ["wayland", "xdg-portal"] }
```

### 3. Contracts

- The default GPUI Linux binary uses XDG Desktop Portal and must not link GTK or
  WebKit solely for file dialogs. GTK is permitted only for the Linux AppIndicator
  system-tray integration; WebKit remains excluded from the default build.
- Any second consumer needing mutually exclusive `rfd` backend features must pin
  a different `rfd` release; Cargo cannot merge conflicting feature sets.
- GPUI awaits `AsyncFileDialog` through its existing async task boundary; the
  dialog backend must not require a Tokio reactor on arbitrary GPUI worker
  threads.

### 4. Validation & Error Matrix

- One `rfd` version has both `gtk3` and `xdg-portal` -> workspace build fails;
  split versions instead of weakening either backend.
- GPUI release contains GTK-backed file-dialog code or links WebKit -> fail the
  default package gate. A GTK dependency attributable only to AppIndicator tray
  support is expected.
- Portal dialog panics with “no reactor running” -> use the rfd backend/version
  whose executor is independent of GPUI worker-thread Tokio context.

### 5. Good/Base/Bad Cases

- Good: GPUI uses `rfd 0.17.x` Portal while any other consumer pins its own
  GTK3 release.
- Base: each app builds independently and `cargo check --workspace --all-targets
  --locked` also succeeds.
- Bad: two consumers share one `rfd` version with conflicting features.

### 6. Tests Required

- `cargo check --workspace --all-targets --locked`.
- `pnpm check:licenses` and the release packaging smoke after dependency changes.

### 7. Wrong vs Correct

#### Wrong

```toml
rfd = { version = "0.16", features = ["xdg-portal"] }
# Another workspace consumer transitively enables rfd 0.16/gtk3 in the same graph.
```

#### Correct

```toml
rfd = { version = "0.17.2", default-features = false,
        features = ["wayland", "xdg-portal"] }
```

## Scenario: GPUI Foundation Lifecycle

### 1. Scope / Trigger

- Trigger: the production GPUI workbench starts the shared desktop runtime, persists
  desktop UI state, or handles window close and application quit.
- Linux is the current release target; macOS and Windows remain deferred without
  platform claims until a future task runs their native checks.

### 2. Signatures

```text
DesktopRuntime::start(DesktopRuntimeConfig::preview_default())
App::on_app_quit -> flush DesktopUiStateV1 -> await DesktopRuntime::shutdown

Lifecycle markers are bounded diagnostics only:
vibex-foundation: ui-state-flushed
vibex-foundation: runtime-stopped
```

### 3. Contracts

- Linux title-bar/window close callbacks must not call `cx.quit()`: GPUI's X11 close
  path can re-enter window removal and panic. When close-to-tray is enabled, the
  tray global retains the existing workbench entity under `QuitMode::Explicit` while
  the application has zero windows; this must not rely on `WindowOptions::show`
  because GPUI's Linux backend maps every newly created window. Restoring creates one
  visible window and must recover from a stale tracked window handle. When disabled,
  the callback switches to `LastWindowClosed` and lets window removal initiate
  application quit. The app-level quit hook is the single owner of final cleanup:
  queue and synchronously flush the current UI state, spawn and await shared runtime
  shutdown, then allow process exit.
- `DesktopRuntime` owns the process/home lock. A second shell must fail while the
  workbench is live, and the same external lock probe must succeed only after awaited
  shutdown and process exit. Cleanup that is merely spawned and abandoned is invalid.
- The preview shell uses the isolated preview app id/home; it must not acquire a
  user's ordinary desktop state or an existing production home.
- The preview shell stays provider-free: managed ACP adapter installation must not
  block lifecycle tests (`VIBEX_FOUNDATION_SKIP_ADAPTER_INSTALL=1`); real adapter
  installation is covered by the ACP bridge smoke gates.

### 4. Validation & Error Matrix

- Window closes without both lifecycle markers -> fail the graceful-close claim.
- A Linux window/title-bar close callback calls `cx.quit()` -> reject the lifecycle
  implementation even if Wayland closes successfully; the X11 path can panic on re-entry.
- UI-state bytes do not change or an injected non-schema sentinel survives -> fail
  final persistence flush.
- A second process acquires the home while GPUI runs -> fail runtime exclusivity.
- The lock remains unavailable after exit -> fail shutdown ownership/release.
- Process exits non-zero or before runtime-ready -> fail the affected scenario.

### 5. Good/Base/Bad Cases

- Good: a real Wayland close dispatch exits zero, rewrites UI state, awaits runtime
  shutdown, and changes the lock probe from `locked by another process` to acquirable.
- Base: Windows and macOS remain deferred and unclaimed.
- Bad: kill the process to exit, or infer cleanup from `Drop` alone.

### 6. Tests Required

- `cargo test -p vibex-desktop-runtime --locked` asserts same-home contention,
  separate preview homes, process-level contention, and crash/drop release.
- `cargo test -p vibex-desktop --locked` covers the shell contract, responsive
  viewports, settings, primitives, fonts, locales, and source-compatible platform
  branches.

### 7. Wrong vs Correct

#### Wrong

```rust
window.on_close(|_, cx| {
    tokio::spawn(runtime.shutdown());
    cx.quit();
});
```

The process may exit before state flush, runtime shutdown, and lock release finish;
on Linux X11 the nested quit can also re-enter window removal and panic.

#### Correct

```rust
// No Linux window-close callback calls cx.quit(); LastWindowClosed starts quit.
cx.on_app_quit(|app, cx| {
    if let Some(writer) = app.ui_writer.as_mut() {
        let _ = writer.flush();
    }
    let shutdown = app.runtime.clone().map(|runtime| {
        gpui_tokio::Tokio::spawn(cx, async move { runtime.shutdown().await })
    });
    async move {
        if let Some(shutdown) = shutdown {
            let _ = shutdown.await;
        }
    }
});
```

One app-level owner completes persistence and runtime cleanup before exit.

## Scenario: GPUI Native Content Surfaces

### 1. Scope / Trigger

- Trigger: GPUI adds or changes the Terminal, PDF, or Office content surfaces, or
  the shared `ContentSurfaceLifecycle` they compose through.

### 2. Signatures

```text
vibex-desktop --native-content-contract <output.json>
vibex-desktop --native-content-workbench [output.json]

native-content-run.v1 -> {
  status,
  terminal { ptyCreated, rawByteSnapshots, imeCapableInput,
             commandSubmitted, commandMarkerObserved, frameRows,
             frameColumns, ingestedBytes, terminalOutputStored },
  privacy,
  limitations[]
}
```

### 3. Contracts

- `vibex-terminal::TerminalManager` remains the PTY owner. GPUI consumes bounded raw
  snapshots and must not create a second terminal/session persistence domain.
- Run and contract reports store booleans and bounded counts only. They must not
  contain the PTY command/output marker, URL, PDF text, Office text, private paths,
  clipboard data, or user content.
- A report proves only the slice it covers; behavior not covered by an owned protocol
  stays an explicit limitation, never an inferred pass.
- Shared `ContentSurfaceLifecycle` owns focus state in addition to visibility: opening
  an overlay clears focus and records `focusReturnPending`; only a current-generation
  `focus_entered` clears that pending state. Close, crash, failure, deactivation, and
  a newer activation clear focus. Same-generation callbacks after `Closed` are ignored.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Report contains marker text, URL, or content fields | Redaction validation fails. |
| Window close leaves the process alive or reports a panic | Clean-close contract fails. |
| Overlay closes without focus returning to the current surface | Focus contract fails; `focusReturnPending` must remain observable until a current-generation focus event. |
| Same-generation callback arrives after close | Ignore it and keep the surface `Closed`; it must not restore visibility or focus. |

### 5. Good/Base/Bad Cases

- Good: one sanitized command flows through IME-capable input, raw PTY snapshots
  observe its marker, close exits zero, and the report retains no output text.
- Bad: create a second terminal persistence domain, or log PTY output, user content,
  or private paths in a run report.

### 6. Tests Required

- `cargo test -p vibex-content -p vibex-terminal -p vibex-desktop --locked` covers
  switch fencing (seven bounded switches, stale/close callback handling), overlay
  focus return, latest bounds preservation, crash recovery, one final
  visible/focused surface, and zero Web allocations.

### 7. Wrong vs Correct

```text
Wrong: GPUI keeps its own PTY/session store, or a report collects command output.
Correct: TerminalManager owns PTYs -> GPUI consumes bounded raw snapshots ->
         reports store booleans and counts only.
```

## Scenario: Terminal Stress And Resource Budgets

### 1. Scope / Trigger

- Trigger: the product Terminal PTY/parser/surface, its memory or repaint budgets,
  or the `vibex-terminal-stress` harness changes.
- Ownership boundary: `TerminalManager` owns PTYs and raw snapshots,
  `TerminalSurfaceBackend` owns VT state, `TerminalFrameCache` owns damaged cells.

### 2. Signatures

```text
vibex-terminal-stress --soak-seconds <seconds> --output <report.json>

terminal-stress-linux-run.v1 -> {
  throughput, burst { renderUpdates, fullRepaints, partialRepaints,
                      changedRows, maxParseFrameMs, boundedRepaint },
  scrollback, lifecycle, resize, sequenceRebuild,
  soak { requestedSeconds, observedSeconds, activityTicks, sequenceGaps,
         rawDroppedChunks, renderUpdates, fullRepaints, partialRepaints },
  resources { rssGrowthBytes, fdLeakObserved, childLeakObserved },
  privacy
}
```

### 3. Contracts

- A quick run may exercise code locally; the full soak runs at least 300 observed
  seconds with recurring PTY writes, raw snapshots, parser sync, frame generation,
  and frame-cache application. It permits no sequence gap or dropped raw chunk.
- The 10 MiB fixture disables PTY output post-processing before hashing; otherwise
  `ONLCR` can turn LF into CRLF and create a false data-loss result.
- Product polling calls `TerminalManager::raw_snapshot_from(terminalId, nextSequence)`.
  It clones only unconsumed chunks; if `nextSequence` was evicted or belongs to an
  older restored runtime, it returns the retained ring so the backend rebuilds. Do not
  clone the full 16 MiB ring every 16 ms after the parser has caught up.
- A 120 FPS source burst must retain all 120 markers while the 16 ms surface frame path
  coalesces work. Counting raw PTY bytes alone is not bounded-repaint verification.
- Scrollback retains at most 10,000 history lines and the terminal model remains within
  128 MiB. Repeated create/kill/restore must leave no live sessions.
- The bounded-repaint load writes a unique counter to a stable viewport row with
  explicit erase/home control sequences; newline-driven scrolling legitimately marks
  the whole viewport damaged and must not be paired with a `fullRepaints <= 2`
  assertion.
- Each soak activity tick waits for its incremental raw snapshot to reach the parser.
  Empty polls are allowed, so `snapshots >= activityTicks`; every completed tick must
  produce exactly one frame-cache update, at most two total full repaints, and at
  least one partial repaint for a non-zero run.
- `/proc` samples bind parent RSS growth to 64 MiB and require final FD and
  direct-child counts to return to baseline (FD tolerance: two observation
  descriptors).
- Reports retain hashes, counts, timings, booleans, and bounded resource values only.
  Raw terminal output, markers, environment, and home paths are forbidden.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| 10 MiB observed hash differs or raw chunks drop | Stress run fails. |
| Source burst loses a marker or frame-cache work is unbounded | Fail the burst assertion. |
| History exceeds 10,000 or model exceeds 128 MiB | Fail the scrollback assertion. |
| Injected sequence gap does not rebuild retained state | Fail the sequence-rebuild assertion. |
| Caught-up incremental snapshot retains bytes, or new snapshot equals the full ring | Fail the incremental raw-copy assertion. |
| Soak activity has no matching parser/frame update, or stable-row output repeatedly causes full repaint | Fail the soak assertion. |
| Final FD/direct-child count grows or RSS grows over 64 MiB | Stress run fails. |

### 5. Good/Base/Bad Cases

- Good: 10 MiB hashes match, 120 source frames coalesce into fewer damage-scoped frame
  updates, 10,000 history lines stay under budget, 16 restore cycles close, and the
  soak returns RSS/FD/child counts within budget.
- Bad: drive the bounded-repaint soak with scrolling newlines, clone the entire raw
  ring on every poll, or report fabricated durations.

### 6. Tests Required

- Run `cargo test -p vibex-content -p vibex-terminal --locked` and the quick stress
  binary while developing; run the full soak on real hardware before a release claim.

### 7. Wrong vs Correct

```text
Wrong: soak tick -> print newline -> scroll viewport -> require fullRepaints <= 2
Correct: erase/home stable row + unique counter -> wait for incremental parser/frame update
```

## Scenario: GPUI PDF Surface With Bounded Background Rendering

### 1. Scope / Trigger

- Trigger: GPUI renders a local PDF through the product `PdfDocumentController` rather
  than the engine-feasibility spike.
- The shared surface may run inside the Native Content workbench or through the
  standalone PDF workbench used for focused validation.

### 2. Signatures

```text
PdfSurface::new(libraryPath, initialDocument?, output?, window, cx)
vibex-desktop --native-content-pdf-workbench \
  <pdfium-library> <fixture.pdf> [output.json]

pdf-surface-run.v1 -> {
  status: "ready" | "error",
  pageCount, currentPage, targetWidth, renderedPageIndexes,
  zoomMode, controls, resources, lastWorkerResources, workerProcesses, uiImages,
  error?: { code, retryAvailable, explicitSystemOpenAvailable },
  privacy, limitations[]
}
```

### 3. Contracts

- The GPUI background executor supervises one helper process per load/render request.
  Only the helper binds PDFium, reads the source, owns `PdfDocumentController`, and
  decodes RGBA pages. The foreground owns UI state and bounded `RenderImage` handles.
- A newer page/zoom/resize request increments a UI request generation, cancels the
  previous token so the supervisor kills and reaps its child, and replaces the single
  pending render. A stale worker cannot publish old bitmaps or state.
- Use a virtual page list for up to 10,000 metadata rows. Decode only the current page
  plus controller overscan; never instantiate 10,000 page images or buttons eagerly.
  The document viewport paints only the selected current page; overscan bitmaps remain
  warm in the bounded image set rather than becoming sibling pages whose async scroll
  position can disagree with the selected page.
- The controller cache is capped at 4 pages / 48 MiB. GPUI image copies are separately
  capped at 3 pages / 72 MiB and prioritize the current page. Fit/zoom target width is
  64-2,048 pixels even though the controller accepts up to 4,096.
- The page-list and document columns have explicit full-height constraints inside their
  horizontal flex parent. Fit-width observes the `PdfSurface` element's allocated bounds
  through `on_prepaint`, not the containing window width, then waits 120 ms before
  rerendering. This keeps embedded/split surfaces from collapsing vertically or clipping
  a page sized for the whole window. Replacing the task cancels the old debounce.
- Physical readiness requires ready phase, no worker, no resize task, no pending render,
  and a target width that matches the latest allocated surface bounds.
- UI states are explicit: empty, loading, ready, rendering, typed error, and closed.
  Controls cover virtual page list, scroll viewport, previous/next, 50-200% zoom,
  fit width, retry, close, file picker, and explicit system open.
- New document load starts a fresh child with no cross-request controller state. Close
  and drop cancel active work; child exit/reap releases controller/native cache state.
- Validate system-open targets as existing `.pdf` files and pass them as process
  arguments, never shell text. Reap the opener child off the foreground thread.
- Linux package discovery checks the reviewed `usr/lib/vibex-desktop/pdfium`
  resource; macOS/Windows candidates remain source-compatible but package-disabled.
- A `ready` JSON report contains counts, budgets, booleans, and limitations only. It
  never contains the path, PDF text, password, or page pixels, and it does not prove
  native pixels, scrolling, keyboard, or pointer input.
- An encrypted document opened without a password writes a redacted `error` report with
  `pdf_password_required`, Retry and explicit System open availability, zero decoded/UI
  resident bytes, and no password field or value. The current surface does not persist a
  password or claim an embedded password-entry workflow.
- Source-size preflight runs on the background worker before PDFium binding and uses the
  bounded shared reader. The surface writes redacted zero-resident error reports for
  `pdf_source_size_invalid`, `pdf_page_count_unsupported`, and
  `pdf_page_exceeds_cache_budget`.
- Every successful child spawn is reaped. A native abort returns `pdf_worker_crashed`;
  a non-returning call is killed at the hard deadline with `pdf_worker_timeout`; a clean
  request after each failure proves restart. `resources` reports zero current native
  residency after exit, while `lastWorkerResources` reports only the bounded child peak.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| PDFium resource cannot be discovered/bound | Typed `pdfium_*` error plus system-open option. |
| Selected path is missing or not exact `.pdf` | `pdf_path_missing` / `pdf_path_extension_invalid`. |
| New request arrives during render | Cancel old token, keep only latest pending request, reject stale publish. |
| Estimated native page RGBA exceeds controller cache | `pdf_page_exceeds_cache_budget` before PDFium allocation. |
| GPUI image copies exceed 3 pages / 72 MiB | Drop overscan by priority; current page must remain or return `pdf_ui_image_budget_exceeded`. |
| Resize changes fit target | Debounced rerender; no identical-width work. |
| Embedded columns have no full-height constraint, or fit uses whole-window width | The page renders blank/cropped in embedded/split surfaces; do not accept model counts alone. |
| Selected page and visible page differ after navigation | State contract fails even when `currentPage` changed. |
| Corrupt/encrypted/native failure | Typed error state with Retry and explicit System open; encrypted fixture reports `pdf_password_required` and zero resident bytes. |
| Source exceeds 256 MiB | `pdf_source_size_invalid` before PDFium binding; zero decoded/UI bytes. |
| Document has 10,001 pages | `pdf_page_count_unsupported`; zero decoded/UI bytes. |
| Extreme page exceeds estimated RGBA budget | `pdf_page_exceeds_cache_budget` before native render; zero decoded/UI bytes. |
| Native call crashes or exceeds a hard deadline | Kill/reap child; `pdf_worker_crashed` / `pdf_worker_timeout`; next isolated request succeeds. |
| Worker report contains unsafe path or invalid bitmap length | `pdf_worker_protocol_failed`; no image publication; temporary directory removed. |
| Close succeeds | Closed state, no metadata, active worker, controller/native cache, or UI images. |

### 5. Good/Base/Bad Cases

- Good: a 12-page PDF starts at page 1, renders indexes `[0, 1]`, reports both cache
  budgets within bounds, and publishes no document identity in the ready report.
- Good: rapid page 2 -> page 5 -> zoom requests kill/reap the obsolete helper and
  publish only the latest generation.
- Good: a 3/5-width embedded surface paints a complete Fit-width Page 2, while decoded
  Page 1/3 overscan remains budgeted but is not rendered as a competing scroll child.
- Good: the encrypted fixture reaches `error/pdf_password_required`, exposes Retry and
  System open, retains no controller/UI image bytes, and stores no password.
- Good: oversized-source, too-many-pages, and extreme-page fixtures each reach their
  exact typed error with Retry/System open and zero resident controller/UI image bytes.
- Base: a 4K window clamps fit rendering to 2,048 pixels instead of allocating an
  unbounded full-width page.
- Bad: keep a controller behind a foreground mutex and render PDFium synchronously from
  a click handler.
- Bad: treat the controller RGBA cache as the only memory budget while retaining an
  unbounded second set of GPUI `RenderImage` copies.
- Bad: report `status=ready` while the page never rendered in its surface.
- Bad: size Fit width from `window.viewport_size()` inside a split, or rely on
  `renderedPages > 0` while the page list/document columns have zero allocated height.
- Bad: cancel a worker future without killing/reaping its child, or report the last
  child's cache as current desktop-process residency.

### 6. Tests Required

- Unit-test fit/percentage width bounds, exact extension policy, RGBA-to-BGRA conversion,
  UI image current-page priority, and both resource metrics.
- Controller tests must reject invalid dimensions and over-budget estimated RGBA before
  the native render call.
- Worker supervisor tests must run normal -> abort -> recovery -> hang -> recovery,
  assert five children started/reaped, exact crash/timeout codes, and privacy. Negative
  tests reject missed failures, unreaped children, recovery drift, and privacy leakage.
- Worker soak runs 49 requests with the frozen 37 normal / four cancel / four crash /
  four timeout matrix, proves 12 recoveries, reaps all 49 children, retains
  FD/direct-child/temp-directory baselines, keeps current native residency zero, and
  stays within 64 MiB parent RSS growth. Negative tests reject every leak dimension.
- Linux smoke runs launch the standalone workbench with reviewed PDFium and the
  deterministic fixtures (12 pages, encrypted without password, oversized source,
  10,001 pages, extreme page) and assert exact typed codes, recovery controls,
  cache/image budgets, privacy, and zero resident bytes where applicable.

### 7. Wrong vs Correct

#### Wrong

```rust
let pages = controller.render_viewport(...)?; // foreground click handler
self.viewport_width = f32::from(window.viewport_size().width); // wrong in a split
self.images.extend(pages.into_iter().map(to_render_image));
```

#### Correct

```rust
let worker = cx.background_spawn(async move {
    run_isolated_pdf_request(
        &library, &document, generation, page, width,
        PDF_WORKER_TIMEOUT, &cancellation, PdfWorkerFaultMode::None,
    )
});
// Convert bounded pages and publish only after child reap and generation validation.
// Observe the surface element bounds, debounce Fit width, and paint only current_page.
```

## Performance Expectations

- Virtualize long session timelines, file trees, Git diffs, and terminal output
  where needed.
- Keep terminal output rendering buffered and throttled.
- Paginate history and large timeline fetches.

## Accessibility Expectations

- Command palette, pairing dialogs, permission dialogs, and settings modals must
  support keyboard navigation and focus management.
- Cards with disclosure state need accessible labels and state.
- Approval buttons must include text labels.
- Color cannot be the only signal for provider health, Git status, or security
  warnings.

## Scenario: GPUI Code Workbench Bounded And Wrapping Lists And Cross-Layer Types

### 1. Scope / Trigger

- Trigger: GPUI renders Files, Git Changes, Git History, or diff rows from a model
  whose total row count can exceed the visible viewport.
- Trigger: Preview persists a horizontal/vertical split that must remain usable at
  the 360 x 620 minimum viewport.
- Trigger: a Rust file DTO adds a leaf enum or value object consumed through the
  shared Backend contract.

### 2. Signatures

```text
bounded_uniform_range(requested, total, limit) -> Range<usize>
uniform_list(id, row_count, render_range).track_scroll(&state).size_full()
list(PatchListState.list, render_row).size_full()
responsive_split_direction(persisted_direction, viewport_width) -> rendered_direction

FileReadResponse.encoding -> FileEncoding
FileReadResponse.line_ending -> FileLineEnding
crates/core/src/file.rs -> vibex-backend -> GPUI
```

### 3. Contracts

- The model owns complete row identity and ordering. Files, Git Changes, and Git
  History request only the visible `uniform_list` range, clamp it through
  `bounded_uniform_range`, and prepare detail only for that bounded window plus
  bounded overscan.
- Every scrolling `uniform_list` must both track its `UniformListScrollHandle` and
  call `.size_full()`. Without an explicit full size, GPUI may give the list an
  intrinsic zero/small height and render a blank pane even when the row callback is
  correct.
- Diff and commit-patch rows must wrap content at the available pane width, so they
  use GPUI's variable-height `list` with a persistent per-tab `ListState`; a
  `uniform_list` measures one row and will overlap or clip wrapped rows. Seed the
  variable list with `DIFF_ROW_HEIGHT`, render one model row per callback, reset it
  when the patch revision changes, reconcile its item count after commit-file
  collapse/expand, and preserve focused-file scrolling.
- Files, Git Changes, Git History, working-tree diff, and commit patch remain
  independently identified virtual surfaces. Their render callbacks must not
  materialize the full model as `.children(...)`; the variable patch list measures
  visible rows plus bounded pixel overdraw, initial diff model work remains capped
  at 500 rows, and any eager fallback remains at or below 5,000 rows.
- Responsive layout is a render projection. Below 760 logical pixels, a persisted
  horizontal Preview split renders vertically; the reducer and persisted
  `SplitDirection` remain horizontal so widening the window restores the user's
  chosen layout.
- `crates/core` owns every public protocol leaf type, including `FileEncoding`
  and `FileLineEnding`; Backend traits and GPUI consumers import those types
  directly instead of defining parallel enums.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Requested virtual range exceeds the eager bound | Clamp the returned range; never allocate all rows. |
| A tracked `uniform_list` omits `.size_full()` | Source contract test fails. |
| A wrapping patch row is rendered in `uniform_list` or keeps a fixed height | Source contract test fails; use the per-tab variable-height list. |
| Patch width, revision, or visible row count changes | Remeasure/reset the `ListState` without materializing the full patch; focused-file navigation still reaches the requested row. |
| 360 px viewport renders a persisted horizontal split | Render panes vertically without mutating persisted direction. |
| A Backend or GPUI layer redefines a Rust protocol leaf type | Rust compilation or protocol tests fail; import the canonical `crates/core` type. |

### 5. Good/Base/Bad Cases

- Good: a 100,000-row tree and 20,000-row diff keep complete model state while GPUI
  renders only visible items in full-height list viewports; long diff content wraps
  and increases only its own row height.
- Good: a horizontal desktop split stacks at 360 px and returns to horizontal after
  resize because persistence was not rewritten.
- Base: a small list uses the same virtual path; no special eager renderer is needed.
- Bad: keep patch rows at a fixed height after enabling wrapping, persist `Vertical`
  when the window narrows, omit `.size_full()`, or assume a compound DTO causes its
  new enum dependency can be redefined independently in a UI layer.

### 6. Tests Required

- Model tests assert exact deep ranges, stable row ids, 100,000 tree rows, 20,000
  diff rows, 500-row initial diff, cache bounds, and stale generation rejection.
- GPUI tests/source contracts assert three `uniform_list` calls with three bounded
  ranges/full-size tracked handles, one variable-height patch-list helper shared by
  working-tree and commit previews, wrapping diff text, and both desktop and
  360 x 620 layouts.

### 7. Wrong vs Correct

#### Wrong

```rust
uniform_list("diff", rows.len(), render_wrapping_rows).track_scroll(&scroll)
fn render_diff_row(row: PreparedDiffRow) -> impl IntoElement {
    h_flex()
        .h(px(DIFF_ROW_HEIGHT))
        .child(div().whitespace_normal().child(row.row.content))
}
self.preview.direction = SplitDirection::Vertical; // narrow-window side effect
```

```rust
// FileReadResponse references these, so assume they will be exported recursively.
push_decl::<FileReadResponse>(&mut output);
```

#### Correct

```rust
list(patch_state.list.clone(), render_one_wrapping_row).size_full();
fn render_diff_row(row: PreparedDiffRow) -> impl IntoElement {
    h_flex()
        .min_h(px(DIFF_ROW_HEIGHT))
        .child(div().min_w_0().flex_1().whitespace_normal().child(row.row.content))
}
let rendered = responsive_split_direction(self.preview.direction, viewport_width);
```

```rust
push_decl::<FileEncoding>(&mut output);
push_decl::<FileLineEnding>(&mut output);
push_decl::<FileReadResponse>(&mut output);
```

## Scenario: Native Advanced Markdown Document Boundary

### 1. Scope / Trigger

- Trigger: Agent timeline content or workspace Markdown preview changes parsing,
  rendering, navigation, selection/copy, resource handling, raw HTML, local math or
  diagram artifacts, or syntax highlighting.
- The framework-neutral document and policy live in `vibex-markdown`; GPUI is one
  renderer of that contract. Product surfaces must not independently parse or
  rewrite Markdown before rendering.

### 2. Signatures

```text
parse_markdown(MarkdownInput) -> MarkdownDocument
ResourcePolicy::resolve(ResourceRole, source, label) -> ResolvedResource
MarkdownView::new(ElementId, MarkdownInput) -> MarkdownView
MarkdownView::from_document(ElementId, Arc<MarkdownDocument>) -> MarkdownView
agent_markdown_preview_path(&ResolvedResource, Option<&str>) -> Option<String>

ArtifactController::schedule(ArtifactRequest) -> ArtifactSchedule
ArtifactController::complete(request, result, view_id, revision, live_nodes)
  -> ArtifactCompletion
render_local_artifact_with_timeout(request, SvgPolicy, timeout)
  -> Result<Arc<SvgArtifact>, ArtifactError>
SvgPolicy::sanitize(svg, id_prefix) -> Result<SvgArtifact, SvgPolicyError>
```

### 3. Contracts

- One canonical `MarkdownDocument` owns source ranges, stable `NodeId` values,
  diagnostics, heading/footnote indexes, and typed resource decisions. Agent and
  file-preview product paths render it with `MarkdownView`; direct
  `TextView::markdown` product calls and `project_markdown_for_host` projections are
  forbidden.
- Parsing and artifact completion are revision-fenced. Streaming may keep the last
  valid document visible, but a completion applies only when view id, revision,
  node id, and artifact key still match live state. Generated math/diagram nodes
  contribute their original source to document-order copy output.
- Every Markdown/HTML link or image crosses the same `ResourcePolicy` and becomes
  `Fragment`, `Workspace`, `DataImage`, `Http`, or `Blocked`. Raw HTML is inert and
  per-tag/per-attribute allowlisted; event attributes, script/style/forms, unsafe
  schemes, active embeds, and workspace escapes never reach GPUI.
- A workspace file link may carry an editor location suffix (`:line` or
  `:line:column`). `ResourcePolicy` removes that suffix from `resolved` while
  preserving the original `source`. When an Agent link uses the current session's
  absolute workspace prefix, the click boundary removes that prefix before calling
  the workspace-scoped preview backend; a leading-slash workspace-root link such as
  `/README.md` keeps its existing root-relative meaning. Do not pre-rewrite the
  Markdown source for one product surface.
- Artifact admission is bounded by source bytes, active slots, queue length, cache
  entries/bytes, timeout, circuit breaker, and stale fencing. Timeout handling uses
  one process-lifetime `OnceLock` worker per engine family with a bounded sync
  channel; it must never spawn and abandon one thread per request after timeout.
- Locally generated SVG is untrusted. `SvgPolicy` rejects DTD/entities, active or
  external content, invalid references, oversized structure/text/path data, and
  dimensions outside policy, then prefixes every allowed fragment id/reference.
  Intrinsic `width` and `height` determine raster pixel budget when present;
  `viewBox` coordinates remain vector-space bounds and are not multiplied as raster
  pixels. A viewBox-only SVG uses its viewBox dimensions as intrinsic dimensions.
- Syntax highlighting is bounded and cached by node/theme through the selected
  `gpui-component` Tree-sitter registry. Unknown languages stay readable plain text;
  diff rows retain prefix/status cues in addition to theme-aware color.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Source exceeds bytes/nodes/depth or parsing is incomplete/malformed | Return a bounded diagnostic and readable literal/last-valid content; never blank or panic. |
| Resource has unsafe scheme, invalid data image, active HTML attribute/tag, or workspace escape | Return `Blocked` plus a stable diagnostic/disabled affordance; preserve readable label/source. |
| Workspace file link carries `:line[:column]` or the current session's absolute workspace prefix | Resolve it as `Workspace`, remove only the editor location, and open the workspace-relative preview target. |
| Artifact source/queue/cache/SVG limit is exceeded | Return a typed local error and source fallback; perform no network request. |
| Artifact times out or completes for a stale revision/node | Ignore the late result, advance bounded queue state, and never create a residual per-request worker thread. |
| SVG has DTD/entity, `script`, `foreignObject`, external URL/reference, unsafe CSS, duplicate root, or invalid dimensions | Reject before GPUI rasterization. |
| MathJax has a large coordinate-space viewBox but bounded `ex`/`em` intrinsic dimensions | Validate viewBox bounds separately and budget pixels from intrinsic dimensions. |
| Product source contains `TextView::markdown` or `project_markdown_for_host` | Fail the source audit; migrate the caller to the canonical document/view. |

### 5. Good / Base / Bad Cases

- Good: one fixture renders GFM, highlighted code/diff, math, Mermaid, the bounded
  local PlantUML subset, footnotes/ToC, callouts, definitions, tasks/progress,
  details, and safe HTML through the same document and resource policy in both
  product surfaces.
- Good: `[app.rs](/work/vibex/src/app.rs:42)` in an Agent session rooted at
  `/work/vibex` opens `src/app.rs` in the existing preview surface.
- Good: a timed-out math job remains isolated on the fixed math worker; later work
  is queue-bounded/circuit-broken and stale output cannot replace a newer revision.
- Base: unknown code language or unsupported diagram syntax renders readable source
  with a bounded diagnostic and working copy action.
- Base: `[README](/README.md)` remains a workspace-root-relative link rather than
  being mistaken for an external filesystem read.
- Bad: pre-rewrite Markdown links for one surface, trust engine SVG because it was
  generated locally, interpret a `viewBox="0 -1500 6000 2000"` as 12 million raster
  pixels despite bounded intrinsic dimensions, or spawn a detached timeout thread
  per artifact.

### 6. Tests Required

- `cargo test -p vibex-markdown --locked` covers canonical parsing/ranges/ids,
  malformed and bounded fallback, HTML/resource attacks, SVG sanitization, local
  engines, artifact queue/cache/circuit/stale behavior, native GPUI rendering,
  mouse selection, clipboard copy, anchors, details state, theme, and narrow layout.
- Desktop regression tests must parse real Agent Markdown and assert that relative
  `:line[:column]`, absolute current-workspace, and leading-slash workspace-root
  links all produce the exact workspace-relative target passed to Preview.
- Run `cargo clippy -p vibex-markdown --all-targets -- -D warnings`, the affected
  desktop model/GPUI tests, a locked no-default-feature check, and
  `rg -n 'TextView::markdown|project_markdown_for_host' apps/desktop crates/desktop-model`.
- Run `pnpm check:licenses`; regenerated notices/SBOM must bind the selected local
  engines and contain no hidden browser, Node, JVM, remote-renderer, or separately
  downloaded Graphviz runtime.

### 7. Wrong vs Correct

#### Wrong

```rust
std::thread::spawn(move || render_local_artifact(&request, policy));
// recv_timeout returns, but every timed-out request can leave another worker alive.

let raster_pixels = view_box.width * view_box.height;
TextView::markdown(project_markdown_for_host(source, path).rendered_source)
let desktop_source = source.replace(workspace_root, "");
// Surface-specific source rewriting bypasses the canonical resource decision.
```

#### Correct

```rust
static MATH: OnceLock<Result<ArtifactWorker, String>> = OnceLock::new();
let worker = MATH.get_or_init(|| ArtifactWorker::start("math"));
worker.sender.try_send((request, policy, completion))?;

validate_view_box(view_box, limits.max_svg_dimension)?;
validate_pixel_area(intrinsic_width, intrinsic_height, limits.max_svg_pixels)?;
MarkdownView::from_document(id, Arc::new(parse_markdown(input)))
let resource = ResourcePolicy::new("").resolve(ResourceRole::Link, target, label);
let preview_path = agent_markdown_preview_path(&resource, Some(workspace_root));
```

## Scenario: GPUI Bounded Office Preview Surface

### 1. Scope / Trigger

- Trigger: GPUI displays an Office file through the bounded read-only models owned by
  `vibex-content`.
- This surface preserves DOCX text/basic structure, XLSX/ODS first-sheet inspection,
  PPTX text extraction, and explicit legacy-format unsupported/system-open behavior.

### 2. Signatures

```text
OfficeSurface::new(document_path: Option<PathBuf>, cx) -> OfficeSurface
OfficeDocumentController::activate(generation) -> GenerationDisposition
OfficeDocumentController::open(path, bytes, generation) -> OfficeDocumentModel
```

### 3. Contracts

- The GPUI layer renders `OfficeDocumentModel`; it must not duplicate ZIP/XML parsing.
- Supported files are read once and passed to the controller, whose 32 MiB decoded,
  512-entry, 80-row × 20-column, 200-slide, XML-depth, cancellation, and timeout limits
  remain authoritative.
- The surface is read-only. It may retry, close, or explicitly request system open; it
  must not execute macros, formulas, embedded objects, or automatic external fallback.
- Closing drops the rendered model and clears the controller's parsed model.
- Diagnostics may store kind, counts, bounds, and action results, but never document
  paths or extracted Office content.

### 4. Validation & Error Matrix

- Source read failure -> `office_source_read_failed` UI error with retry when a path remains.
- Controller archive/XML/size/timeout/cancellation failure -> preserve the typed controller code.
- DOC/XLS/PPT -> ready unsupported model with an explicit system-open action.
- Unknown extension -> ready unsupported model; do not attempt archive parsing.
- Close -> closed UI state and zero retained parsed model.

### 5. Good/Base/Bad Cases

- Good: DOCX paragraphs, bounded first-sheet cells, and ordered slide text render from
  controller models inside the native workbench.
- Base: a legacy `.doc` shows the typed unsupported reason and an explicit system-open button.
- Bad: the GPUI surface opens ZIP parts directly, expands the 80 × 20 table limit, logs
  extracted text, or automatically launches another application after parser failure.

### 6. Tests Required

- `cargo test -p vibex-content --locked` for supported, legacy, malformed, oversized,
  cancellation, timeout, traversal, encoding, and zip-bomb behavior.
- `cargo test -p vibex-desktop --locked` plus GPUI compile/Clippy coverage.

### 7. Wrong vs Correct

#### Wrong

```rust
let archive = zip::ZipArchive::new(file)?;
render(parse_xml_without_shared_limits(archive));
```

#### Correct

```rust
controller.activate(generation)?;
let model = controller.open(path, bounded_source_bytes, generation)?;
render_office_model(model);
```

The content controller owns untrusted-document validation; GPUI owns presentation and
explicit user actions only.

## Anti-Patterns

- Do not build provider-specific chat UIs.
- Do not hide failed reconnect or stale timeline state from the user.
- Do not make dark mode depend on one global inversion hack.
- Do not expose destructive actions as swipe-only or hover-only interactions.

## Native Mobile Device Validation

Device qualification is tied to the exact source, Cargo lock, and produced
application artifact. A host-side check proves source and type contracts only.

Required device scenarios are:

- first frame and safe-area layout;
- touch scrolling plus full-page Sessions/workspace-tools gestures from the
  center session page and inverse gestures from both open pages;
- IME commit, selection, paste, keyboard resize, and focus recovery;
- GUI timeline streaming, Markdown, process expansion, and approval actions;
- send, stop, continue, disconnect, reconnect, and authoritative catch-up;
- Direct/Tailnet/Relay route selection and credential persistence/redaction;
- foreground/background lifecycle and network transition.

Missing device scenarios remain untested and are never inferred from a successful
Rust or Gradle/Xcode compile. Qualification notes store bounded platform labels and
status only, never pairing links, tokens, device serials, prompts, file contents, or
terminal bytes.

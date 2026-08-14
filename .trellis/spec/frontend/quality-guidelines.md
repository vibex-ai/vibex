# Frontend Quality Guidelines

Frontend quality means native desktop and the installed mobile runtime operate
against the same Vibex domain contract while presenting the right amount of
control for each form factor.

Current evidence: [Architecture Baseline](../guides/architecture-baseline.md), GPUI Desktop source-bound
evidence, and current runtime/protocol tests.

The long React/Tauri scenarios retained below are historical validation evidence
for deleted migration surfaces. Their paths, commands, and writers are not
current gates and must not be restored. New UI quality evidence must cover
shared GPUI components, platform dependency isolation, Wide on native desktop,
the compact native mobile client, native input, and the
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
- Zedra alignment is visual and ergonomic: restrained dark surfaces, compact
  spacing, small radii, clear secondary text, edge drawer navigation, and
  thumb-safe explicit actions. Vibex labels, state semantics, and approval
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
- The session drawer follows the pointer continuously after a horizontal
  threshold, cancels clearly vertical pans, then snaps by final direction or
  half-width with a faster close animation. A tap remains a tap and must not
  steal vertical timeline scrolling.
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
| Approval or composer action is unavailable while the server is busy | The control is disabled with an explicit busy state; no duplicate mutation is sent. |
| Timeline generation/sequence is stale | Ignore the result and request authoritative refresh. |

### 5. Tests Required

- `cargo test -p vibex-mobile --locked` covers storage permissions/atomicity and
  malformed-file removal, secret-redacted `Debug`, UTF-8/UTF-16 IME editing,
  drawer snap decisions, Markdown block projection, and route bundle validation.
- `cargo test -p vibex-ui --locked` covers the shared controller, timeline
  projection, approval surfaces, and compact shell semantics.
- `node scripts/check-mobile-native.mjs --self-test` covers the negative source
  contract, native entry points, vendored platform dependencies, GUI session
  markers, and forbidden legacy paths.
- Android and iOS device validation must separately exercise touch/keyboard,
  drawer navigation, safe-area/IME changes, timeline streaming, approval and
  elicitation resolution, new-session creation, send/stop/continue, reconnect,
  bundled Latin/CJK text, and credential redaction before a release claim.

### 6. Wrong vs Correct

```text
Wrong: copy a terminal screen into mobile, keep a second session reducer, or
       load a remote page at runtime.
Correct: native GPUI -> shared AgentWorkflowController -> authoritative desktop
         timeline -> compact GUI cards and composer.
```


## Scenario: Deterministic Desktop Browser Fixtures

### 1. Scope / Trigger

- Trigger: desktop workflow screenshots or scripted UI actions must run without
  Tauri, a Provider process, real credentials, or developer browser persistence.
- The fixture layer is browser evidence only. Native window, IME, WebView,
  Terminal, PDF, DPI, and package behavior still require native evidence.

### 2. Signatures

```text
apps/desktop/src/fixtures/desktop-behavioral-v1.json
  fixtures[workflowId] -> {
    states, initialView, actions, expectedCalls, expectedEvents, expectedState
  }

?fixture=<workflow-id>&state=<state>&theme=<light|dark>&locale=<en|zh-CN>

window.__VIBEX_DESKTOP_FIXTURE__.getSnapshot() -> {
  fixtureId, state, theme, locale, deterministic, providerFree, ready, calls[]
}

desktop-react-fixture-evidence.v2 -> {
  scope: { coreMatrix, browserInteractions, notClaimed },
  captures[]: {
    group, catalogActionId, interaction, browserState,
    actions, assertions, interactionCallContracts, screenshot
  }
}
```

### 3. Contracts

- The catalog is the single workflow contract. Manifest generation, browser
  runtime selection, actions, screenshots, and evidence checks reference the
  same stable workflow id.
- Fixture activation is explicit through `fixture`. A missing selector keeps
  ordinary browser mock behavior, and Tauri always invokes the native command.
- Initialize the fixture before dynamically importing `App`. Zustand persist
  hydrates during module evaluation, so clearing or seeding storage after a
  static `App` import is too late and produces machine-dependent screenshots.
- Clear only Vibex-owned local/session storage keys. Seed theme, locale, and
  lightweight workbench navigation; authoritative workflow data still enters
  through typed `api` mock responses and TanStack Query.
- Use a fixed monotonic fixture clock and sanitize the public call trace.
  Secret/token/password fields and real home paths must not enter evidence.
- Project Agent states with generated `AgentSession`, `TimelinePage`,
  `PermissionRequest`, and runtime DTOs. UI code must not read fixture-specific
  provider payloads or branch on fixture ids.
- Distinguish transport errors from authoritative timeline errors. An invoke
  failure rejects the Promise; a persisted `TimelinePayload.type === "error"`
  resolves normally and renders recovery from authoritative data.
- Screenshot evidence records input hashes, browser/environment identity,
  viewport, actions/assertions, call outcomes, PNG size, and PNG SHA-256. A
  representative matrix must explicitly say it is not full Cartesian coverage.
- A complete browser matrix must name the stable fixture/state and exact Cartesian
  dimensions it covers. Do not shorten "one fixture x theme x locale x viewport" to
  an unqualified "full matrix" or imply every workflow/state combination ran.
- Interaction captures reference an action id declared by the same workflow fixture.
  Store exact sanitized command projections for behavior under test, but explicitly
  omit nondeterministic fields such as generated idempotency keys instead of dropping
  the entire request contract.
- Controlled Radix Dialogs whose keyboard trigger is outside the `Dialog` root must
  capture `document.activeElement` in `onOpenAutoFocus`. In `onCloseAutoFocus`, prevent
  the default and focus that element only while it is still connected. This preserves
  Escape/Cancel return without focusing a trigger that a successful action removed.

### 4. Validation & Error Matrix

- Unknown fixture id or undeclared state -> fail before rendering.
- Missing workflow, command, event, action, or expected-state record -> fail
  the baseline generator.
- `loading` -> the fixture entry command remains pending and evidence asserts
  the pending outcome.
- Transport `error` -> the configured command rejects and the error surface is
  visible.
- Timeline `error` -> `agent_fetch_timeline` resolves with a typed error item;
  evidence asserts the error copy and recovery action.
- `empty` -> list/page DTOs retain their protocol shape with empty collections;
  do not return an arbitrary `null`.
- Changed runtime/catalog/app/lockfile input -> offline evidence check fails and
  requires explicit recapture.
- Missing theme x locale x required-viewport core cell, unknown catalog action id,
  changed scripted assertion, or changed deterministic command projection -> offline
  evidence check fails.
- Dialog Escape/Cancel does not return to its still-mounted trigger, or forward/reverse
  Tab leaves an open modal -> browser interaction capture fails.
- Browser evidence claims native CJK/IME or platform-window behavior -> evidence scope
  is invalid even if synthetic DOM composition or Chromium input passed.
- Browser console/page error or blank/incomplete DOM -> capture fails.

### 5. Good/Base/Bad Cases

- Good: a permission fixture renders the normal permission card from a typed
  `TimelinePage`, and its call trace contains only sanitized local mock calls.
- Base: a representative pass covers each state/theme/locale/viewport value once and
  labels itself representative rather than implying every combination ran.
- Good: a complete browser pass records all 24 cells for one stable fixture across two
  themes, two locales, and six required viewports, while listing native CJK/IME and
  platform evidence under `notClaimed`.
- Bad: override component props or query cache directly to manufacture a
  screenshot that bypasses the command and protocol boundaries.
- Bad: reject `agent_fetch_timeline` to represent a persisted recoverable error;
  this tests transport failure and never exercises the error timeline renderer.
- Bad: import `App` statically, then clear localStorage in `main.tsx`; Zustand may
  already have hydrated stale navigation state.

### 6. Tests Required

- `pnpm check:desktop-baseline` asserts catalog/manifest/source consistency.
- `pnpm capture:desktop:fixtures` is the explicit Playwright recapture command.
- `pnpm check:desktop-fixtures` validates committed inputs, assertions, call
  outcomes, PNG presence/size, and hashes without launching a browser.
- Keyboard/focus interaction captures assert both forward and reverse focus trapping,
  Escape/Cancel trigger return, Enter versus Shift+Enter, and Composer focus after
  send. Drag/drop asserts the exact typed `file_rename` path/newPath request.
- `pnpm check:frontend` and the desktop Vite build cover fixture DTO imports,
  dynamic bootstrap order, and production bundle separation.
- Visually inspect every changed screenshot, including the absolute 360 x 620
  minimum; record baseline defects instead of silently accepting them as parity.

### 7. Wrong vs Correct

#### Wrong

```tsx
import { App } from "./App";

localStorage.clear();
render(<App fixtureTimeline={rawProviderEvent as TimelinePage} />);
```

#### Correct

```tsx
const fixture = initializeDesktopFixtureRuntime();
const { App } = await import("./App");

render(<App />); // typed api mocks remain the data boundary
```

#### Wrong: controlled Dialog without a trigger

```tsx
<Dialog open={open} onOpenChange={setOpen}>
  <DialogContent />
</Dialog>
```

When the actual trigger lives elsewhere in the workbench, Radix has no
`Dialog.Trigger` to receive focus after Escape.

#### Correct: connected opening-element return

```tsx
const returnFocusRef = useRef<HTMLElement | null>(null);

<DialogContent
  onOpenAutoFocus={() => {
    returnFocusRef.current = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
  }}
  onCloseAutoFocus={(event) => {
    if (returnFocusRef.current?.isConnected) {
      event.preventDefault();
      returnFocusRef.current.focus({ preventScroll: true });
    }
  }}
/>
```

## Scenario: Native File Dialog Backend Isolation

### 1. Scope / Trigger

- Trigger: GPUI adds or changes a native file/directory chooser while the Tauri
  workspace still uses `tauri-plugin-dialog`.

### 2. Signatures

```toml
# GPUI uses a distinct rfd release so Cargo cannot merge its Portal features
# with Tauri's GTK3 features.
rfd = { version = "0.17.2", default-features = false,
        features = ["wayland", "xdg-portal"] }
```

### 3. Contracts

- The default GPUI Linux binary uses XDG Desktop Portal and must not link GTK or
  WebKit solely for file dialogs. GTK is permitted only for the Linux AppIndicator
  system-tray integration; WebKit remains excluded from the default build.
- Tauri may retain its independently versioned GTK3 dialog backend.
- The two surfaces must not resolve to the same `rfd` package version when they
  require mutually exclusive backend features.
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

- Good: GPUI uses `rfd 0.17.x` Portal while Tauri uses `rfd 0.16.x` GTK3.
- Base: each app builds independently and `cargo check --workspace --all-targets
  --locked` also succeeds.
- Bad: both apps use `rfd 0.16` with conflicting features and only package-local
  checks are run.

### 6. Tests Required

- `cargo check --workspace --all-targets --locked`.
- `node scripts/capture-linux-package.mjs --write`, then its read-only check.
- Verify the release `NEEDED` set and license/SBOM graph after dependency changes.

### 7. Wrong vs Correct

#### Wrong

```toml
rfd = { version = "0.16", features = ["xdg-portal"] }
# Tauri transitively enables rfd 0.16/gtk3 in the same workspace graph.
```

#### Correct

```toml
rfd = { version = "0.17.2", default-features = false,
        features = ["wayland", "xdg-portal"] }
```

## Scenario: Native Tauri Evidence Claims

### 1. Scope / Trigger

- Trigger: desktop behavior depends on a native window, display scale, input method,
  system dialog, Terminal, embedded WebView, platform lifecycle, or package rather
  than browser DOM behavior.
- Browser fixtures and synthetic displays may contribute evidence, but they cannot
  satisfy a claim that remains assigned to a physical native protocol. An approved
  hosted policy may exclude named macOS/Windows GUI claims from the decision
  denominator; exclusion still does not satisfy those claims.

### 2. Signatures

```text
pnpm capture:tauri:native-baseline -> explicit local evidence writer
pnpm check:tauri-native-baseline   -> offline identity/schema/artifact check

tauri-native-baseline-evidence.v1 -> {
  protocol, source, requiredNativeScenarios, requiredTargets,
  nativeGateSatisfied, targets[]
}

target -> {
  id: linux_x11 | linux_wayland | macos | windows,
  status: captured_synthetic | captured_native |
          blocked_runner_unavailable | blocked_backend_unavailable,
  requiredNativeGateSatisfied, ui, runner, window, capture,
  scenarios[], blockers[], limitations[]
}
```

### 3. Contracts

- Every target and required scenario in the historical capture artifact has a stable
  id, status, owner, and either an observation or an actionable capture blocker. A
  current decision consumer must overlay the reviewed hosted-runner policy: failed
  hosted build/package checks remain blockers, while its exact five GUI exclusions
  are non-decision skips rather than passes or inferred deviations.
- `requiredNativeGateSatisfied` is true only when the target and every required
  scenario in this physical protocol have `captured_native` evidence.
  `captured_synthetic` always leaves that physical gate false. This legacy aggregate
  is not the denominator for policy-approved hosted exclusions.
- Runner identity must match the target: Linux X11/Wayland cannot satisfy macOS or
  Windows, and XWayland cannot satisfy native Wayland surface identity.
- Capture with disposable `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, and
  `XDG_CACHE_HOME`. Fix and record theme/locale. Never load user databases, browser
  profiles, prompts, terminal history, credentials, clipboard content, or user files.
- Build the Tauri evidence binary with embedded frontend assets and
  `tauri/custom-protocol`. A release-mode binary built without that feature may load
  the development URL and produce a connection-refused or blank client; that is a
  failed capture input, not native rendering evidence.
- Bind evidence to lockfiles, Tauri config, desktop/shared source, capture script, and
  protocol hashes. Normal checks never regenerate evidence.
- For physical Linux capture, screenshot only the application window/client or a
  dedicated isolated synthetic output. For nested Wayland, create a dedicated
  headless output, disable the nested visible output before app launch, assert native
  `xdg_toplevel`, and crop/poll the discovered application client. Hosted macOS/
  Windows jobs do not capture screenshots.
- Pixel readiness must be measured on the application client. A non-uniform
  compositor wallpaper or notification around a uniform white/gray client is not
  rendered-application evidence.
- A first non-uniform client frame can still be a loading state. Require a bounded
  settling interval and repeated stable pixel signatures before committing the frame.

### 4. Validation & Error Matrix

- Input/protocol/lock/config hash changed -> offline check fails; run the explicit
  writer and review all generated artifacts.
- Target OS or display backend mismatches the row -> fail; cross-platform evidence is
  not substitutable.
- Screenshot missing, hash/byte/dimension mismatch, or client remains uniform -> fail
  capture.
- Full synthetic output is non-uniform but the cropped client is uniform -> keep
  polling or fail; never accept background pixels.
- Synthetic target or scenario claims a completed native gate -> fail schema check.
- Incomplete target lacks an owner-assigned blocker -> fail schema check.
- Native target still has a required scenario blocked -> keep its gate false.
- macOS/Windows hosted runner unavailable for a decision-bearing build/package check
  -> block that check; do not use a Linux compile or cross-build as evidence.
- macOS/Windows physical GUI claim is one of the five approved exclusions -> retain
  `skipped_by_product_decision` in the hosted policy layer with
  `decisionImpact: false`; do not promote the legacy capture row to a pass.

### 5. Good/Base/Bad Cases

- Good: isolated Xvfb captures a non-uniform Tauri client, records window identity and
  minimum hints, and explicitly blocks physical DPI/IME/dialog/package claims.
- Good: nested Wayland disables every visible nested output, verifies
  `xwayland: false`, crops the Tauri client, and records an undersized request plus
  observed minimum enforcement.
- Base: a hosted build/package runner is absent and its decision-bearing checks remain
  blocked with exact replay steps, while approved GUI exclusions remain non-decision
  skips.
- Bad: count a successful Tauri compile, window handle, compositor wallpaper, or
  browser composition event as native pixels/IME/platform parity.
- Bad: mark macOS or Windows passed from Linux cross-compilation or screenshots.

### 6. Tests Required

- Run `pnpm capture:tauri:native-baseline` only as an explicit reviewed write.
- Visually inspect each changed PNG at original resolution for application pixels,
  overlap, host-desktop leakage, private data, and the recorded theme/locale.
- Run `pnpm check:tauri-native-baseline`; assert target/scenario completeness, source
  identities, status/gate consistency, platform/backend identity, and PNG hashes.
- Keep `pnpm check:tauri-native-baseline` in root `pnpm check` so source drift fails
  offline.
- Run the complete physical protocol on Linux X11 and Linux Wayland, plus the reviewed
  hosted build/test/package/lifecycle protocol on macOS and Windows. Do not require
  the five hosted GUI exclusions to complete the decision-bearing checklist.

### 7. Wrong vs Correct

#### Wrong

```json
{
  "id": "windows",
  "status": "captured_native",
  "runner": { "os": "linux", "displayBackend": "x11" },
  "requiredNativeGateSatisfied": true
}
```

#### Correct

```json
{
  "id": "windows",
  "claim": "ime_composition",
  "status": "skipped_by_product_decision",
  "decisionImpact": false,
  "notEvidenceOfParity": true
}
```

## Scenario: Tauri Process-Tree Performance Evidence

### 1. Scope / Trigger

- Trigger: a desktop migration baseline or performance claim covers Tauri startup,
  memory, CPU, large workbench data, Terminal, Web content, resize, or idle behavior.
- Service-only scale smoke and browser fixture timing are different evidence layers;
  neither may be renamed as a desktop process-tree baseline.

### 2. Signatures

```text
pnpm capture:tauri:performance-baseline -> explicit release evidence writer
pnpm check:tauri-performance-baseline   -> offline source/raw/summary checker

desktop-tauri-process-tree-baseline.v1 -> {
  protocol, source, policy, requiredTargets, requiredScenarios,
  measurementGateSatisfied, targets[]
}

target -> {
  id, status, runner, runCount, summaries, runs[], scenarios[],
  requiredMeasurementGateSatisfied, blockers[], limitations[]
}
```

### 3. Contracts

- Build the release application with embedded frontend assets and
  `tauri/custom-protocol`. A release binary loading the development URL produces a
  blank client and is a failed build, not a startup measurement.
- Use at least five fresh process/profile runs per measured scenario. Record raw run
  values plus median, nearest-rank p95, and min/max range; summaries must be derived
  and offline-recomputed rather than hand-maintained.
- Measure from the `vibex-desktop` root through every descendant. Include WebKit Web,
  Network, GPU, and sandbox helpers; exclude the display server/compositor, launch
  wrapper, and private D-Bus daemon.
- Retain aggregate RSS and PSS. Summed RSS intentionally counts each process mapping;
  PSS remains available when shared-page attribution matters.
- CPU is one-core-normalized process-tree CPU and may exceed 100 percent. Record the
  kernel clock-tick rate and sampling interval that define the conversion.
- Give every run disposable HOME/XDG config/data/cache/temp state and remove common
  credential/Provider/Agent environment variables. Do not retain full paths, process
  command lines, environments, prompts, terminal output, or user workspace data.
- A native surface plus stable non-uniform pixels proves a stable rendered frame, not
  time to interactive. Keep TTI null until a sanitized native input/readiness
  round-trip completes.
- Start/end pixel equality does not prove that hidden repaint cadence is zero. Keep
  repaint status partial until frame/damage instrumentation exists.
- Every required scaled workflow retains a row. Missing native timeline/file/diff/
  Terminal/Web/resize drivers are owner-assigned blockers, never zero-valued results.
- Synthetic Linux rows cannot satisfy physical Linux gates or any hosted macOS/
  Windows decision-bearing check. Approved hosted GUI exclusions are outside the
  denominator rather than gates satisfied by Linux data.
- Keep observed Tauri values separate from GPUI planning targets and frozen budgets.
  Budget disposition is a separately reviewed, versioned artifact.

### 4. Validation & Error Matrix

- Fewer than five runs, shortened idle series, or non-monotonic samples -> reject the
  measured target.
- Ready/sample tree lacks the Tauri root or WebKit Web child -> reject the full-tree
  claim.
- Aggregate RSS/CPU disagrees with raw process rows -> reject the artifact.
- Interaction probe is absent but TTI is non-null -> reject fabricated readiness.
- Repaint instrumentation is absent but repaint gate is complete -> reject the claim.
- Blocked scenario carries measured values -> reject the scenario.
- Source/protocol/lock/capture input changed -> fail offline and require explicit
  reviewed recapture.
- Missing physical Linux measurement -> retain blocked rows; do not copy synthetic or
  hosted values.
- Missing hosted macOS/Windows GUI performance measurement -> retain the approved
  `skipped_by_product_decision` disposition with `decisionImpact: false`; do not copy
  Linux values or invent an absolute budget.

### 5. Good/Base/Bad Cases

- Good: five X11 release runs include the Rust parent and WebKit descendants, retain
  25 samples over two-minute idle, and label stable pixels separately from TTI.
- Good: a 100,000-entry native file-tree route is absent, so its row names the fixture
  owner and required action with no fabricated memory value.
- Base: synthetic X11/Wayland startup and idle values inform later budgets while the
  overall measurement gate stays false.
- Bad: report `vibex-desktop` parent RSS as total desktop RSS.
- Bad: reuse `pnpm baseline:performance` and label its service fixture a Tauri
  timeline/file/diff measurement.
- Bad: treat a browser mock or a blank app as a measured native scenario.

### 6. Tests Required

- Run `pnpm capture:tauri:performance-baseline` only for explicit reviewed recapture.
- Run `pnpm check:tauri-performance-baseline` offline and keep it in root `pnpm check`.
- Inspect target summaries against raw runs and confirm no capture processes remain.
- Search committed evidence for home paths, credentials, command lines, environment
  values, prompts, terminal output, and real workspace identifiers.
- Repeat every decision-bearing scenario on physical Linux X11 and Wayland before
  completing the performance checklist or freezing final budgets. Hosted macOS/
  Windows GUI performance exclusions remain explicit and do not enter that gate.

### 7. Wrong vs Correct

#### Wrong

```json
{
  "scenario": "cold_start_to_interactive",
  "timeToInteractiveMs": 15000,
  "measurement": "stable screenshot"
}
```

#### Correct

```json
{
  "scenario": "cold_start_to_interactive",
  "stableRenderedFrameMs": 15000,
  "timeToInteractiveMs": null,
  "blocker": "native input round-trip required"
}
```

## Scenario: GPUI Foundation Lifecycle And Linux Evidence

### 1. Scope / Trigger

- Trigger: the production GPUI workbench starts the shared desktop runtime, persists
  desktop UI state, renders component overlays, or claims native Foundation behavior.
- Linux is the physical execution target for the current release. macOS and Windows
  remain deferred without source, build, runtime, pixel, input, DPI, or package claims
  until a future task runs their native checks.

### 2. Signatures

```text
DesktopRuntime::start(DesktopRuntimeConfig::preview_default())
App::on_app_quit -> flush DesktopUiStateV1 -> await DesktopRuntime::shutdown

pnpm check:foundation:linux
node scripts/capture-foundation-linux.mjs --write

foundation-linux.v1 -> {
  runner, requiredViewports, scenarios[], lifecycle: {
    processExitCode, uiStateExitFlush, runtimeShutdownAwaited,
    homeLockedWhileRunning, homeLockReleasedAfterExit
  }
}
```

Lifecycle markers are bounded diagnostics only:

```text
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
- GPUI Foundation uses the isolated preview app id/home; it must not acquire the stable
  Tauri home or read a user's ordinary desktop state during evidence capture.
- Foundation capture is provider-free: it sets
  `VIBEX_FOUNDATION_SKIP_ADAPTER_INSTALL=1` so managed ACP adapter installation
  cannot block the lifecycle/window evidence. Real adapter installation remains
  covered by the separate ACP bridge and daily-driver smoke gates.
- Physical Linux evidence requires a native Wayland `xdg_toplevel` (`xwayland=false`),
  a scale-1 monitor, stable non-uniform client pixels, the six required workbench
  viewports, and a separate dark Traditional Chinese Settings Sheet capture. The
  current matrix therefore contains seven client captures.
- The offline verifier binds `Cargo.lock`, the Foundation source tree, every PNG hash,
  viewport/window identity, lifecycle fields, and Settings Sheet observation. Normal
  `pnpm check` verifies the artifact and never recaptures it.
- Tauri native and process-tree reference writers build with embedded frontend assets
  and `tauri/custom-protocol`; GPUI evidence and Tauri evidence remain separate
  protocols and cannot satisfy one another.

### 4. Validation & Error Matrix

- Window closes without both lifecycle markers -> fail the graceful-close claim.
- A Linux window/title-bar close callback calls `cx.quit()` -> reject the lifecycle
  implementation even if Wayland closes successfully; the X11 path can panic on re-entry.
- UI-state bytes do not change or an injected non-schema sentinel survives -> fail
  final persistence flush.
- A second process acquires the home while GPUI runs -> fail runtime exclusivity.
- The lock remains unavailable after exit -> fail shutdown ownership/release.
- Process exits non-zero, before runtime-ready, or before the Settings Sheet marker ->
  fail the affected scenario.
- XWayland, synthetic display, wrong scale/viewport, uniform pixels, missing PNG, or
  source/hash drift -> reject the Linux evidence.
- macOS/Windows source checks run from Linux -> retain deferred native status; do not
  promote them to platform smoke passes.

### 5. Good/Base/Bad Cases

- Good: a real Wayland close dispatch exits zero, rewrites UI state, awaits runtime
  shutdown, and changes the lock probe from `locked by another process` to acquirable.
- Base: Windows and macOS remain deferred and unclaimed; Linux evidence names the
  accepted deviation without inferring build or native parity.
- Bad: kill the process after taking screenshots, infer cleanup from `Drop`, or mark a
  Tauri/Web fixture as GPUI lifecycle evidence.

### 6. Tests Required

- `cargo test -p vibex-desktop-runtime --locked` asserts same-home contention,
  separate preview homes, process-level contention, and crash/drop release.
- `cargo test -p vibex-desktop --locked` covers the shell contract, responsive
  viewports, settings, primitives, fonts, locales, and source-compatible platform
  branches.
- Run `capture-foundation-linux.mjs --write` only on the reviewed physical Linux
  Wayland runner; visually inspect all seven PNGs at original resolution.
- Run `pnpm check:foundation:linux`, `pnpm check:tauri-native-baseline`, and
  `pnpm check:tauri-performance-baseline` before committing changed evidence.

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

## Scenario: GPUI Native Content And Physical Linux Capture

### 1. Scope / Trigger

- Trigger: GPUI adds or changes Terminal, PDF, Office, or the
  Native Content evidence workbench.
- Linux is the first physical target. A source check, headless compositor, or old
  screenshot cannot replace a current physical input/window run.

### 2. Signatures

```text
vibex-desktop --native-content-contract <output.json>
vibex-desktop --native-content-workbench [output.json]
pnpm capture:native-content:linux
pnpm check:native-content
pnpm check:native-content:linux

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
- The physical capture runner requires an active native Wayland monitor and a
  non-XWayland window. Monitor `-1`, an empty monitor list, or a created headless
  output is a blocker, not a physical pass.
- The run report stores booleans and bounded counts only. It must not contain the PTY
  command/output marker, URL, PDF text, Office text, private paths, clipboard data, or
  user content.
- A screenshot proves only the rendered Native Content slice it shows. X11,
  terminal stress/soak, PDF page interaction, and Office rendering retain explicit
  blocked rows until their own protocols run.
- Aggregate evidence imports source-bound Terminal stress and X11 evidence by exact
  path, SHA-256, and status. It must rerun both owning validators before promoting a
  claim; editing only the aggregate JSON cannot turn a blocked row into a pass.
- Aggregate Native Content evidence must bind every embedded implementation source, not
  only the workbench shell. If `NativeContentWorkbench` embeds `PdfSurface`, its source
  input tree includes `pdf_surface.rs`; otherwise PDF behavior could drift while the
  aggregate evidence remains falsely current.
- Shared `ContentSurfaceLifecycle` owns focus state in addition to visibility: opening an
  overlay clears focus and records `focusReturnPending`; only a current-generation
  `focus_entered` clears that pending state. Close, crash, failure, deactivation, and a
  newer activation clear focus. Same-generation callbacks after `Closed` are ignored.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| No active physical monitor / active window | Capture fails before input injection. |
| Window is XWayland or screenshot pixels are uniform | Reject physical Wayland evidence. |
| PTY marker is not observed through raw snapshots | Run report is not written as passed. |
| Web surface/profile/cache/network allocation is non-zero | Contract and physical checks fail. |
| Report contains marker text, URL, or content fields | Redaction gate fails. |
| Window close leaves the process alive or reports a panic | Clean-close claim fails. |
| Overlay closes without focus returning to the current surface | Focus integration gate fails; `focusReturnPending` must remain observable until a current-generation focus event. |
| Same-generation callback arrives after close | Ignore it and keep the surface `Closed`; it must not restore visibility or focus. |
| Source/lock/screenshot hash drifts | Read-only evidence check fails; explicit recapture required. |
| Embedded surface source is absent from the evidence roots | Evidence schema/source validation fails. |

### 5. Good/Base/Bad Cases

- Good: a physical Hyprland window receives one sanitized command through the
  IME-capable input, raw PTY snapshots observe its marker, the screenshot is
  non-uniform, close exits zero, and the report retains no output text.
- Base: code, contract, package, and headless PDF checks pass while the current
  session has no active monitor; the physical row stays blocked and the capture
  runner remains ready for the next active-output session.
- Bad: create a headless Hyprland output or reuse historical Terminal/PDF evidence and
  label it a current physical Native Content pass.
- Bad: bind `native_content.rs` but omit the embedded `pdf_surface.rs` from source
  identity because the runner invokes only the outer workbench.

### 6. Tests Required

- `cargo test -p vibex-content -p vibex-terminal -p vibex-desktop --locked`.
- `pnpm check:native-content` and its zero-allocation/redaction assertions.
- Assert the Native Content source roots include every directly embedded surface module.
- On an active physical Linux output, run the capture writer, visually inspect the
  screenshot at original resolution, then run the read-only check and negative
  self-test.
- Run terminal stress/soak and PDF/Office interaction protocols separately before
  marking the aggregate Native Content gate complete.
- Run the Native Content switch contract and assert seven bounded switches, stale/close
  callback fencing, overlay focus return, latest bounds preservation, crash recovery,
  one final visible/focused surface, and zero Web allocations. Run its negative
  self-test before marking the integration row complete.
- Run root `pnpm check` and the Linux package verifier before commit.

### 7. Wrong vs Correct

#### Wrong

```text
hyprctl output create headless -> inject command -> mark native Wayland physical pass
```

#### Correct

```text
require active physical monitor -> require xwayland=false -> inject sanitized input
-> observe raw PTY marker -> store booleans/counts only -> capture pixels -> close zero
```

## Scenario: GPUI Terminal Stress And Independent Xorg Evidence

### 1. Scope / Trigger

- Trigger: the product Terminal PTY/parser/surface, its memory or repaint budgets, or
  Linux X11 Native Content evidence changes.
- This is a cross-layer evidence boundary: `TerminalManager` owns PTYs and raw
  snapshots, `TerminalSurfaceBackend` owns VT state, `TerminalFrameCache` owns damaged
  cells, and the platform runner owns display identity.

### 2. Signatures

```text
vibex-terminal-stress --soak-seconds <seconds> --output <report.json>
pnpm capture:terminal-stress:linux
pnpm check:terminal-stress:linux
pnpm capture:native-content:x11:linux
pnpm check:native-content:x11:linux

terminal-stress-linux-run.v1 -> {
  throughput, burst { renderUpdates, fullRepaints, partialRepaints,
                      changedRows, maxParseFrameMs, boundedRepaint },
  scrollback, lifecycle, resize, sequenceRebuild,
  soak { requestedSeconds, observedSeconds, activityTicks, sequenceGaps,
         rawDroppedChunks, renderUpdates, fullRepaints, partialRepaints },
  resources { rssGrowthBytes, fdLeakObserved, childLeakObserved },
  privacy
}

native-content-x11-linux-evidence.v1 ->
  status: passed | blocked
  runner { displayBackend, syntheticDisplay, physicalXorgProcessObserved,
           independentXorgAuthorized, xwaylandDetected, physicalConnector,
           dri3Observed, xtestObserved }
```

### 3. Contracts

- Task evidence runs at least 300 observed seconds under the workspace user's
  2026-07-19 duration decision. A quick/zero-duration run may test code locally, but
  cannot be committed as five-minute evidence.
- The 10 MiB fixture disables PTY output post-processing before hashing; otherwise
  `ONLCR` can turn LF into CRLF and create a false data-loss result.
- Product polling calls `TerminalManager::raw_snapshot_from(terminalId, nextSequence)`.
  It clones only unconsumed chunks; if `nextSequence` was evicted or belongs to an
  older restored runtime, it returns the retained ring so the backend rebuilds. Do not
  clone the full 16 MiB ring every 16 ms after the parser has caught up.
- A 120 FPS source burst must retain all 120 markers while the 16 ms surface frame path
  coalesces work. Run the real parser and `TerminalFrameCache`; counting raw PTY bytes
  alone is not bounded-repaint evidence.
- Scrollback retains at most 10,000 history lines and the terminal model remains within
  128 MiB. Repeated create/kill/restore must leave no live sessions.
- The soak performs recurring PTY writes, raw snapshots, parser sync, frame generation,
  and frame-cache application. It permits no sequence gap or dropped raw chunk. Its
  bounded-repaint load writes a unique counter to a stable viewport row with explicit
  erase/home control sequences; newline-driven scrolling legitimately marks the whole
  viewport damaged and must not be paired with a `fullRepaints <= 2` assertion.
- Each soak activity tick waits for its incremental raw snapshot to reach the parser.
  Empty polls are allowed, so `snapshots >= activityTicks`; every completed tick must
  produce exactly one frame-cache update, at most two total full repaints, and at least
  one partial repaint for a non-zero run.
- Linux `/proc` samples bind parent RSS growth to 64 MiB and require final FD and direct-
  child counts to return to baseline (FD tolerance: two observation descriptors).
- X11 passes only on an authorized independent Xorg server with a non-virtual connected
  output, DRI3, XTEST input, non-uniform pixels, a PTY marker, and clean exit.
- `XWAYLAND`, Xvfb, Xephyr, an inaccessible SDDM Xorg, or a physical Xorg process whose
  active output cannot be proved is `blocked`, never a physical X11 pass. Never store an
  Xauthority path or cookie in evidence.
- Reports retain hashes, counts, timings, booleans, stable blocker codes, and bounded
  resource values only. Raw terminal output, markers, environment, workspace/home paths,
  and authorization material are forbidden.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| 10 MiB observed hash differs or raw chunks drop | Stress run fails; do not write passed evidence. |
| Source burst loses a marker or frame-cache work is unbounded | Fail `terminal_120_fps_burst`. |
| History exceeds 10,000 or model exceeds 128 MiB | Fail `terminal_10000_line_scrollback`. |
| Injected sequence gap does not rebuild retained state | Fail `terminal_sequence_rebuild`. |
| Caught-up incremental snapshot retains bytes, or new snapshot equals the full ring | Fail the incremental raw-copy assertion. |
| Soak is shorter than 300 seconds | Read-only evidence validator rejects it. |
| Soak activity has no matching parser/frame update, or stable-row output repeatedly causes full repaint | Fail `terminal_five_minute_activity`. |
| Final FD/direct-child count grows or RSS grows over 64 MiB | Stress run fails. |
| `DISPLAY` advertises the `XWAYLAND` extension | Record `xwayland-rejected`; X11 claim remains blocked. |
| Independent Xorg exists but authentication/output is unavailable | Record `physical-xorg-authorization-unavailable`; capture/run stay null. |
| X11 report contains an auth path/cookie, terminal marker, or private path | Privacy validator fails. |

### 5. Good/Base/Bad Cases

- Good: 10 MiB hashes match, 120 source frames coalesce into fewer damage-scoped frame
  updates, 10,000 history lines stay under budget, 16 restore cycles close, and a real
  five-minute run returns RSS/FD/child counts within budget.
- Good: an authorized Xorg session reports a physical connector, injects `t` through
  XTEST, captures non-uniform pixels, and exits through the window close path.
- Base: the active desktop is Wayland, `DISPLAY` is XWayland, and an independent SDDM
  Xorg cannot be authenticated. Commit a source-bound blocked row with no pixel/input
  claim so the runner is ready for a later physical Xorg session.
- Bad: edit `soakObservedSeconds` to 300 after a quick run, count only raw burst bytes,
  drive the bounded-repaint soak with scrolling newlines, clone the entire raw ring on
  every poll, or call XWayland an X11 physical matrix.

### 6. Tests Required

- Run `cargo test -p vibex-content -p vibex-terminal --locked` and the quick stress
  binary while developing.
- Before release evidence, run `pnpm capture:terminal-stress:linux`; assert exactly
  10 MiB/hash equality, 120 markers, bounded repaint, 10,000-line cap, 16 restores,
  300 observed seconds, one render update per activity tick, at most two soak full
  repaints, zero gaps/drops, and bounded `/proc` metrics.
- Run the stress negative self-test; it must reject short duration, hash/frame/repaint
  drift, resource leaks, source drift, and retained output.
- Run the X11 writer and negative self-test. A blocked run must have null capture/run/
  process fields; a passed run must reject XWayland substitution, missing connector,
  missing marker, and stale screenshot identity.
- Rerun the aggregate Wayland Native Content capture after either source-bound evidence
  changes, then run root `pnpm check`.

### 7. Wrong vs Correct

#### Wrong

```text
DISPLAY=:1 (XWAYLAND extension) -> non-uniform pixels -> x11_native_matrix=passed
quick stress --soak-seconds 0 -> edit JSON duration to 300 -> soak=passed
soak tick -> print newline -> scroll viewport -> require fullRepaints <= 2
```

#### Correct

```text
probe server extension + connector + authorization
  -> XWayland/inaccessible Xorg: blocked with null run
  -> independent active Xorg: XTEST input + pixels + clean close

run PTY + parser + frame cache for >=300 observed seconds
  -> erase/home stable row + unique counter -> wait for incremental parser/frame update
  -> bind source hashes -> validate negative mutations -> aggregate by exact SHA-256
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
- Physical PDF/Office evidence is separately source-bound. Capture after Page 2 is
  selected at Fit width, measure a fixed in-window PDF region, and require at least 100
  colors plus non-zero entropy and standard deviation before accepting page pixels.
  Then prove zoom input and Office close-to-zero residency; run the evidence's negative
  self-test from the root check.
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
- Evidence source identities are byte-based across their declared source roots. Even a
  formatting-only change under `apps/desktop/src` invalidates PDF feasibility,
  controller, surface, Native Content, and package evidence that includes that tree;
  binary equivalence does not make the older evidence current.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| PDFium resource cannot be discovered/bound | Typed `pdfium_*` error plus system-open option. |
| Selected path is missing or not exact `.pdf` | `pdf_path_missing` / `pdf_path_extension_invalid`. |
| New request arrives during render | Cancel old token, keep only latest pending request, reject stale publish. |
| Estimated native page RGBA exceeds controller cache | `pdf_page_exceeds_cache_budget` before PDFium allocation. |
| GPUI image copies exceed 3 pages / 72 MiB | Drop overscan by priority; current page must remain or return `pdf_ui_image_budget_exceeded`. |
| Resize changes fit target | Debounced rerender; no identical-width work. |
| Embedded columns have no full-height constraint, or fit uses whole-window width | Physical pixel capture fails because the page is blank/cropped; do not accept model counts alone. |
| Selected page and visible page differ after navigation | Physical crop/state contract fails even when `currentPage` changed. |
| Corrupt/encrypted/native failure | Typed error state with Retry and explicit System open; encrypted fixture reports `pdf_password_required` and zero resident bytes. |
| Source exceeds 256 MiB | `pdf_source_size_invalid` before PDFium binding; zero decoded/UI bytes. |
| Document has 10,001 pages | `pdf_page_count_unsupported`; zero decoded/UI bytes. |
| Extreme page exceeds estimated RGBA budget | `pdf_page_exceeds_cache_budget` before native render; zero decoded/UI bytes. |
| Native call crashes or exceeds a hard deadline | Kill/reap child; `pdf_worker_crashed` / `pdf_worker_timeout`; next isolated request succeeds. |
| Worker report contains unsafe path or invalid bitmap length | `pdf_worker_protocol_failed`; no image publication; temporary directory removed. |
| Close succeeds | Closed state, no metadata, active worker, controller/native cache, or UI images. |
| No active physical monitor | Ready report may pass; pixel/input claim remains blocked. |
| A declared source input changes after capture | Read-only evidence checks fail as stale; recapture through the owning writer. |

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
- Base: `rustfmt` changes only whitespace in the GPUI source tree; every evidence writer
  whose source roots include that tree still requires recapture.
- Bad: keep a controller behind a foreground mutex and render PDFium synchronously from
  a click handler.
- Bad: treat the controller RGBA cache as the only memory budget while retaining an
  unbounded second set of GPUI `RenderImage` copies.
- Bad: record `status=ready` and claim the page was physically visible or scrollable.
- Bad: size Fit width from `window.viewport_size()` inside a split, or rely on
  `renderedPages > 0` while the page list/document columns have zero allocated height.
- Bad: cancel a worker future without killing/reaping its child, or report the last
  child's cache as current desktop-process residency.

### 6. Tests Required

- Unit-test fit/percentage width bounds, exact extension policy, RGBA-to-BGRA conversion,
  UI image current-page priority, and both resource metrics.
- Controller tests must reject invalid dimensions and over-budget estimated RGBA before
  the native render call.
- Worker supervisor evidence must run normal -> abort -> recovery -> hang -> recovery,
  assert five children started/reaped, exact crash/timeout codes, and privacy. Its
  negative self-test rejects missed failures, unreaped children, recovery drift, and
  privacy leakage.
- Worker soak evidence must run 49 Linux requests with the frozen 37 normal / four
  cancel / four crash / four timeout matrix, prove 12 recoveries, reap all 49 children,
  retain FD/direct-child/temp-directory baselines, keep current native residency zero,
  and stay within 64 MiB parent RSS growth. Negative tests reject every leak dimension.
- Real Linux smoke launches the standalone workbench with reviewed PDFium and the
  12-page fixture, waits for `pdf-surface-run.v1`, and asserts page count,
  `[0, 1]`, cache/image budgets, controls, and privacy. Terminating that smoke is not a
  clean-window-close or physical-input claim.
- Launch the same workbench with the deterministic encrypted fixture and no password;
  assert typed error code, recovery controls, zero resident decoded/UI bytes, privacy,
  and negative self-tests that reject a changed code or retained bytes.
- Launch sparse oversized-source, deterministic 10,001-page, and extreme-page runs;
  assert exact typed codes, recovery controls, zero resident bytes, and zero preflighted
  native render requests where applicable.
- Run controller evidence, Native Content blocked/current evidence, license/SBOM,
  generic Linux package, Native Content package, and root `pnpm check` after source or
  lock drift.
- Run formatting before the final evidence pass. If formatting or another late source
  edit occurs, recapture PDF feasibility, controller, surface, and Native Content
  evidence; rebuild the generic Linux package; rebuild the Native Content package last
  because both packagers share target artifact paths; then regenerate the feasibility
  decision record before the final read-only gates.
- On an active physical output, separately capture and inspect PDF scrolling, page-list
  selection, fit/zoom, retry/error, system-open, resize, and clean close. The committed
  interaction capture must bind source inputs, visibly show selected Page 2 at Fit width,
  meet the PDF-region pixel thresholds, prove zoom and Office close, and pass negative
  mutations for source drift, blank pixels, stale screenshot, missed input, cache leak,
  and retained content.

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

## Scenario: GPUI Hosted-Runner Evidence Scope

### 1. Scope / Trigger

- Trigger: macOS and Windows GPUI feasibility checks run without locally available
  native machines.
- GitHub-hosted evidence covers reproducible build, test, initialization, packaging,
  lifecycle, artifact, and supply-chain claims. It does not close the overall
  feasibility task or choose its final decision by itself.

### 2. Signatures

```text
node scripts/capture-x11-first-frame.mjs --write-linux-native
node scripts/check-hosted-runner-evidence.mjs --policy
node scripts/check-hosted-runner-evidence.mjs --self-test
.github/workflows/native-gate.yml -> workflow_dispatch

hosted-runner-target-evidence.v1 -> {
  policy, source, target, runner, toolchain,
  checks[], skippedClaims[], probes, package, decisionSummary
}

hosted-runner-matrix-evidence.v1 -> {
  policy, source, requiredTargets, targets[], skippedClaims[],
  hostedGateSatisfied, decisionSummary, limitations[]
}
```

### 3. Contracts

- Pin the matrix to `macos-15` and `windows-2022`, Rust 1.97.0, Node 22, pnpm 11.3.0,
  and cargo-packager 0.11.8. A latest alias or Linux cross-build cannot replace either
  target.
- Decision-bearing checks cover pinned toolchain, locked metadata/source identity,
  workspace and GPUI tests, frontend quality, supply chain, release linking, bounded
  platform initialization, minimal native packaging, install/probe/uninstall, and
  artifact hashes.
- The initialization check may launch the real process and require it to remain alive
  for a bounded interval. It records no screenshot, native pixel, window correctness,
  or real-input claim and terminates the process after observation.
- macOS packages use an isolated copied `.app`; Windows uses an isolated current-user
  NSIS install. The installed binary must return the same bounded `--probe` contract
  and retain the linked release binary SHA-256 before uninstall.
- Real-window screenshots/native pixels, IME, keyboard/pointer/clipboard/drag-drop
  input, DPI/scale transitions, and multi-monitor behavior are exactly
  `skipped_by_product_decision` with `decisionImpact: false`. A skip is excluded from
  the denominator and is not a pass, failure, blocker, accepted deviation, or parity
  evidence.
- Per-target evidence binds the policy, lockfiles, source/config/scripts, SBOM,
  notices, and workflow by SHA-256. The merge job accepts exactly one native artifact
  from each required target and requires identical source inputs.
- A failed decision-bearing hosted check keeps the hosted sub-gate false. A skipped
  claim cannot change that result in either direction, and hosted scope never weakens
  physical Linux pixels or Linux behavior-spike requirements.

### 4. Validation & Error Matrix

- Runner label, OS, architecture, package format, or tool version mismatches policy ->
  reject the target artifact.
- Required decision check is missing, duplicated, reordered, or marked non-decision ->
  reject the artifact.
- Failed check lacks a bounded failure summary, or a dependency-blocked check lacks
  the failed dependency id -> reject the artifact.
- Hosted skip is missing, renamed, marked decision-bearing, or presented as parity ->
  reject the artifact.
- `--probe` reports native pixels, the wrong platform/source revision, or an unknown
  schema -> fail the probe check.
- Platform process exits before the observation interval -> fail initialization; do
  not substitute a screenshot or synthetic input.
- Package is missing, installed binary hash changes, probe fails, or uninstall leaves
  the executable -> fail the corresponding decision-bearing check.
- Policy/source hash changes, target source inputs differ, or one target artifact is
  missing -> reject the merged matrix as stale or incomplete.

### 5. Good/Base/Bad Cases

- Good: both pinned runners pass locked tests, initialize, package, install, probe,
  hash, and uninstall; the merged hosted sub-gate passes while all five GUI claims
  remain explicit non-decision skips.
- Base: one release link fails, dependent package checks are recorded as blocked, the
  target artifact is still uploaded, and the merged hosted sub-gate stays false.
- Bad: call a five-second live process observation a real-window or input pass.
- Bad: remove skipped rows to make the pass percentage appear higher, or count them as
  blockers to force a final feasibility decision.

### 6. Tests Required

- Parse the workflow as YAML and run `--policy` plus `--self-test` locally.
- Dispatch the workflow only from a committed source revision. Retain each native
  package, per-target JSON, and merged matrix artifact.
- Inspect failed action logs without editing target JSON by hand; rerun from a reviewed
  source change when a decision-bearing check fails.
- After downloading the merged artifact, run `--validate` before committing it.
- Confirm the Windows installation and macOS copied app no longer exist after each
  target job.

### 7. Wrong vs Correct

#### Wrong

```json
{
  "target": "macos",
  "nativePixels": "passed because the process stayed alive",
  "ime": "assumed",
  "decisionImpact": true
}
```

#### Correct

```json
{
  "id": "real_window_screenshots_native_pixels",
  "status": "skipped_by_product_decision",
  "decisionImpact": false,
  "decisionDenominator": "excluded",
  "notEvidenceOfParity": true
}
```

## Scenario: GPUI Feasibility Decision Record

### 1. Scope / Trigger

- Trigger: a baseline/feasibility task closes with unresolved execution or release
  work that product explicitly accepts rather than treating as passed.

### 2. Signatures

```text
node scripts/check-feasibility-decision.mjs --write
node scripts/check-feasibility-decision.mjs
node scripts/check-feasibility-decision.mjs --self-test

feasibility-decision.v1 -> {
  decision, sources, evidence[], passedGates[], selectedRoutes,
  hostedNonDecisionExclusions[], acceptedDeviations[], revisedEstimates[],
  inheritedPrerequisites[], rollback, closure
}
```

### 3. Contracts

- The decision value is exactly `GO`, `GO_WITH_ACCEPTED_DEVIATIONS`, or `NO_GO`.
- `GO_WITH_ACCEPTED_DEVIATIONS` enumerates every unrun or incomplete item with a
  stable id, owner, rationale, follow-up, user impact, and reopen behavior. An
  accepted deviation is never added to `passedGates`.
- Bind every decision-bearing evidence, policy, workflow, budget, SBOM, and support
  matrix input by path, byte length, and SHA-256. Normal checks verify; only the
  explicit writer regenerates the record.
- Route selection and distribution readiness are separate claims. A proven Linux
  implementation route may be selected while cross-platform runtime, license notices,
  or package registration remains an accepted follow-up.
- A deferred performance record retains every numerical contract and owner, reports
  no budget pass, and states whether later failure reopens the feasibility decision.
- Hosted GUI exclusions remain outside the denominator. Pending hosted
  decision-bearing checks are accepted only with an explicit reopen-on-failure rule.
- Keep the current production shell and rollback path until release cutover passes.

### 4. Validation & Error Matrix

- Missing evidence or source hash drift -> reject the decision as stale.
- Strict `GO` with any accepted deviation or incomplete hosted execution -> reject.
- Accepted deviation missing owner/follow-up/user impact -> reject.
- Deferred test represented in `passedGates` -> reject.
- Selected native route silently registered as production-distributable without its
  license/package evidence -> reject.
- Performance sampling deferred with a dropped scenario or numerical target -> reject.
- Reopen condition removed from a decision-bearing deferred check -> reject.

### 5. Good/Base/Bad Cases

- Good: Linux proves a PDF route, product selects it, and the record separately carries
  macOS/Windows runtime plus binary-license/package work with reopen conditions.
- Base: a hosted workflow is committed but not run; the decision may close with an
  accepted deviation, while any decision-bearing failure reopens the gate.
- Bad: mark WebView, performance, package release readiness, or hosted execution as
  passed because the user allowed the task to continue.

### 6. Tests Required

- Run the verifier and negative self-test.
- Mutate the value to strict `GO`, remove a deviation, fabricate hosted completion,
  and remove a reopen condition; each mutation must be rejected.
- Run the focused evidence validators, license gate, and root `pnpm check` before
  committing the decision.

### 7. Wrong vs Correct

#### Wrong

```json
{
  "decision": "GO",
  "passedGates": ["webview", "performance", "hosted_packages"]
}
```

#### Correct

```json
{
  "decision": { "value": "GO_WITH_ACCEPTED_DEVIATIONS" },
  "acceptedDeviations": [
    {
      "id": "hosted_native_execution_pending",
      "status": "accepted",
      "owner": "desktop-platform",
      "reopenGateOnFailure": true
    }
  ]
}
```

## Performance Expectations

- Virtualize long session timelines, file trees, Git diffs, and terminal output
  where needed.
- Avoid loading Monaco/CodeMirror into Web/mobile remote bundles.
- Keep terminal output rendering buffered and throttled.
- Paginate history and large timeline fetches.

## Accessibility Expectations

- Command palette, pairing dialogs, permission dialogs, and settings modals must
  support keyboard navigation and focus management.
- Cards with disclosure state need accessible labels and state.
- Approval buttons must include text labels.
- Color cannot be the only signal for provider health, Git status, or security
  warnings.

## Scenario: Composer Image Attachment UX

### 1. Scope / Trigger

- Trigger: Agent composer inputs accept uploaded or pasted images and render
  pending image attachments before send.

### 2. Signatures

```text
ComposerDraft { text: string, attachments: ComposerImageAttachment[] }
ComposerImageAttachment extends MessageAttachment { id: string, previewUrl: string }
SendAgentMessageRequest.attachments -> MessageAttachment[]
```

### 3. Contracts

- Uploaded and pasted images must use the same composer attachment token UI.
- Clipboard image file items and pasted HTML `data:image/*` sources must be
  intercepted before the browser inserts a raw `<img>` into a contentEditable
  composer.
- ContentEditable composers must also sanitize any embedded `<img>` on input as
  a fallback because browsers differ in paste payload shape.
- Hover previews must render in a viewport-level/fixed layer, not as a child of
  a token inside a clipped or height-animated composer container.
- Clicking or keyboard-activating an image token must open an accessible Dialog
  with a title, full-size image inspection, and a save/download action.
- New-session composers and in-session composers must share the same attachment
  behavior and must send the same `MessageAttachment[]` contract.

### 4. Validation & Error Matrix

- Unreadable image file -> skip that file without breaking the remaining paste
  or upload operation.
- Unsupported pasted image URL -> remove the raw image node rather than leaving
  a large inline image inside the composer.
- Empty text plus one or more attachments -> still allowed to send.

### 5. Good/Base/Bad Cases

- Good: screenshot paste creates a compact image token; hover shows an unclipped
  preview; click opens a full-screen preview with save.
- Base: plain-text paste remains text-only.
- Bad: raw contentEditable paste leaves a large `<img>` in the input box.
- Bad: token hover preview is absolutely positioned under an ancestor with
  `overflow: hidden` and becomes invisible.

### 6. Tests Required

- `pnpm check:frontend` after composer changes.
- Desktop build or browser smoke for dialog/import correctness.
- Manual smoke should cover upload image, paste clipboard image, click preview,
  save/download link presence, and new-session initial image attachment send.

### 7. Wrong vs Correct

#### Wrong

```tsx
<span className="composer-image-token">
  <span className="absolute bottom-full">...</span>
</span>
```

#### Correct

```tsx
<ComposerImageHoverPreview style={viewportFixedPosition} />
<Dialog>
  <DialogContent>
    <DialogTitle>Image preview</DialogTitle>
    ...
  </DialogContent>
</Dialog>
```

## shadcn / Vite Shell Verification

- Tailwind class-based dark mode must be driven by explicit theme state or
  `prefers-color-scheme`; do not hardcode `className="dark ..."` on app root
  containers. Hardcoding dark mode makes light-mode regressions impossible to
  see in screenshots.
- Monaco or other embedded editors must receive the same resolved theme state
  as the app shell, such as `theme={isDarkMode ? "vs-dark" : "vs"}`.
- Generated or app-local `Dialog`, `CommandDialog`, `Sheet`, and `Drawer`
  wrappers must keep their title component inside the content component. Use an
  `sr-only` title when the design should not show visible heading text.
- Browser screenshot checks must include first-party 4xx/5xx responses. Missing
  assets such as `/favicon.ico` count as acceptance issues because they create
  noisy console errors in clean browser sessions.

#### Wrong

```tsx
<div className="dark flex min-h-full bg-background text-foreground" />

<Dialog>
  <DialogHeader className="sr-only">
    <DialogTitle>Command Palette</DialogTitle>
  </DialogHeader>
  <DialogContent>{children}</DialogContent>
</Dialog>
```

#### Correct

```tsx
<div className={cn(isDarkMode && "dark", "flex min-h-full bg-background text-foreground")} />

<Dialog>
  <DialogContent>
    <DialogHeader className="sr-only">
      <DialogTitle>Command Palette</DialogTitle>
    </DialogHeader>
    {children}
  </DialogContent>
</Dialog>
```

## Scenario: Phase 2 PC Workbench UI Shell

### 1. Scope / Trigger

- Trigger: Phase 2 replaces the session-only desktop screen with a PC workbench
  shell that shows Agent, files/Monaco, Git diff/actions, terminal tabs, and a
  right rail for one local workspace.
- This is a frontend contract because layout, generated protocol types, browser
  screenshot mocks, Monaco, xterm, TanStack Query, and Zustand state all need
  consistent ownership.

### 2. Signatures

Feature ownership:

```text
app/App.tsx                         root providers only
features/workspace/WorkspaceShell   shell, tabs, rails, active workspace
features/files/*                    file tree, file read/save, Monaco pane
features/git/*                      status, diff, stage/unstage/revert/commit
features/terminal/*                 xterm surface and terminal mutations
features/agent/*                    provider-neutral session/timeline UI
lib/tauri.ts                        typed invoke wrapper and browser mock
```

State ownership:

```text
TanStack Query -> backend state from Tauri commands
Zustand        -> local workbench selection, active tab, rail, editor buffers
React state    -> form inputs and transient confirmations
```

### 3. Contracts

- Frontend code consumes Vibex DTOs through the shared Rust Backend contracts; it must not redefine
  transport contracts or branch on native Codex/Claude payloads.
- Monaco belongs only in the desktop app and is lazy-loaded. Its container chain
  must have explicit `flex`, `min-h-0`, and `h-full`/`flex-1` sizing before
  using `height="100%"`.
- File language ids must match Monaco ids: `.tsx` -> `typescriptreact` and
  `.jsx` -> `javascriptreact`.
- xterm.js owns raw terminal rendering. React should mutate xterm through refs
  and snapshots rather than re-rendering per byte.
- Browser screenshot mode may use `lib/tauri.ts` mock responses only when Tauri
  internals are unavailable. Native desktop runtime must still call
  `__TAURI_INTERNALS__`.

### 4. Validation & Error Matrix

- Missing workspace -> disable workspace-scoped actions and render an empty
  state, not a throwing component.
- Missing selected file -> render a no-file state; do not mount Monaco against
  an undefined buffer.
- Destructive file delete or Git revert -> require `window.confirm` before
  invoking the mutation in this slice.
- Terminal helper input generated by xterm -> add accessible metadata such as a
  stable `name` after `terminal.open`.
- Browser screenshot console -> Vite debug logs are acceptable; runtime errors
  and unresolved accessibility issues should be fixed before acceptance.

### 5. Good/Base/Bad Cases

- Good: Agent, file tree/Monaco, Git diff/actions, terminal tabs, and right rail
  are simultaneously visible for the selected workspace.
- Base: the narrow browser screenshot viewport may compress panels, but text
  must remain readable and controls must not overlap incoherently.
- Bad: Monaco renders as a black or one-line pane because the parent height is
  implicit; xterm initialization loops because callbacks are unstable effect
  dependencies; browser mocks diverge from real command DTO names.

### 6. Tests Required

- `pnpm check:frontend` or root `pnpm check` for typecheck/lint.
- Desktop `vite build` after adding Monaco/xterm dependencies.
- For requested or high-risk visual changes, screenshots can be mapped to the
  matching GPUI Desktop workbench surfaces: Agent/right rail, file explorer,
  editor, Git diff, and terminal mode.
- When screenshots are captured, check the browser console after reloads: no
  runtime errors and no unresolved form-field accessibility issues from
  first-party controls.

### 7. Wrong vs Correct

#### Wrong

```tsx
<div className="overflow-hidden">
  <Editor height="100%" value={content} />
</div>
```

The editor can collapse because no parent in the chain owns a real height.

#### Correct

```tsx
<div className="flex min-h-0 flex-1 overflow-hidden">
  <section className="flex h-full min-h-0 flex-1 flex-col overflow-hidden">
    <Editor height="100%" value={content} />
  </section>
</div>
```


## Scenario: Phase 6 ACP Provider Settings Surface

### 1. Scope / Trigger

- Trigger: Phase 6 exposes bundled ACP catalog presets and editable ACP command
  configuration in desktop Provider settings.
- The UI consumes shared ACP/capability DTOs through the typed Backend facade.

### 2. Contracts

- Provider settings must not parse `ProviderProfile.providerOptions.entries`
  for ACP config. It must call the typed ACP config command and render
  `AcpProviderConfig`.
- ACP preset creation must use `provider_create_acp_profile`; ACP config saves
  must use `provider_update_acp_profile_config`.
- Browser mocks may encode ACP config internally for fixture parity, but mock
  UI consumers must still use typed API helpers.
- Injection preview must show ACP command, args, cwd/options, and env references
  as redacted Provider preview fields.
- Browser mock catalog/profile examples must not contain plaintext secrets.
- ACP capability status, source, freshness, and supported/unsupported flags must
  render from `ProviderCapabilitySummary`, not by parsing
  `ProviderProfile.providerOptions.entries`.
- Capability refresh controls call `provider_run_capability_probes`; disabled
  UI states are advisory only because backend runtime gates remain
  authoritative.

### 3. Tests Required

- `pnpm --filter @vibex/desktop typecheck`.
- `pnpm check:frontend` and root `pnpm check` before archiving.
- For UI-changing ACP work, capture a Provider settings screenshot only when
  requested, when visual regression risk is high, or when local browser
  rendering is already part of validation.

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
- Physical evidence input roots include every source that can change the probe or
  capture result. A contract producer such as `apps/desktop/src/testing.rs`
  and the owning `crates/core` DTO source cannot be omitted merely because the
  main renderer source is already listed.
- Code Workbench evidence also binds the complete shared Backend facade/native
  adapter source set and `crates/vibex-ui/src/shell.rs`. A facade, Terminal,
  capability, or Shell change can alter the exercised workbench even when
  `code_workbench.rs` itself is unchanged.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Requested virtual range exceeds the eager bound | Clamp the returned range; never allocate all rows. |
| A tracked `uniform_list` omits `.size_full()` | Source-contract/evidence check fails. |
| A wrapping patch row is rendered in `uniform_list` or keeps a fixed height | Source-contract/evidence check fails; use the per-tab variable-height list. |
| Patch width, revision, or visible row count changes | Remeasure/reset the `ListState` without materializing the full patch; focused-file navigation still reaches the requested row. |
| 360 px viewport renders a persisted horizontal split | Render panes vertically without mutating persisted direction. |
| A Backend or GPUI layer redefines a Rust protocol leaf type | Rust compilation or protocol tests fail; import the canonical `crates/core` type. |
| Probe-producing source is absent from evidence inputs | Evidence review fails; add the source and recapture. |
| Shared Backend/Shell changes but Code Workbench source identity remains unchanged | Source binding is incomplete; add the producer and rerun capture plus self-test. |
| Screenshot is blank, overlaps controls, or has wrong dimensions | Reject the capture even when model tests pass. |

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
- Run `pnpm check:code-workbench` and `pnpm check`.
- `pnpm check:code-workbench` must run both ordinary verification and its
  negative evidence self-test after Backend or Shell ownership changes.
- After renderer or evidence-input changes, run
  `pnpm capture:code-workbench` on the physical Wayland runner and inspect all
  eight PNGs at original resolution: Files light desktop/narrow, Diff dark
  desktop/narrow, and Markdown light/dark desktop/narrow.

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
  diagram artifacts, syntax highlighting, or related physical evidence.
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

pnpm capture:code-workbench -> eight physical Wayland PNGs plus bound evidence
pnpm check:code-workbench   -> offline identity/contract check and negative self-test
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
- Physical capture is fail-closed while any exact `hyprlock`, `swaylock`, `gtklock`,
  or `waylock` process is active. The Code Workbench matrix contains exactly eight
  original-resolution captures: Files light desktop/narrow, Diff dark
  desktop/narrow, and Markdown light/dark desktop/narrow. Inspect every image for a
  real application frame, nonblank math/diagrams, and text/control overlap before
  accepting evidence.

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
| Physical capture starts while a supported screen locker is active | Refuse before launching/capturing and write no replacement evidence. |
| One of the eight PNGs is missing, stale, blank, lock-screen content, or overlaps | Reject the visual matrix even when unit and GPUI tests pass. |

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
- Run `cargo metadata --locked`, `pnpm check:graph`, and
  `pnpm check:licenses`; regenerated notices/SBOM must bind the selected local
  engines and contain no hidden browser, Node, JVM, remote-renderer, or separately
  downloaded Graphviz runtime.
- After source/lock changes, capture the eight Code Workbench scenarios on unlocked
  physical Wayland, inspect every PNG at original resolution, then run
  `pnpm check:code-workbench`, the related dependency revalidation, and the
  feasibility-decision writer/check.

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
- Diagnostics and physical evidence may store kind, counts, bounds, and action results,
  but never document paths or extracted Office content.

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
- Physical Linux evidence must load deterministic bounded Office fixtures, inspect the
  rendered model and explicit controls, close cleanly, and reject content/path leakage.
- Keep PDF/Office physical interaction blocked until that active-output protocol passes;
  controller unit tests or the presence of a status row are insufficient.

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

## Scenario: Native Mobile Device Evidence

Native SDK/device evidence is tied to the exact source, Cargo lock, vendored Zed
revision, and produced application artifact. A host-side check proves source and
type contracts only.

Required device scenarios are:

- first frame and safe-area layout;
- touch scrolling and session-drawer edge gesture;
- IME commit, selection, paste, keyboard resize, and focus recovery;
- GUI timeline streaming, Markdown, process expansion, and approval actions;
- send, stop, continue, disconnect, reconnect, and authoritative catch-up;
- Direct/Tailnet/Relay route selection and credential persistence/redaction;
- foreground/background lifecycle and network transition.

Each scenario records `passed`, `failed`, or `not_tested`. Missing device evidence
remains `not_tested`; it is never inferred from a successful Rust or Gradle/Xcode
compile. Evidence stores hashes, bounded platform labels, and status only, never
pairing links, tokens, device serials, prompts, file contents, or terminal bytes.

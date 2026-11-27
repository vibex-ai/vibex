# Vibex Mobile on Native Shell

The Vibex mobile client, rebuilt on [Native Shell](https://nsh.dev) (`nsh`).
It replaces `apps/mobile`'s GPUI view layer screen by screen; the rest of
`apps/mobile` keeps shipping until the last screen moves.

## Why

`apps/mobile` draws its whole surface with GPUI (`gpui-pre-mobile`), including
text input, selection, scrolling and navigation. On a phone that means the app
reimplements what the platform already does, and the result feels foreign:
caret and selection handles, the keyboard, scroll physics and the back gesture
are drawn by us instead of by iOS and Android.

`nsh` mounts each platform's own views — UIKit on iOS, Android views on
Android — over a Rust engine. The same Rust crate runs on both, and the
platform owns text, input, selection, scrolling, accessibility and navigation.
That is the reason for the move; it is not a dependency swap.

The desktop stays on GPUI (`gpui-fast`). `nsh`'s GPUI backend draws with the
same crate, so the desktop can embed these screens later if it moves, but it
does not today.

## Layout

| Path | What |
| --- | --- |
| `apps/mobile-nsh` | The `nsh` app: `nsh.json`, the root component, the pairing service, host seams, theme generation |
| `crates/vibex-nsh-ui` | The shared UI layer: theme, styled controls, screens |
| `crates/vibex-mobile-pairing` | Platform-neutral pairing: entries, claims, credentials, the on-device store |
| `theme.json` | The palette, generated from `crates/vibex-ui/theme/tokens.json` |

`crates/vibex-nsh-ui` depends only on `nsh-core` and `nsh-base`; it does not
link GPUI. The screens are written to be reusable on the desktop when it
moves, so they take the platform's behavior instead of drawing their own.

## What is there today

The pairing page, which is the phone's front door: the wordmark, the saved
runtimes list, the three ways in (scan the QR code, find a runtime on the same
network, paste a link) and the manual and nearby sheets.

The page is backed by a real pairing service:

- entries (a `vibex://` link, a server connection string, or a code plus the
  server address) are classified and claimed on a background task through
  `crates/vibex-mobile-pairing`;
- a claimed credential is stored in `pairing.json` in the app's data directory
  (owner-only, written atomically) and appears in the saved list, which probes
  each runtime's reachability;
- paste reads the system clipboard through `nsh`.

Scan and Find are native on both phones:

- **Scan** uses the custom host's camera (`android/` with Android's camera,
  `ios/` with AVFoundation): the host polls the app's request queue, opens the
  scanner and reports the code back through the host ABI. Both scanners match
  the GPUI client's: portrait, a scrimmed viewfinder with corner brackets,
  close, the pairing hint, and "Choose from photos" for a screenshot (a picked
  image without a `vibex://` pairing entry says so and keeps the scanner
  open). Without a host the app falls back to the system photo picker and
  `rqrr`; nsh gives a Rust app's views no camera capability, which is why the
  host owns the camera.
- **Find** uses the host's discovery when the custom host is present (Android
  NSD with the `NEARBY_WIFI_DEVICES` permission, Bonjour on iOS), and falls
  back to browsing `_vibex._tcp.local.` in Rust with `mdns-sd`. Both validate
  the zero-config TXT record, list the runtimes they resolve, and start a
  `ZeroConfigLanPairingSession` on tap; the desktop must approve the six-digit
  code the sheet shows.

The iOS host is a first-class shell, not a fallback: the scanner, Bonjour
discovery, the composer camera, notifications, a share extension, a Home Screen
quick action and a background task stand in for the Android host's equivalents
(see "The iOS host" below). iOS has no foreground service, so "stay connected"
is a `beginBackgroundTask` assertion for as long as the system allows rather
than a service that keeps running.

### The session shell

A successful claim or a tapped saved runtime opens the three-page workspace.
The center is the conversation, Sessions is to its left, and Files, Changes
and Terminal share the workbench to its right. Use the header controls or
horizontal drags; returning from a side page leaves the conversation mounted.

- Sessions renders the runtime's sidebar organization, including compact and
  workspace hierarchies, nested folders and groups, search, batch selection,
  and locating the open session. The runtime owns collapse, pin and order;
- the transcript is `vibex-desktop-model`'s projection over the fetched page,
  so a row means the same thing it means on the desktop: user bubbles, Agent
  prose, reasoning, tool cards, notices and errors;
- the composer sends through the session's own runtime selection and reads the
  transcript back (`send_message` then `fetch_timeline`);
- the runtime's event stream is followed for as long as the shell runs: a live
  timeline item is applied to the same projection, so a streamed reply grows
  in place, and session state changes update the drawer;
- Agent messages render Markdown through `nsh`'s `TextView`: tables, code
  blocks, lists and links natively, with a streaming reply's open syntax
  repaired while it arrives;
- a pending permission is a card above the composer: the title says what is
  asked, the lines say what it would do, and every answer the runtime allows
  is a button. Answering sends the resolution as a human reply and re-reads
  the transcript;
- a pending question is a typed form: one field per kind (text, a number, a
  choice, yes/no, multi-select), required fields checked before submitting,
  and the answers converted back to the runtime's typed values;
- during a running turn, the primary action stops the turn when the draft is
  empty and queues the message when the draft has content or an attachment;
- the composer offers the session's `/` commands and `@` references (one
  discovery per session each, filtered locally; a reference replaces the
  `@…` token in place), attaches photos and files through the platform's own
  picker (`pick_images`, `pick_files`, encoded as the `data:` URLs the runtime
  materializes), shows them as removable chips, and sends them with the text;
  the drawer opens a full-screen new-session composer. Its location and model
  sheets use `list_workspaces` and `list_runtime_options`; the location sheet
  also offers a new worktree and a temporary session;
- a session's "⋯" opens its actions: Rename, Archive, and Delete (after the
  native confirmation); removing the active session opens the next one;
- the Terminal tab attaches to the runtime's PTY. Terminal-mode Agent
  sessions open their own PTY instead of an ACP composer. The runtime's PTY
  frame stream backfills output and keeps it live. Split UTF-8 is carried
  across frames; input and size changes are sent in order from a background
  task. The header shows running terminal tabs. Closing the surface leaves the
  PTY running; closing a tab asks before stopping it;
- Files shows an expandable tree and searches names or contents. A content
  result opens the read-only viewer at its line. Markdown and images have
  rendered previews, and source files support find and go to line;
- Changes shows the workspace's Git status: branch, changed paths with
  additions and deletions, and a path's diff from `git_diff`; each row stages
  or unstages its path (`stage`, `unstage`), the status the runtime returns
  replaces the list, a message field commits the selected paths (`commit`), the
  History view lists recent commits (`git_history`), a commit opens its message
  and patch (`git_commit_detail`), and an unstaged diff can be discarded with
  a native confirmation (`git_revert`). The branch sheet lists local branches
  and checks one out (`git_branch_checkout`), then refreshes status and
  history; Worktrees are listed and archived, restored, merged or discarded
  through the runtime's preflight calls.

Attachments, `@`-mentions, the prompt queue and scheduled sends (both owned by
SidebarBackend), session search,
Sidebar folders/groups and the Android host integrations are covered by the
headless and unit suites where the backend is a fixture. The shell's tests
mount it over that fixture; live answer, terminal, Sidebar and Git paths are
compile-verified and use the same backend types the desktop tests cover.

## Run it

Install the `nsh` CLI and use a Rust toolchain (see
[nsh.dev/docs/installation](https://nsh.dev/docs/installation)):

```sh
cd apps/mobile-nsh
nsh run --target ios        # iOS simulator
nsh run --target android    # emulator or device
nsh run --target catalyst   # Mac Catalyst window
```

The iOS/Android hosts come from `nsh`'s generated runners for a Rust app.
Shipping in the existing Kotlin/Swift shells (which own notifications, the
foreground service, the scanner and sharing) is the embedding path: the
platform crates mount this crate's root in an `AppView`/`NshView`, and the
host keeps its native integrations; see "Embedding in the existing shells".

## Build and release

`nsh package` turns the app into an installable project/binary; the scripts
here wrap it with the toolchain checks that make a failure actionable:

```sh
apps/mobile-nsh/scripts/build-android.sh        # needs ANDROID_HOME and an NDK
apps/mobile-nsh/scripts/build-android-host.sh   # the Kotlin host around the library
apps/mobile-nsh/scripts/build-ios.sh            # the nsh runner, needs Xcode
apps/mobile-nsh/scripts/build-ios-host.sh       # the Swift host around the library
```

The two `build-*-host.sh` scripts are the ones that ship: they build the Rust
library for each phone and package the custom host app that owns the native
integrations (`android/`, `ios/`). They take extra `xcodebuild` arguments (iOS)
or a `GRADLE` binary (Android). `build-ios.sh` and `build-android.sh` stay for
the `nsh` CLI's own runners, which are how a screen is iterated on.

Signing, the App Store/Play upload and the store metadata stay with the
generated Gradle/Xcode projects (or a release CI step) rather than this
repository; the app's name, id and version live in `nsh.json`, and the theme is
generated from the design tokens (see "Theme").

The Android path was exercised on a Linux machine with `nsh` 0.6.0, the
Android SDK (platform/build-tools 35) and NDK 27.2: `nsh package` produced a
debug-signed universal APK (arm64-v8a, armeabi-v7a, x86_64). A build of the
same app installs and opens the pairing page and its Link sheet on an API 35
x86_64 emulator, and the universal APK installs and launches there too. The
custom host was rebuilt with `nsh` 0.6.1 — crates, embed AAR and all three
ABIs — and still packages a release APK. A store release needs `--keystore`.

## Upgrading from the GPUI client

This client replaces `apps/mobile` on the same phones, so it keeps that app's
identity: Android `applicationId ai.vibex.mobile` with the same `versionCode`
scheme, iOS bundle id `ai.vibex.mobile`, and no iOS entitlements (the GPUI
client had none, and a free Apple ID — how this app is installed — cannot sign
a managed one). An installed GPUI build is upgraded in place; nothing asks for
the old app to be uninstalled first, and no second icon appears.

The data the old build left is imported on the first launch
(`src/migration.rs`): saved runtimes and their credentials, the install-wide
device identity, appearance, language and notification switches, and unsent
drafts. `nsh` keeps its state under `<support>/<app id>`, while the GPUI client
wrote at the top of the Android files directory and in `Application
Support/Vibex` on iOS, so the imports look in both places. They run only when
the new store has no value yet, they never delete or move the old files, and a
second launch therefore imports nothing.

`scripts/check-upgrade.mjs` pins all of that against the GPUI client's build
files — the application id, the `versionCode` derivation, the iOS bundle ids and
the legacy file names — so a change that would turn an update into a second app
fails `pnpm check:mobile-native` instead of on someone's phone.

The iOS host (`ios/`) is written but has never been built: this machine has no
Xcode, so what is checked here is the Rust iOS target (`cargo check` and
`clippy` for `aarch64-apple-ios`), the Swift sources' syntax, and the C ABI
between them (`tests/ios_host_abi.rs`). `scripts/build-ios-host.sh` builds the
static library for the device and the Apple-silicon simulator, merges it with
`lipo`, generates `Vibex.xcodeproj` from `project.yml` with XcodeGen, and
builds the app and its share extension. The first Mac run is expected to find
the remaining Swift/API mismatches; none of them can be filed from here.

## Tests

What is covered, and what a device still has to confirm, is in
[`VERIFICATION.md`](VERIFICATION.md).

```sh
cargo test -p vibex-mobile-nsh
```

Two layers:

- **Theme sync**: `theme.json` matches the token source, and `nsh`'s real theme
  parser reads every token (including translucent and custom ones).
- **Headless flows**: the root is mounted with `nsh`'s headless driver, with no
  simulator: the pairing page shows its three entries, the link sheet opens and
  an empty entry is refused with the copy that names what to paste, a bare
  code without an address is refused with the copy that says what is missing,
  and the nearby sheet opens. The session shell's flows are covered too: the
  drawer lists sessions, switching one swaps the transcript, a streaming
  Markdown reply renders, the composer appends what was sent and clears, a
  permission card is answered and leaves, a question collects a typed answer
  and leaves, the `/` and `@` palettes insert a command and a reference, New
  session opens through the picker and its sheet collects a workspace and a
  runtime, a session is renamed, archived and deleted from its drawer actions,
  a running turn is stopped from the composer, the terminal shows a program's
  output, the files surface lists a
  directory and reads a file, and the Git surface lists a change and shows its
  diff, stages a change, commits and switches to history. Each migrated screen
  adds the flows its GPUI pair has.

`cargo test --workspace` and `cargo clippy --workspace -- -D warnings` in CI
cover the new crates as they do the rest of the workspace.

## Embedding in the existing shells

The `nsh` CLI's generated runners do not carry Vibex's native integrations.
The app therefore mounts into the Kotlin and Swift shells that already own the
notifications, the foreground service, the camera and the discovery bridge.

**Android** (Kotlin; the symbol names match this class):

```kotlin
// The Java package is this crate's own (`namespace`); the application id is
// `ai.vibex.mobile`, the GPUI client's, so an install upgrades it in place.
// The library is `libnsh.so`, this crate's name, not the crate's package id.
package ai.vibex.mobile.nsh

object NshHost {
    init { System.loadLibrary("nsh") }
    external fun nativeRunRoot(handle: Long)
    external fun nativeScanResult(value: String)
    external fun nativeDiscoveryEvent(value: String)
    external fun nativeSetDeviceName(value: String)
}

// Once, before mounting: the runtime's paired-device list shows this name.
NshHost.nativeSetDeviceName(android.os.Build.MODEL)

val view = com.longbridge.nsh.NshView(this)
container.addView(view, LayoutParams.MATCH_PARENT)
val handle = view.javaClass.getDeclaredField("handle")
    .apply { isAccessible = true }
    .getLong(view)
NshHost.nativeRunRoot(handle)
```

The `handle` field is the `NshView`'s native handle that nsh documents for
custom Rust hosts; reading it is the only package-private access involved.

**iOS**: `ios/` holds the shipping shell, and its ABI is declared in
`ios/Headers/VibexMobile.h` (imported through
`ios/Vibex/Vibex-Bridging-Header.h`). A host of its own needs the same five
calls the Android one does:

```swift
vibex_mobile_nsh_install_notifications()       // before the app finishes launching
vibex_mobile_nsh_set_device_name(UIDevice.current.name)
let app = vibex_mobile_nsh_attach(container)
vibex_mobile_nsh_scan_result(payload)          // a scanned pairing link
vibex_mobile_nsh_discovery_event(json)         // one discovery event
vibex_mobile_nsh_detach(app)                   // when the shell goes away
```

`vibex_mobile_nsh_set_{scanner,camera,discovery}_available` say which
integrations the host carries, and `vibex_mobile_nsh_camera_photo` /
`_camera_failed` / `_share_text` / `_new_session` answer the rest. The whole
set is checked against the Rust exports and the Swift call sites by
`tests/ios_host_abi.rs`, so a renamed symbol fails `cargo test` on Linux
instead of at link time on a Mac.

The app asks the shell to act through a small polled queue: the Scan entry
queues "open the camera" and the Find sheet queues "start discovery". The
shell polls the queue on its own timer (the press rate makes a push callback
unnecessary across JNI):

```kotlin
// on the host's UI loop, every 200 ms or so
when (NshHost.nativeTakeHostRequest()) {
    1 -> openPairingScanner()
    2 -> startLanDiscovery()
    3 -> stopLanDiscovery()
}
```

```swift
switch vibex_mobile_nsh_take_host_request() {
case 1: openPairingScanner()
case 2: startLanDiscovery()
case 3: stopLanDiscovery()
default: break
}
```

The shells in `android/` and `ios/` are the reference implementations: each
mounts the app, reports the device name, and answers the queue with its own
camera and discovery. A host that supplies no camera leaves Scan on the photo
picker; the app does not require one.

## The iOS host

`ios/` is a small Swift app (`project.yml` generates `Vibex.xcodeproj` with
XcodeGen) around the same Rust library the Android host mounts:

| File | What it does |
| --- | --- |
| `Vibex/AppDelegate.swift` | The window, mounting the app, URLs, the quick action, foreground/background |
| `Vibex/HostBridge.swift` | Polls the request queue; answers scan, Find, camera, settings, screen-awake and background |
| `Vibex/PairingScannerViewController.swift` | The pairing scanner (AVFoundation), gallery decode (Vision) |
| `Vibex/LanDiscovery.swift` | Bonjour browsing of `_vibex._tcp`, emitting the discovery JSON contract |
| `Vibex/CameraCapture.swift` | The composer's camera (`UIImagePickerController`), writing a JPEG for the app |
| `Vibex/ShareIntake.swift` | Reads the text the share URL carried (`vibex://share?text=…`) |
| `VibexShare/ShareViewController.swift` | The "Share to Vibex" extension: opens that URL, pasteboard for long text |
| `Resources/Info.plist` | The `vibex` scheme, Bonjour, the camera/photo/local-network usage strings |

The share handoff is a URL rather than a shared App Group container. A free
Apple ID — the account the unsigned install path uses — cannot sign the App
Groups entitlement, and that entitlement is also the one capability the GPUI
client did without. The extension therefore puts the text in a `vibex://share`
URL (or on the pasteboard, with `?pasteboard=1`, when it is too long), which
keeps the share working on the same install path an upgrade lands on.

Notifications are nsh's: `vibex_mobile_nsh_install_notifications` makes the
platform's notification center the app's delegate before it launches, so a
press that starts the process is delivered, and the app's own settings decide
what shows.

iOS has no foreground service, so "stay connected in the background" is the
nearest equivalent: `HostBridge` holds a `beginBackgroundTask` assertion while
a runtime is connected, which keeps the process runnable for as long as iOS
allows (about 30 seconds). When the assertion expires the system suspends the
app: the connection stops, a turn still in flight cannot land, and nothing
notifies until the person opens the app again. That is a real difference from
the always-on service Android gets, and it is the reason the iOS build should
finish a turn before it goes away rather than rely on the background.

## Theme

`theme.json` is generated; do not hand-edit it. After a token change in
`crates/vibex-ui/theme/tokens.json`:

```sh
cargo run -p vibex-mobile-nsh --bin gen-theme
```

`cargo test -p vibex-mobile-nsh` fails when the checked-in file is stale.

The phone's spacing, radius and type ladder live in `crates/vibex-nsh-ui/src/theme.rs`
and come from `apps/mobile/src/theme.rs`. The palette is shared with the
desktop through the token source.

## Parity contract

The migration keeps the phone's product decisions while the platform owns
behavior:

- **Kept 1:1** — information architecture, page structure, copy, palette,
  spacing rhythm, icons, states and what every action does.
- **The platform owns** — text metrics and line breaking, scroll physics, the
  keyboard, selection menus, system sheets and navigation transitions,
  accessibility. These will look native, not pixel-identical; that is the
  point of the move.
- **Reimplemented** — sheet drag feel, toasts and motion curves keep their
  rhythm, not their exact frames.

A feature is migrated only when every entry point it has today is reachable and
does the same thing. Screens are compared against screenshots and recordings of
the shipping GPUI client, taken before the screen moves.

## Next

The [verification checklist](VERIFICATION.md) separates tested interactions
from device and live-runtime checks. The old GPUI client remains the visual
reference for the remaining screen-level differences.

# Desktop Application Updates

Vibex desktop discovers signed releases from GitHub, downloads a package for
the current installation source, verifies it, and delegates installation to the
appropriate platform mechanism. `DesktopRuntime` owns update state; GPUI is a
subscriber and command surface.

## Scope And Ownership

- `crates/app-update` owns release discovery, manifest trust, artifact download,
  verification, installation dispatch, retry timing, and the state machine.
- `crates/desktop-runtime` owns the one production `AppUpdateService` and its
  automatic-check task.
- `apps/desktop` subscribes to snapshots and requests check, download, install,
  and restart actions. It does not fetch or verify release data itself.
- Mobile clients and Relay never modify the desktop installation. Store-managed
  mobile updates remain platform-owned.

## Signed Release Contract

Each supported GitHub Release publishes these assets together:

```text
vibex-<version>-<os>-<arch>-<package>.<extension>
vibex-update.json
vibex-update.json.sig
```

The Ed25519 signature covers the exact `vibex-update.json` bytes. Release CI
holds `VIBEX_UPDATE_SIGNING_KEY`; desktop release builds contain only
`VIBEX_UPDATE_PUBLIC_KEY`. The manifest generator must verify its output and
confirm that the signing key derives the public key embedded by the build.

The signed manifest binds:

- schema and minimum updater version;
- channel, SemVer version, tag, publication time, and release-notes URL;
- artifact OS, architecture, package type, install mode, exact GitHub Release
  URL, byte length, SHA-256, and optional SHA-512.

Before JSON parsing can establish trusted fields, verify the signature over the
bounded raw response. Reject an invalid signature, unsupported schema, channel
mismatch, inconsistent `v<version>` tag, downgrade/equal version, invalid digest,
or URL outside the exact `vibex-ai/vibex` release/tag boundary.

Stable builds accept versions without prerelease identifiers. RC and Preview
builds accept only `rc.*` and `preview.*` versions respectively. A channel never
falls through to another channel.

## Discovery And Download

- Read the official GitHub Releases Atom feed and select the highest SemVer in
  the compiled channel.
- Reuse `ETag` and `Last-Modified`; bound feed, manifest, and signature bodies.
- Permit HTTPS redirects only within the reviewed GitHub asset hosts and cap
  redirect depth.
- Match artifacts by normalized OS, architecture, and detected package source.
- Stream into a new `.part` file below the runtime update directory. Never write
  into a project, settings, database, or credential directory.
- Bound the stream by the signed size, sync it, then independently re-read and
  verify size plus every signed hash before renaming it to a staged package.
- Coalesce identical operations. Progress snapshots are throttled so network
  chunk size cannot flood the UI subscriber.

## State And Scheduling

Snapshots contain a monotonically increasing `seq` and one of:

```text
Idle -> Checking -> Available -> Downloading -> Verifying -> Staged
     -> Installing -> RestartRequired
Unsupported
Error
```

The UI subscribes before reading the current watch value and rejects snapshots
older than its current `seq`. Automatic checks do not replace a downloading,
verifying, staged, installing, or restart-required state.

The first automatic check waits 30 seconds. Stable checks run about every six
hours; RC and Preview checks run about every two hours. Successful intervals use
small jitter. Retryable automatic failures back off through 15 minutes, 30
minutes, one hour, then six hours. Automatic failures remain in diagnostic state
without user interruption; manual failures publish a typed visible error.

## Installation And Recovery

- AppImage updates copy the verified artifact beside the current AppImage,
  preserve permissions, sync it, retain one fixed-name backup, write a bounded
  recovery marker, then atomically rename the replacement over the target.
- Marker paths must exactly match paths derived from the detected installation;
  marker content cannot redirect cleanup to another file.
- A new version's first startup removes its backup and marker. If startup finds
  an incomplete pre-replacement marker and a missing target, it restores the
  backup before continuing.
- Debian/RPM packages and native macOS/Windows installers are launched through
  the visible system installer path after verification. The updater never
  silently writes privileged system locations.
- Store, Flatpak, and other externally managed sources remain `Unsupported` for
  self-install and present the verified release page or package-manager path.
- Restart helpers wait for the current process to exit before launching the
  replacement. User data is never rolled back with application binaries.

## Desktop Presentation

- About shows current version/channel and every updater state with its valid
  next action.
- An available or unsupported release can show a compact title-bar entry before
  mobile pairing. The entry and one-time notice obey `show_update_prompts`.
- Record `last_update_prompted_version` only when the notice is actually shown.
  Disabling prompts does not disable background checks or the About controls.
- Persist both fields only in bounded desktop UI state; they are not Relay data.

## Error And Logging Rules

Updater errors use stable `app_update_*` codes, bounded user-facing messages,
and an explicit retryable flag. Never expose request headers, signing material,
local paths, or raw HTTP errors. Recovery cleanup failures are structured
warnings and do not prevent desktop startup.

## Required Verification

```text
cargo test -p vibex-app-update --locked
cargo test -p vibex-desktop-model --locked
cargo check -p vibex-desktop --locked
cargo clippy -p vibex-app-update --all-targets --locked -- -D warnings
```

Tests cover signature tampering, channel and URL rejection, feed selection,
download hash rejection, monotonic state, staged-state preservation, AppImage
backup cleanup/recovery, and marker path validation. Release workflow changes
also run `pnpm check:release` and must declare only artifacts the workflow
actually builds.

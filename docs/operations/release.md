# Desktop Release Operations

Desktop owns the stable native application identity. Release and rollback use
published, exact-hash artifacts built from the current GPUI source tree.

## Channels And Homes

| Channel | App id | Home | Ownership |
| --- | --- | --- | --- |
| Preview | `dev.vibex.desktop.preview` | `desktop-preview` | Isolated local preview |
| RC | `dev.vibex.desktop.rc` | `desktop-rc` | Explicit opt-in candidate |
| Stable | `dev.vibex.desktop` | `desktop-stable` | Stable native product |

Every channel acquires `.vibex-runtime.lock` before opening SQLite, Agent
processes, or PTYs. Preview, RC, and Stable must not share a live home.

Channel identity is compiled into the release binary. Use the repository
wrappers so the binary, application id, package metadata, and output directory
stay aligned:

```bash
pnpm package:preview
pnpm package:rc
pnpm package:stable
```

Linux packages contain the GPUI desktop binary and reviewed PDFium runtime.
They do not contain or host the mobile GPUI-WASM runtime. Outputs are written below
`target/release-packages/<channel>/`.

## Data Safety

The desktop runtime owns `desktop-ui-state.json` and preserves:

- versioned schema decoding and deterministic migration;
- atomic private-file replacement and parent-directory synchronization;
- read-only startup inspection before the runtime lock is acquired;
- corrupt-file quarantine with bounded backup retention;
- explicit UI-state backup metadata for release rollback.

SQLite migration, business-data backup/restore, diagnostics, device grants,
Agent sessions, terminals, and Relay trust remain owned by their Rust services.
A UI-state failure must not overwrite those stores.

## Preflight

Run deterministic checks before producing a candidate:

```bash
pnpm check:release
pnpm smoke:backup
pnpm smoke:diagnostics
pnpm e2e:regression
pnpm release:build-smoke
```

`pnpm release:preflight` runs the release checks and writes the bounded report
to `docs/release/release-preflight.json`. Generate that report only for the
exact committed candidate it describes.

## Candidate Evidence

A native release claim needs platform-host evidence for every claimed target.
For Linux, record at least:

1. Exact source commit and `Cargo.lock` SHA-256.
2. Preview, RC, and Stable package SHA-256 values.
3. Clean install, upgrade, uninstall, and retained-data results.
4. X11 and Wayland launch, accessibility, process-tree, and input results.
5. Redacted diagnostics and package privacy scan results.
6. Rollback to the previously published exact-hash desktop artifact.

macOS and Windows require their own build, package, signing, install, native
behavior, and rollback evidence. Linux results do not establish support on
another platform.

## Rollback

1. Stop the failing GPUI process and verify the home lock is released.
2. Preserve the current UI-state file, SQLite backup manifest, diagnostics, and
   package identity before mutation.
3. Restore the prior published desktop artifact by its recorded SHA-256.
4. Restore data only when the compatibility matrix requires it; do not replace a
   newer compatible database merely to change the UI artifact.
5. Verify schema startup, sessions, terminals, device grants, Direct access, and
   user-self-hosted Relay connectivity with provider-free smokes.
6. Record the trigger, artifact hashes, backup identity, bounded diagnostics,
   and final active version.

Source history is not an operational rollback mechanism. A release rollback
uses a previously published artifact and compatible data backup.

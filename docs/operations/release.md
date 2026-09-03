# Release Runbook

## Scope

Vibex release work covers the native desktop package, optional native Android/iOS
clients, and the transport-only Relay service. Desktop remains the authority for
all Agent and workspace state.

## Preflight

```bash
pnpm install --frozen-lockfile
pnpm check
pnpm check:mobile-native
pnpm check:release
pnpm check:licenses
git diff --check
```

Confirm that the public repository contains only source-owned changes and that
private Trellis task paths are not staged. Do not publish a package from a dirty
or unverified source identity.

## Desktop

The tag-triggered GitHub Actions release uses standard GitHub-hosted runners and
keeps one native job per desktop platform. Linux produces the reviewed PDFium
backed `.deb` and AppImage; macOS produces a `.dmg`; Windows produces an NSIS
installer. Each job normalizes its filenames, writes SHA-256 sidecars, and
uploads an immutable artifact. A final Ubuntu job collects the matrix before
creating the GitHub Release, so a partial platform build can never be published.

The local equivalent for a single desktop target is:

```bash
pnpm prepare:pdfium --offline
pnpm package:preview
```

The release job uses `node scripts/package-desktop-release.mjs`, which keeps the
Linux-only PDFium distribution approval separate from the macOS and Windows
packages. Those platforms are build and package evidence until their native
runtime review is completed.

RC and Stable builds additionally require the signing, rollback, and operator
approvals recorded by the release owner.

Every published artifact remains immutable and addressable by its version tag.
Rollback publishes or re-selects a previously verified release artifact; it
never rewrites an existing tag, reuses an asset name with different bytes, or
rolls user data back with the application package. For AppImage self-updates,
retain the previous binary until the new version completes its first startup.

## Mobile

Android and iOS are built independently from the desktop package. Tagged
releases publish unsigned Android APK/AAB files plus an unsigned iOS simulator
app and XCFramework as GitHub Release artifacts. They are useful for source-bound
testing and store handoff; signing, provisioning, device validation, and store
upload remain explicit follow-up steps.

```bash
pnpm package:mobile:android
pnpm build:mobile:ios
```

The Android release job installs the checked-in Gradle wrapper, Android API 35,
and a pinned NDK on the free Ubuntu runner. The iOS job runs on a free
GitHub-hosted macOS runner, installs XcodeGen, builds both Apple Rust targets,
and performs a code-signing-disabled simulator build. No signing secret is
required for the default open-source pipeline.

Updater manifest signing is optional. Set the repository variable
`VIBEX_UPDATE_SIGNING_ENABLED=true`, `VIBEX_UPDATE_PUBLIC_KEY`, and the
`VIBEX_UPDATE_SIGNING_KEY` secret to add the signed desktop updater manifest;
when these values are absent, the immutable release assets are still published.
The Linux package carries the currently approved PDFium runtime; macOS and
Windows remain build/package evidence until their target runtime review passes.

Before a release claim, validate the exact generated artifact on the intended
device class. Exercise pairing, Direct/Tailnet/Relay route selection, reconnect,
timeline catch-up, approval resolution, send/stop/continue, and credential
redaction. Keep signing material, provisioning profiles, and device identifiers
out of the repository and evidence logs.

## Relay

Relay deployment remains zero-knowledge and transport-only. Run the local smoke,
then validate the operator's TLS, reverse proxy, NAT, room limits, and health
endpoints against the same source and Cargo lockfile.

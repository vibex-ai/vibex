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

Prepare PDFium, build the selected channel, and run the package-specific native
content and installation checks:

```bash
pnpm prepare:pdfium --offline
pnpm package:preview
```

RC and Stable builds additionally require the signing, rollback, and operator
approvals recorded by the release owner.

Every published artifact remains immutable and addressable by its version tag.
Rollback publishes or re-selects a previously verified release artifact; it
never rewrites an existing tag, reuses an asset name with different bytes, or
rolls user data back with the application package. For AppImage self-updates,
retain the previous binary until the new version completes its first startup.

## Mobile

Android and iOS are built independently from the desktop package:

```bash
pnpm package:mobile:android
pnpm build:mobile:ios
```

Before a release claim, validate the exact generated artifact on the intended
device class. Exercise pairing, Direct/Tailnet/Relay route selection, reconnect,
timeline catch-up, approval resolution, send/stop/continue, and credential
redaction. Keep signing material, provisioning profiles, and device identifiers
out of the repository and evidence logs.

## Relay

Relay deployment remains zero-knowledge and transport-only. Run the local smoke,
then validate the operator's TLS, reverse proxy, NAT, room limits, and health
endpoints against the same source and Cargo lockfile.

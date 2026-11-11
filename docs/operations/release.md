# Release Runbook

## Scope

Vibex release work covers the native desktop package, optional native Android/iOS
clients, the transport-only Relay service, and the two published container
images (`vibex-server`, `vibex-relay`). Desktop remains the authority for
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
keeps one native job per desktop platform and architecture. Linux x86_64
produces the reviewed PDFium backed `.deb` and AppImage; Linux aarch64 produces
a `.deb` cross-built on the x86_64 runner; macOS produces `.dmg` images for
aarch64 and x86_64 on the arm64 `macos-15` image; Windows produces NSIS
installers for x86_64 and aarch64.
Each job normalizes its filenames, writes SHA-256 sidecars, and uploads an
immutable artifact. A final Ubuntu job collects the matrix before creating the
GitHub Release, so a partial platform build can never be published.

Every macOS job pins `macos-15`: GitHub retired all macOS 14 runner images on
2026-11-02, and a workflow that still asks for one is rejected while the job is
being provisioned with "The configuration 'macos-14-arm64-us-default' is not
supported" before any step runs. Both macOS architectures stay on the arm64
image; the x86_64 packages are cross-built there.

AppImage packaging stays x86_64-only: the AppImage tooling (linuxdeploy) cannot
process cross-architecture binaries, so an aarch64 AppImage would require a
qemu binfmt compatibility layer that is not acceptable for release artifacts.

The PDFium distribution approval covers linux-x86_64 only. The Linux aarch64
deb and the macOS and Windows packages are built and packaged without the
embedded PDFium runtime until their native runtime review completes.

Each macOS app bundle is ad-hoc signed before it is packed into the dmg, and the
job reads the signature back from the dmg before uploading it; see
[macOS Bundle Signing Rules](./release-packaging-matrix.md#macos-bundle-signing-rules).
The packages are not notarized, so Gatekeeper still asks the user to allow the
first launch.

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

### Release notes

The tagged commit must contain `docs/operations/release-notes-v<version>.md`
before the release is published. The publish job stages that file as
`vibex-release-notes.md`, fails the release when it is missing or has no
`## English` section, and uploads it beside `vibex-update.json`. The desktop
About page downloads the asset from the verified release tag and shows the
section that matches the selected interface language, so the document is what
users read as the changelog.

Write one level-two section per language, using the headings the desktop
resolves:

```markdown
## English

## 简体中文

## 繁體中文
```

English is required; `## 中文` remains accepted as the generic Chinese section.
Missing translations fall back to the generic section and then to English, so a
release may ship fewer sections than the interface offers. The notes asset is
informational and is not covered by the manifest signature: never move install
instructions, artifact hashes, or anything the updater must trust into it.

### Release page

The GitHub Release page is composed by the publish job from three inputs, in
this order:

1. **English**, then the Chinese section when the document carries one — the
   tagged notes, unchanged.
2. **Downloads** — an English table of the desktop client and headless server
   package for every platform, plus the Android and iOS packages. Each row is
   rendered from the assets the run actually publishes, so a platform whose job
   failed drops out instead of leaving a dead link, and a missing server tarball
   renders as `—`.
3. **New Contributors** and **Full Changelog** — English only. The job asks
   GitHub to generate the release notes over the range and reuses only the
   contributor credit, which names each new contributor's first pull request; a
   range that carries only direct commits generates none, so the composer
   credits the author of a first commit between the previous tag and this one.

The page deliberately carries no per-change list. A release spans several
hundred commits, so a `What's Changed` list is longer than everything a reader
came for, and the notes above already say what changed in the terms a reader
cares about. The composer drops that section even when GitHub generates it, and
the publish job fails if it ever reaches the body.

`scripts/release-body.mjs` builds the body and `scripts/release-body.test.mjs`
pins its rules. A local draft needs the tagged notes and, optionally, the asset
directory and a generated changelog:

```bash
gh api "repos/vibex-ai/vibex/releases/generate-notes" \
  -f tag_name=v0.1.1 -f previous_tag_name=v0.1.0 --jq .body \
  > target/release-body/generated-changelog.md

node scripts/release-body.mjs \
  --version 0.1.1 \
  --notes docs/operations/release-notes-v0.1.1.md \
  --assets target/publish \
  --changelog target/release-body/generated-changelog.md \
  --output target/release-body/vibex-release-body.md
```

Without `--assets` the Downloads table is omitted, and an empty or missing
`--changelog` selects the commit-log fallback for the contributor credit. The
publish job reads the file through `gh release create --notes-file`, so the page
and the notes asset always describe the same release.

The Downloads table is a convenience for readers. The updater never reads it:
artifact trust stays with the signed `vibex-update.json`, and every link is
built from the verified tag.

## Mobile

Android and iOS are built independently from the desktop package. Tagged
releases publish signed Android APK/AAB files plus an unsigned iOS simulator
app, an unsigned iOS device IPA, and the XCFramework as GitHub Release
artifacts. Android signing credentials stay in GitHub Actions secrets and are
never checked into the repository.

```bash
pnpm package:mobile:android
pnpm build:mobile:ios
pnpm package:mobile:ios
```

The Android release job installs the checked-in Gradle wrapper, Android API 35,
and a pinned NDK on the free Ubuntu runner, then aligns and verifies the APK
with `apksigner` and signs the AAB with `jarsigner`. RC and preview releases
fall back to an ephemeral CI key when no repository key is configured; stable
releases fail until a release keystore is configured. The iOS job runs on a free
GitHub-hosted macOS runner, installs XcodeGen, builds both Apple Rust targets,
performs a code-signing-disabled simulator build, and packages the unsigned
device IPA. No Apple account, certificate, or provisioning profile is involved
anywhere in that job: the installer re-signs the IPA with their own Apple ID.
`.github/workflows/mobile-ios-unsigned.yml` builds the same IPA on demand for a
commit that is not a release tag. See
[Installing the Unsigned iOS Build](./ios-unsigned-install.md).

For stable Android releases, configure the repository secret
`VIBEX_ANDROID_KEYSTORE_BASE64` together with
`VIBEX_ANDROID_KEYSTORE_PASSWORD`; set `VIBEX_ANDROID_KEY_PASSWORD` when the key
password differs, and set the `VIBEX_ANDROID_KEY_ALIAS` repository variable when
the alias is not `vibex`. Keep one release key for every channel: Android only
replaces an installed app when the new APK carries the same signing key, so an
rc installed under the ephemeral CI key cannot be updated in place by a stable
release.

### Android updates

The Android client has no application store. It installs the signed APK from
the GitHub Release, and the release manifest carries a matching
`android | arm64 | apk | system_installer` artifact next to the desktop
packages. Builds that should check for updates need `VIBEX_UPDATE_PUBLIC_KEY`
in the Android build environment (the release workflow passes the repository
variable of the same name), exactly like the desktop updater.

- Only the stable channel is offered. A prerelease install still learns about
  the stable release it leads up to; this client never follows rc or preview
  tags.
- The app verifies the manifest signature and the APK SHA-256 before it hands
  the staged file to the system package installer through its FileProvider.
  Android then enforces signing-key continuity.
- Users can either download and install in the app or open the signed download
  in a browser; both start from the same verified release metadata.
- `REQUEST_INSTALL_PACKAGES` and the `${applicationId}.updates` provider are
  the only new platform surface. Because the app is not distributed through
  Google Play, the sideload flow carries no store policy constraint; a Play
  build would have to use Play In-App Updates instead.
- A build without the verification key still shows the version and the update
  row, but states that update checks are not enabled and offers no action.
  Export the key when building locally to exercise the real flow:

  ```bash
  VIBEX_MOBILE_PROFILE=release VIBEX_UPDATE_PUBLIC_KEY=<repository variable> \
    bash apps/mobile/scripts/build-android.sh
  ```
- Settings reach the update section from the drawer footer and from the
  top-right entry on the first pairing screen, so an update can be installed
  before the first desktop is paired.

Updater manifest signing is optional. Set the repository variable
`VIBEX_UPDATE_SIGNING_ENABLED=true`, `VIBEX_UPDATE_PUBLIC_KEY`, and the
`VIBEX_UPDATE_SIGNING_KEY` secret to add the signed desktop updater manifest;
when these values are absent, the immutable release assets are still published.
The manifest enumerates one artifact per platform, package format, and
architecture found in the release, and the updater client resolves its own
architecture (`arm64` or `x86_64`) against it. The Linux x86_64 package carries
the currently approved PDFium runtime; every other packaging target remains
build/package evidence until its target runtime review passes.

The same signed manifest carries optional `server_artifacts` entries for the
headless runtime (`linux/x86_64`, `linux/arm64`). They are built in the pinned
Debian 12 image, checked against the glibc 2.35 floor, and published as
`vibex-server-<version>-linux-<arch>.tar.gz` with a `.sha256`. Older clients
ignore the field; the desktop selects a server artifact by OS and architecture
and verifies its digest before uploading it over SSH.

Before a release claim, validate the exact generated artifact on the intended
device class. Exercise pairing, Direct/Tailnet/Relay route selection, reconnect,
timeline catch-up, approval resolution, send/stop/continue, and credential
redaction. Keep signing material, provisioning profiles, and device identifiers
out of the repository and evidence logs.

## Relay

Relay deployment remains zero-knowledge and transport-only. Run the local smoke,
then validate the operator's TLS, reverse proxy, NAT, room limits, and health
endpoints against the same source and Cargo lockfile.

## Container images

A tag push publishes `ghcr.io/vibex-ai/vibex-server` and
`ghcr.io/vibex-ai/vibex-relay` from
`.github/workflows/publish-container-images.yml`, independently of the desktop
and mobile jobs: a packaging failure in one product must not withhold the other.
The workflow verifies the tag against both container binaries, builds
`linux/amd64` and `linux/arm64` natively, merges the digests into one manifest
list, and boots each published image once before the run succeeds.

- Release candidates publish `0.1.1-rc.x`, `v0.1.1-rc.x`, and the moving `rc`
  tag; stable releases additionally move `latest`. The default branch publishes
  `edge` and `sha-<commit>`.
- Published version tags are immutable. Rollback pulls a previously verified
  version tag; it never retags an existing version.
- Both registry packages must be Public once, or operators have to authenticate
  before an anonymous `docker pull` works. The workflow warns when a package is
  still private.
- A release is not complete until both images answer their health endpoint from
  the published registry tag, not only from a local build.

### Registry mirrors

A stable release mirrors both manifest lists to Docker Hub and to the CNB
artifact registry, in the same workflow, after the GHCR publish job has
succeeded. Each mirror copies the index GHCR already published with
`docker buildx imagetools create`, so every registry serves the same digest CI
verified instead of a second build with its own provenance.

| Registry | Image | Why |
| --- | --- | --- |
| GHCR | `ghcr.io/vibex-ai/vibex-{server,relay}` | Canonical release |
| Docker Hub | `vibexai/vibex-{server,relay}` | Operators who already pull from there |
| CNB | `docker.cnb.cool/vibex-ai/vibex/vibex-{server,relay}` | Reachable where GHCR is slow or blocked |

Shared rules for both mirrors:

- Stable only. The `dockerhub` and `cnb` jobs are skipped for any tag containing
  `-`, because an operator pulling from a mirror is choosing an image for a
  deployment they keep, and an `rc` tag there reads as supported. GHCR still
  carries every channel.
- Both jobs are `continue-on-error`: a mirror is a convenience channel, so a
  registry outage or a rejected token must never withhold the images operators
  already pull from GHCR.
- Tags are `0.1.1`, `v0.1.1`, and `latest` for a stable release.
- `scripts/mirror-registry.mjs` verifies the source really is a
  two-architecture index before writing and rechecks both architectures on
  every mirrored tag.

Docker Hub needs two repository secrets: `DOCKERHUB_USERNAME` (the account that
owns the token) and `DOCKERHUB_TOKEN` (a personal access token with **Read &
Write** repository permission). The optional repository variable
`DOCKERHUB_NAMESPACE` defaults to `vibexai`. The namespace is separate from the
token identity so an organization keeps a stable namespace across maintainer
changes; a free personal account can only push under its own Docker ID, which
cannot contain a hyphen.

CNB needs one repository secret, `CNB_TOKEN`, an access token whose
`registry-package` scope allows writes. CNB always accepts the literal `cnb`
username, so the token is the password. The optional repository variable
`CNB_NAMESPACE` defaults to `vibex-ai/vibex`: CNB addresses an artifact as
`<repository>/<image>`, so the repository path is the namespace and the image
keeps its published name. CNB repositories start private and inherit their
visibility from the repository, so the CNB project must be **Public** before an
anonymous `docker pull` works.

A local dry run prints the plan without touching any registry:

```bash
# Docker Hub
node scripts/mirror-registry.mjs --version 0.1.1 --namespace vibexai --dry-run

# CNB
node scripts/mirror-registry.mjs \
  --version 0.1.1 --target-registry docker.cnb.cool \
  --namespace vibex-ai/vibex --dry-run
```


## AUR

`.github/workflows/publish-aur.yml` publishes the `vibex-bin` AUR package from a
stable GitHub Release, independently of `release.yml`: an AUR outage must not
withhold the release, and a single older tag can be replayed with
`workflow_dispatch`.

The AUR stores packaging sources rather than binaries, so the job reuses the
Linux `.deb` files the Release already carries:

1. It refuses anything that is not a stable `x.y.z` tag. A GitHub `release`
   event also fires for prereleases, and AUR carries exactly one version, so an
   rc would move every Arch user backwards.
2. `scripts/publish-aur.mjs` writes `pkgver`, records the SHA-256 of both
   architecture payloads and the license, and regenerates `.SRCINFO`.
3. `makepkg` rebuilds the package from those exact bytes against the recorded
   checksums, so a payload that drifted from the Release fails in CI.
4. `namcap` inspects the package and the PKGBUILD, then the script clones
   `ssh://aur@aur.archlinux.org/vibex-bin.git`, commits `PKGBUILD` and
   `.SRCINFO`, and pushes to `master`.

The job runs in the rolling `archlinux:base-devel` container, so its first step
is a full `pacman -Syu` together with the tooling install. A bare `pacman -Sy`
refreshes only the databases and leaves the image's libraries behind: pacman
then resolves a newer `openssl` against `curl`, `pacman`, `sudo` and
`libarchive` that still depend on `libcrypto.so=3-64`, and aborts the whole
transaction with "could not satisfy dependencies" before any packaging step
runs. `scripts/check-release.mjs` keeps the full upgrade under contract.

The package installs the GUI plus the `vibex` terminal client and renders its
own desktop entry: the packaged entry keeps `NoDisplay=true`, so it stays hidden
from application menus and the AUR package supplies the visible one.

Local dry run before a release:

```bash
node scripts/publish-aur.mjs --version 0.2.0 --dry-run
```

Configure the repository secret `AUR_SSH_PRIVATE_KEY` with the private key whose
public half is registered on the AUR account that owns `vibex-bin`. The workflow
pins the AUR host keys instead of scanning them.

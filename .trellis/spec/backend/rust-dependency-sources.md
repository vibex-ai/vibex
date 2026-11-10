# Rust Dependency Source Guidelines

Vibex consumes third-party Rust projects from their upstream Cargo sources. The
repository does not maintain copied, patched, or shimmed third-party source trees
beyond two reviewed exceptions: the small first-party `gpui-tokio` bridge and the
vendored Android IME host.

## Scenario: Published gpui-pre And gpui-kit With A Git-Pinned gpui-pre-mobile

### 1. Scope / Trigger

- Trigger: changing `Cargo.lock`, bumping the `gpui-pre` family, bumping the
  gpui-kit family, bumping `gpui-pre-mobile`, or changing a third-party
  license decision.
- GPUI arrives from three sources: the published `gpui-pre` family on crates.io
  (zed snapshots), the published gpui-kit crates on crates.io, and
  `gpui-pre-mobile` pinned to a Git revision for the Android/iOS platform layer.
  Glue with no published equivalent lives in this workspace.

### 2. Signatures

```toml
# Cargo.toml
[workspace.dependencies]
gpui = { package = "gpui-pre", version = "=0.3.11" }
gpui_platform = { package = "gpui-pre-platform", version = "=0.3.11", features = ["font-kit", "runtime_shaders", "wayland", "x11"] }
gpui_tokio = { package = "gpui-tokio", path = "crates/gpui-tokio" }
gpui-component = "0.9.0"
gpui-fps = "0.9.0"
gpui-kit-assets = "0.9.0"
gpui-base = "0.9.0"
```

```toml
# apps/mobile/Cargo.toml
[target.'cfg(any(target_os = "android", target_os = "ios"))'.dependencies]
gpui-mobile = { package = "gpui-pre-mobile", git = "https://github.com/<reviewed-gpui-mobile-remote>", rev = "<pinned-gpui-mobile-rev>" }
```

```text
crates/gpui-tokio/                    first-party copy of zed's gpui_tokio (no published crate)
apps/mobile/src/platform.rs           the only place that builds the mobile Platform
apps/mobile/android/app/src/main/java/dev/gpui/mobile/GpuiInputActivity.java
                                      vendored IME host; package and class names are the JNI contract
Cargo.lock                            one Vibex workspace lockfile
cargo metadata --locked --format-version 1
pnpm check:licenses                   SPDX, asset, SBOM, and notice gate
pnpm check:rust                       locked fmt/check/clippy/test gate
pnpm check:mobile-native              native mobile crate and project contract
```

### 3. Contracts

- The `gpui-pre` family resolves from crates.io at the exact `=0.3.11` pins. Do not
  reintroduce a `[patch.crates-io]` block that redirects it to a local fork, and do
  not rename a fork to satisfy the version constraint.
- `gpui-pre-mobile` resolves from its pinned Git revision; the pin moves only with
  a reviewed dependency-source change, never as a floating branch.
- gpui-kit 0.9.0 pins `gpui-pre =0.3.11`, and Cargo cannot hold two exact pins in
  the same `0.3` range, so the mobile platform layer must accept 0.3.11 or the
  workspace does not resolve at all. Upstream `longbridge/gpui-mobile` still pins
  `=0.3.10` at its latest revision `7548ebc`, so the pin names the reviewed
  `PeatBoy/gpui-mobile` revision `f6e8b37`: upstream `7548ebc` plus the 0.3.11
  version bump, the re-vendored 0.3.11 `gpui-pre-wgpu` with the headless/offscreen
  patch re-applied, and the `WgpuContext::new` call sites the 0.3.11 renderer API
  moved (adapter selection now tests the surface configuration the renderer will
  use). That branch is proposed upstream as `longbridge/gpui-mobile#46`. The
  revision is a commit hash, so its contents are fixed by the pin; when the pull
  request lands, the pin returns to upstream at the merge commit and this
  paragraph names that revision instead. Keep `scripts/check-mobile-native.mjs`
  carrying whichever revision is pinned.
- `crates/gpui-tokio` is the only first-party copy of upstream Rust code. It is a
  verbatim copy of zed's Apache-2.0 `gpui_tokio` with re-pointed dependency
  coordinates, kept because no `gpui-pre-tokio` package exists. Keep it verbatim;
  changes belong upstream.
- The vendored Android IME host is the only copied third-party source tree. Its
  Java package (`dev.gpui.mobile`) and class names are referenced by
  `#[no_mangle]` JNI exports, so renaming or relocating it breaks the keyboard.
- The gpui-kit family resolves from crates.io on caret requirements, so a patch
  release is picked up by `cargo update` within the reviewed lockfile. Move a
  pin only with a reviewed dependency-source change; do not vendor or patch the
  family locally. `gpui-base` is a direct dependency because `Plot::hover`
  implementations sample its motion layer (`Spring`, `spring`), which
  `gpui-component` does not re-export; it must resolve to the same version the
  component crate uses, or their shared types would not unify.
- Reproducibility is the combination of the crates.io pins, the one Git revision
  pin, and the root `Cargo.lock`. A bump reviews and commits the lockfile and the
  regenerated license outputs together.
- A gpui-kit bump is not complete until the kit surfaces Vibex bridges are
  re-verified: `crates/vibex-ui`'s `component_palette` test fails when the kit
  adds or removes a color token the bridge does not map, and
  `scripts/check-mobile-native.mjs` carries the reviewed `gpui-pre-mobile`
  revision, so both move in the same change as the manifest.
- A `gpui-pre` bump is not complete until the two hand-written platform adapters
  compile against the new snapshot, because nothing on the default host target
  reaches them:
  - `apps/desktop/src/platform/bounded_text.rs` implements `Platform` by
    delegation, so every method the snapshot adds must be forwarded there. The
    0.3.9 bump added `on_displays_changed`; 0.3.11 added `platform_info` (which
    keyboard conventions resolve from), `power_mode` and `on_power_mode_change`.
  - `apps/mobile/src/android_host.rs` is `cfg(target_os = "android")`, and the
    Android build needs an NDK the quality gates do not require. 0.3.9 changed
    `gpui-pre-mobile`'s Android host entry from a process-global surface
    (`surface_created(window, scale)`, `surface_destroyed()`, `resumed()`,
    `paused()`) to one keyed by a `HostId`, and split window construction out
    into `on_open_window`, which must be registered before the first surface
    arrives. `apps/mobile/Cargo.toml` carries the reviewed revision and
    `scripts/check-mobile-native.mjs` asserts the registration.
- Moving the `gpui-pre-mobile` pin can move crates outside the GPUI family: the
  platform layer pins `libc = "=0.2.189"` for the iOS targets, and because Cargo
  cannot hold two exact requirements inside one compatible range, that exact pin
  reaches the whole graph. The 0.3.9 move left `libc` where the lock already had
  it, but a package that needs a newer `libc` fails to resolve rather than
  silently diverging. Review the whole lockfile diff, not just the `gpui-*` rows.
- No other tracked `vendor/` tree, Git submodule, local third-party path patch,
  compatibility shim, or copied upstream source is approved.
- Use crates.io packages unmodified unless the user explicitly approves a new
  source policy. Future-incompatibility warnings require an exact package/version
  allowlist with owner and removal condition; they do not justify another fork.
- Vibex package metadata uses `AGPL-3.0-or-later`. Approved dependency licenses may
  include `GPL-3.0-or-later`, but dependency license metadata is never rewritten as
  Vibex's license.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| A `[patch.crates-io]` entry redirects `gpui-pre*` to a path | Reject the change: the tree must not carry a renamed GPUI fork. |
| `cargo metadata` resolves two `gpui-pre` versions | Reject: every GPUI consumer must share one version. |
| `gpui-base` resolves to a different version than `gpui-component` | Reject: the motion types a `Plot::hover` implementation passes would not unify with the kit's. |
| `apps/mobile` resolves a `vendor/zed` path dependency | `pnpm check:mobile-native` fails. |
| The `gpui-pre-mobile` revision in `apps/mobile/Cargo.toml` moves without `scripts/check-mobile-native.mjs` | `pnpm check:mobile-native` fails; the reviewed revision and the checker move together. |
| The `gpui-pre-mobile` pin names a revision that does not accept the pinned `gpui-pre` | `cargo metadata` fails to resolve the graph; review the pin before changing anything else. |
| A `gpui-pre` snapshot adds a `Platform` or `PlatformWindow` method | The delegating adapter in `apps/desktop/src/platform/bounded_text.rs` fails to compile on the host target; forward the method there. |
| A `gpui-pre-mobile` bump changes the Android host entry | `pnpm check:rust` still passes, because `android_host.rs` is `cfg(target_os = "android")` and nothing on the host target compiles it. Type-check it against the Android target before committing; see the Tests Required section. |
| A gpui-kit bump adds or removes a color token the palette bridge does not map | `cargo test -p vibex-ui` fails on `the_bridge_covers_every_framework_token`. |
| The vendored `GpuiInputActivity` package or class name changes | `pnpm check:mobile-native` fails; the JNI export would no longer match. |
| An unapproved or missing SPDX selection enters the graph | `pnpm check:licenses` fails; do not silently broaden the policy. |
| A font or icon asset moves between trees | Update the policy `assetInputs`/`fontInputs` count and tree hash in the same change; see [Audited Asset Provenance Pin](#scenario-audited-asset-provenance-pin-and-generated-license-artifacts). |
| A workspace crate is added or removed | The license graph approves it from `license.workspace`, but the SBOM and the notices stay stale until `node scripts/check-licenses.mjs --write` runs. |
| Generated SBOM, notices, or baseline inventory drift | Regenerate the owning artifact and rerun its verification command. `--write` never rewrites a policy pin, so a stale pin is a separate repair. |

### 5. Good / Base / Bad Cases

- Good: bump the gpui-kit family to a published version, regenerate `Cargo.lock`,
  review the resolved graph, regenerate licenses, then run `pnpm check:rust`,
  `pnpm check:mobile-native`, and `pnpm check:licenses`.
- Base: ordinary development uses `--locked`; neither the crates.io pins nor the
  Git revision pin move automatically.
- Base: the `gpui-pre-mobile` pin names a `longbridge/gpui-mobile` revision that
  accepts the pinned `gpui-pre`; the manifest and
  `scripts/check-mobile-native.mjs` name the same revision. While upstream lags
  the pinned `gpui-pre`, the pin names the reviewed `PeatBoy/gpui-mobile`
  revision carrying the version bump that is open upstream as
  `longbridge/gpui-mobile#46`.
- Base: a non-GPUI dependency changes the root lock without moving any pin; no
  source-policy review is needed.
- Bad: fork zed again and publish it under the `gpui-pre` name so the version
  constraint is satisfied.
- Bad: move a font or icon out of a third-party checkout without registering the
  new provenance entry.
- Bad: patch a warning-producing crates.io package locally.

### 6. Tests Required

- `cargo metadata --locked --format-version 1` resolves successfully with exactly
  one `gpui-pre` version in the graph.
- `pnpm check:mobile-native` verifies the mobile manifest pins, the platform
  facade, and the vendored IME host.
- A `gpui-pre` or `gpui-pre-mobile` bump type-checks `apps/mobile/src/android_host.rs`
  for an Android target, not only the host target the quality gates cover. With
  no NDK installed, compile that file from a scratch crate that includes it by
  `#[path]`, supplies its `crate::` imports, and stubs the crates whose C
  dependencies need the NDK; a build of the pre-bump file must fail against the
  new platform layer, or the harness proves nothing.
- `cargo test -p vibex-ui` proves the palette bridge still covers every color
  token the resolved gpui-kit version defines.
- `pnpm check:rust` accepts an empty reviewed future-incompatibility allowlist and
  rejects every unlisted package or stale exception.
- `pnpm check:licenses` verifies asset/font provenance, the SBOM, the full Cargo
  graph, notices, and the intended AGPL/GPL selections.
- Run repository-level `pnpm check` before committing a dependency-source migration.

### 7. Wrong vs Correct

#### Wrong

```toml
gpui = { package = "gpui-pre", path = "vendor/zed/crates/gpui" }
gpui-component = { path = "vendor/gpui-component/crates/ui" }
```

This reintroduces a renamed GPUI fork and a vendored component tree.

#### Correct

```toml
gpui = { package = "gpui-pre", version = "=0.3.11" }
gpui_tokio = { package = "gpui-tokio", path = "crates/gpui-tokio" }
gpui-component = "0.9.0"
```

## Scenario: Redistributed Native Runtime With A Bounded Package Transform

### 1. Scope / Trigger

- Trigger: GPUI packages a native runtime such as PDFium that is downloaded outside
  Cargo, registered in the SBOM, and copied into `.deb` or AppImage resources.
- This is a supply-chain boundary even when the Rust wrapper itself comes from
  crates.io.

### 2. Signatures

```text
node scripts/prepare-pdfium-runtime.mjs
target/native/pdfium/linux-x86_64/{libpdfium.so,licenses/*}
pnpm package:native-content:linux
pnpm check:native-content-package
```

The package verifier records source/package SHA-256, ELF Build ID, `NEEDED`, RUNPATH,
and the exact license-file count.

### 3. Contracts

- Lock the archive URL, archive SHA-256, native library SHA-256/size, architecture,
  wrapper version, engine build, and every redistributed license file.
- Register the native input in the license policy, notices, and SBOM before enabling
  it in a production package.
- Linux AppImage may add only an `$ORIGIN` RUNPATH required for colocated loading.
  The reviewed ELF Build ID and `NEEDED` set must remain unchanged; record both the
  source and transformed SHA-256.
- Ship the complete reviewed license bundle beside the runtime. Do not replace it
  with a summary notice.
- The explicit package writer may preserve the developer's configured proxy while
  `appimagetool` fetches its type-2 runtime. The package verifier remains
  offline and never invokes packaging.
- Keep macOS/Windows runtime resources package-disabled until their target-specific
  probes pass; Linux approval does not infer another platform's result.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Archive/library hash or size differs | Preparation fails before packaging. |
| One reviewed license file is missing or changed | `pnpm check:licenses` and `pnpm check:native-content-package` fail. |
| AppImage changes Build ID or `NEEDED` | Reject the package as an unbounded binary transform. |
| AppImage RUNPATH is not the approved `$ORIGIN` form | Reject the package. |
| Unexecuted target is enabled in package resources | Review the Packager config before shipping. |
| Packaged `--probe` differs from the release binary | `pnpm check:native-content-package` fails. |

### 5. Good/Base/Bad Cases

- Good: the `.deb` preserves the reviewed library bytes; AppImage changes only
  RUNPATH, preserves Build ID/`NEEDED`, includes all licenses, and passes the same
  bounded probe.
- Base: macOS/Windows archive identities are recorded for future probes but their
  package resources remain disabled.
- Bad: accept any `linuxdeploy` rewrite because the package launches, or register
  only the wrapper crate's MIT license while omitting the native runtime bundle.

### 6. Tests Required

- Run preparation in verify mode and `pnpm check:licenses`.
- Build both Linux formats and run `pnpm check:native-content-package`.
- Assert source/transformed SHA-256, Build ID, `NEEDED`, RUNPATH, license
  bundle, package probe equivalence, and clean extraction/install behavior.
- Package-writer command failures retain stderr or, when stderr is empty, stdout so
  runtime-download failures remain diagnosable.
- Run root `pnpm check` after changing native input metadata or package resources.

### 7. Wrong vs Correct

#### Wrong

```text
Package libpdfium.so, let linuxdeploy rewrite it arbitrarily, and retain only LICENSE.
```

#### Correct

```text
Verify source SHA-256 -> package all reviewed licenses -> permit only $ORIGIN RUNPATH
-> compare Build ID and NEEDED -> record transformed SHA-256 -> run packaged --probe.
```

## Scenario: Audited Asset Provenance Pin And Generated License Artifacts

### 1. Scope / Trigger

- Trigger: adding, removing, or editing a file under a license-policy
  `assetInputs`, `fontInputs`, or native input root; adding or removing a
  workspace crate.
- Each audited root is pinned by file count and tree digest, so the gate fails
  until a human re-reviews the input and re-pins it. The failure is the
  mechanism working, not a defect.
- The pin and the generated artifacts are two independent states. Repairing one
  does not repair the other, and the pin check runs first, so a stale pin hides
  everything behind it.

### 2. Signatures

```text
docs/licenses/desktop-policy.json              hand-maintained input; never generated
  assetInputs[] | fontInputs[]
    root                  audited directory
    extensions[]          the only file types the root may contain
    expectedFileCount     pin, half 1
    treeSha256            pin, half 2
    licenseExpression | source | licenseEvidence
    files[]               optional explicit list; absent means walk the root

docs/licenses/desktop.cdx.json                 generated; embeds the digest, the
                                               count, and bom-refs derived from the
                                               first 16 digest characters
docs/licenses/desktop-third-party-notices.md   generated; embeds the count

pnpm check:licenses                            verify only, never writes
node scripts/check-licenses.mjs --write         rewrites exactly the two generated files
```

The pin covers every file under `root` whose extension is in `extensions`,
walked recursively with each directory's entries sorted. The digest is SHA-256
over, per file in that order: the POSIX path relative to `root`, `\0`, the file
bytes, `\0`.

### 3. Contracts

- `--write` rewrites the SBOM and the notices. **It never rewrites a pin.** A
  stale pin is repaired by editing the policy by hand, so the two repairs are
  ordered: re-pin first, then regenerate.
- `expectedFileCount` and `treeSha256` are one pin. Update both, or the gate
  keeps failing on whichever half still disagrees.
- A new file inside an existing audited root joins that entry's declared
  provenance. Confirm it really carries the `licenseExpression` the entry
  declares before re-pinning; a file that does not needs its own entry.
- A new workspace crate inherits `license.workspace` and needs no new license
  decision, but it is absent from the SBOM and the notices until `--write` runs.
- `--write` is idempotent: a second run produces no further diff.
- The unregistered-asset scan covers source-owned trees only. Generated build
  outputs — `apps/mobile/android/app/src/main/jniLibs`,
  `apps/mobile/android/app/build`, and `apps/mobile/ios/VibexFFI.xcframework` —
  are excluded by `GENERATED_ASSET_ROOTS`, because they are copies of inputs the
  policy already pins (Gradle's `mergeReleaseAssets/fonts/` copy of the Android
  emoji font, the packaged native libraries). Pinning a copy would either
  double-count the same bytes or demand a second policy entry for a file nobody
  authored; add a new root to that list only when the directory is build output,
  never to make a real source asset disappear.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| A file is added or removed under an audited root without re-pinning | `pnpm check:licenses` fails with `<id> provenance changed: expected <count>/<digest>, found <count>/<digest>`. |
| A pinned file's bytes change | The same failure: the digest moves even though the count does not. |
| A file with an unlisted extension is added under a pinned root | Fails with `<id> contains unreviewed file types: <paths>` before the pin is even compared. |
| A pin is corrected but the generated artifacts are not regenerated | Fails with `docs/licenses/desktop.cdx.json is stale; review the graph and run node scripts/check-licenses.mjs --write`. |
| A workspace crate is added and only `--write` runs | The crate is registered; a concurrently stale asset pin still fails, because the two states are independent. |
| The SBOM or the notices are hand-edited | The check compares them byte for byte against regenerated content and fails. |
| An unapproved SPDX selection enters the graph | Fails; do not broaden the policy to silence it. |

A single failure is not evidence of a single cause. Fix the reported one, re-run,
and expect the next one: the pin check precedes the generated-artifact check, so
`--write` alone changes nothing visible until the pin is correct.

### 5. Good / Base / Bad Cases

- Good: add an icon, confirm it carries the license its entry declares, prove the
  drift is only that file, then write both pin fields from the values the failure
  reported, run `--write`, and read every regenerated hunk before running
  `pnpm check:licenses`.
- Base: editing Rust that moves no dependency and touching no audited root needs
  no license work.
- Base: a dependency bump regenerates the SBOM and the notices without touching
  any asset pin, because the graph changed and the audited trees did not.
- Bad: run `--write` and commit the regenerated SBOM while the pin stays stale.
  The diff looks plausible and the gate still fails.
- Bad: write the pin from the checker's `found` value without first confirming
  what drifted. The pin would then record whatever the tree happens to contain,
  which is the review the pin exists to force.

### 6. Tests Required

- `pnpm check:licenses` passes and reports the expected package, bundle, and
  native-input counts.
- Before writing a pin, prove the drift is the change being made:
  `git diff --name-status <last-re-pin-commit> HEAD -- <audited-root>` must list
  only the files this change touches. Take `<last-re-pin-commit>` from
  `git log -1 --format=%H -- docs/licenses/desktop-policy.json`.
- Recompute the digest from the tree independently and confirm it equals the
  value about to be written, so the pin records the tree rather than the
  checker's own output.
- `--write` twice produces no second diff.
- Every hunk in the regenerated diff is attributable to this change. An
  unattributable hunk means another drift is riding along and needs its own
  review.

### 7. Wrong vs Correct

#### Wrong

```bash
# The pin is not regenerated by --write, so this repairs nothing.
node scripts/check-licenses.mjs --write
git add docs/licenses/desktop.cdx.json
git commit -m "chore(licenses): refresh the SBOM"
```

#### Correct

```bash
# 1. What actually drifted since the pin was last written?
git log -1 --format=%H -- docs/licenses/desktop-policy.json
git diff --name-status <that-commit> HEAD -- apps/mobile/assets/icons

# 2. Review each listed file against the provenance its entry declares.
# 3. Write the count and digest the failure reported into both pin fields.
# 4. Regenerate only after the pin is correct.
node scripts/check-licenses.mjs --write

# 5. Read the regenerated diff, then verify.
pnpm check:licenses
```

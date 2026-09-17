# Rust Dependency Source Guidelines

Vibex consumes third-party Rust projects from their upstream Cargo sources. The
repository does not maintain copied, patched, or shimmed third-party source trees
beyond two reviewed exceptions: the small first-party `gpui-tokio` bridge and the
vendored Android IME host.

## Scenario: Published gpui-pre With A Git-Pinned gpui-kit Family

### 1. Scope / Trigger

- Trigger: changing `Cargo.lock`, bumping the `gpui-pre` family, bumping the
  pinned gpui-kit family, bumping `gpui-pre-mobile`, or changing a third-party
  license decision.
- GPUI arrives from three sources: the published `gpui-pre` family on crates.io
  (zed snapshots), the gpui-kit crates pinned to a Git revision, and
  `gpui-pre-mobile` pinned to a Git revision for the Android/iOS platform layer.
  Glue with no published equivalent lives in this workspace.

### 2. Signatures

```toml
# Cargo.toml
[workspace.dependencies]
gpui = { package = "gpui-pre", version = "=0.3.5" }
gpui_platform = { package = "gpui-pre-platform", version = "=0.3.5", features = ["font-kit", "runtime_shaders", "wayland", "x11"] }
gpui_tokio = { package = "gpui-tokio", path = "crates/gpui-tokio" }
gpui-component = { git = "https://github.com/longbridge/gpui-kit", rev = "<pinned-gpui-kit-rev>" }
gpui-fps = { git = "https://github.com/longbridge/gpui-kit", rev = "<pinned-gpui-kit-rev>" }
gpui-kit-assets = { git = "https://github.com/longbridge/gpui-kit", rev = "<pinned-gpui-kit-rev>" }
```

```toml
# apps/mobile/Cargo.toml
[target.'cfg(any(target_os = "android", target_os = "ios"))'.dependencies]
gpui-mobile = { package = "gpui-pre-mobile", git = "https://github.com/longbridge/gpui-mobile", rev = "<pinned-gpui-mobile-rev>" }
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

- The `gpui-pre` family resolves from crates.io at the exact `=0.3.5` pins. Do not
  reintroduce a `[patch.crates-io]` block that redirects it to a local fork, and do
  not rename a fork to satisfy the version constraint.
- `gpui-pre-mobile` resolves from its pinned Git revision; the pin moves only with
  a reviewed dependency-source change, never as a floating branch.
- `crates/gpui-tokio` is the only first-party copy of upstream Rust code. It is a
  verbatim copy of zed's Apache-2.0 `gpui_tokio` with re-pointed dependency
  coordinates, kept because no `gpui-pre-tokio` package exists. Keep it verbatim;
  changes belong upstream.
- The vendored Android IME host is the only copied third-party source tree. Its
  Java package (`dev.gpui.mobile`) and class names are referenced by
  `#[no_mangle]` JNI exports, so renaming or relocating it breaks the keyboard.
- The gpui-kit family is pinned to a Git revision so unreleased component work is
  usable without waiting for crates.io. Do not vendor or patch it locally.
- Reproducibility is the combination of the crates.io pins, the two Git revision
  pins, and the root `Cargo.lock`. A bump reviews and commits the lockfile and the
  regenerated license outputs together.
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
| `apps/mobile` resolves a `vendor/zed` path dependency | `pnpm check:mobile-native` fails. |
| The vendored `GpuiInputActivity` package or class name changes | `pnpm check:mobile-native` fails; the JNI export would no longer match. |
| An unapproved or missing SPDX selection enters the graph | `pnpm check:licenses` fails; do not silently broaden the policy. |
| A font or icon asset moves between trees | Update the policy `assetInputs`/`fontInputs` count and tree hash in the same change. |
| Generated SBOM, notices, or baseline inventory drift | Regenerate the owning artifact and rerun its verification command. |

### 5. Good / Base / Bad Cases

- Good: bump `gpui-pre` to a published version, regenerate `Cargo.lock`, review the
  resolved graph, regenerate licenses, then run `pnpm check:rust`,
  `pnpm check:mobile-native`, and `pnpm check:licenses`.
- Base: ordinary development uses `--locked`; neither the crates.io pins nor the
  Git revision pins move automatically.
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
gpui = { package = "gpui-pre", version = "=0.3.5" }
gpui_tokio = { package = "gpui-tokio", path = "crates/gpui-tokio" }
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

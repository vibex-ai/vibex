# Rust Dependency Source Guidelines

Vibex consumes third-party Rust projects from their upstream Cargo sources. The
repository does not maintain copied, patched, or shimmed third-party source trees
beyond the one reviewed GPUI fork submodule.

## Scenario: Forked Zed Submodule With Registry gpui-component

### 1. Scope / Trigger

- Trigger: moving the `vendor/zed` submodule pointer, changing `Cargo.lock`,
  changing a third-party license decision, or bumping the gpui-component family.
- The GPUI ecosystem uses two source controls: the Zed fork is a pinned Git
  submodule, while gpui-component 0.6 ships from crates.io.

### 2. Signatures

```ini
# .gitmodules
[submodule "vendor/zed"]
    path = vendor/zed
    url = https://github.com/vibex-ai/zed.git
    branch = main
    shallow = true
```

```toml
# Cargo.toml
[workspace]
exclude = ["vendor/zed"]

[workspace.dependencies]
gpui = { package = "gpui-pre", path = "vendor/zed/crates/gpui" }
gpui_platform = { path = "vendor/zed/crates/gpui_platform" }
gpui_tokio = { path = "vendor/zed/crates/gpui_tokio" }
gpui-component = "0.6.0"
gpui-kit-assets = "0.6.0"

[patch.crates-io]
gpui-pre = { path = "vendor/zed/crates/gpui" }
gpui-pre-macros = { path = "vendor/zed/crates/gpui_macros" }
gpui-pre-sum-tree = { path = "vendor/zed/crates/sum_tree" }
```

```text
git submodule update --init --recursive          initialize the pinned Zed tree
Cargo.lock                                       one Vibex workspace lockfile
cargo metadata --locked --format-version 1       resolved source identity
pnpm check:licenses                              SPDX, asset, SBOM, and notice gate
pnpm check:rust                                  locked fmt/check/clippy/test gate
```

### 3. Contracts

- `vendor/zed` is the only approved vendor entry. It is a Git submodule, not a
  copied or directly edited source tree, and its committed gitlink is the exact Zed
  revision used by the build and the SBOM.
- The submodule URL is `https://github.com/vibex-ai/zed.git`. The tracked branch is
  `main`, but ordinary builds use the committed gitlink; they never select remote
  `main` automatically.
- Exclude `vendor/zed` from the Vibex workspace. Without the exclusion, Cargo makes
  Zed crates inherit Vibex's `[workspace.dependencies]` and manifest loading fails.
- The workspace `gpui` alias maps `gpui-pre` to the submodule copy so first-party
  code compiles against exactly one GPUI. The `[patch.crates-io]` block redirects
  the whole `gpui-pre-*` family (gpui, gpui_macros, sum_tree) — which registry
  gpui-component consumes — back to the same submodule.
- All Zed-family packages in Cargo metadata must resolve from the one submodule
  tree. No package may remain on either the official Zed Git source or a separate
  Git fetch of the fork.
- gpui-component and gpui-kit-assets come from crates.io at the versions pinned by
  the root `Cargo.lock`; do not fork, vendor, or patch them without an explicit
  approved source-policy change.
- Reproducibility is the combination of the committed Zed gitlink and root
  `Cargo.lock`. A Zed update reviews and commits the submodule pointer, lockfile,
  and regenerated license outputs together.
- Every CI checkout that builds, checks, or packages must enable recursive
  submodule checkout.
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
| `vendor/zed` is absent, uninitialized, not a gitlink, or points at another URL | `cargo metadata --locked` fails and builds stop. |
| A direct GPUI dependency does not use its exact `vendor/zed` path | `cargo metadata` fails to resolve. |
| The `[patch.crates-io]` block omits a `gpui-pre-*` member | Registry gpui-component drags in a second GPUI from crates.io; review the lockfile diff before committing. |
| Cargo metadata contains an official or fork Zed Git package | Inspect `cargo metadata` output and reject the escaped packages manually. |
| `proc-macro-error2` re-enters the graph, or another future-incompatible package appears | `pnpm check:rust` fails until the graph or reviewed allowlist is corrected. |
| An unapproved or missing SPDX selection enters the graph | `pnpm check:licenses` fails; do not silently broaden the policy. |
| Generated SBOM, notices, or baseline inventory drift | Regenerate the owning artifact and rerun its verification command. |

### 5. Good / Base / Bad Cases

- Good: fetch and review a fork commit, check out that exact revision inside
  `vendor/zed`, review the gitlink and `Cargo.lock` diffs, regenerate licenses,
  then run `pnpm check:rust` and `pnpm check:licenses`.
- Base: ordinary development initializes the submodule once and uses `--locked`;
  neither the fork revision nor gpui-component version moves automatically.
- Base: a non-GPUI dependency changes the root lock without moving the gitlink;
  no submodule review is needed.
- Bad: run `git submodule update --remote` and commit the result without reviewing
  the fork diff, resolved graph, and licenses.
- Bad: remove the `[patch.crates-io]` block and let registry gpui-component pull a
  second GPUI from crates.io.
- Bad: copy Zed files into `vendor/zed`, vendor gpui-component, or patch a
  warning-producing crates.io package locally.

### 6. Tests Required

- `git submodule status --recursive` reports the initialized reviewed revision.
- `cargo metadata --locked --format-version 1` resolves successfully and every
  Zed-family manifest path is under `vendor/zed`.
- `pnpm check:rust` accepts an empty reviewed future-incompatibility allowlist
  and rejects every unlisted package or stale exception.
- `pnpm check:licenses` verifies path-package provenance, the fork revision in the
  SBOM, the full Cargo graph, assets, notices, and intended AGPL/GPL selections.
- Run repository-level `pnpm check` before committing a dependency-source migration.

### 7. Wrong vs Correct

#### Wrong

```toml
gpui = { git = "https://github.com/vibex-ai/zed.git", branch = "main" }
gpui-component = { path = "vendor/gpui-component/crates/ui" }
```

This fetches branch state through Cargo and bypasses the reviewed submodule gitlink.

#### Correct

```toml
gpui = { package = "gpui-pre", path = "vendor/zed/crates/gpui" }

[patch.crates-io]
gpui-pre = { path = "vendor/zed/crates/gpui" }
gpui-pre-macros = { path = "vendor/zed/crates/gpui_macros" }
gpui-pre-sum-tree = { path = "vendor/zed/crates/sum_tree" }
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

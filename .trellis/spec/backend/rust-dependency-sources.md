# Rust Dependency Source Guidelines

Vibex consumes third-party Rust projects from their upstream Cargo sources. The
repository does not maintain copied, patched, or shimmed third-party source trees.

## Scenario: Forked Zed Submodule With A Reviewed Root Lockfile

### 1. Scope / Trigger

- Trigger: adding or updating a Rust Git dependency, moving the `vendor/zed`
  submodule pointer, changing `Cargo.lock`, changing a third-party license decision,
  or changing evidence whose validity depends on the resolved Rust graph.
- The GPUI ecosystem uses two source controls: the Zed fork is a pinned Git
  submodule, while gpui-component remains an unqualified upstream Git dependency
  pinned by the committed root `Cargo.lock`.

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
gpui = { path = "vendor/zed/crates/gpui" }
gpui_platform = { path = "vendor/zed/crates/gpui_platform" }
gpui_tokio = { path = "vendor/zed/crates/gpui_tokio" }
gpui-component = { git = "https://github.com/longbridge/gpui-component" }
gpui-component-assets = { git = "https://github.com/longbridge/gpui-component" }

[patch."https://github.com/zed-industries/zed"]
gpui = { path = "vendor/zed/crates/gpui" }
gpui_macros = { path = "vendor/zed/crates/gpui_macros" }
```

```text
git submodule update --init --recursive          initialize the pinned Zed tree
Cargo.lock                                       one Vibex workspace lockfile
cargo metadata --locked --format-version 1       resolved source identity
pnpm check:graph                                 submodule and source-shape gate
pnpm check:licenses                              SPDX, asset, SBOM, and notice gate
pnpm check:rust                                  locked fmt/check/clippy/test gate
```

```text
upstream-dependency-revalidation.v1 {
  currentSource,
  historicalSources: [{ source, evidence[], sourceInputTreeSha256ByEvidence? }]
}
```

### 3. Contracts

- `vendor/zed` is the only approved vendor entry. It is a Git submodule, not a
  copied or directly edited source tree, and its committed gitlink is the exact Zed
  revision used by source-identity and evidence tooling.
- The submodule URL is `https://github.com/vibex-ai/zed.git`. The tracked branch is
  `main`, but ordinary builds use the committed gitlink; they never select remote
  `main` automatically.
- Exclude `vendor/zed` from the Vibex workspace. Without the exclusion, Cargo makes
  Zed crates inherit Vibex's `[workspace.dependencies]` and manifest loading fails.
- Vibex's direct `gpui`, `gpui_platform`, and `gpui_tokio` dependencies are path
  dependencies. The patch for the canonical Zed URL is required because
  gpui-component still declares `gpui` and `gpui_macros` from that URL.
- All Zed-family packages in Cargo metadata must resolve from the one submodule
  tree. No package may remain on either the official Zed Git source or a separate
  Git fetch of the fork.
- gpui-component declarations remain unqualified upstream Git dependencies without
  `rev`, `tag`, or `branch`; the root lockfile selects their reviewed commit.
- Reproducibility is the combination of the committed Zed gitlink and root
  `Cargo.lock`. A Zed update reviews and commits the submodule pointer, lockfile,
  license outputs, source identities, and evidence disposition together.
- Every CI checkout that builds, checks, packages, or validates evidence must enable
  recursive submodule checkout.
- No other tracked `vendor/` tree, Git submodule, local third-party path patch,
  compatibility shim, or copied upstream source is approved.
- Use crates.io packages unmodified unless the user explicitly approves a new
  source policy. Future-incompatibility warnings require an exact package/version
  allowlist with owner and removal condition; they do not justify another fork.
- Vibex package metadata uses `AGPL-3.0-or-later`. Approved dependency licenses may
  include `GPL-3.0-or-later`, but dependency license metadata is never rewritten as
  Vibex's license.
- Physical evidence tied to a previous source policy or `Cargo.lock` remains
  historical. A reviewed revalidation disposition must classify it as
  `historical_pending_recapture`; do not rewrite old captures as current proof.
- Each historical generation is keyed by its exact Zed revision, gpui-component
  revision, and `Cargo.lock` SHA-256. Each evidence path belongs to one generation.
  Preserve any reviewed `sourceInputTreeSha256ByEvidence` binding.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| `vendor/zed` is absent, uninitialized, not a gitlink, or points at another URL | `check:graph` fails with the missing submodule contract. |
| A direct GPUI dependency does not use its exact `vendor/zed` path | Graph check fails. |
| The upstream Zed patch omits `gpui` or `gpui_macros` | Graph check fails before duplicate sources can enter the graph. |
| Cargo metadata contains an official or fork Zed Git package | Graph check fails and reports the escaped package names. |
| gpui-component contains `rev`, `tag`, or `branch`, or resolves from multiple commits | Graph check fails. |
| Another entry appears under `vendor/` or another root-managed lockfile appears | Graph check fails. The Zed submodule's own lockfile is outside the Vibex lock scan. |
| `proc-macro-error2` re-enters the graph, or another future-incompatible package appears | Dependency or Rust quality checks fail until the graph or reviewed allowlist is corrected. |
| An unapproved or missing SPDX selection enters the graph | License check fails; do not silently broaden the policy. |
| Physical evidence identity differs from the current root source | Classify it as historical only when the revalidation disposition recognizes it; otherwise fail as stale. |
| Generated SBOM, notices, baseline inventory, or decision hashes drift | Regenerate the owning artifact and rerun its verification command. |

### 5. Good / Base / Bad Cases

- Good: fetch and review a fork commit, check out that exact revision inside
  `vendor/zed`, review the gitlink and `Cargo.lock` diffs, regenerate licenses and
  evidence identities, then run the full dependency-source gates.
- Base: ordinary development initializes the submodule once and uses `--locked`;
  neither the fork revision nor gpui-component revision moves automatically.
- Base: a non-Zed dependency changes the root lock without moving the gitlink;
  evidence is recaptured or recorded under an exact historical lock generation.
- Bad: run `git submodule update --remote` and commit the result without reviewing
  the fork diff, resolved graph, licenses, and evidence disposition.
- Bad: remove the canonical-URL patch and allow gpui-component to reintroduce a
  second GPUI package from the official repository.
- Bad: copy Zed files into `vendor/zed`, vendor gpui-component, or patch a
  warning-producing crates.io package locally.

### 6. Tests Required

- `git submodule status --recursive` reports the initialized reviewed revision.
- `cargo metadata --locked --format-version 1` resolves successfully and every
  Zed-family manifest path is under `vendor/zed`.
- `pnpm check:graph` asserts the fork URL, gitlink mode, exact path declarations,
  canonical-URL patch, one gpui-component commit, no proc-macro-error2 exception,
  forked Zed tracing, one Vibex root lockfile, and no extra vendor entry.
- `pnpm check:rust` accepts an empty reviewed future-incompatibility allowlist
  and rejects every unlisted package or stale exception.
- `pnpm check:licenses` verifies path-package provenance, the fork revision in the
  SBOM, the full Cargo graph, assets, notices, and intended AGPL/GPL selections.
- Evidence checks assert either `current` or the reviewed
  `historical_pending_recapture` classification from exact source identities.
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
gpui = { path = "vendor/zed/crates/gpui" }

[patch."https://github.com/zed-industries/zed"]
gpui = { path = "vendor/zed/crates/gpui" }
gpui_macros = { path = "vendor/zed/crates/gpui_macros" }
```

#### Wrong

```json
{
  "historicalSource": { "cargoLockSha256": "newest-old-lock" }
}
```

This loses the identity of earlier evidence generations.

#### Correct

```json
{
  "historicalSources": [
    { "source": { "cargoLockSha256": "vendor-lock" }, "evidence": ["old.json"] },
    { "source": { "cargoLockSha256": "prior-lock" }, "evidence": ["newer.json"] }
  ]
}
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
  `appimagetool` fetches its type-2 runtime. The read-only evidence verifier remains
  offline and never invokes packaging. A future hermetic writer must pin and supply a
  reviewed local runtime file before removing network/proxy access.
- Keep macOS/Windows runtime resources package-disabled until their target-specific
  probes pass; Linux approval does not infer another platform's result.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Archive/library hash or size differs | Preparation/package gate fails. |
| One reviewed license file is missing or changed | License and package gates fail. |
| AppImage changes Build ID or `NEEDED` | Reject the package as an unbounded binary transform. |
| AppImage RUNPATH is not the approved `$ORIGIN` form | Reject the package. |
| Unexecuted target is enabled in package resources | Hosted/package policy fails. |
| Packaged `--probe` differs from the release binary | Package gate fails. |
| Writer removes proxy/network access without a pinned local AppImage runtime | Packaging fails; retain combined stdout/stderr and do not write evidence. |

### 5. Good/Base/Bad Cases

- Good: the `.deb` preserves the reviewed library bytes; AppImage changes only
  RUNPATH, preserves Build ID/`NEEDED`, includes all licenses, and passes the same
  bounded probe.
- Base: macOS/Windows archive identities are recorded for future probes but their
  package resources remain disabled.
- Base: the explicit writer downloads the AppImage type-2 runtime through the existing
  local proxy, then the committed evidence is checked later without networking.
- Bad: accept any `linuxdeploy` rewrite because the package launches, or register
  only the wrapper crate's MIT license while omitting the native runtime bundle.
- Bad: delete proxy variables to look offline while `appimagetool` still requires a
  runtime download, then discard stdout because stderr happened to be empty.

### 6. Tests Required

- Run preparation in verify mode and `pnpm check:licenses`.
- Build both Linux formats and run `pnpm check:native-content-package`.
- Assert source/transformed SHA-256, Build ID, `NEEDED`, RUNPATH, 16-file license
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

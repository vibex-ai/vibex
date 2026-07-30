# Rust Dependency Source Guidelines

Vibex consumes third-party Rust projects from their upstream Cargo sources. The
repository does not maintain copied, patched, or shimmed third-party source trees.

## Scenario: Upstream Git Dependencies With A Reviewed Root Lockfile

### 1. Scope / Trigger

- Trigger: adding or updating a Rust Git dependency, changing `Cargo.lock`, changing
  a third-party license decision, or changing evidence whose validity depends on the
  resolved Rust graph.
- The GPUI ecosystem is the reference contract: Zed GPUI and gpui-component use
  upstream Git declarations without `rev`, `tag`, or `branch`; the committed root
  `Cargo.lock` is the exact reviewed snapshot.

### 2. Signatures

```toml
# Cargo.toml
[workspace.dependencies]
gpui = { git = "https://github.com/zed-industries/zed" }
gpui_platform = { git = "https://github.com/zed-industries/zed" }
gpui_tokio = { git = "https://github.com/zed-industries/zed" }
gpui-component = { git = "https://github.com/longbridge/gpui-component" }
gpui-component-assets = { git = "https://github.com/longbridge/gpui-component" }
```

```text
Cargo.lock                                      one committed workspace lockfile
cargo metadata --locked --format-version 1     resolved source identity
pnpm check:graph                           source-shape and single-source gate
pnpm check:licenses                        SPDX, asset, SBOM, and notice gate
pnpm check:rust                                 locked fmt/check/clippy/test gate
```

```text
upstream-dependency-revalidation.v1 {
  currentSource,
  historicalSources: [{ source, evidence[], sourceInputTreeSha256ByEvidence? }]
}
```

### 3. Contracts

- Third-party Git manifests name the canonical upstream repository. Do not add
  `rev`, `tag`, or `branch` to the GPUI or gpui-component declarations.
- Reproducibility comes from the committed root `Cargo.lock` and normal `--locked`
  commands. A dependency update is a reviewed lockfile change, not an automatic
  fetch of upstream HEAD during ordinary builds.
- All Zed packages resolve to one Git source commit, and both gpui-component
  packages resolve to one upstream gpui-component commit.
- Do not add a tracked `vendor/` tree, Git submodule, local third-party path patch,
  compatibility shim, or copied upstream source as a fallback.
- Use crates.io packages unmodified unless the user explicitly approves a new
  source policy. Known future-incompatibility warnings require an exact
  package/version allowlist with owner and removal condition; they do not justify a
  local fork.
- Vibex package metadata uses `AGPL-3.0-or-later`. Approved dependency licenses may
  include `GPL-3.0-or-later`, but dependency license metadata is never rewritten as
  Vibex's license.
- Physical evidence tied to a previous `Cargo.lock` remains historical. A reviewed
  revalidation disposition must classify it as `historical_pending_recapture`; do
  not regenerate metadata that presents old screenshots as current-lock proof.
- Repositories may retain more than one historical dependency generation. Record
  each generation separately under `historicalSources`, keyed by its exact Zed
  revision, gpui-component revision, and `Cargo.lock` SHA-256. Each evidence path
  belongs to one generation; do not overwrite the prior generation when the lock
  changes again.
- Long-running or unavailable physical captures may also bind their historical
  `sourceInputTreeSha256` in `sourceInputTreeSha256ByEvidence`. The verifier must
  reject a changed tree hash even while returning `historical_pending_recapture`.
  Deterministic/headless evidence should be recaptured against the current graph
  when practical instead of being needlessly carried as historical.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| GPUI manifest contains `rev`, `tag`, or `branch` | `check:graph` fails. |
| Zed or gpui-component resolves from multiple source commits | Graph check fails and reports the conflicting identities. |
| A tracked `vendor/` directory, third-party path patch, or nested lockfile appears | Graph check fails. |
| `proc-macro-error2` is not crates.io `2.0.1`, or another future-incompatible package appears | Rust quality check fails until the graph or reviewed allowlist is corrected. |
| An unapproved or missing SPDX selection enters the graph | License check fails; do not silently broaden the policy. |
| Physical evidence lock identity differs from the current root lock | Classify it as historical only when the revalidation disposition recognizes it; otherwise fail as stale. |
| Evidence path is absent from every historical generation, appears in two generations, or its revisions/lock do not match its generation | Fail closed; do not infer the closest generation. |
| Historical evidence has a reviewed source-tree identity and the stored tree hash changes | Reject the artifact even though its lock generation is recognized. |
| Generated SBOM, notices, baseline inventory, or decision hashes drift | Regenerate the owning artifact and rerun its verification command. |

### 5. Good / Base / Bad Cases

- Good: update upstream dependencies with `cargo update`, review the exact
  `Cargo.lock` diff, regenerate notices/evidence identities, and run `pnpm check`.
- Base: ordinary development uses `cargo check --locked`; no network-selected
  dependency revision changes occur when the lockfile is unchanged.
- Base: a service dependency changes the root lock without changing the Zed commit;
  current physical artifacts are recaptured where practical and the remaining prior
  lock artifacts move into a new exact historical generation.
- Bad: pin a Zed `rev` to avoid reviewing future lock updates, copy gpui-component
  into `vendor/`, or patch a warning-producing crate locally.
- Bad: overwrite historical evidence source hashes after a dependency transition
  without rerunning the physical protocol.
- Bad: replace the single historical source with the newest old lock and thereby
  make older vendor-era evidence unclassifiable.

### 6. Tests Required

- `cargo metadata --locked --format-version 1` resolves successfully.
- `pnpm check:graph` asserts unqualified upstream Git declarations, one source
  commit per upstream family, crates.io proc-macro-error2, upstream Zed tracing, no
  vendor directory, and exactly one root lockfile.
- `pnpm check:rust` accepts only the reviewed `proc-macro-error2 v2.0.1`
  future-incompatibility entry and rejects any additional package.
- `pnpm check:licenses` verifies the full Cargo graph, asset provenance, SBOM,
  notices, and the intended AGPL/GPL selections.
- Evidence checks assert either `current` or the reviewed
  `historical_pending_recapture` classification from exact lockfile identities;
  negative tests mutate a bound historical source-tree hash and must be rejected.
- Run the repository-level `pnpm check` before committing a dependency-source
  migration.

### 7. Wrong vs Correct

#### Wrong

```toml
gpui = { git = "https://github.com/zed-industries/zed", rev = "819fe337" }
gpui-component = { path = "vendor/gpui-component/crates/ui" }

[patch.crates-io]
proc-macro-error2 = { path = "vendor/proc-macro-error2" }
```

#### Correct

```toml
gpui = { git = "https://github.com/zed-industries/zed" }
gpui-component = { git = "https://github.com/longbridge/gpui-component" }
```

Commit and review the resulting root `Cargo.lock`; keep builds and checks on
`--locked`.

#### Wrong

```json
{
  "historicalSource": { "cargoLockSha256": "newest-old-lock" }
}
```

This loses the identity of any earlier physical evidence generation.

#### Correct

```json
{
  "historicalSources": [
    { "source": { "cargoLockSha256": "vendor-lock" }, "evidence": ["old.json"] },
    { "source": { "cargoLockSha256": "prior-upstream-lock" }, "evidence": ["newer.json"] }
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

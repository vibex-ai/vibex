# Rust Toolchain And Quality Gate

Vibex development, CI, formatting, Clippy, and release builds use Rust `1.97.0`.
The version is pinned in `rust-toolchain.toml`, the workspace `rust-version`, and
the three-platform GitHub Actions matrix.

The root `pnpm check:rust` command is the review gate. It runs:

```text
cargo fmt <every Vibex workspace package> -- --check
cargo check --workspace --all-targets --locked --future-incompat-report
cargo clippy --workspace --all-targets --locked -- -D warnings <reviewed allows>
cargo test --workspace --locked
```

Formatting enumerates Cargo workspace members from locked metadata, so the check
covers all Vibex crates without treating third-party source checkouts as
workspace-owned code.

## Clippy Allowlist

The exact Clippy exceptions live in `docs/development/rust-clippy-allowlist.json`.
Each entry has an owner and a removal gate; the quality script rejects additions,
removals, reordering, and incomplete ownership. New code therefore cannot expand
the allowlist without an explicit policy change.

## Future-Incompatibility Allowlist

The upstream GPUI graph exposes rustc E0365 in
`proc-macro-error2 2.0.1 -> stacksafe 0.1.4 -> gpui 0.2.2`. Vibex uses the
unmodified MIT/Apache-2.0 crates.io package. Rust 1.97 caps the lint for
dependencies, so the workspace builds while Cargo emits a future-incompatibility
report.

The exact temporary exception lives in
`docs/development/rust-future-incompat-allowlist.json`. The quality script fails
on every additional reported package, and also fails when the exception becomes
stale, so the upstream issue cannot silently expand or outlive the fix.

## Platform Coverage

A successful Linux run is not cross-platform evidence. GitHub Actions remains
responsible for native Linux, macOS, and Windows confirmation.

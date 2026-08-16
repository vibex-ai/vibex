# Rust Toolchain And Quality Gate

Vibex development, CI, formatting, Clippy, and release builds use Rust `1.97.0`.
The version is pinned in `rust-toolchain.toml`, the workspace `rust-version`, and
the three-platform GitHub Actions matrix.

The root `pnpm check:rust` command is the review gate. It runs:

```text
cargo fmt <every Vibex workspace package> -- --check
cargo check --workspace --all-targets --locked --future-incompat-report
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Formatting enumerates Cargo workspace members from locked metadata, so the check
covers all Vibex crates without treating third-party source checkouts as
workspace-owned code.

## Clippy Policy

The workspace gate passes no global lint exceptions. A reviewed exception that
is intrinsic to one existing API must stay adjacent to that item as a scoped
`#[allow(...)]`; new code cannot inherit a workspace-wide exemption.

## Future-Incompatibility Allowlist

The reviewed GPUI graph currently needs no future-incompatibility exceptions.
Zed's upgrade to `stacksafe 1.0.3` removed the former
`proc-macro-error2 2.0.1` path and its rustc E0365 report.

Reviewed temporary exceptions live in
`docs/development/rust-future-incompat-allowlist.json`, whose package list may be
empty. The quality script fails on every unlisted reported package and also
fails when an exception becomes stale, so an upstream issue cannot silently
expand or outlive its fix.

## Platform Coverage

A successful Linux run is not cross-platform evidence. GitHub Actions remains
responsible for native Linux, macOS, and Windows confirmation.

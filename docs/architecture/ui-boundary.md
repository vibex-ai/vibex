# UI Architecture Boundary

## Authority

`.trellis/spec/guides/architecture-baseline.md` is the product and architecture
authority. `apps/desktop` is the only visual, interaction, and information
architecture source.

## Product Boundary

- Desktop uses GPUI with an in-process NativeBackend.
- Web uses GPUI-WASM with WebRemoteBackend; the source is `apps/web`.
- Mobile packages the same Web GPUI-WASM artifact in Capacitor; the source is
  `apps/mobile`, which packages only `apps/web/dist` under the `dev.vibex.remote`
  application identity.
- DesktopRuntime is authoritative for Agent, files, Git, PTY, Provider config,
  device permission, sequence/revision, and mutation results.
- Wide, Medium, and Compact are shells over one component and state model.
- V1 supports Direct LAN/private-network connections and user self-hosted Relay.
  It does not provide an official Vibex Relay.

## Shared Code Boundary

Shared across shells:

- Rust domain DTOs and request/response semantics in `crates/core`.
- Framework-neutral projection/state in `crates/desktop-model`.
- DesktopRuntime and Agent/File/Git/Terminal/Provider services.
- Remote/Relay service behavior and protocol tests.
- GPUI theme, primitives, icons, View behavior, and evidence fixtures.

Not permitted:

- A Capacitor packaging relationship to any non-GPUI Web artifact.
- A second component family for touch surfaces.

## Shared Design Contract

The structured token source is `crates/vibex-ui/theme/tokens.json`. It owns
semantic light/dark colors, syntax highlighting, typography defaults, radii,
spacing, border widths, and shadow policy. Generated Rust is a derived artifact.
Desktop and WASM consumers use the same crate and source identity.

The source uses schema `vibex-design-tokens.v1` and binds the exact GPUI and
gpui-component revisions resolved by the root `Cargo.lock`. It freezes 53 semantic
colors per theme and the locked gpui-component Default light/dark highlight themes.
`scripts/generate-tokens.mjs` validates the schema and produces only
`crates/vibex-ui/src/generated_tokens.rs`. `apps/desktop/src/theme.rs` consumes the
shared colors, typography policy, radii, shadow policy, and syntax highlight values
through the crate.

Component semantics are stable across shells:

- Agent timeline, approval, diff, file tree, Terminal, and ManagementCenter names
  and states remain shared.
- Compact may reorder and reduce density, and a dialog/popover may become a sheet.
  It must not change brand language or create a second component family.
- Critical actions cannot depend on hover. Touch surfaces provide explicit buttons,
  More sheets, or a documented long-press action.

## Asset And Workbench Inventory

The canonical icon and brand inventory is `apps/desktop/assets/icons`:

- `vibex-mark.svg` is the product mark.
- Agent and model brands include Claude, OpenAI, Gemini, Copilot, OpenCode, and Qwen.
- `open-tools/` owns the reviewed IDE/tool brand set.
- File-kind, navigation, approval, Agent, Git, Terminal, import/download, and panel
  action glyphs use the checked-in GPUI assets or gpui-component primitives.

Future extraction moves reviewed GPUI assets into the shared crate without
redrawing them. Multicolor brand assets render as images; monochrome action glyphs
follow semantic theme color.

The workbench/state inventory is:

- Wide: project/session rail, primary workspace, contextual right rail, and optional
  Terminal; Medium retains one primary plus one auxiliary surface.
- Compact: one task stack with host/project/session/detail navigation and explicit
  sheets for auxiliary surfaces; it reorders shared domain components rather than
  introducing mobile-specific copies.
- Agent timeline, Permission, Plan, Tool call, command, Diff, files, Git, Terminal,
  ManagementCenter, preview, and error/system cards retain their existing names and
  state semantics.
- Baselines cover light/dark, Chinese text, loading, empty, streaming, error,
  reconnecting, approval-pending, destructive confirmation, and recovery states.

## Baseline Evidence

GPUI Desktop evidence is the visual/interaction baseline:

- `pnpm check:tokens`
- `pnpm check:graph`
- `pnpm check:foundation:linux`
- `pnpm check:acp`
- `pnpm check:code-workbench`
- `pnpm check:terminal`
- `pnpm check:native-content`

Required structural viewports are 360x620, 360x800, 390x844, 768x1024, 1200x800,
and 1440x900. Evidence must state which dimensions are physical, headless,
model-only, or deferred; one category cannot silently satisfy another.

# Platform Support Matrix

This page describes the current GPUI product surfaces and the evidence required
for a support claim. A successful compile is build evidence only; native pixels,
input, packaging, and physical-device behavior require their own checks.

## Client Surfaces

| Surface | Current role | Build or validation entry point | Support boundary |
| --- | --- | --- | --- |
| Linux desktop | Primary native development target and full local workbench | `pnpm dev:desktop`, `pnpm package:preview` | Release claims require Linux package, X11/Wayland, input, and rollback evidence for the exact source and lockfile. |
| macOS desktop | Native build target | `cargo check -p vibex-desktop --locked` on macOS | No stable packaging, signing, notarization, or physical UI claim is currently published. |
| Windows desktop | Native build target | `cargo check -p vibex-desktop --locked` on Windows | No stable packaging, signing, install, or physical UI claim is currently published. |
| Mobile WASM development host | Local automation host, not a product | `pnpm check:mobile-wasm-host` | Chromium verifies Compact/Medium rendering and host bridges only. No browser, PWA, hosting, or accessibility support claim follows. |
| Android | Capacitor shell for the dedicated GPUI-WASM mobile runtime | `pnpm --filter @vibex/mobile android:debug` | APK construction is supported; each rebuilt APK needs source-bound physical-device validation before a release claim. |
| iOS | Capacitor shell for the dedicated GPUI-WASM mobile runtime | `pnpm --filter @vibex/mobile ios:debug` on macOS | Requires Xcode and physical/simulator validation. No iOS production support claim is currently published. |
| Relay server | Optional user-self-hosted transport | `pnpm smoke:relay:local` | Relay is transport only. Public deployment, TLS, NAT, and device proof are operator-owned checks. |

Desktop is the only authoritative runtime. Mobile is a remote client through
`WebRemoteBackend`; it does not run local Agents, Git, PTY, or workspace
filesystem services. Shared UI lives in `crates/vibex-ui`; Wide is a desktop
shell, while the mobile runtime is capped at Medium.

## Capability Boundaries

| Capability | Native desktop | Mobile |
| --- | --- | --- |
| Agent, files, Git, Provider configuration | Owned by `DesktopRuntime` and exposed through `NativeBackend` | Remote projection and mutation through `WebRemoteBackend`; server permissions remain authoritative |
| Terminal | Native PTY plus shared terminal UI | Remote binary stream and input; no local shell |
| PDF | Native PDFium worker on supported packaged targets | Remote or browser-safe presentation only; no native PDFium in the client |
| Office documents | Bounded native projection where supported | Remote/mobile-safe presentation only |
| Secure credentials | OS/native storage owned by the desktop runtime | Capacitor secure storage requires device validation; development-host storage is never a product credential store |

## Evidence Rules

Machine-readable evidence under `docs/platform/evidence/` and
`docs/parity/evidence/` is checked by the matching `capture:*` or `check:*`
command. Evidence is valid only for the source identities it records. A stale
artifact may document a tested route, but it is not proof for a different
`Cargo.lock`, package, browser, APK, or platform.

Relevant gates include:

```bash
pnpm check:graph
pnpm check:licenses
pnpm check:foundation:linux
pnpm check:acp
pnpm check:code-workbench
pnpm check:terminal
pnpm check:native-content
pnpm check:wasm-gate
pnpm release:build-smoke
```

Physical and release-host captures are deliberately separate from the default
developer loop. Run a matching writer only when the tested source, package, and
environment are available; do not edit evidence artifacts by hand.

## Known Constraints

- Linux is the only desktop packaging route currently defined by the repository.
- macOS and Windows require host-specific build, package, install, and UI
  validation before their support status can be promoted.
- GPUI-WASM requires WebGPU and currently lacks a complete accessibility tree.
- Android/iOS WebView IME, clipboard, keyboard layout, soft keyboard, lifecycle,
  secure storage, and high-DPI behavior need representative physical coverage.
- Android and iOS release distribution additionally require their platform
  signing and store workflows, which are not part of the default checks.

# Platform Support Matrix

This page describes the native Vibex product surfaces and the evidence required
for a support claim. A successful compile is build evidence; native pixels,
input, packaging, and physical-device behavior require platform-specific checks.

## Client Surfaces

| Surface | Current role | Build or validation entry point | Support boundary |
| --- | --- | --- | --- |
| Linux desktop | Primary native development target and full local workbench | `pnpm dev:desktop`, `pnpm package:preview` | Release claims require package, input, and rollback evidence for the exact source and lockfile. |
| macOS desktop | Native build target | `cargo check -p vibex-desktop --locked` | Packaging, signing, and physical UI claims require macOS evidence. |
| Windows desktop | Native build target | `cargo check -p vibex-desktop --locked` | Packaging, signing, install, and physical UI claims require Windows evidence. |
| Android | Native GPUI client using `gpui_android` and NativeActivity | `pnpm build:mobile:android` | Each rebuilt APK needs source-bound device or emulator validation before a release claim. |
| iOS | Native GPUI client using `gpui_ios` and UIKit | `pnpm build:mobile:ios` on macOS | Simulator/device, signing, and distribution validation remain separate release evidence. |
| Relay server | Optional user-self-hosted encrypted transport | `pnpm smoke:relay:local` | Relay is transport only; deployment, TLS, NAT, and device proof are operator-owned. |

Desktop is the only authoritative runtime. Mobile is a remote client through the
typed backend facade and `AutoRemoteTransport`; it does not run local Agents,
Git, PTY, or workspace filesystem services.

## Capability Boundaries

| Capability | Native desktop | Native mobile |
| --- | --- | --- |
| Agent sessions and timeline | Owned by `DesktopRuntime` and rendered locally | GUI projection and typed mutations; timeline authority remains on desktop |
| Files, Git, and providers | Local services | Remote projection and capability-gated actions |
| Terminal | Native PTY plus shared terminal UI | Remote terminal data when a session exposes it; the Agent session page remains GUI-first |
| Credentials | OS/native desktop storage | App-sandbox credential bundle with restrictive file permissions |
| Pairing and routes | Publishes Direct/Tailnet/Relay offers | Claims one-time offers and selects validated Direct/Tailnet/Relay routes |

## Evidence Rules

Machine-readable evidence under `docs/platform/evidence/` is checked by its
matching command and is valid only for the source and lockfile identities it
records. Do not hand-edit generated evidence.

```bash
pnpm check:graph
pnpm check:licenses
pnpm check:mobile-native
pnpm release:build-smoke
pnpm smoke:relay:local
```

Physical and release-host captures are deliberately separate from the default
developer loop. Run them only when the requested SDK, device, signing team, and
exact source identity are available.

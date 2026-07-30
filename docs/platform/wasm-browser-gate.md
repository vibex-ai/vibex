# GPUI-WASM Web And Mobile Gate

## Current Status

- Web validation: Chromium and Firefox render the shared GPUI client and pass
  the source-bound browser interaction suite on the recorded test versions.
- Production release: blocked until browser accessibility and high-DPI canvas
  behavior meet the release requirements below.
- Android packaging: available through the Capacitor shell. Every rebuilt APK
  returns to `not_tested` until physical results are captured for that exact
  artifact.
- iOS physical validation: pending and requires a macOS/Xcode host.

The machine-readable sources of truth are:

- `docs/platform/evidence/wasm-browser-gate.json`
- `docs/platform/evidence/wasm-android-build.json`
- `docs/platform/evidence/wasm-mobile-physical.json`

## Toolchain Contract

- GPUI and gpui-component revisions come from the root `Cargo.lock`.
- `wasm-bindgen-cli` must match the version resolved by the lockfile.
- The WASM compiler is `nightly-2026-07-24`.
- The runtime uses `single_threaded_web`; no SharedArrayBuffer is required.

## Browser Coverage

| Target | Gate coverage | Support statement |
| --- | --- | --- |
| Chromium on Linux | GPUI pixels, keyboard, DOM paste/composition, pointer/touch, scroll, resize, lifecycle bridge, local storage, bounded Fetch, and WebSocket | Applies only to the version recorded in browser evidence; it is not a minimum-version claim. |
| Firefox on Linux | GPUI pixels with WebGPU enabled | Applies only to the version recorded in browser evidence. |
| WebGPU missing or blank canvas | Visible diagnostic page with a stable error code | Canvas existence alone is never a pass. |

Automated paste, composition, and touch exercise the browser event bridge. They
do not prove a physical CJK IME, soft keyboard, clipboard menu, or non-US
hardware layout.

## Responsive And Performance Contract

- Wide: width greater than or equal to 1100 CSS px.
- Medium: width from 760 through 1099 CSS px.
- Compact: width below 760 CSS px; 360x800 is mandatory.
- First-frame budget: 5000 ms.
- Frame p95 budget for the 48-row timeline fixture: 50 ms.
- Input-to-two-presented-frames budget: 500 ms.

High-DPI validation must confirm both the reported device-pixel ratio and the
canvas backing-store dimensions. A DPR greater than one with a CSS-sized backing
store is not a sharp high-DPI pass.

## Network And Storage

- Browser storage read/write/remove is covered by the Web gate, but browser
  storage is not a credential store.
- Fetch probes enforce a bounded response. Large streams require a streaming
  implementation or native bridge.
- WebSocket echo is covered by the Web gate.
- Capacitor secure storage requires a source-bound physical-device result.

## Accessibility

The shared View defines GPUI roles, labels, headings, list items, and application
semantics. The current GPUI Web platform does not expose the complete semantic
tree to browser accessibility APIs. A production Web/mobile claim requires an
upstream adapter or a bounded, tested host layer that preserves role, name,
state, focus, and action semantics.

## Mobile Validation

The Android build contract uses API 24 or later and target API 36. Building an
APK proves package construction only. The physical evidence file binds every
result to an APK hash and records each scenario independently:

- IME commit
- touch scrolling
- keyboard focus and inset handling
- Compact dialog and sheet behavior
- Android Back
- rotation and lifecycle recovery
- network and secure storage
- WebGPU pixels
- physical clipboard
- non-US hardware keyboard

Omitted scenarios remain `not_tested`; partial physical evidence never satisfies
the release gate. iOS results cannot be inferred from Android and require their
own signed-device or simulator capture on macOS.

## Commands

```bash
pnpm capture:wasm-web
pnpm check:wasm-web
pnpm capture:wasm-android
pnpm check:wasm-mobile
pnpm check:wasm-gate
```

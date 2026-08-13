# Vibex Mobile Shell

This Capacitor shell packages only `apps/mobile-wasm/dist` and exposes typed
safe-area, keyboard, lifecycle, storage, camera, file, share, URL, and network
capabilities. The bundled GPUI-WASM runtime is the only mobile UI; there is no
browser WebUI/PWA distribution or legacy React fallback.

## Validate

```bash
pnpm --filter @vibex/mobile validate
```

## Build Android Debug APK

The build script discovers the repository's supported JDK 21 and Android SDK,
generates the ignored native project when needed, syncs release GPUI-WASM
assets, and copies the resulting APK into the ignored artifacts directory.

```bash
pnpm --filter @vibex/mobile android:debug
pnpm --filter @vibex/mobile android:release
```

Output:

```text
apps/mobile/artifacts/vibex-gate-debug.apk
apps/mobile/artifacts/android-debug-build.json
```

The APK proves packaging only. Physical Android input, IME, rotation,
foreground/background, secure storage, WebGPU pixels, and resource behavior
remain pending until captured by `scripts/capture-wasm-mobile.mjs`.

On macOS/Xcode, the same mobile runtime build is validated in unsigned simulator Debug
and Release shells with `ios:debug` and `ios:release`. Signed physical-device
artifacts remain part of the cross-platform release Gate.

The build scripts regenerate the ignored native platform tree and install
`vibex://open/<transport>#/pair/...` plus
`dev.vibex.remote://open/<transport>#/pair/...` URL handlers. `<transport>` is
`direct`, `tailnet`, or `self_hosted_relay` and must match an option in the
signed pairing offer. Set `VIBEX_APP_LINK_HOST` on a deployment build to add an
HTTPS App/Universal Link host using `/open/<transport>`; the fragment is
scrubbed by the mobile runtime host before claim.

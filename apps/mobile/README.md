# Vibex Mobile Shell

This Capacitor shell packages only `apps/web/dist` and exposes only typed
safe-area, keyboard, lifecycle, storage, camera, file, share, URL, and network
capabilities. It is separate from
the legacy React mobile shell so the Gate cannot accidentally ship
`apps/web/dist`.

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

On macOS/Xcode, the same Web build is validated in unsigned simulator Debug
and Release shells with `ios:debug` and `ios:release`. Signed physical-device
artifacts remain part of the cross-platform release Gate.

The build scripts regenerate the ignored native platform tree and install the
`vibex://open#/pair/...` plus `dev.vibex.remote://open#/pair/...` URL handlers.
Set `VIBEX_APP_LINK_HOST` on a deployment build to add an HTTPS App/Universal
Link host; the fragment is still scrubbed by the shared Web host before claim.

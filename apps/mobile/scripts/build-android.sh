#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PROFILE="${VIBEX_MOBILE_PROFILE:-debug}"
RUST_FLAGS=()
GRADLE_TASKS=(assembleDebug)
ANDROID_TARGETS=(arm64-v8a x86_64)
GRADLE_ARGS=()

# cargo-ndk defaults to API 21, but the NDK only ships libnativewindow.so from
# API 26 on and gpui-pre-mobile links it. Keep this in step with minSdk in
# apps/mobile/android/app/build.gradle.
ANDROID_API="${VIBEX_MOBILE_ANDROID_API:-28}"

if [[ "$PROFILE" == "release" ]]; then
  RUST_FLAGS+=(--release)
  GRADLE_TASKS=(assembleRelease bundleRelease)
  ANDROID_TARGETS=(arm64-v8a)
fi

if [[ -n "${VIBEX_MOBILE_ANDROID_TARGETS:-}" ]]; then
  read -r -a ANDROID_TARGETS <<<"$VIBEX_MOBILE_ANDROID_TARGETS"
fi

if [[ -n "${VIBEX_MOBILE_VERSION:-}" ]]; then
  GRADLE_ARGS+=("-PvibexVersion=${VIBEX_MOBILE_VERSION}")
fi

NDK_TARGET_ARGS=()
for target in "${ANDROID_TARGETS[@]}"; do
  NDK_TARGET_ARGS+=(-t "$target")
done

cd "$ROOT"
command -v cargo-ndk >/dev/null || {
  echo "cargo-ndk is required (cargo install cargo-ndk)" >&2
  exit 1
}

# cargo-ndk writes only the libraries it just built, so anything an earlier
# build left behind - a debug build's unstripped .so, or an ABI this profile no
# longer targets - is still packaged into the APK. The directory is generated
# and gitignored, so clear it rather than merging into it.
JNI_LIBS_DIR=apps/mobile/android/app/src/main/jniLibs
rm -rf "$JNI_LIBS_DIR"

cargo ndk \
  "${NDK_TARGET_ARGS[@]}" \
  --platform "$ANDROID_API" \
  -o "$JNI_LIBS_DIR" \
  build -p vibex-mobile --lib "${RUST_FLAGS[@]}"

cd apps/mobile/android
./gradlew "${GRADLE_TASKS[@]}" "${GRADLE_ARGS[@]}"

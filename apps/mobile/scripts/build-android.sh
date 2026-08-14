#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PROFILE="${VIBEX_MOBILE_PROFILE:-debug}"
RUST_FLAGS=()
GRADLE_TASK="assembleDebug"
ANDROID_TARGETS=(arm64-v8a x86_64)

if [[ "$PROFILE" == "release" ]]; then
  RUST_FLAGS+=(--release)
  GRADLE_TASK="assembleRelease"
  ANDROID_TARGETS=(arm64-v8a)
fi

if [[ -n "${VIBEX_MOBILE_ANDROID_TARGETS:-}" ]]; then
  read -r -a ANDROID_TARGETS <<<"$VIBEX_MOBILE_ANDROID_TARGETS"
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

cargo ndk \
  "${NDK_TARGET_ARGS[@]}" \
  -o apps/mobile/android/app/src/main/jniLibs \
  build -p vibex-mobile --lib "${RUST_FLAGS[@]}"

cd apps/mobile/android
./gradlew "$GRADLE_TASK"

#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PROFILE="${VIBEX_MOBILE_PROFILE:-debug}"
PROFILE_DIR="debug"
RUST_FLAGS=()

if [[ "$PROFILE" == "release" ]]; then
  PROFILE_DIR="release"
  RUST_FLAGS+=(--release)
fi

cd "$ROOT"

resolve_staticlib() {
  local target="$1"
  local base="target/${target}/${PROFILE_DIR}"
  if [[ -f "${base}/libvibex_mobile.a" ]]; then
    printf '%s\n' "${base}/libvibex_mobile.a"
  elif [[ -f "${base}/deps/libvibex_mobile.a" ]]; then
    printf '%s\n' "${base}/deps/libvibex_mobile.a"
  else
    return 1
  fi
}

build_staticlib() {
  local target="$1"
  cargo build -p vibex-mobile --target "$target" "${RUST_FLAGS[@]}"
  resolve_staticlib "$target" || {
    cargo rustc -p vibex-mobile --target "$target" "${RUST_FLAGS[@]}" -- --crate-type staticlib
    resolve_staticlib "$target"
  }
}

DEVICE_LIB="$(build_staticlib aarch64-apple-ios)"
SIMULATOR_LIB="$(build_staticlib aarch64-apple-ios-sim)"
OUT="apps/mobile/ios/VibexFFI.xcframework"
TEMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/vibex-ios.XXXXXX")"
trap 'rm -rf "$TEMP_DIR"' EXIT

xcodebuild -create-xcframework \
  -library "$DEVICE_LIB" -headers apps/mobile/ios/Headers \
  -library "$SIMULATOR_LIB" -headers apps/mobile/ios/Headers \
  -output "$TEMP_DIR/VibexFFI.xcframework"

rm -rf "$OUT"
mv "$TEMP_DIR/VibexFFI.xcframework" "$OUT"

command -v xcodegen >/dev/null || {
  echo "xcodegen is required (brew install xcodegen)" >&2
  exit 1
}
(cd apps/mobile/ios && xcodegen generate)


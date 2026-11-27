#!/usr/bin/env bash
# Packages the nsh mobile app for Android.
#
# The nsh CLI builds the Rust library and the generated Android project; the
# SDK and NDK must already be installed. Signing is the generated project's
# (or a later CI step's) concern.
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v nsh >/dev/null 2>&1; then
    echo "error: the nsh CLI is not on PATH; see https://nsh.dev/docs/installation" >&2
    exit 1
fi

sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
if [[ -z "$sdk" ]]; then
    echo "error: set ANDROID_HOME (or ANDROID_SDK_ROOT) to the Android SDK" >&2
    exit 1
fi

ndk="${ANDROID_NDK_HOME:-}"
if [[ -z "$ndk" ]]; then
    ndk="$(find "$sdk/ndk" -maxdepth 1 -mindepth 1 -type d 2>/dev/null | sort -V | tail -1 || true)"
fi
if [[ -z "$ndk" ]]; then
    echo "error: no NDK found; set ANDROID_NDK_HOME or install one under $sdk/ndk" >&2
    exit 1
fi
export ANDROID_NDK_HOME="$ndk"

echo "packaging for Android with NDK $ndk"
nsh package . --target android "$@"

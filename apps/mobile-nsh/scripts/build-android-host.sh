#!/usr/bin/env bash
# Builds the custom Android host: a small Activity that mounts the Rust app
# (as libnsh.so) and lends it a ZXing camera scanner for pairing QR codes.
#
# Needs the Rust Android targets, the Android SDK (platform 35, build-tools
# 35, an NDK), JDK 21 and Gradle 8.9+ (GRADLE can name the binary). nsh is
# not involved: this skips its generated runner to own the activity.
set -euo pipefail

cd "$(dirname "$0")/.."
here="$(pwd)"
android="$here/android"

: "${ANDROID_HOME:?set ANDROID_HOME to the Android SDK}"
: "${JAVA_HOME:?set JAVA_HOME to a JDK 17-21 (Gradle does not run on 25)}"
GRADLE="${GRADLE:-gradle}"
command -v "$GRADLE" >/dev/null 2>&1 || {
    echo "error: Gradle 8.9+ is not on PATH (or set GRADLE)" >&2
    exit 1
}
ndk="${ANDROID_NDK_HOME:-}"
if [[ -z "$ndk" ]]; then
    ndk="$(find "$ANDROID_HOME/ndk" -maxdepth 1 -mindepth 1 -type d 2>/dev/null | sort -V | tail -1 || true)"
fi
[[ -n "$ndk" ]] || {
    echo "error: no NDK under $ANDROID_HOME/ndk; set ANDROID_NDK_HOME" >&2
    exit 1
}
toolchain="$ndk/toolchains/llvm/prebuilt/linux-x86_64"

build_abi() {
    local triple="$1" abi="$2" cc="$3" ar="$4"
    echo "building libnsh.so for $abi"
    env "CC_$(echo "$triple" | tr '-' '_')=$toolchain/bin/$cc" \
        "AR_$(echo "$triple" | tr '-' '_')=$toolchain/bin/$ar" \
        "CARGO_TARGET_$(echo "$triple" | tr '-' '_' | tr '[:lower:]' '[:upper:]')_LINKER=$toolchain/bin/$cc" \
        cargo build --release -p vibex-mobile-nsh --target "$triple"
    mkdir -p "$android/app/src/main/jniLibs/$abi"
    cp "$here/../../target/$triple/release/libvibex_mobile_nsh.so" \
        "$android/app/src/main/jniLibs/$abi/libnsh.so"
}

build_abi aarch64-linux-android arm64-v8a aarch64-linux-android26-clang llvm-ar
build_abi armv7-linux-androideabi armeabi-v7a armv7a-linux-androideabi26-clang llvm-ar
build_abi x86_64-linux-android x86_64 x86_64-linux-android26-clang llvm-ar

# The nsh AAR without its own libnsh.so: the app's Rust library replaces it
# so NshView and Native.runRust are this app.
mkdir -p "$android/app/libs"
if [[ ! -f "$android/app/libs/nsh.aar" ]]; then
    zip="${NSH_EMBED_ZIP:-$HOME/tmp/nsh-embed-android.zip}"
    if [[ ! -f "$zip" ]]; then
        echo "downloading the nsh embed AAR"
        curl -fsSL -o "$zip" \
            "https://github.com/longbridge/nsh/releases/download/v0.6.0/nsh-embed-android.zip"
    fi
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT
    unzip -q "$zip" -d "$work"
    mkdir "$work/aar"
    (cd "$work/aar" && unzip -q "$work/nsh-embed-android/nsh.aar" && rm -rf jni && zip -qr "$android/app/libs/nsh.aar" .)
fi

cd "$android"
"$GRADLE" -q :app:assembleRelease
apk="$android/app/build/outputs/apk/release/app-release.apk"
[[ -f "$apk" ]] || {
    echo "error: Gradle did not produce $apk" >&2
    exit 1
}
echo "packaged $apk"

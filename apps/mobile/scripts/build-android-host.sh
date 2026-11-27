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
# The project's own wrapper when it is there (it downloads the pinned Gradle and
# is what CI uses), otherwise whatever `gradle` the machine has.
if [[ -z "${GRADLE:-}" ]]; then
    if [[ -x "$android/gradlew" ]]; then
        GRADLE="$android/gradlew"
    else
        GRADLE="gradle"
    fi
fi
("$GRADLE" --version >/dev/null 2>&1) || {
    echo "error: Gradle 8.9+ is not available (project wrapper missing, or set GRADLE)" >&2
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
        cargo build --release -p vibex-mobile --target "$triple"
    mkdir -p "$android/app/src/main/jniLibs/$abi"
    cp "$here/../../target/$triple/release/libvibex_mobile.so" \
        "$android/app/src/main/jniLibs/$abi/libnsh.so"
}

# A published APK ships arm64 only, as the GPUI client's release did: the other
# ABIs exist for a local emulator run. Override with VIBEX_MOBILE_ABIS.
# A local build packages every ABI the host script produced, so the emulator and
# a phone both run from the same APK.
abis="${VIBEX_MOBILE_ABIS:-arm64-v8a,armeabi-v7a,x86_64}"

# minSdk is 28 in `app/build.gradle`: the API level here has to match it, or the
# NDK links against a libc the app does not ship.
api="${VIBEX_MOBILE_ANDROID_API:-28}"
for abi in ${abis//,/ }; do
    case "$abi" in
        arm64-v8a)
            build_abi aarch64-linux-android arm64-v8a "aarch64-linux-android${api}-clang" llvm-ar
            ;;
        armeabi-v7a)
            build_abi armv7-linux-androideabi armeabi-v7a "armv7a-linux-androideabi${api}-clang" llvm-ar
            ;;
        x86_64)
            build_abi x86_64-linux-android x86_64 "x86_64-linux-android${api}-clang" llvm-ar
            ;;
        *)
            echo "error: unknown ABI ${abi} (arm64-v8a, armeabi-v7a, x86_64)" >&2
            exit 1
            ;;
    esac
done

# The nsh AAR without its own libnsh.so: the app's Rust library replaces it
# so NshView and Native.runRust are this app.
#
# Its `classes.jar` is compiled from the nsh sources this workspace pins, not
# taken from the released embed archive. The released AAR carries 0.6.1's Java,
# and the modal keyboard fix this app needs lives in our fork's Java on top of
# that release — a Java-only change, so `cargo` fetches the crates and never
# the AAR. Taking the archive would silently drop the fix again: a dialog holds
# its own window and its own IME, and without `keyboardFromModal` the app reads
# `env(keyboard-inset-height)` as zero inside every sheet and lays its fields
# under the keys.
mkdir -p "$android/app/libs"
nsh_src="$(cargo metadata --format-version 1 --locked 2>/dev/null \
    | python3 -c 'import json,sys; print(next(p["manifest_path"] for p in json.load(sys.stdin)["packages"] if p["name"] == "nsh-platform-android"))' \
    | xargs dirname)"
[[ -d "$nsh_src/java" ]] || {
    echo "error: nsh-platform-android's Java sources are not at $nsh_src/java" >&2
    exit 1
}
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
echo "compiling the nsh Java runtime from $nsh_src/java"
find "$nsh_src/java" -name '*.java' > "$work/sources.txt"
"$JAVA_HOME/bin/javac" --release 17 -Xlint:-options -encoding UTF-8 \
    -classpath "$ANDROID_HOME/platforms/android-35/android.jar" \
    -d "$work/classes" @"$work/sources.txt"
"$JAVA_HOME/bin/jar" cf "$work/classes.jar" -C "$work/classes" .
# The AAR's own files: the manifest, the empty R, the R8 rules, and the Java.
# `jni/` stays out — the Rust library above is what the APK ships.
cat > "$work/AndroidManifest.xml" <<'MANIFEST'
<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.longbridge.nsh">
    <uses-sdk android:minSdkVersion="26" />
    <uses-permission android:name="android.permission.INTERNET" />
    <!-- Text actions (Translate and the like) the selection menu offers,
        as TextView's: Android 11+ hides other apps' activities otherwise. -->
    <queries>
        <intent>
            <action android:name="android.intent.action.PROCESS_TEXT" />
            <data android:mimeType="text/plain" />
        </intent>
    </queries>
</manifest>
MANIFEST
: > "$work/R.txt"
cat > "$work/proguard.txt" <<'RULES'
# Native Shell: libnsh.so finds these classes and members by name over JNI
# and binds native methods by their Java names.
-keep class com.longbridge.nsh.** { *; }
# The WebSocket client's TLS verifier (rustls-platform-verifier) calls these
# over JNI.
-keep class org.rustls.platformverifier.** { *; }
RULES
rm -f "$android/app/libs/nsh.aar"
(cd "$work" && zip -qr "$android/app/libs/nsh.aar" \
    AndroidManifest.xml classes.jar proguard.txt R.txt)

cd "$android"
# The release workflow stamps the tag's version in; a local build uses the one
# `build.gradle` derives the versionCode from. The two have to agree: Android
# only replaces an installed app whose versionCode the new one reaches.
gradle_args=("-PvibexAbis=${abis}")
if [[ -n "${VIBEX_MOBILE_VERSION:-}" ]]; then
    gradle_args+=("-PvibexVersion=${VIBEX_MOBILE_VERSION}")
fi
"$GRADLE" -q :app:assembleRelease :app:bundleRelease "${gradle_args[@]}"
# The release build is unsigned on purpose: `scripts/sign-android-release.sh`
# signs it with the release keystore, which is the other half of an in-place
# upgrade (a different signing key is as fatal as a different application id).
apk="$android/app/build/outputs/apk/release/app-release-unsigned.apk"
[[ -f "$apk" ]] || {
    echo "error: Gradle did not produce $apk" >&2
    exit 1
}
aab="$android/app/build/outputs/bundle/release/app-release.aab"
[[ -f "$aab" ]] || {
    echo "error: Gradle did not produce $aab" >&2
    exit 1
}
echo "packaged $apk (unsigned) and $aab"

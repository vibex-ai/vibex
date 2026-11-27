#!/usr/bin/env bash
# Builds the custom iOS host: the Swift shell in `ios/` around the Rust app,
# with the pairing scanner, Bonjour discovery, notifications, the camera, the
# share extension, the quick action and the background task that iOS has
# instead of a foreground service.
#
# Needs macOS with Xcode (and its command-line tools), XcodeGen, and the Rust
# iOS targets. `nsh` is not involved: like `build-android-host.sh`, this skips
# the CLI's generated runner to own the application delegate.
#
#   rustup target add aarch64-apple-ios aarch64-apple-ios-sim
#   brew install xcodegen
#
# The static library is built for the device and the Apple-silicon simulator
# and merged into one universal archive; `ios/Frameworks/` and the generated
# `Vibex.xcodeproj` are build products and are not tracked.
set -euo pipefail

cd "$(dirname "$0")/.."
here="$(pwd)"
ios="$here/ios"
profile="${VIBEX_MOBILE_PROFILE:-release}"

flags=()
profile_dir="debug"
if [[ "$profile" == "release" ]]; then
    flags+=(--release)
    profile_dir="release"
fi

for tool in xcodegen xcodebuild lipo; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "error: $tool is required (Xcode's command-line tools and 'brew install xcodegen')" >&2
        exit 1
    }
done

build_staticlib() {
    local triple="$1"
    echo "building libvibex_mobile_nsh.a for $triple"
    cargo rustc -p vibex-mobile-nsh --target "$triple" --lib --crate-type staticlib "${flags[@]}"
    local lib="target/$triple/$profile_dir/libvibex_mobile_nsh.a"
    [[ -f "$lib" ]] || {
        echo "error: $lib was not produced" >&2
        exit 1
    }
    printf '%s\n' "$lib"
}

device_lib="$(build_staticlib aarch64-apple-ios)"
simulator_lib="$(build_staticlib aarch64-apple-ios-sim)"

mkdir -p "$ios/Frameworks"
rm -f "$ios/Frameworks/libvibex_mobile_nsh.a"
lipo -create "$device_lib" "$simulator_lib" -output "$ios/Frameworks/libvibex_mobile_nsh.a"

# The version the tag stamps in: `MARKETING_VERSION` from `project.yml` is the
# development default, and the release workflow passes the tag's. Both clients
# share a bundle id, so the version is also what an installer's "update" line
# shows.
#
# `CFBundleVersion` is derived from it with the Android versionCode's arithmetic
# (`0.1.9` → `109999`) rather than left at the project's literal 1: iOS compares
# build numbers when it decides whether a bundle may replace the installed one,
# and the GPUI client shipped build 1 for every release, which leaves no room to
# grow. The expression mirrors `apps/mobile-nsh/android/app/build.gradle`.
version_args=()
if [[ -n "${VIBEX_MOBILE_VERSION:-}" ]]; then
    if [[ ! "${VIBEX_MOBILE_VERSION}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(-(preview|rc)\.([0-9]+))?$ ]]; then
        echo "error: cannot derive an iOS build number from '${VIBEX_MOBILE_VERSION}'" >&2
        exit 1
    fi
    major="${BASH_REMATCH[1]}"
    minor="${BASH_REMATCH[2]}"
    patch="${BASH_REMATCH[3]}"
    channel="${BASH_REMATCH[4]}"
    sequence="${BASH_REMATCH[6]:-0}"
    case "${channel}" in
        "") rank=999 ;;
        rc) rank=$((500 + sequence)) ;;
        *) rank=$((100 + sequence)) ;;
    esac
    build_number=$((major * 10000000 + minor * 100000 + patch * 1000 + rank))
    version_args+=("MARKETING_VERSION=${VIBEX_MOBILE_VERSION}" "CURRENT_PROJECT_VERSION=${build_number}")
fi

(cd "$ios" && xcodegen generate --spec project.yml)

# Signing stays the project's: a device build needs a team, a simulator build
# does not. Pass extra `xcodebuild` arguments through this script.
if [[ "${VIBEX_IOS_SKIP_XCODEBUILD:-}" == "1" ]]; then
    echo "generated $ios/Vibex.xcodeproj (xcodebuild skipped)"
    exit 0
fi

destination="${VIBEX_IOS_DESTINATION:-generic/platform=iOS}"
echo "building for $destination"
xcodebuild \
    -project "$ios/Vibex.xcodeproj" \
    -scheme Vibex \
    -configuration "$( [[ "$profile" == release ]] && echo Release || echo Debug )" \
    -destination "$destination" \
    -derivedDataPath "$here/../../target/mobile-nsh-ios" \
    CODE_SIGNING_ALLOWED="${VIBEX_IOS_SIGNING_ALLOWED:-YES}" \
    "${version_args[@]}" \
    "$@" \
    build

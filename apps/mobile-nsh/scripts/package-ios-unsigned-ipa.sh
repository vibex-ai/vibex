#!/usr/bin/env bash
# Packages the unsigned iOS device IPA of the nsh client.
#
# Same contract as `apps/mobile/scripts/package-ios-unsigned-ipa.sh` — and the
# same idea: producing the file needs no Apple account, certificate or
# provisioning profile, because whoever installs it re-signs it with their own
# free Apple ID. The bundle id is the GPUI client's (`ai.vibex.mobile`), so a
# re-signed build replaces the installed app instead of landing beside it, and
# the installer keeps its data. See docs/operations/ios-unsigned-install.md.
#
# The device build itself is `build-ios-host.sh`'s, so the version and build
# number stamped into the bundle are the ones the release builds — one
# derivation, not two that can drift.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$ROOT"

DERIVED_DATA="target/mobile-nsh-ios"
OUTPUT_DIR="${VIBEX_MOBILE_OUTPUT_DIR:-target/mobile-nsh-ios-artifacts}"

command -v xcodebuild >/dev/null 2>&1 || {
  echo "xcodebuild is required (this step runs on macOS with Xcode installed)" >&2
  exit 1
}

resolve_version() {
  if [[ -n "${VIBEX_MOBILE_VERSION:-}" ]]; then
    printf '%s\n' "${VIBEX_MOBILE_VERSION}"
    return
  fi
  local version
  version="$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' apps/mobile-nsh/Cargo.toml)"
  printf '%s\n' "$version"
}

VERSION="$(resolve_version)"
if [[ -z "$VERSION" ]]; then
  echo "could not resolve a marketing version; set VIBEX_MOBILE_VERSION" >&2
  exit 1
fi

# Unsigned on purpose: a build that quietly acquired a signature would no
# longer be reproducible without credentials, and the artifact's name would
# stop describing it.
VIBEX_MOBILE_PROFILE=release \
VIBEX_MOBILE_VERSION="$VERSION" \
VIBEX_IOS_DESTINATION="generic/platform=iOS" \
VIBEX_IOS_SIGNING_ALLOWED=NO \
  bash apps/mobile-nsh/scripts/build-ios-host.sh

APP="${DERIVED_DATA}/Build/Products/Release-iphoneos/Vibex.app"
if [[ ! -d "$APP" ]]; then
  echo "expected the device bundle at ${APP}" >&2
  exit 1
fi

if [[ -e "${APP}/_CodeSignature" || -e "${APP}/embedded.mobileprovision" ]]; then
  echo "refusing to publish a signed bundle as an unsigned IPA" >&2
  exit 1
fi

# The bundle id is the upgrade: a different one would install a second app and
# leave the GPUI client and its data untouched.
bundle_id="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "${APP}/Info.plist")"
if [[ "${bundle_id}" != "ai.vibex.mobile" ]]; then
  echo "the built app is ${bundle_id}, not ai.vibex.mobile: an update would install beside the old app" >&2
  exit 1
fi

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
IPA="${OUTPUT_DIR}/vibex-${VERSION}-ios-unsigned.ipa"

STAGING="$(mktemp -d "${TMPDIR:-/tmp}/vibex-mobile-nsh-ios-ipa.XXXXXX")"
trap 'rm -rf "$STAGING"' EXIT
mkdir -p "${STAGING}/Payload"
ditto "$APP" "${STAGING}/Payload/Vibex.app"

# An IPA is a zip whose root holds a Payload directory.
(cd "$STAGING" && ditto -c -k --sequesterRsrc --keepParent Payload "$IPA")

listing="$(unzip -l "$IPA")"
if [[ "$listing" != *"Payload/Vibex.app/Info.plist"* ]]; then
  echo "packaged IPA has no Payload/Vibex.app root" >&2
  exit 1
fi

(cd "$OUTPUT_DIR" && shasum -a 256 "$(basename "$IPA")" > "$(basename "$IPA").sha256")

printf '%s\n' "$IPA"
echo "unsigned: the installer re-signs it with their own Apple ID" >&2
echo "see docs/operations/ios-unsigned-install.md" >&2

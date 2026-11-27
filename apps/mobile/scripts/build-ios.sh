#!/usr/bin/env bash
# Packages the nsh mobile app for iOS.
#
# The nsh CLI builds the Rust library and the generated Xcode project; Xcode
# and its command-line tools must already be installed. Signing and the App
# Store upload are the generated project's (or a later CI step's) concern.
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v nsh >/dev/null 2>&1; then
    echo "error: the nsh CLI is not on PATH; see https://nsh.dev/docs/installation" >&2
    exit 1
fi
if ! command -v xcodebuild >/dev/null 2>&1; then
    echo "error: Xcode's command-line tools are required" >&2
    exit 1
fi

echo "packaging for iOS"
nsh package . --target ios "$@"

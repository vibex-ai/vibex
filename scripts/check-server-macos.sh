#!/bin/sh
# Verify a macOS server binary before packaging. Run on macOS.
set -eu

BINARY=${1:?usage: check-server-macos.sh <binary> <x86_64|aarch64>}
ARCH=${2:?usage: check-server-macos.sh <binary> <x86_64|aarch64>}
test -x "$BINARY" || { echo "check-server-macos: binary is not executable" >&2; exit 1; }

case "$ARCH" in
  aarch64) EXPECTED="arm64" ;;
  x86_64) EXPECTED="x86_64" ;;
  *) echo "check-server-macos: unsupported arch $ARCH" >&2; exit 1 ;;
esac

file "$BINARY" | grep -q "Mach-O 64-bit executable $EXPECTED" || {
  echo "check-server-macos: unexpected Mach-O architecture" >&2
  file "$BINARY" >&2
  exit 1
}
codesign --verify "$BINARY"
if xattr -p com.apple.quarantine "$BINARY" >/dev/null 2>&1; then
  echo "check-server-macos: binary has a quarantine attribute" >&2
  exit 1
fi
echo "check-server-macos: ok $BINARY ($ARCH)"

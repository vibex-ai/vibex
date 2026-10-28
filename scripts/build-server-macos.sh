#!/bin/sh
# Build and package the headless server on an Apple Silicon Mac.
#
# Usage:
#   scripts/build-server-macos.sh [version]
#
# The tarball contains exactly `vibex-server`; no Developer ID or notarization
# is attempted. The binary is linker/ad-hoc signed and is uploaded over SSH by
# the desktop, so it never acquires a quarantine attribute.
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
  echo "build-server-macos: run this on macOS" >&2
  exit 1
fi

VERSION=${1:-$(node -p "require('./package.json').version")}
ARCH=aarch64
OUT=target/server-artifacts-macos
STAGE=$(mktemp -d "${TMPDIR:-/tmp}/vibex-server-macos.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT INT TERM

PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
cargo build -p vibex-server --release --locked
BINARY=target/release/vibex-server
test -x "$BINARY"
codesign --verify "$BINARY"

cp "$BINARY" "$STAGE/vibex-server"
chmod 755 "$STAGE/vibex-server"
mkdir -p "$OUT"
ARTIFACT="$OUT/vibex-server-${VERSION}-macos-${ARCH}.tar.gz"
tar -czf "$ARTIFACT" -C "$STAGE" vibex-server
shasum -a 256 "$ARTIFACT" > "$ARTIFACT.sha256"
echo "$ARTIFACT"
cat "$ARTIFACT.sha256"

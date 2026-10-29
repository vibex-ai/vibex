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
ARCH=${VIBEX_MACOS_ARCH:-aarch64}
case "$ARCH" in
  aarch64) TARGET="" ;;
  x86_64) TARGET="x86_64-apple-darwin" ;;
  *) echo "build-server-macos: unsupported arch $ARCH" >&2; exit 1 ;;
esac
OUT=target/server-artifacts-macos
STAGE=$(mktemp -d "${TMPDIR:-/tmp}/vibex-server-macos.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT INT TERM

PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
if [ -n "$TARGET" ]; then
  rustup target add "$TARGET" >/dev/null 2>&1 || true
  cargo build -p vibex-server --release --locked --target "$TARGET"
  BINARY="target/$TARGET/release/vibex-server"
else
  cargo build -p vibex-server --release --locked
  BINARY=target/release/vibex-server
fi
test -x "$BINARY"
codesign --force --sign - "$BINARY"
codesign --verify "$BINARY"

cp "$BINARY" "$STAGE/vibex-server"
chmod 755 "$STAGE/vibex-server"
mkdir -p "$OUT"
ARTIFACT="$OUT/vibex-server-${VERSION}-macos-${ARCH}.tar.gz"
tar -czf "$ARTIFACT" -C "$STAGE" vibex-server
shasum -a 256 "$ARTIFACT" > "$ARTIFACT.sha256"
echo "$ARTIFACT"
cat "$ARTIFACT.sha256"

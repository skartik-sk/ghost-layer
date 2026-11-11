#!/bin/sh
# Install the prebuilt `ghost` and `zcash-devtool` binaries from the latest GitHub release.
#   curl -fsSL https://github.com/skartik-sk/ghost-layer/releases/latest/download/install.sh | sh
# Options (env): GHOST_VERSION=v0.2.0 (default: latest), GHOST_BIN_DIR=~/.local/bin
# The archive's SHA-256 is checked before anything is installed. No sudo.
set -eu

REPO="skartik-sk/ghost-layer"
VERSION="${GHOST_VERSION:-latest}"
BIN_DIR="${GHOST_BIN_DIR:-$HOME/.local/bin}"

die() { echo "ghost install: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "needs '$1' on PATH"; }
need curl; need tar; need uname

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)  TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64) TARGET=x86_64-apple-darwin ;;
  Linux-x86_64)  TARGET=x86_64-unknown-linux-gnu ;;
  *) die "no prebuilt binary for $(uname -s) $(uname -m); build from source: cargo install ghost-layer" ;;
esac

if [ "$VERSION" = latest ]; then
  VERSION=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" | sed 's#.*/tag/##')
  [ -n "$VERSION" ] || die "could not find the latest release"
fi
case "$VERSION" in v[0-9]*) ;; *) die "bad version '$VERSION' (expected like v0.2.0)" ;; esac

NAME="ghost-$VERSION-$TARGET"
URL="https://github.com/$REPO/releases/download/$VERSION/$NAME.tar.gz"
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT

echo "Downloading $NAME"
curl -fsSL "$URL" -o "$TMP/$NAME.tar.gz" || die "download failed: $URL"
curl -fsSL "$URL.sha256" -o "$TMP/$NAME.tar.gz.sha256" || die "checksum download failed"

want=$(awk '{print $1}' "$TMP/$NAME.tar.gz.sha256")
if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum "$TMP/$NAME.tar.gz" | awk '{print $1}')
else got=$(shasum -a 256 "$TMP/$NAME.tar.gz" | awk '{print $1}'); fi
[ "$want" = "$got" ] || die "checksum mismatch (expected $want, got $got)"

tar -xzf "$TMP/$NAME.tar.gz" -C "$TMP"
mkdir -p "$BIN_DIR"
for b in ghost zcash-devtool; do
  install -m 0755 "$TMP/$NAME/$b" "$BIN_DIR/$b"
done
echo "Installed ghost and zcash-devtool $VERSION to $BIN_DIR"

case ":$PATH:" in *":$BIN_DIR:"*) ;; *) echo "Add it to your PATH:  export PATH=\"$BIN_DIR:\$PATH\"" ;; esac
echo "Next:  ghost init"

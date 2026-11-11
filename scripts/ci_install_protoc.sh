#!/usr/bin/env bash
# CI: install protoc. macOS: Homebrew. Linux: apt, and if the Ubuntu archive is unreachable (it happens),
# the official protoc release, checked against a pinned SHA-256.
set -euo pipefail
if [ "$(uname -s)" = Darwin ]; then brew install protobuf; exit 0; fi
if sudo apt-get update && sudo apt-get install -y protobuf-compiler; then exit 0; fi
echo "apt failed; installing the pinned protoc release instead"
V=29.3
SHA=3e866620c5be27664f3d2fa2d656b5f3e09b5152b42f1bedbf427b333e90021a
T=$(mktemp -d)
curl -fsSL --retry 3 -o "$T/protoc.zip" "https://github.com/protocolbuffers/protobuf/releases/download/v$V/protoc-$V-linux-x86_64.zip"
echo "$SHA  $T/protoc.zip" | sha256sum -c -
sudo unzip -o -q "$T/protoc.zip" -d /usr/local bin/protoc 'include/*'
protoc --version

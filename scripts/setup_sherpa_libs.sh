#!/usr/bin/env bash
# Download the prebuilt sherpa-onnx static libs and stash them
# under ~/.local/share/sherpa-onnx/lib so the build script can
# find them without re-downloading.
#
# Why this exists:
#   sherpa-onnx-sys (a transitive dep of `voice`) downloads a
#   ~20 MB tar.bz2 of prebuilt C/C++ libs from GitHub Releases
#   at build time. That download uses ureq with a 10-second
#   default timeout, which is too short on a flaky network and
#   makes the build look like a compile error when it's really
#   just a transient network blip. By stashing the archive
#   locally and setting SHERPA_ONNX_LIB_DIR, we sidestep the
#   network entirely.
#
# Run once after cloning:
#   ./scripts/setup_sherpa_libs.sh
#
# Then build normally:
#   cargo build
#
# The matching `.cargo/config.toml` then sets SHERPA_ONNX_LIB_DIR
# for every `cargo` invocation in this workspace, so no manual
# env var is needed.

set -euo pipefail

VERSION="${SHERPA_ONNX_VERSION:-1.13.2}"
DEST="${HOME}/.local/share/sherpa-onnx/lib"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

if [[ -d "$DEST" && -f "$DEST/libsherpa-onnx-c-api.a" ]]; then
    echo "sherpa-onnx libs already present at $DEST, skipping download."
    exit 0
fi

mkdir -p "$DEST"
URL="https://github.com/k2-fsa/sherpa-onnx/releases/download/v${VERSION}/sherpa-onnx-v${VERSION}-linux-x64-static-lib.tar.bz2"
ARCHIVE="$TMP/sherpa.tar.bz2"

echo "Downloading $URL ..."
curl -fsSL --retry 3 --retry-delay 2 -o "$ARCHIVE" "$URL"

echo "Extracting..."
tar -xjf "$ARCHIVE" -C "$TMP"
cp "$TMP"/sherpa-onnx-v${VERSION}-linux-x64-static-lib/lib/* "$DEST/"

echo "Installed to $DEST"
ls "$DEST" | head

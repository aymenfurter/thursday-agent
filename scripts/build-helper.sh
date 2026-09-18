#!/usr/bin/env bash
# Builds the native overlay helper into helper/build/thursday-agent Helper.app
set -euo pipefail
cd "$(dirname "$0")/../helper"
ARCH="$(uname -m)"
APP="build/thursday-agent Helper.app"
OUT="$APP/Contents"
mkdir -p "$OUT/MacOS" "$OUT/Resources"
export TMPDIR="$PWD/build"
binary="build/thursday-agent-helper-$$"
[[ ! -e "$binary" ]]
trap 'rm -f "$binary"' EXIT
swiftc -O \
  -module-cache-path build/module-cache \
  -target "${ARCH}-apple-macos14.0" \
  -framework AppKit -framework ApplicationServices -framework CoreGraphics -framework Metal -framework MetalKit -framework ScreenCaptureKit -framework CoreMedia -framework CoreVideo \
  -o "$binary" \
  Sources/*.swift
mv "$binary" "$OUT/MacOS/thursday-agent-helper"
cp Info.plist "$OUT/Info.plist"
cp Sources/Shaders.metal "$OUT/Resources/Shaders.metal"
codesign --force --sign - "$APP" >/dev/null 2>&1 || true
echo "built $APP"

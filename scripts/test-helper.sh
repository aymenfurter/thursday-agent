#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p helper/build
export TMPDIR="$PWD/helper/build"
binary="helper/build/thursday-helper-tests-$$"
[[ ! -e "$binary" ]]
trap 'rm -f "$binary"' EXIT
swiftc \
  -module-cache-path helper/build/module-cache \
  helper/Sources/WindowTracking.swift helper/Tests/WindowTrackingTests.swift \
  -o "$binary"
"$binary"
swiftc \
  -module-cache-path helper/build/module-cache \
  helper/Sources/Protocol.swift helper/Tests/ProtocolTests.swift \
  -o "$binary"
"$binary"
swiftc \
  -module-cache-path helper/build/module-cache \
  helper/Sources/ScreenFeedLifecycle.swift helper/Tests/ScreenFeedLifecycleTests.swift \
  -o "$binary"
"$binary"

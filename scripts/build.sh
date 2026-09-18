#!/usr/bin/env bash
# Builds everything: the Swift helper and the Rust binary (release).
set -euo pipefail
cd "$(dirname "$0")/.."
scripts/build-helper.sh
cargo build --release --target-dir target
echo "binary: target/release/thursday-agent"

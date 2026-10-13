#!/usr/bin/env bash
# Builds the example WASM plugins (ADR-019).
# Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
#
# Usage: scripts/build-spike-plugins.sh
#
# Output: examples/plugins/<name>/plugin.wasm
# The integration tests hard-fail (not skip) when the artifacts are missing —
# run this script first. Requires the wasm32-wasip1 target:
#   rustup target add wasm32-wasip1
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"

command -v cargo >/dev/null || { echo "cargo not found — install rustup first" >&2; exit 1; }

PLUGINS=(hello sleepy mute)
for p in "${PLUGINS[@]}"; do
    dir="$ROOT/examples/plugins/$p"
    echo "==> building $p"
    (cd "$dir" && cargo build --release --target wasm32-wasip1)
    # crate name == aether-plugin-example-<name> → artifact aether_plugin_example_<name>.wasm
    cp "$dir/target/wasm32-wasip1/release/aether_plugin_example_$p.wasm" "$dir/plugin.wasm"
done

echo "==> artifacts:"
ls -la "$ROOT"/examples/plugins/*/plugin.wasm

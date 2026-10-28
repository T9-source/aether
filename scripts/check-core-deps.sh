#!/usr/bin/env bash
# ADR-014 enforcement: aether-core must have zero infrastructure/protocol
# dependencies. Checks the [dependencies] section (comments stripped) for
# forbidden markers and fails CI on a hit. A test-only runtime under
# [dev-dependencies] is allowed. Keep in sync with
# docs/adr/ADR-014-core-dependency-policy.md.
# Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).
set -euo pipefail

MANIFEST="crates/aether-core/Cargo.toml"

# strip comment lines so the policy text itself never triggers a match
CODE=$(grep -v '^\s*#' "$MANIFEST")

# extract one section (from [name] to the next [header]) as plain lines
section() {
  echo "$CODE" | sed -n "/^\[$1\]/,/^\[/p" | grep -v '^\[' || true
}

DEPS=$(section dependencies)
DEVDEPS=$(section dev-dependencies)

fail() {
  echo ""
  echo "aether-core must have ZERO infrastructure/protocol dependencies."
  echo "See docs/adr/ADR-014-core-dependency-policy.md before touching deps."
  exit 1
}

# 1. infrastructure feature markers inside [dependencies] (quoted names)
for marker in '"net"' '"fs"' '"rt"' '"rt-multi-thread"' '"full"' '"signal"' '"io-util"' '"macros"'; do
  if echo "$DEPS" | grep -qF "$marker"; then
    echo "FORBIDDEN feature marker $marker in [dependencies] of $MANIFEST (ADR-014)"
    fail
  fi
done

# 2. infrastructure/protocol crates anywhere in [dependencies]
for marker in hyper rustls tokio-rustls tokio-tungstenite sqlx tauri reqwest rcgen; do
  if echo "$DEPS" | grep -qE "^[a-z_-]*${marker}|\"${marker}|= \"${marker}| ${marker} ="; then
    echo "FORBIDDEN crate '${marker}' in [dependencies] of $MANIFEST (ADR-014)"
    fail
  fi
done

# 3. tokio must be declared primitives-only (sync + time), not inherited
#    from the workspace (whose features serve the daemon/http crates)
if ! echo "$DEPS" | grep -qF 'tokio = { version = "1", default-features = false, features = ["sync", "time"] }'; then
  echo "ADR-014: core must declare tokio primitives-only (sync, time), not workspace-inherited"
  fail
fi

# 4. a test-only runtime is allowed — but only under [dev-dependencies]
if ! echo "$DEVDEPS" | grep -qF '"rt"'; then
  echo "ADR-014: [dev-dependencies] must carry the tokio runtime for #[tokio::test]"
  fail
fi

echo "ADR-014 check passed: aether-core has zero infrastructure/protocol deps."

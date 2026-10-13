#!/usr/bin/env bash
# Performance regression gate (ADR-018 item 5).
# Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
#
# Runs `aether bench proxy` against the in-repo baseline
# (docs/bench/baseline.json) with explicit factors. CI-visible drift is a
# failure (ADR-018). Budget policy lives in docs/PERF.md:
#   p50/p95  <= 2.0x baseline   (real regressions move medians, not outliers)
#   p99      <= 3.0x baseline OR baseline + 50ms, whichever is larger
#            (tail outliers are scheduler/SQLite-checkpoint noise on shared CI)
#   overhead <= 2.0x baseline OR baseline + 2ms, whichever is larger
#            (the proxy's *added* latency is the product promise)
#
# Usage: scripts/check-perf-budget.sh [requests-per-path]
# Env:   BENCH_REQUESTS, BASELINE (default docs/bench/baseline.json)
set -euo pipefail
cd "$(dirname "$0")/.."

REQUESTS="${1:-${BENCH_REQUESTS:-300}}"
BASELINE="${BASELINE:-docs/bench/baseline.json}"
CURRENT="$(mktemp /tmp/aether-bench-current.XXXXXX.json)"
trap 'rm -f "$CURRENT"' EXIT

BIN="./target/release/aether"
if [ ! -x "$BIN" ]; then
  echo "error: $BIN not found — build first: cargo build --release -p aether-cli" >&2
  exit 2
fi

"$BIN" bench proxy --requests "$REQUESTS" --json "$CURRENT" > /dev/null

python3 - "$BASELINE" "$CURRENT" <<'PY'
import json, sys, math

baseline = json.load(open(sys.argv[1]))
current = json.load(open(sys.argv[2]))

def ok(metric, cur, base, factor, floor_ms=0.0):
    limit = max(base * factor, base + floor_ms)
    verdict = cur <= limit
    print(f"  {metric:<38} cur={cur:7.3f}ms base={base:7.3f}ms limit={limit:7.3f}ms  {'ok' if verdict else 'REGRESSION'}")
    return verdict

print("perf budget check (ADR-018 item 5):")
fail = False
for path in ("direct_tls", "via_proxy_plain", "via_proxy_mitm"):
    b, c = baseline[path], current[path]
    for p, factor, floor in (("p50_ms", 2.0, 1.0), ("p95_ms", 2.0, 2.0), ("p99_ms", 3.0, 50.0)):
        fail |= not ok(f"{path}.{p}", c[p], b[p], factor, floor)
bo = baseline["overhead_p50_ms"]; co = current["overhead_p50_ms"]
fail |= not ok("overhead_p50_ms.plain", co["plain"], bo["plain"], 2.0, 2.0)
fail |= not ok("overhead_p50_ms.mitm", co["mitm"], bo["mitm"], 2.0, 2.0)

if fail:
    print("perf gate: FAIL — a budget was breached. Profile before optimizing; refresh the baseline only with a documented reason (docs/PERF.md).")
    sys.exit(1)
print("perf gate: PASS")
PY

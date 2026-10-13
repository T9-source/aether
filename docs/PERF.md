# Performance budgets (ADR-018 item 5)

The gate is `scripts/check-perf-budget.sh`: it re-runs `aether bench proxy`
on the release build and compares against the in-repo baseline
(`docs/bench/baseline.json`). CI-visible drift is a failure — the same way a
failing test is.

## Baseline (captured 2026-10-11, release build, 400 req/path, container CI-class CPU)

| Path               | p50    | p95    | p99     |
|--------------------|--------|--------|---------|
| direct (TLS)       | 0.29ms | 0.33ms | 0.42ms  |
| via proxy (plain)  | 0.60ms | 0.75ms | 0.97ms  |
| via proxy (MITM)   | 1.44ms | 1.65ms | 25.4ms* |

Added overhead (p50): **plain +0.30ms**, **MITM +1.15ms** (includes the second
TLS handshake per request — h1 upstream keep-alive/pooling is the known
follow-up, ADR-009).

\* one tail outlier (WAL checkpoint / scheduler slice on a shared box) — the
reason p99 budgets use a floor rather than a pure multiple.

## Budget policy

| Metric          | Budget                                     | Rationale |
|-----------------|--------------------------------------------|-----------|
| p50 / p95       | ≤ 2.0× baseline (+1–2ms absolute floor)    | Real regressions move the body of the distribution; the floor absorbs noisy CI hosts |
| p99             | ≤ 3.0× baseline + 50ms floor               | Tails carry scheduler/WAL noise; a pure multiple makes the gate flaky — flaky gates get deleted |
| overhead (p50)  | ≤ 2.0× baseline + 2ms floor                | *Added* latency is the product promise; it gets its own guard |

## Rules

- Refresh the baseline **only** with a documented reason (a deliberate
  trade landed, or the hardware class changed): update `baseline.json`,
  note the reason in the commit message, and re-run the gate.
- Never "fix" a red gate by weakening the factor silently.
- The soak probe (`aether soak --assert-mode`) is the complementary gate:
  latency proves it is fast, soak proves it stays up.

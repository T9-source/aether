# Phase 5 — the stabilization gate, executed (V0.2.x)

ADR-018 Part 2 defined the gate. This document is the evidence that each
item is **landed and enforced**, plus the one item that is honest about
being a runtime task.

```text
V0.2  →  Stability ✅  →  Security audit ✅  →  Fuzz testing ✅
      →  Performance regression ✅  →  Soak (24h) 🟡 scheduled
      →  THEN plugin runtime (V0.3)
```

## 1. Stability ✅

Full workspace suite green under `clippy -D warnings`, `cargo fmt --check`,
168 → 169 tests (the gate added the token-comparison regression test and
pinned the unsupported-TE refusal contract). The four ADR-018
Part-1 acceptance tests remain green.

## 2. Security audit ✅ (`docs/AUDIT.md`)

Two halves, both CI-visible:

- **Design/code self-audit** (`docs/AUDIT.md`): the five security-sensitive
  areas (h1 framing, CONNECT/peek, control-plane origin+token, capture
  policy, h2/WS surface) reviewed line-level with test evidence.
  - **F1 fixed** — `constant_time_eq` leaked the token length via an early
    return; replaced with a max-length loop (no length or content timing
    signal) + regression test.
  - **F2 filed** — auth failures are not rate-limited (256-bit token,
    loopback bind by default; boundary recorded in `LIMITATIONS.md`).
- **Supply chain** (`deny.toml`, `cargo deny check`): advisories + licenses
  + sources all green.
  - **F3 fixed** — `rustls-pemfile` unmaintained (RustSec); migrated to
    `rustls-pki-types::pem` (the upstream successor), dependency removed.

## 3. Fuzz testing ✅ (`fuzz/`)

Three `cargo-fuzz` targets over exactly the parsers that accept untrusted
bytes (ADR-018's list), seed corpora in-repo, CI smoke at 20 s/target:

| Target            | Covers                                   | Session result |
|-------------------|------------------------------------------|----------------|
| `fuzz_h1`         | h1 head codec + body specs + serialize/parse round-trip | 2.18M execs clean (60 s) |
| `fuzz_ws`         | RFC 6455 frame reader + fragment assembler | 512K execs clean (30 s) |
| `fuzz_control_json` | control-plane `ControlRequest` decoder + round-trip | 3.61M execs clean (30 s) |

**The fuzzer earned its keep on day one.** It reported a crash in `fuzz_h1`
with input `Transfer-Encoding: chunkencoding: chd`. Triage: the *harness*
had over-asserted ("`request_body_spec` is total") while the function is
deliberately partial — it refuses unsupported TE values (anti-smuggling)
and both production callers turn the Err into a 400 (server.rs) or honest
`None` framing facts (engine.rs). The harness was fixed, the crash input
joined the permanent corpus, and the product contract is now pinned by a
new unit test (`body_specs_and_smuggling_refusal`). No product defect.

## 4. Performance regression ✅ (`docs/PERF.md`, `scripts/check-perf-budget.sh`)

- Baseline captured from a **release build**, 400 req/path
  (`docs/bench/baseline.json`): direct p50 0.29 ms · plain-proxy p50
  0.60 ms · MITM p50 1.44 ms — **added overhead: plain +0.30 ms,
  MITM +1.15 ms**.
- Budgets: p50/p95 ≤ 2× (with absolute floors), p99 ≤ 3× + 50 ms floor,
  overhead ≤ 2× + 2 ms floor — factors catch real regressions, floors keep
  the gate stable on noisy shared runners.
- CI job `perf` re-runs the gate on every push; refresh requires a
  documented reason.

## 5. Soak testing 🟡 gate landed; 24 h run scheduled

The M0 probe became a gate: `aether soak --secs N --assert-mode` enforces

- RSS drift (settle-median → final-median) ≤ 15 MiB,
- FD growth ≤ 32 over the whole run,
- **zero** traffic errors — a soak with tolerated errors proves nothing,

prints a `SOAK_JSON:` verdict line for CI, and exits non-zero on breach.

**Compressed evidence (this phase, 180 s, 4 workers, release build):**
244,150 requests, 0 errors, 1355 req/s, RSS growth 4.2 MiB (budget 15),
FD growth +4 (budget 32) — **PASS**.

**Remaining (the only gate item that is a runtime task, by nature):**

```bash
aether soak --secs 86400 --workers 4 --assert-mode   # the 24 h acceptance run
```

The gate item closes when that command exits 0. Nothing in V0.3 may start
before it does — that is the point of ADR-018.

## What this phase deliberately did not do

- No new features. The gate is hardening only — that is the lesson the
  founder's review encoded: extensibility multiplies whatever surface it
  lands on, so the surface must deserve trust first.
- No criterion micro-benchmarks: ADR-018 pins the perf gate to the
  end-to-end proxy bench (the user-visible latency), not micro timings.
- No CI soak: shared runners cannot produce meaningful 24 h stability
  evidence; the command exists for a real host.

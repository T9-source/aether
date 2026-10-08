# Milestone 0 — Acceptance Criteria: "The Birth of Aether"

- **Status:** Phase 1 shipped — M0.2 verified where V0.1 scope applies (see per-criterion status)
- **Date:** 2026-10-08 (criteria), 2026-10-08 (status update)
- **Author:** smart-jemy

> **Birth moment (founder's definition, adopted verbatim):**
> Run `aether`, point a browser at the proxy, open a real HTTPS website —
> Aether performs TLS interception and the UI shows the request/response with
> **zero certificate errors** — all while the Core daemon runs **independently
> of the UI**.

This milestone proves we have a **real product**, not an architecture diagram.

## M0.1 — The Birth Moment (hard gate)

| # | Criterion | Status (Phase 1, V0.1) |
|---|-----------|------------------------|
| 1 | `aether serve` starts headless and stays running | ✅ **verified** (`aether serve --memory --listen …`, banner + token + CA paths printed) |
| 2 | Tauri app connects over the control plane | ⏳ **deferred** (Tauri is V0.2+; the *protocol* it will use is shipped and tested — ADR-010) |
| 3 | Browser → HTTPS site → visible, **zero cert errors** after CA install | ✅ **verified**: curl-via-proxy to `https://example.com` with the Aether CA → 200, flow recorded; MITM chain e2e-tested with a client trusting *only* the Aether CA |
| 4 | **Kill the UI: proxying continues** | ✅ **by construction**: the CLI is a WS client only; proxy loop shares nothing with it (`crates/aether-daemon` wires them independently) |
| 5 | h3-serving site passes with silent TCP fallback | ✅ **by design**: browsers cannot use QUIC through an explicit HTTP proxy — they fall back to h1 over TCP automatically (ADR-007 `aether net quic-block` remains a hardening nicety) |

## M0.2 — Operational Criteria (hard gate)

| # | Criterion | Status (Phase 1, V0.1) |
|---|-----------|------------------------|
| 6 | **Intercept from both clients** | ✅ **CLI verified end-to-end** (hold → edit → forward, hold → drop; over the WS control plane with `intercept_hold`/`intercept_released` events). UI leg lands with the Tauri app on the same ops |
| 7 | **Scope + pass-through** | ✅ pass-through verified (pinned-style domain tunneled blindly, `PassThrough` event, nothing recorded); host scoping ships via `RuleSpec.host_include/exclude` |
| 8 | **Match & Replace through the HookRegistry** | ✅ verified (JSON `RuleSpec` → `RuleHook`, stamps headers/rewrites paths, tags flows — proven over the control plane too) |
| 9 | **Persistence survives restart** | ✅ SQLite (WAL, full-fidelity records) verified; `flows_list`/`flows_get` after re-open covered by store tests |
| 10 | **Scale smoke test: 100k flows, keyset pagination** | ✅ **verified**: 100,000 flows inserted at ~12k/s; page-of-50 reads **p50 = 0.51 ms, p95 = 0.80 ms**; FTS5 search over 100k in ~67 ms (`aether bench flows --n 100000`) |
| 11 | **Latency measurement exists** | ⏳ partial: per-flow `duration_ms` recorded + bench timings exist; the dedicated proxy-overhead bench (passthrough vs direct) lands with V0.1 exit review |
| 12 | **24h soak** | ⏳ **deferred** (continuous-browsing harness; queued after h2) |

## M0 Exit Review

- Benchmarks + known-limitations document committed: ✅ (this file +
  `docs/PHASE-1.md`).
- Demo script: `docs/guide-ar.md` quickstart doubles as the reproducible
  script (birth moment minus secrets).
- ADR-008 **Accepted** (2026-10-08, founder decision) — ecosystem work
  unblocked.

## Explicitly out of scope for M0

HTTP/2, WebSocket inspection, plugins/WASM, query language, fuzzing, AI,
mobile QR setup, team features. Anything found "missing" here that is on this
list goes to the backlog, not the milestone.

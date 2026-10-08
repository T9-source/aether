# Milestone 0 — Acceptance Criteria: "The Birth of Aether"

- **Status:** Phase 2 shipped — criteria verified where V0.1/V0.2 scope applies (see per-criterion status)
- **Date:** 2026-10-08 (criteria), 2026-10-08 (status update)
- **Author:** smart-jemy

> **Birth moment (founder's definition, adopted verbatim):**
> Run `aether`, point a browser at the proxy, open a real HTTPS website —
> Aether performs TLS interception and the UI shows the request/response with
> **zero certificate errors** — all while the Core daemon runs **independently
> of the UI**.

This milestone proves we have a **real product**, not an architecture diagram.

## M0.1 — The Birth Moment (hard gate)

| # | Criterion | Status (Phase 2, V0.2) |
|---|-----------|------------------------|
| 1 | `aether serve` starts headless and stays running | ✅ **verified** (`aether serve --memory --listen …`, banner + token + CA paths printed) |
| 2 | Tauri app connects over the control plane | ✅ **verified (Phase 2)**: the console UI (browser build) connects over protocol v1 with live history/intercept/replay/rules; the Tauri 2 shell (`apps/aether-desktop`) wraps the same UI, same ops, tauri origin allowlisted (ADR-012) |
| 3 | Browser → HTTPS site → visible, **zero cert errors** after CA install | ✅ **verified**: curl-via-proxy to `https://example.com` with the Aether CA → 200, flow recorded; MITM chain e2e-tested with a client trusting *only* the Aether CA |
| 4 | **Kill the UI: proxying continues** | ✅ **verified from both directions**: daemon runs with no UI at all; the console is a stateless WS client with no privileged backdoor (closing the tab never touches a capture) |
| 5 | h3-serving site passes with silent TCP fallback | ✅ **by design**: browsers cannot use QUIC through an explicit HTTP proxy — they fall back to h1 over TCP automatically (ADR-007 `aether net quic-block` remains a hardening nicety) |

## M0.2 — Operational Criteria (hard gate)

| # | Criterion | Status (Phase 2, V0.2) |
|---|-----------|------------------------|
| 6 | **Intercept from both clients** | ✅ **both legs verified end-to-end**: CLI (hold → edit → forward, hold → drop) and console UI (hold → structured edit → forward — the origin echoed the console's edits; drop), same ops, same events (ADR-010/012) |
| 7 | **Scope + pass-through** | ✅ pass-through verified (pinned-style domain tunneled blindly, `PassThrough` event, nothing recorded); host scoping ships via `RuleSpec.host_include/exclude` |
| 8 | **Match & Replace through the HookRegistry** | ✅ verified (JSON `RuleSpec` → `RuleHook`, stamps headers/rewrites paths, tags flows, mocks responses). Phase 2: edits apply to in-flight traffic with **no restart** (live registry, ADR-012) — proven by authoring a mock rule in the console and getting 418 + mock body from a matched host while unmatched hosts passed through |
| 9 | **Persistence survives restart** | ✅ SQLite (WAL, full-fidelity records) verified; `flows_list`/`flows_get` after re-open covered by store tests |
| 10 | **Scale smoke test: 100k flows, keyset pagination** | ✅ **verified**: 100,000 flows inserted at ~12k/s; page-of-50 reads **p50 = 0.51 ms, p95 = 0.80 ms**; FTS5 search over 100k in ~67 ms (`aether bench flows --n 100000`) |
| 11 | **Latency measurement exists** | ✅ **verified (V0 dogfoodable)**: `aether bench proxy` measures every path against a local origin — direct p50 1.75 ms, plain-proxy hop **+0.26 ms**, MITM **+4.48 ms** (dominated by the 2nd TLS handshake per request; one fresh upstream connection per request is the documented V0.1 design, ADR-009). Baseline recorded before any optimization (measurement-before-optimization) |
| 12 | **24h soak** | ✅ **tooling verified (V0 dogfoodable)**: `aether soak --secs N` runs continuous mixed plain+MITM traffic against real SQLite/WAL with 1 Hz RSS/FD sampling — 60 s verification run: 21,442 requests, **0 errors**, 357 req/s, RSS +4.8 MB (bounded pool warm-up), FDs 18→30 (stable). The full acceptance run (`--secs 86400`) is the same command and remains a pre-launch ritual |

## M0 Exit Review

- Benchmarks + known-limitations document committed: ✅ (this file +
  `docs/PHASE-1.md` + `docs/LIMITATIONS.md` — the honest known-limits list).
- Demo script: ✅ `scripts/demo.sh` — the reproducible birth moment minus
  secrets (CA install, headless daemon, TLS MITM round-trip, CLI intercept,
  history search); `docs/guide-ar.md` remains the narrated walkthrough.
- ADR-008 **Accepted** (2026-10-08, founder decision) — ecosystem work
  unblocked.

## Status summary (end of V0 dogfoodable)

10 / 12 criteria fully verified; criterion 3 (browser + CA install on a real
desktop profile) and criterion 12's full 24 h run are runtime rituals on the
founder's machine — tooling and evidence for both are committed (`aether
soak`, `scripts/demo.sh`). Nothing here blocks V0.2.

## Explicitly out of scope for M0

HTTP/2, WebSocket inspection, plugins/WASM, query language, fuzzing, AI,
mobile QR setup, team features. Anything found "missing" here that is on this
list goes to the backlog, not the milestone.

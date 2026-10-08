# Phase 1 — Definition of Done & Status

> **Goal:** make the proxy *real*: explicit HTTP/1.1 forward proxy with TLS
> MITM, flow history in SQLite, live match & replace rules, manual
> interception from the CLI, and a WebSocket control plane — with the
> Phase-0 domain core unchanged as the single source of truth.

## DoD Checklist

| # | Item | Status |
|---|------|--------|
| 1 | `crates/aether-http` — hand-rolled h1 wire codec (wire truth, ADR-003/009) | ✅ `wire.rs`: heads/CL/chunked/EOF bodies, obs-fold rejection, CL+TE refusal, bounded reads — 12 unit tests |
| 2 | CA manager (ADR-004): persistent root + per-host leaf cache | ✅ `ca.rs`: create/reload identical CA, IP+DNS SANs, leaf chaining — tested |
| 3 | TLS MITM over CONNECT, SNI-independent fixed leaf | ✅ e2e: client trusting *only* the Aether CA talks to `https://…` with **zero cert errors**; TLS facts recorded on the flow |
| 4 | Pass-through list (ADR-004): blind tunnel, never recorded | ✅ e2e: tunneled TLS works, `PassThrough` event, storage stays empty |
| 5 | Flow engine — single path through pipeline → storage → events | ✅ `engine.rs`: FlowStarted → request hooks → upstream → response hooks → FlowCompleted; drops are tagged decisions, never silent |
| 6 | Match & replace rules (ADR-006): JSON → live hooks | ✅ `core::rules`: headers/path/body regex/tag/drop/respond-with; registered from `rules.json` AND added live over WS |
| 7 | Manual interception (Milestone-0 gate #6) | ✅ hold → list → show → release-edited / drop; `InterceptHold`/`InterceptReleased` events; cooperative blocking via oneshot |
| 8 | Storage backend (ADR-005): SQLite/WAL/FTS5/keyset | ✅ `aether-store-sqlite`: full-fidelity records, batched transactions, FTS5 with graceful fallback, conformance-tested against `MemoryStorage` |
| 9 | Control plane (ADR-002/010): WS + JSON + auth + origin | ✅ `aether-daemon/src/control.rs`: constant-time token check, loopback-only origin, event push, 15 ops |
| 10 | CLI as a pure client (ADR-001) | ✅ `serve`, `ca export/path`, `flows list/show/search`, `intercept ls/show/drop/release/set`, `rules ls/add/rm/enable/disable`, `bench flows`, `demo/info/about` |
| 11 | Bounded capture (ADR-011) | ✅ 2 MiB body through a 64 KiB window arrives complete at the client, prefix in history; oversized requests → 413 |
| 12 | Tests | ✅ **122 green** across the workspace (74 core lib + 8 core integration/serialization + 20 http unit + 9 http e2e + 7 sqlite + 3 rules + 1 control-plane e2e) |
| 13 | `cargo fmt --check` + `cargo clippy -D warnings` + CI | ✅ clean |
| 14 | Milestone-0 scale gate | ✅ **100k flows: insert ~12k/s; page-of-50 p50 0.51 ms / p95 0.80 ms; FTS5 search ~67 ms** |
| 15 | Real-world smoke | ✅ `aether serve` + curl-via-proxy → `https://example.com` → 200 through the MITM, flow visible via `aether flows list` |

**Result: Phase 1 DONE — the birth moment works from real shells.**

## Known limitations carried forward (documented, not forgotten)

- h2/WebSocket inspection → V0.2 (model already speaks the semantics).
- Upstream connection pooling → V0.2 (one fresh upstream per request now).
- Intercept holds engage on fully-captured bodies only (ADR-011).
- Streamed-remainder tails are not FTS-searchable until spooling lands.
- `aether bench latency` (proxy-overhead measurement) → V0.1 exit review
  (per-flow `duration_ms` already recorded).

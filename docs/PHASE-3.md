# Phase 3 — V0 dogfoodable + V0.2 Modern Web (h2 + WebSocket)

**Status: Complete** · 2026-10-09 · Author: smart-jemy

Phase 3 closes two chapters at once: the **Milestone-0 exit review** (V0 as
a dogfoodable, measured product) and the **V0.2 "Modern Web" rung** of the
frozen ladder (ADR-013): HTTP/2 on both legs, then WebSocket inspection.

## Part A — V0 dogfoodable (Milestone-0 exit review)

- **`aether bench proxy`** — the latency gate (M0 #11): measures every path
  against a local TLS origin — direct, plain-proxy hop, MITM tunnel — and
  reports p50/p95/p99. First recorded baseline (debug build, loopback):
  plain hop **+0.26 ms**, MITM **+4.48 ms** p50 (dominated by the second
  TLS handshake per request — the documented V0.1 one-connection-per-request
  design, ADR-009). Measurement before optimization.
- **`aether soak`** — steady-state health (M0 #12 tooling): continuous
  mixed plain+MITM traffic through the embedded engine with real SQLite/WAL,
  1 Hz RSS/FD sampling. 60 s verification: 21,442 requests, **0 errors**,
  357 req/s, FDs stable. The 24 h acceptance run is the same command
  (`--secs 86400`).
- **`scripts/demo.sh`** — the reproducible birth moment minus secrets: local
  HTTPS origin, headless daemon, CA-only-trust TLS MITM round-trip, FTS
  search, CLI intercept hold→edit→forward with the origin confirming the
  edit. Exits non-zero on any failure — CI-usable.
- **`docs/LIMITATIONS.md`** — every known limit is listed as a decision
  pointing at its ADR or backlog item; nothing hidden, nothing pretended.

## Part B — V0.2 Modern Web (ADR-013)

### HTTP/2

- **ALPN on both legs**: the MITM server config offers `[h2, http/1.1]`;
  the dispatch after the handshake routes to the matching codec. The
  upstream leg negotiates independently (`upstream_alpn_h2 = true` default,
  h1-only for upgrades and when disabled).
- **All four combinations** work and are e2e-tested: h2↔h2, h2 client → h1
  origin, h1 client → h2 origin, h1↔h1 (regression-covered by the Phase-1
  suite, still green).
- **One engine path**: each h2 request stream runs
  `FlowEngine::handle_request` — intercept, rules, events, storage and the
  console are protocol-blind (ADR-006 #1 held under the hardest test yet).
- **Flow-control discipline**: every received DATA frame releases its
  window; sends go through reserve→poll-capacity loops; bounded capture
  (ADR-011) with streaming remainder, identical to the h1 contract.
- **Multiplexing**: one client connection serves many concurrent streams
  (e2e-proven); upstream h2 connections are shared per client connection.
- Console shows an `HTTP/2` chip + `alpn h2` in the viewer meta row.

### WebSocket

- **Full lifecycle through the engine**: upgrade requests take
  `handle_upgrade` — the same pipeline as `handle_request` (flow record,
  request/response intercept, hooks, rules; a mock rule can answer a WS
  handshake). On `101` the daemon becomes a **relay**.
- **Byte-for-byte relay**: frames cross untouched (masks included); two
  RFC 6455 readers assemble messages for the record — never for rewriting.
- **Bounded session record** on the flow (`WsSession`): ≤500 messages,
  ≤2 KiB previews (text or base64), exact counters; persisted on
  connection end (new `ws_json` column with in-place migration) and pushed
  live via `ws_opened` / `ws_message` / `ws_closed` events.
- **permessage-deflate stripped** by default (tag `ws-deflate-stripped`)
  so recorded frames are human-readable — mitmproxy's proven default.
- **CONNECT tunnel peek** (ADR-013 decision 3, discovered by a real
  browser): Chrome tunnels `ws://` as CONNECT + *plaintext* upgrade inside.
  The proxy now reads the tunnel's first three bytes — TLS ClientHello →
  MITM, anything else → plaintext h1 served inside the tunnel. Wire truth
  over guesswork; the peeked bytes are replayed to whichever leg wins.
- **Console**: history shows a `WS` chip for 101 flows; the viewer gains a
  WebSocket tab (direction ↑/↓, kind, size, preview, copy session).
- **Real-browser proof** (`examples/ws-echo-server.js` +
  `examples/ws-demo.html`, driven by Playwright): a real Chromium proxied
  through a real daemon opens `ws://` through the proxy, exchanges
  messages, closes; the console autoconnects, shows the `WS` chip, opens
  the WebSocket tab with the recorded session (screenshot
  `download/screenshots/09-ws-console.png`).

## Verification

- Rust: **156 tests green** (was 129) — includes 5 new V0.2 e2e tests with
  real TLS, real h2 handshakes and real WebSocket frames.
- `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt`
  clean.
- UI: production build OK (~61 KB gzip), **29 vitest green**.
- `scripts/demo.sh` end-to-end green after the changes (the h1 path
  regressed nothing).
- `aether bench proxy` and `aether soak` green after the changes.

## What this phase deliberately did not do

- No upstream connection pooling across client connections (measured,
  backlog).
- No WS frame editing (relay-only; a plugin-era primitive discussion).
- No h2 trailers, no WS-over-h2 (RFC 8441), no h2c — all documented in
  `docs/LIMITATIONS.md`.

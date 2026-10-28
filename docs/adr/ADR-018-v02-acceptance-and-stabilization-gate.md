# ADR-018 — V0.2 acceptance criteria and the stabilization gate before plugins

- **Status:** Accepted
- **Date:** 2026-10-09
- **Owner:** T9-source (founder)

## Context

V0.2 shipped HTTP/2 and WebSocket inspection (ADR-013) and the version
ladder scheduled V0.3 as the WASM/Extism plugin runtime. Two problems:

1. **"Shipped" had no teeth.** Saying "h2 + WebSocket shipped" without a
   defined bar means the word carries no information. The user bar is:
   every claim in the README must have a test suite behind it.
2. **Ordering risk.** A plugin ecosystem on top of an unstable core is an
   API-compatibility catastrophe. If the Traffic Model, hooks, events and
   control plane are not hardened *first*, every plugin ever written breaks
   at every release.

## Decision

**Accepted.**

### Part 1 — V0.2 is "shipped" only against these acceptance criteria

HTTP/2 (client leg + upstream leg):

- ✓ ALPN negotiation on both legs (recorded in flow TLS facts)
- ✓ multiplexed streams on one connection
- ✓ concurrent streams through the single engine path
- ✓ request/response correlation per stream (one flow per stream)
- ✓ stream cancellation: RST_STREAM/CANCEL from the client is absorbed; the
  connection keeps serving (`h2_stream_reset_is_handled_and_connection_survives`)
- ✓ flow control: DATA windows released as received; sends via
  reserve→poll-capacity (ADR-013)
- ✓ malformed input: garbage where an h2 preface belongs kills the
  *connection*, never the daemon (`malformed_h2_bytes_are_rejected_and_daemon_survives`)
- ✓ graceful GOAWAY: shutdown sends GOAWAY, in-flight streams complete,
  new streams are never served a fresh 200, and a client that ignores
  GOAWAY is cut after `H2_DRAIN_DEADLINE` (30s — nginx
  `worker_shutdown_timeout`, same idea)
  (`h2_shutdown_sends_graceful_goaway`)

WebSocket (RFC 6455):

- ✓ text frames, binary frames, fragmentation (assembler, ADR-013)
- ✓ ping/pong pass through the relay untouched
  (`ws_relay_forwards_control_frames_and_megabyte_messages`)
- ✓ close handshake ends the relay and persists the session
- ✓ megabyte messages relay byte-for-byte (streaming, bounded staging)
- ✓ recording honesty: sizes counted truly, previews bounded (ADR-011)
- ✗ WebSocket *interception* (edit frames in flight) — **deferred**, listed
  in `LIMITATIONS.md`, does not block "shipped" for *inspection*

### Part 2 — the stabilization gate (hardening before extensibility)

**V0.3 (plugin runtime) does not start until this gate passes:**

```text
V0.2  →  Stability  →  Security audit  →  Fuzz testing  →  Soak testing
      →  Performance regression  →  THEN plugin runtime (V0.3)
```

Concretely, the gate requires:

1. **Stability** — the full suite green under `clippy -D warnings` with no
   silent unwrap paths in the proxy legs; the acceptance tests above green.
2. **Security audit** — a structured self-audit of the security-sensitive
   surface (h1 framing decisions, CONNECT peek, origin checks, token
   handling, capture policy), documented with findings fixed or filed.
3. **Fuzz testing** — `cargo-fuzz` targets for the h1 codec, the WS frame
   parser, and the control-plane JSON decoder (all parse untrusted bytes).
4. **Soak testing** — the `aether soak` harness (M0) run long-duration
   (≥ 24h) with RSS/FD/error budget assertions.
5. **Performance regression** — the `aether bench proxy` baselines (M0)
   captured in-repo with explicit budgets; CI-visible drift is a failure.

Only after the gate does V0.3 add the WASM runtime — which then becomes
"just a runtime over an existing, frozen contract" (ADR-006, ADR-016
services, ADR-017 categories).

## Where it lives in code

- `crates/aether-http/tests/v02_e2e.rs` — the four acceptance tests above.
- `crates/aether-http/src/h2x.rs` — `H2_DRAIN_DEADLINE` + graceful drain.
- `docs/LIMITATIONS.md` — WS interception deferred; drain deadline noted.
- Gate artifacts (Phase 5, landed):
  - `fuzz/` — three `cargo-fuzz` targets (`fuzz_h1`, `fuzz_ws`,
    `fuzz_control_json`) over the parsers that accept untrusted bytes, with
    seed corpora kept in-repo; CI runs a 20 s smoke per target.
  - `docs/AUDIT.md` — the structured security self-audit (findings fixed
    in-tree: F1 token-length timing leak, F3 unmaintained `rustls-pemfile`;
    filed: F2 no auth rate limiting).
  - `docs/bench/baseline.json` + `scripts/check-perf-budget.sh` +
    `docs/PERF.md` — the performance-regression gate (baseline captured
    from a release build; budgets = factors with absolute floors).
  - `aether soak --assert-mode` — the soak probe as a gate: RSS-drift /
    FD-growth / zero-error budgets, `SOAK_JSON:` machine-readable verdict,
    non-zero exit on breach. The 24 h acceptance run remains a scheduled
    runtime task; the gate command is identical.

## Consequences

- The roadmap ladder (README) gains an explicit V0.2.x hardening phase
  before V0.3 — a deliberate pause, not a delay: plugins multiply the API
  surface they are built on, and the surface they freeze must deserve trust.
- "Shipped" is now falsifiable: a criterion without a green test means the
  claim comes off the README.

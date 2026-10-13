# Phase 4 — Architecture consolidation (V0.2.x): ADR-014 → ADR-018

> Status: **complete**. This phase converts an external architecture review
> into binding decisions + code, *before* any new feature work. Author of
> record: **smart-jemy**.

## Why this phase exists

An independent review of the V0.2 architecture approved the structure as a
baseline but demanded four hardening decisions before anything else ships —
and one sequencing change (no plugin runtime until the core is stabilized).
Every point below is a reviewer requirement turned into an Accepted ADR
plus the code/tests that make it true, not aspirational.

## What was decided and built

### 1. Core dependency policy (ADR-014)
- Reviewer: *"aether-core has zero infrastructure/protocol dependencies"*
  replaces the brittle "regex-only" phrasing.
- Code: `aether-core` now declares `tokio` primitives-only (`sync`, `time`)
  — deliberately NOT workspace-inherited — with `rt`/`macros` moved to
  `[dev-dependencies]`. Policy enforced in CI by `scripts/check-core-deps.sh`.

### 2. Wire truth model (ADR-015)
- Reviewer: raw bytes must not be the canonical model; Observed / Logical /
  Forwarded are distinct concepts (`ObservedWire → Parser → Logical Model →
  ForwardedWire`).
- Code: `Framing` enum + four explicit `FlowMeta` facts
  (`observed_req/forwarded_req/observed_res/forwarded_res_framing`),
  recorded by the engine on every path (plain, MITM, h2, upgrade, mock),
  persisted as a `wire_facts_json` sidecar column with migration, exported
  to TS bindings. `UpstreamReply.leg_h2` + `ConnInfo.client_h2` carry the
  leg protocol the facts are decided from.

### 3. Daemon decomposition (ADR-016)
- Reviewer: *daemon owns lifecycle, services own behavior* — no God object.
- Code: new `services/` layer (`TrafficService`, `InterceptService`,
  `RulesService`, `ReplayService`, `CaService`); `control.rs` rewritten
  protocol-only (every dispatch arm is a one-line delegation); `daemon.rs`
  documented as composition-only. Control-plane wire format unchanged
  (ADR-010) — clients need no update.

### 4. Event taxonomy (ADR-017)
- Reviewer: prevent event explosion; distinguish transport / pipeline /
  security / lifecycle; low-level telemetry stays tracing, forever.
- Code: `EventCategory` + exhaustive `AetherEvent::category()` (a new
  variant that is not classified is a compile error); event frames now
  carry `"category"` (additive for clients).

### 5. V0.2 acceptance criteria + stabilization gate (ADR-018)
- Reviewer: *"shipped" needs a test suite behind it* — h2 and WS checklists;
  and **no plugin runtime until** stability → security audit → fuzzing →
  soak → perf regression.
- Code/tests (all new, all real sockets/TLS):
  - `h2_stream_reset_is_handled_and_connection_survives` — RST_STREAM/CANCEL
    absorbed; connection keeps serving; h2 wire facts asserted.
  - `h2_shutdown_sends_graceful_goaway` — shutdown while a stream is
    in-flight: the stream **completes**; new streams never see a fresh 200;
    the connection terminates. Implementation detail worth recording:
    per RFC 7540 §6.8 the *client* owns the close after GOAWAY, so the
    drain has a `H2_DRAIN_DEADLINE` (30s) — a client that ignores GOAWAY
    cannot hold the daemon hostage (nginx `worker_shutdown_timeout`, same
    idea). h2's server `Connection` is not a Future; the drain is driven by
    polling `accept()` under a deadline select.
  - `malformed_h2_bytes_are_rejected_and_daemon_survives` — HTTP/1.1 bytes
    as an h2 preface kill the connection, never the daemon.
  - `ws_relay_forwards_control_frames_and_megabyte_messages` — ping/pong
    pass through untouched; 1 MiB message relays byte-for-byte (bounded
    staging); a 128 KiB message is recorded at its true size with a bounded
    preview; the 101 flow records `forwarded_res_framing = upgrade`.
  - engine unit tests pin the framing-helper semantics (RFC 9110 cases).

## Definition of Done (met)

- [x] ADR-014…018 Accepted and indexed; README invariants + ladder updated.
- [x] Core compiles with no net/fs/runtime in its library graph; CI guard added.
- [x] Wire facts recorded on every engine path and persisted across restarts.
- [x] `services/` behavior layer in place; control plane delegating only.
- [x] Event categories computed exhaustively and pushed on the wire.
- [x] All four V0.2 acceptance tests green; **168 rust tests** (was 156) +
      29 UI tests; clippy `-D warnings` clean; fmt clean.
- [x] LIMITATIONS.md updated (WS interception stays deferred; drain deadline
      documented).

## What this phase deliberately does NOT do

- No WASM/Extism work — the stabilization gate (ADR-018 Part 2) blocks it:
  security audit → fuzzing → long soak → perf regression come first.
- No new UI features — the control-plane JSON is additive-only
  (`category` on event frames; new optional `FlowMeta` fields), so existing
  clients keep working unmodified.

# ADR-002: Control Plane Transport

- **Status:** Accepted (amended from founder's proposal)
- **Date:** 2026-10-08
- **Deciders:** Project founder + architecture review

## Context

Exactly one control-plane protocol for the MVP (anti-overengineering rule:
infrastructure must not dwarf the product). Requirements:
bidirectional (server-push events like `FlowCompleted`, `FindingCreated`),
request/response commands, long-lived subscriptions (live traffic view,
intercept queue, fuzz runs), multiple simultaneous clients, binary payloads
(request/response bodies can be megabytes), and a browser-based webview client.

Candidates: gRPC (tonic), REST + SSE, WebSocket + JSON, WebSocket + MessagePack,
Unix socket with custom framing.

## Decision

**WebSocket as the single transport. JSON envelope. JSON-RPC 2.0-style semantics.**

- Commands: client → server requests (`id`, `method`, `params`) with responses.
- Events: server → client notifications (no id), including subscriptions
  (`subscribe` / `unsubscribe` commands return a subscription id).
- Handshake frame, both directions: `{"aether": 1, "codec": "json"}`.

### Guardrails (the amendments)

1. **One codec, not two.** JSON only for now (debuggable with any WS client,
   human-inspectable, local bandwidth is irrelevant). The handshake carries a
   `codec` field so MessagePack or a binary codec can be negotiated in later
   versions **without breaking the contract**. Do not build both.
2. **Typed contract from Rust, single source of truth.** Command/event types are
   defined once in Rust and TS types are auto-generated (`ts-rs` or `specta`).
   Hand-written TS types are forbidden.
3. **Bodies travel as WS binary frames** (`flow_id` + part index), never as
   base64-in-JSON. Threshold: payloads > 32 KiB switch to binary frames.
4. **Slow-consumer policy per subscription:** live views may drop with a
   explicit marker (`"dropped": N`); storage keeps everything regardless.
5. **Auth is mandatory (this is a security tool).** Default bind `127.0.0.1` only.
   First frame within 5s must be `{"auth": "<token>"}` or the connection closes.
   Token: random, per-project, stored with `0600` perms, overridable via env/flag.
   **Origin/Host check on handshake** — browsers allow any webpage to attempt
   `ws://localhost:PORT` (cross-site WebSocket hijacking / DNS rebinding).
   Allowlist: Tauri webview origins + token-bearing non-browser clients.

## Consequences

Positive: one protocol everywhere including the webview; trivially debuggable;
adding a gRPC facade later is cheap because commands/events are already typed in Rust.

Negative: backpressure and reconnection logic are our responsibility (no framework);
no automatic codegen for other languages yet (acceptable — SDK work is post-V0.3).

## Alternatives Considered

- **gRPC/tonic** — rejected for MVP: proto toolchain tax, grpc-web friction inside
  the webview, harder ad-hoc debugging. Revisit as an *additional* facade when
  remote/team scenarios arrive (not before).
- **REST + SSE** — rejected: two channels where one duplex channel suffices;
  intercept queue semantics get awkward.
- **MessagePack from day 1** — rejected: kills debuggability for zero local benefit.

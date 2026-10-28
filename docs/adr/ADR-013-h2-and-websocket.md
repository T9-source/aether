# ADR-013 — HTTP/2 legs and WebSocket inspection (V0.2)

- **Status:** Accepted
- **Date:** 2026-10-09
- **Owner:** T9-source (founder)

## Context

ADR-003 froze the Traffic Model on h1+h2 semantics from day 1 and promised
"wire truth" — which is why ADR-009 hand-rolled the h1 codec (hyper
normalizes aggressively, exactly wrong for a research tool). The version
ladder then scheduled V0.2: **HTTP/2 (ALPN, streams), then WebSocket
inspection**. This ADR records how the h2 promise was kept without
repeating the h1 decision.

## Decision 1 — HTTP/2: delegate the machinery, keep the truth

**Accepted.** The `h2` crate (hyper team) supplies HPACK, flow-control
windows, and frame encoding/decoding. Aether keeps:

- the **single engine path** — `h2x::serve_connection` accepts request
  streams and feeds the same `FlowEngine::handle_request` the h1 leg uses;
  rules, intercept, events, storage and the console cannot tell the legs
  apart (ADR-006 #1),
- **bounded capture** (ADR-011) on both directions, with the excess exposed
  as a streaming remainder exactly like h1,
- **flow-control discipline**: received DATA releases its capacity window
  immediately; sends go through a reserve→poll-capacity→emit-≤16KiB-frames
  loop (a stalled window is a deadlock, not a safety feature),
- **honest framing**: `Content-Length` is recomputed, never trusted;
  hop-by-hop headers are stripped in both directions (the `h2` crate
  additionally *refuses* to send them),
- ALPN strategy: the MITM server config offers `[h2, http/1.1]` — the
  client picks; the dispatch after the handshake routes to the matching
  leg codec. The upstream leg offers the same pair when
  `upstream_alpn_h2 = true` (default), h1-only otherwise and always for
  WebSocket upgrades. Negotiations are independent in both directions, so
  all four combinations (h2↔h2, h2→h1, h1→h2, h1↔h1) work and are
  e2e-tested.

**Known trade-off (documented, accepted):** `http::HeaderMap` does not
preserve original header *order* — duplicate headers and exact values are
preserved, ordering is best-effort. Hand-rolling HPACK to recover order is
tracked in `docs/LIMITATIONS.md` and only justified by a real-world case.

**Rejected alternative:** hand-roll HPACK + h2 framing the way h1 was
hand-rolled. Wire-truth value would be header order; risk would be an
interoperability black hole in the most stateful protocol Aether touches.
The h1 codec stays hand-rolled because it is small and strict; h2 is
neither.

## Decision 2 — WebSocket: relay, don't proxy

**Accepted.** After an RFC 6455 upgrade the socket is no longer HTTP. The
daemon becomes a **relay**:

- frames cross **byte-for-byte untouched** (masks included) — inspection
  never delays or alters the wire,
- one RFC 6455 reader per direction assembles messages (continuations,
  control frames) into the bounded `WsSession` record on the flow:
  ≤500 messages, ≤2 KiB previews per message, ≤1 MiB session previews;
  counters (`total_messages`, `total_bytes`) stay exact regardless,
- frames >16 MiB end the relay (memory-exhaustion guard, `MAX_FRAME_BYTES`),
- the upgrade exchange itself runs through the **same** engine path
  (`handle_upgrade`): flow record, request/response intercept, hooks,
  rules — a rule can mock a WS handshake exactly like any other request,
- `permessage-deflate` is **stripped** from the upstream handshake by
  default (tagged `ws-deflate-stripped` on the flow) — compressed frames
  would be uninspectable noise; this mirrors mitmproxy's proven default,
- the first bytes of the session often arrive with the handshake — the h1
  head reader's buffered prefix is re-attached to the relay
  (`PrefixStream`) so no byte is lost,
- WS-over-h2 (RFC 8441 `:protocol` CONNECT) is out of scope; browsers use
  it only for their own internal channels.

## Decision 3 — CONNECT tunnels: peek, don't guess

**Accepted** (added when a real Chromium proved the need). After
`200 Connection Established`, the proxy **reads the tunnel's first bytes**
and routes by wire truth:

- `0x16 0x03 …` (TLS ClientHello) → the MITM path as usual,
- anything else → **plaintext h1 served inside the tunnel**.

Real browsers do exactly this for `ws://`: a CONNECT to the proxy, then an
*unencrypted* RFC 6455 upgrade inside the tunnel. A proxy that assumes
"CONNECT ⇒ TLS" kills every plain-WebSocket origin. Peeking is three bytes,
decided by the client's own first bytes — no guessing, no config. The
peeked bytes are replayed to whichever leg wins (the `PrefixStream`
re-erasure), so nothing is consumed or lost.

## Where it lives

- `crates/aether-http/src/h2x.rs` — model ⇄ http types, bounded capture,
  `H2Remainder`, upstream client exchange, per-connection serve loop
- `crates/aether-http/src/ws.rs` — RFC 6455 codec, `MessageAssembler`,
  `WsRecorder`, `relay_leg`
- `crates/aether-http/src/server.rs` — ALPN dispatch, upgrade detection,
  tunnel peek (`PrefixStream` replay), relay lifecycle, close-delimited h1
  responses
- `crates/aether-http/src/upstream.rs` — `open_upgrade`, h2 dispatch,
  `UpstreamCfg.alpn_h2`
- `crates/aether-http/src/tls.rs` — `ALPN_H2_H1` / `ALPN_H1`,
  `upstream_client_config_with_alpn`
- `crates/aether-core/src/model/ws.rs` — `WsSession`/`WsMessage` (primitive #1)
- `crates/aether-core/src/events.rs` — `WsOpened`/`WsMessage`/`WsClosed`
- `crates/aether-store-sqlite/src/lib.rs` — `ws_json` column (+ migration)
- `apps/aether-ui/src/components/FlowViewer.tsx` — WebSocket session tab

## Consequences

- Browsers negotiate h2 through the proxy automatically; flows show
  `HTTP/2` + `alpn h2`, and everything downstream works unchanged.
- WS sessions are queryable in history (FTS covers message previews) and
  visible in the console with direction/kind/size/preview.
- Upstream h2 connections are shared per client connection, not globally —
  failure isolation beats a small latency win (pooling stays on the
  performance backlog).
- The 100% honest-framing rule extends: chunked origins beyond the capture
  cap are served to h1 clients close-delimited (RFC 9110 §6.3) rather than
  lying with an invented `Content-Length`.

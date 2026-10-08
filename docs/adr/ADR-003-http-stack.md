# ADR-003: HTTP Stack & Protocol Roadmap

- **Status:** Accepted
- **Date:** 2026-10-08
- **Deciders:** Project founder + architecture review

## Context

Founder's reclassification (accepted): HTTP/2 is not a checkbox — it is the
hardest engineering milestone after the plugin API (ALPN negotiation, stream
multiplexing, HPACK state, flow control, connection lifecycle; precedent:
mitmproxy's multi-year h2 hardening, still shipping limitations like cleartext
h2/gRPC — mitmproxy issue #6998). HTTP/3/QUIC is explicitly out of MVP scope
(see ADR-007).

## Decision

**Frozen protocol ladder:**

| Version | Protocol scope |
|---------|----------------|
| V0.1 | HTTP/1.1 + HTTPS (TLS MITM) only |
| V0.2 | HTTP/2 via ALPN on both legs |
| V0.3 | WebSocket frame inspection inside tunnels |
| Later | HTTP/3 (reverse-proxy mode first, per ADR-007) |

**Crate choices:**
- Listener/proxy: `tokio` + `hyper` 1.x (server + client).
- TLS: `rustls` (ADR-004).
- WebSocket framing (V0.3): `tokio-tungstenite` for frame codec, or own minimal
  frame codec if hook granularity demands it — decided at V0.3, the V0.1 design
  must treat tunnel contents as opaque bytes until then.
- Compression: gzip / deflate / brotli / zstd decompression as a pipeline stage.
  The **original body is always preserved** next to the decoded view.

**Traffic Model rules (the real content of this ADR):**
1. The `Flow` model is protocol-agnostic from day 1: `method, scheme, authority,
   path, ordered headers, body chunks` — never h1-specific types. Headers are an
   **ordered, duplicate-capable list** (h1 truth: repeated `Set-Cookie`, header
   order significance for smuggling research).
2. Each flow records **client-leg and server-leg protocol separately** — they may
   differ (h2 client leg downgraded to h1 server leg in V0.1–V0.2 transition).
3. **Wire truth:** raw request/response bytes are stored capped (default 2 MB),
   alongside the parsed model. Enables protocol-diff debugging and lossless replay.
4. Bodies are streamed; spooled to disk beyond a threshold (default 8 MB) so
   live traffic never balloons RAM.

## Consequences

- The Traffic Model designed against h1-only types would make h2 a rewrite, not a
  feature. This ADR exists to prevent exactly that.
- V0.1 must pass acceptance tests against h2-serving sites (fallback to h1 must
  be silent and correct) even before we speak h2.

## Alternatives Considered

- **Start with h2 in MVP** — rejected: doubles the risk surface before the core
  loop (intercept → modify → replay) is proven.
- **Async-h2 custom implementation** — rejected: use `hyper`/`h2` crates; build,
  don't re-research.

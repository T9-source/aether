# ADR-009: Network stack — tokio + rustls + rcgen, hand-rolled h1 codec

- **Status:** Accepted
- **Date:** 2026-10-08
- **Author:** smart-jemy
- **Phase:** 1 (implementation phase for V0.1)

## Context

Phase 1 must ship the real proxy: an explicit HTTP/1.1 forward proxy with
TLS MITM (ADR-003/004/007). We needed an async runtime, a TLS stack, and an
HTTP/1.1 implementation strategy.

## Decision

1. **Runtime:** `tokio` (multi-thread). Already the core's timeout budget
   host; the network layer standardizes on it.
2. **TLS:** `rustls` 0.23 with the `ring` provider, `tokio-rustls` for both
   accept and connect legs. `rcgen` 0.13 (with `x509-parser`) mints the CA
   and per-host leaves (ADR-004).
3. **HTTP/1.1: hand-rolled codec in `aether-http::wire` — hyper is
   deliberately NOT used.**

## Why hand-rolled h1 (the controversial part)

A security proxy must *own its bytes*:

- **Wire truth (ADR-003):** header order, duplicates, original casing, exact
  chunk framing. hyper normalizes aggressively; research tooling cannot
  work through that filter.
- **Smuggling research:** Aether must be able to *surface* ambiguous
  framings (e.g. `Content-Length` + `Transfer-Encoding` together), not
  silently fix them. Our codec refuses such requests loudly instead of
  laundering them upstream.
- **Bounded capture + streaming (ADR-011):** we need a capture-prefix
  reader that streams the remainder — custom by nature.
- **Scope:** h1 client/server framing is small (~700 lines, fully tested);
  hyper's abstraction tax was not worth it for V0.1.

**Re-evaluation trigger:** V0.2 (h2). The `h2` crate will be evaluated for
the h2 legs; the *domain model* is guaranteed unchanged either way (that was
the point of ADR-003).

## Implementation rules

- Upstream leg: one fresh connection per request, `Connection: close`
  (no pooling in V0.1 — pooling is a V0.2 concern behind the same engine).
- Client leg: keep-alive honored per the client's own preference.
- The MITM acceptor uses a **fixed leaf** per CONNECT authority
  (SNI-independent); `ClientHello.server_name()` is not required.
- The CONNECT → TLS → serve recursion is type-erased at the boundary
  (`AsyncReadWrite`) so `Send` proofs stay finite (E0275).
- Hop-by-hop headers are stripped in both directions, including headers
  named by `Connection:` (RFC 9110 §7.6.1).

## Consequences

- `aether-http` depends on: tokio, rustls, tokio-rustls, rustls-pemfile,
  rustls-pki-types, webpki-roots, rcgen, time — and nothing hyper-shaped.
- Every framing rule is unit-tested in `wire.rs` and end-to-end in
  `crates/aether-http/tests/proxy_e2e.rs` (real sockets, real TLS).

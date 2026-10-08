# ADR-011: Body capture policy + upstream TLS validation knobs

- **Status:** Accepted
- **Date:** 2026-10-08
- **Author:** smart-jemy
- **Phase:** 1

## Context

Security tools must stay responsive on multi-gigabyte downloads yet keep
history faithful. Phase 0 materialized bodies fully (documented handover);
Phase 1 needed the real policy.

## Decision

**1. Bounded capture, honest streaming.**

- `capture_max_body` (default **8 MiB**, per body, request and response).
- **Requests** above the limit fail closed with `413 Content Too Large`
  before any upstream bytes are sent (a captured-prefix-only request body
  cannot be replayed coherently).
- **Responses** above the limit are *streamed through* to the client
  (remainder reader continues exactly after the captured prefix) while the
  history keeps the first `capture_max_body` bytes. The client is never
  short-changed; only history is.
- Content-Length honesty: when a body is fully in memory (decoded, mocked,
  or edited), the daemon recomputes `Content-Length` instead of trusting
  upstream framing. Streaming-remainder responses keep the upstream
  declaration (correct: full length is delivered).
- Intercept holds engage on **fully-captured** bodies only (an edited body
  cannot be reconciled with a streaming remainder).

**2. Upstream TLS validation is configurable, loudly.**

- Default: full webpki-roots validation.
- `upstream_tls_verify = false` (labs/tests) installs an accept-all
  verifier; the daemon logs a warning on startup. Never a silent default.

**3. Host overrides for labs.**

- `host_overrides` maps `"host"`/`"host:port"` → `"socket://host:port"` so
  captures can be replayed against local mocks — also the mechanism that
  makes the e2e suite hermetic.

## Consequences

- Memory is bounded regardless of traffic size.
- The e2e suite proves the streaming path: a 2 MiB response through a
  64 KiB capture window arrives complete at the client, with only the
  prefix in history.
- FTS5 search covers captured bodies; streamed-remainder tails are not
  searchable (documented limitation, revisit with spooling in V0.2).

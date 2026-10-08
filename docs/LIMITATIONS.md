# Aether — Known Limitations (V0 dogfoodable snapshot)

> Status: living document. Every limitation here is a **decision**, not an
> oversight — each points at the ADR that froze it or the backlog item that
> will lift it. A security tool that hides its limits is itself a risk.

## Protocol coverage

| Limit | Why | When it lifts |
|---|---|---|
| HTTP/1.1 and HTTP/2 only; no HTTP/3 upstream leg | ADR-007: QUIC/ECH interception is a research project, not an MVP feature. Browsers silently fall back to TCP when a proxy is configured (h3 is not proxied over CONNECT), so nothing breaks | Explicit, non-goal until the ecosystem demand is proven |
| WebSocket inspection covers h1 upgrades only | RFC 6455 upgrade is an h1 mechanism — over CONNECT tunnels too (the tunnel is peeked: plaintext upgrades are served inside, ADR-013 #3). WS-over-h2 (RFC 8441 `CONNECT + :protocol`) is not intercepted — Chrome uses it only for its own internal channels | Backlog (V0.3+) |
| h2 header **order** is best-effort | The h2 leg uses the `h2` crate (ADR-013); `http::HeaderMap` does not guarantee original ordering. Duplicate headers and values are preserved exactly; ordering is not | If a real-world case needs exact h2 header order, the HPACK layer becomes hand-rolled (tracked, expensive) |
| No h2 trailers (either direction) | Trailers are rare in practice; forwarding them through the intercept model adds a third body phase to every primitive | Backlog |
| h2c (cleartext h2) not supported on the client leg | Browsers never speak h2c to a proxy; prior-knowledge h2c clients are rare | Backlog |

## Body handling

| Limit | Why | When it lifts |
|---|---|---|
| Request bodies are buffered (8 MiB capture cap default) | The pipeline and intercept need the full body to decide. Beyond the cap the request is refused (413/418-class), never silently truncated | Streaming/spooling pass-through mode is designed (ADR-011) and lands with the performance phase |
| Response bodies above the cap stream to the client with only the prefix recorded | Same ADR-011 trade — bounded memory beats full capture for big downloads | Spooling (disk-backed capture) lifts this |
| Response intercept only applies to fully-captured bodies | An edited body cannot be reconciled with an already-streamed remainder — this is a correctness rule, not a missing feature | Same as above |
| No automatic content-decoding (gzip/br shown as received) | The proxy refuses to launder wire truth: what the origin sent is what you see. Decompress-on-display is a viewer concern | Console viewer decompression — backlog |

## Upstream connections

| Limit | Why | When it lifts |
|---|---|---|
| h1 upstream: one fresh connection per request (`Connection: close`) | The V0.1 design (ADR-009): simplest correct framing while the engine was being hardened. The cost is measured, not guessed: `aether bench proxy` attributes ~+4 ms to MITM, largely this | Pooling/keep-alive phase (next performance step after V0.2) |
| Upstream h2 connections are shared **per client connection**, not globally | Multiplexing across unrelated client tunnels complicates failure isolation (one dead origin connection stalls many flows) | Revisit with pooling |

## WebSocket (V0.2)

| Limit | Why | When it lifts |
|---|---|---|
| Relay forwards frames untouched — no frame editing/dropping | Inspection first; modifying live WS frames changes protocol state machines in ways the pipeline model doesn't yet express (that's a V0.3+ primitive discussion) | Plugin-era feature |
| `permessage-deflate` is stripped from the handshake by default | Compressed frames would be uninspectable noise in the record; stripping is the mitmproxy-proven default. Flows are tagged `ws-deflate-stripped` when it happened | Opt-in pass-through of the extension — backlog |
| One frame > 16 MiB ends the relay | Memory-exhaustion guard (`MAX_FRAME_BYTES`), documented in ADR-013 | Configurable cap — backlog |
| Session record is bounded (500 messages / ~1 MB previews) | ADR-011 philosophy: bounded capture. Counters keep the truth (totals always exact) | Spooling |

## Operations

| Limit | Why | When it lifts |
|---|---|---|
| `aether bench proxy` / `aether soak` run an embedded lab, not the installed daemon | They measure the engine in isolation (no control-plane clients attached), which is the correct isolation for a latency/health baseline | A `--control` mode attaching to a live daemon is trivial and planned |
| Soak measures RSS/FD of the lab process | Criterion 12 asks about the daemon's steady state; the lab runs the identical engine+storage wiring | Same as above |
| Console UI ships as a dev build (`apps/aether-ui`); the Tauri bundle needs a desktop machine | Cross-compiling Tauri bundles for macOS/Windows is a founder-machine ritual (M0 criterion 3) | Release engineering phase |

## Things we refuse to do (by design)

- No telemetry, ever (ADR-008).
- No silent header/body rewriting: every mutation is an event, a tag, or a rule.
- No obs-fold, no CL+TE smuggling vectors laundered through the proxy (wire.rs refuses them loudly).
- No upstream TLS verification off by default (`upstream_tls_verify` exists for labs and tests only).

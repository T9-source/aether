# ADR-007: HTTP/3, QUIC, ECH & Interception Strategy

- **Status:** Accepted
- **Date:** 2026-10-08
- **Deciders:** Project founder + architecture review

## Context

Browsers are shipping QUIC/HTTP-3 and ECH (Encrypted Client Hello) by default.
Research findings that constrain us:
- mitmproxy reached full HTTP/3 support only in v11 (Oct 2024) — and Chrome
  **does not trust user-added CAs for QUIC**, making h3 interception impractical
  for real browser testing even for the most mature open-source tool.
- ECH (59%+ browser support, default-on in Firefox/Chrome) encrypts the
  ClientHello. It does **not** break explicit-proxy MITM (we terminate the inner
  TLS with our own generated cert — we control that handshake), but it
  permanently kills transparent/passive network-level interception.
- QUIC is UDP; classic `CONNECT` proxying is TCP-only.

## Decision

1. **Explicit proxy only.** `Browser → (proxy configured) → Aether → Internet`.
   No transparent/redirect mode in V1 — ever, in any phase before V2 planning.
2. **QUIC/HTTP-3 interception: out of scope for V1.** Mitigation strategy:
   - Tooling command `aether net quic-block` (adds a local firewall rule blocking
     UDP/443 while testing) so browsers silently fall back to TCP — the standard
     pragmatic path.
   - Detection & surfacing: when a scoped host is known/likely h3, show a hint in
     the UI ("h3 traffic is bypassing the proxy — enable quic-block").
3. **ECH: no special handling required** for the explicit-proxy path; document
   the reasoning (inner TLS is ours) in user docs. Revisit only if a
   reverse-proxy mode lands.
4. **HTTP/3 roadmap:** later phase, **reverse-proxy mode first** (the only mode
   where h3 interception is practical — mitmproxy 11 precedent), never as
   transparent magic.
5. Acceptance tests for every milestone must include at least one h3-serving
   site (e.g., a major search engine) to prove silent TCP fallback works.

## Consequences

- Massive complexity reduction in V0.x: no UDP/QUIC state machine, no transparent
  network plumbing, no ECH edge cases on the outer handshake.
- Known, documented limitation instead of silent weirdness: h3 traffic either
  falls back (good) or bypasses visibly (surfaced by the hint above).

## Alternatives Considered

- **Transparent interception platform** — rejected: ECH makes it a dead end for
  browsers, and it triples platform-specific plumbing (NFQUEUE/WinDivert/PF).
- **h3 in MVP** — rejected: even mitmproxy ships it with mode restrictions;
  Chrome's user-CA QUIC behavior makes the effort near-useless for our users today.

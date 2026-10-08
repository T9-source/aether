# ADR-004: TLS Interception & Certificates

- **Status:** Accepted
- **Date:** 2026-10-08
- **Deciders:** Project founder + architecture review

## Context

Aether is an **explicit proxy** (ADR-007) performing TLS interception with
dynamically generated certificates. Requirements: per-host leaf certs signed on
the fly by a project-local CA, browser-trustable after CA installation,
pass-through for pinned connections, mobile-friendly CA export later.

## Decision

- **TLS library: `rustls`** (with the `aws-lc-rs` or `ring` provider) on both legs.
- **Certificate generation: `rcgen`** — project-local CA generated per project;
  leaf certificates signed on demand.
- **Leaf cert must satisfy real browser checks** (copy mitmproxy's hard-won
  behavior): SAN `dNSName` for the SNI host *and* the CONNECT authority, plus
  `dNSName`/`iPAddress` SANs for IP hosts, sane validity (short-lived leaves),
  CN kept for legacy tooling.
- **MITM policy per host:** intercept-by-default within scope; **pass-through
  list** (exact host / wildcard / port) for certificate-pinned domains — a pinned
  connection must *pass through untouched and keep working*, never break.
- **Upstream verification:** `rustls-native-certs` root store + optional
  per-project custom CA pool. Disabling upstream verification is an explicit,
  loud, per-project insecure flag (security tooling legitimately needs it).
- **CA key handling:** V0.1 stores the CA private key in the project with `0600`
  perms (plaintext) with an opt-in passphrase; encrypted-at-rest becomes default
  once UX supports it.
- **Installation UX:** `aether ca install` (OS trust store), export `.pem`/`.p12`
  for manual mobile install. The QR-config page (serve proxy settings + CA
  download from the daemon — the `mitm.it` pattern) is scheduled post-M0 as a
  raised-priority item per founder's directive.

## Consequences

- Acceptance tests must include: browser → real HTTPS site with **zero cert
  errors**; pinned-domain pass-through; iOS/Android manual install from export.
- Cert generation must be fast (hot path on first visit per host) — cache leaves.

## Alternatives Considered

- **OpenSSL** — rejected: FFI/build friction, safety, weaker ALPN ergonomics in Rust.
- **native-tls** — rejected: not enough control over cert generation + ALPN plumbing.

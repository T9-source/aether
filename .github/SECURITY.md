# Security Policy

Aether is a security tool — its own security posture is treated as part of
the product, not an afterthought. Zero telemetry, ever.

## Supported versions

| Version | Supported |
|---------|-----------|
| 0.2.x   | ✅        |
| < 0.2   | ❌        |

## Reporting a vulnerability

**Do not open a public GitHub issue for security reports.**

Use GitHub **Security Advisories → Report a vulnerability** (private
disclosure) on the `smart-jemy/aether` repository.

Please include:

- Affected component (`aether-core`, `aether-http`, `aether-store-sqlite`,
  `aether-daemon`, `aether-cli`, console UI) and version (`aether --version`).
- A minimal reproduction (commands, config, traffic sample — **scrub secrets
  and real captured bodies before attaching anything**).
- Expected vs observed behavior, and the ADR invariant you believe was
  violated (see `docs/adr/`), if applicable.

## What to expect

- **48h**: acknowledgement and a severity triage (CVSS v4, context of a
  local-first developer tool).
- **7 days**: confirmed reports receive a fix-or-mitigation plan with a
  target release.
- **Disclosure**: coordinated; credit given unless you prefer anonymity.

## Scope notes

Aether terminates TLS by design (MITM with a locally-trusted CA) and holds
captured traffic, tokens for its own control plane, and generated CA keys on
disk. Threat-model reports should assume the operator's machine, not the
network, is the trust boundary — see `docs/LIMITATIONS.md` for known and
accepted limits, and `docs/AUDIT.md` for the latest internal security audit.

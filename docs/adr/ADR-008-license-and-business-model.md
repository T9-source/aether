# ADR-008: License & Business Model

- **Status:** Accepted (ratified by founder, 2026-10-08)
- **Date:** 2026-10-08
- **Deciders:** Project founder (ratified)

## Context

The positioning (frozen): **local-first, open-source, developer-first,
AI-native API security proxy.** Competitor landscape: Burp Pro at $499/user/year
(2026 pricing); Caido with free tier + ~$200/year individual + team plans;
Hugin as a free single-binary alternative. The business model must fund
long-term development without betraying "local-first" (the wedge), and the
license must be chosen *before* the plugin ecosystem exists — changing a
project's license after people build on it is a trust-breaking event.

## Decision (ratified)

### License: Apache-2.0 for the whole open core

- Core daemon, CLI, Tauri UI, docs, plugin SDK, example plugins — Apache-2.0.
- Why Apache-2.0 over MIT: explicit **patent grant** matters for a dual-use
  security tool; NOTICE file discipline helps when companies embed Aether.
- Why not AGPL: hosting-fear suppresses exactly the corporate plugin authors we
  need for ecosystem cold-start; Caido already occupies freemium-proprietary
  space. Open-core with a clean OSI license is the sharper contrast.
- Future paid tier (team/cloud features, if ever built) ships as proprietary
  add-ons that connect to the core via the public control plane — never as
  forked core code.

### Business model skeleton

| Layer | Price | Scope |
|-------|-------|-------|
| Local (forever free) | $0 | Full proxy, TLS MITM, history, replay, plugins, query language, CLI, single-user local projects |
| Team (paid, later) | TBD | Shared/synced projects, RBAC, hosted collaboration — opt-in cloud only |
| AI | **BYO API key** at first | User chooses provider; calls go only to the provider the user configured |

- **AI + local-first honesty rule:** AI is off by default; every analysis is an
  explicit action; BYO-key means we carry zero inference margin and users carry
  the data decision. "Your traffic never leaves your machine" must remain true
  for everything except actions the user explicitly fires.
- **Telemetry: zero by default, opt-in, documented.** A local-first wedge dies
  instantly if telemetry contradicts it.

### Trademark note

**"HTTPQL" is Caido's query-language name** — Aether's query language must ship
under its own name (candidate: `FlowQL`; availability check required before
announcement).

## Consequences of ratification

- Ecosystem contributors know the ground rules before writing code.
- Marketplace policy (who may charge for plugins, review process) becomes a
  follow-up ADR only after this one is Accepted.

## Alternatives Considered

- **MIT** — acceptable, but drops the patent grant; strictly weaker for this domain.
- **AGPL-3.0** — maximal copyleft protection, wrong fit: suppresses corporate
  plugin authors (the ecosystem cold-start problem is the real enemy).
- **Closed core + open SDK (Caido-style)** — rejected: "open-source" is a
  load-bearing pillar of the chosen positioning, not decoration.

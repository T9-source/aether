# ADR-016 — Daemon decomposition: services own behavior, daemon owns lifecycle

- **Status:** Accepted
- **Date:** 2026-10-09
- **Owner:** T9-source (founder)

## Context

ADR-001 made the daemon the single state owner and `daemon::run` has stayed
a small composition root (~200 lines). But behavior had begun to accrete in
the two places that happened to be nearby: `control.rs` (473 lines) both
implemented the WS protocol *and* owned every operation's behavior (flow
queries, intercept decisions, rule mutations, replay spawning), and
`daemon::run` assembled raw dependencies (`ControlShared` was a bag of
eight loosely-related fields). Left alone, the daemon drifts into a God
object and V1 features (query language, automation, AI) would all land in
`control.rs`.

## Decision

**Accepted.** The layering is made explicit and enforced by structure:

> **The daemon owns lifecycle; services own behavior; the control plane
> owns the wire protocol.**

1. **`services` module** — the behavior layer, one service per capability:
   - `TrafficService` — history reads: list (keyset pagination, ADR-005),
     get, search (FTS5), stats.
   - `InterceptService` — the hold queue: list, show, drop, release,
     enable/disable what the engine holds.
   - `RulesService` — declarative rule CRUD with immediate re-projection
     into the live `HookRegistry` (ADR-006 #1). Rules stay **declarative
     match + action** — deterministic, serializable, inspectable. Rules
     must never grow into a scripting language; complex behavior belongs to
     plugins (ADR-006), which plug into hooks, not into rules.
   - `ReplayService` — background re-execution, id reserved immediately
     (ADR-012 semantics).
   - `CaService` — the CA facts an operator may ask for (ADR-004).
   `Services` is built exactly once by `daemon::run` and never re-built.
2. **`control.rs` is protocol-only** — origin check, auth handshake,
   WS framing, op dispatch. Every dispatch arm is a one-line delegation to
   a service; it serializes whatever the service returns and never touches
   `Storage`/`InterceptState`/`RulesStore` directly. The one exception is
   the event *tap* (`ControlShared.bus`) — forwarding bus events to clients
   is protocol plumbing, not behavior; publishing stays with engine/services.
3. **Growth rule**: a new capability means a new service method (or a new
   service), never a new branch of ad-hoc logic in `control.rs` or
   `daemon.rs`. This is what keeps the daemon from becoming a God object
   through V1.

## Where it lives in code

- `crates/aether-daemon/src/services/{mod,traffic,intercept,rules,replay}.rs`
- `crates/aether-daemon/src/control.rs` — `ControlShared { token, services, bus, … }`
- `crates/aether-daemon/src/daemon.rs` — composition only, documented.

## Consequences

- Every control op is unit-testable without a WebSocket (call the service).
- The UI/CLI/API surface is unchanged — `ControlRequest`/`StatsInfo` DTOs
  and event frames are byte-identical (ADR-010), so clients need no update.
- V0.3 plugins and V1 services (query, automation, AI) slot in as services
  behind the same boundary the control plane already speaks.

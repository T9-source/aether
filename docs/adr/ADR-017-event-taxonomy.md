# ADR-017 — Domain event taxonomy: four categories, no telemetry events

- **Status:** Accepted
- **Date:** 2026-10-09
- **Owner:** smart-jemy (founder)

## Context

ADR-006 #3 said "every security-relevant state change is an event". Without
a taxonomy, two failure modes threaten the bus as the system grows:

1. **Event explosion** — `HeaderParsed`, `ByteRead`, `ByteWritten`, … the
   bus degenerates into m traces with millions of micro-events, drowning
   the operators and the future plugin hosts.
2. **Unclassified growth** — every new feature invents its own event shape,
   and UI filters / plugin subscriptions must hard-code variant lists.

## Decision

**Accepted.** Every `AetherEvent` belongs to exactly one of four
categories, exposed as `EventCategory` and computed by an exhaustive
`AetherEvent::category()` — adding a variant without classifying it is a
compile error:

| Category | Meaning | Variants today |
|---|---|---|
| **Transport** | connection-level decisions an operator must see (never per-byte telemetry) | `PassThrough` |
| **Pipeline** | the request/response pipeline | `RequestReceived/Modified/Forwarded`, `ResponseReceived/Modified/Forwarded`, `HookExecuted`, `WsOpened/Message/Closed` |
| **Security** | operator/analysis outcomes | `FindingCreated`, `InterceptHold`, `InterceptReleased` |
| **Lifecycle** | flow lifecycle and daemon health | `FlowStarted`, `FlowCompleted`, `FlowDropped`, `ErrorOccurred` |

1. **Low-level networking telemetry is not events.** Bytes read/written,
   socket churn, TLS handshake timing, timer ticks — that is
   tracing/metrics (`tracing`), forever. The bus stays at domain altitude.
2. **The category rides the wire**: control-plane event frames become
   `{"type":"event","event":{…},"category":"pipeline"}` so UI filters and
   plugins subscribe by category without hard-coding variant names
   (additive — ADR-010 clients that ignore the field keep working).
3. **Reviewer checklist**: a proposed event that fits none of the four
   categories is by definition not a domain event — it belongs in tracing.

## Where it lives in code

- `aether-core/src/events.rs` — `EventCategory`, `category()`, doc policy.
- `aether-daemon/src/control.rs` — category on pushed frames.

## Consequences

- V0.3 plugins subscribe by category across the WASM boundary (ADR-006) —
  a stable, small contract instead of a growing variant zoo.
- The bus capacity math stays trivial: events are per-exchange and
  per-decision, bounded by traffic, not by bytes.

# Aether — Architecture Decision Records

**Aether** is a local-first, open-source API security proxy built for developers,
with a long-term ambition to become a programmable security workspace.

> Wedge: developers who build and test modern APIs (OpenAPI / GraphQL / JWT / OAuth / WebSocket).
> Expansion path: security researchers, bug bounty hunters, AppSec engineers.
> North star rule: **Don't build 50 features. Build 5 primitives that enable 50 features.**

The five primitives (frozen):
1. **Traffic Model** — protocol-agnostic Flow representation (h1 + h2 semantics)
2. **Pipeline** — single unified traffic pipeline with hook points
3. **Event System** — EventBus emitting every security-relevant state change
4. **Plugin Architecture** — internal extension points from day 1, WASM runtime later
5. **Storage / Query Engine** — SQLite-first, abstraction trait, HTTPQL-like query later (new name required — HTTPQL is Caido's feature name)

## Status Index

| ADR | Title | Status |
|-----|-------|--------|
| [ADR-001](ADR-001-core-daemon.md) | Core daemon vs in-process | **Accepted** |
| [ADR-002](ADR-002-control-plane.md) | Control plane transport | **Accepted** (amended) |
| [ADR-003](ADR-003-http-stack.md) | HTTP stack & protocol roadmap | **Accepted** |
| [ADR-004](ADR-004-tls.md) | TLS interception & certificates | **Accepted** |
| [ADR-005](ADR-005-storage.md) | Storage engine | **Accepted** |
| [ADR-006](ADR-006-plugin-runtime.md) | Plugin runtime (deferred) | **Accepted** |
| [ADR-007](ADR-007-http3-strategy.md) | HTTP/3, QUIC, ECH & interception strategy | **Accepted** |
| [ADR-008](ADR-008-license-and-business-model.md) | License & business model | **Accepted** (Apache-2.0) |
| [MILESTONE-0](MILESTONE-0.md) | Milestone 0 acceptance criteria ("birth moment") | **Active** |

## ADR Lifecycle

`Proposed → Accepted → Superseded`. An Accepted ADR can only be changed by a
new ADR that supersedes it, never by editing history silently.

## Version Ladder (frozen)

| Version | Theme | Scope |
|---------|-------|-------|
| **V0.1** | Usable MVP | HTTP/1.1 + TLS MITM, intercept, history, replay, match & replace, scope/pass-through, SQLite, daemon + Tauri UI + CLI |
| **V0.2** | Modern Web | HTTP/2 (ALPN, streams), then WebSocket inspection |
| **V0.3** | Extensibility | WASM plugin host (Extism), permissions, 3 dogfood plugins, public SDK |
| **V1.0** | Security Platform | Query language, tagging, smart diff, Intruder-style fuzzing, automation, OpenAPI/GraphQL awareness, AI layer (last) |

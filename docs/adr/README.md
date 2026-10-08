# ADR Index — Aether

Architecture Decision Records. One decision, one file, permanent status.
**Author of record: smart-jemy** (founder). All statuses below are final.

| ADR | Title | Status |
|-----|-------|--------|
| [001](ADR-001-core-daemon.md) | Core daemon independent of any UI | Accepted |
| [002](ADR-002-control-plane.md) | WebSocket + JSON control plane | Accepted (amended) |
| [003](ADR-003-http-stack.md) | HTTP stack: h1 first, h2 semantics in the model, wire truth | Accepted |
| [004](ADR-004-tls.md) | TLS MITM: rustls + rcgen, per-project CA, pass-through | Accepted |
| [005](ADR-005-storage.md) | Storage: SQLite via sqlx, keyset pagination, FTS5 | Accepted |
| [006](ADR-006-plugin-runtime.md) | Plugin architecture early, plugin runtime late | Accepted |
| [007](ADR-007-http3-strategy.md) | HTTP/3 strategy: explicit proxy, no h3 interception | Accepted |
| [008](ADR-008-license-and-business-model.md) | License & business model: Apache-2.0 + BYO-key AI, zero telemetry | Accepted |
| [009](ADR-009-network-stack.md) | Network stack: tokio + rustls + rcgen, **hand-rolled h1** | Accepted |
| [010](ADR-010-control-plane-protocol.md) | Control-plane protocol v1 (ops, auth, origin) | Accepted |
| [011](ADR-011-body-capture-policy.md) | Body capture policy + upstream TLS knobs | Accepted |
| [012](ADR-012-console-ui-and-replay.md) | Console UI stack, desktop shell, flows_replay, live rules | Accepted |
| [013](ADR-013-h2-and-websocket.md) | HTTP/2 legs + WebSocket inspection (V0.2) | Accepted |

## Where each decision lives in code

- **ADR-001** → `crates/aether-daemon` (state owner) vs `apps/aether-cli` (pure client)
- **ADR-002/010** → `crates/aether-daemon/src/control.rs`
- **ADR-003** → `crates/aether-core/src/model/` + `crates/aether-http/src/wire.rs`
- **ADR-004** → `crates/aether-http/src/{ca,tls,server}.rs`
- **ADR-005** → `crates/aether-core/src/storage.rs` (port) + `crates/aether-store-sqlite` (impl)
- **ADR-006** → `crates/aether-core/src/{hooks,pipeline,rules}.rs`
- **ADR-007** → explicit proxy only (`aether-http/src/server.rs`); h3 out of scope by design
- **ADR-013** → `aether-http/src/{h2x,ws,server,upstream,tls}.rs` + `aether-core/src/model/ws.rs`
- **ADR-008** → `LICENSE`, `NOTICE`, README licensing section
- **ADR-009** → `crates/aether-http/src/{wire,upstream,server}.rs`
- **ADR-011** → capture limits in `crates/aether-http/src/{wire,upstream,engine}.rs`
- **ADR-012** → `apps/aether-ui/src/**` (console), `apps/aether-desktop/` (shell), `flows_replay` + `registry` in `crates/aether-daemon/src/control.rs`, live registry in `crates/aether-core/src/hooks.rs`, `sync_into` in `crates/aether-daemon/src/rules.rs`

## Convention

- English for ADRs and code (open-source convention), Arabic user docs where
  they help the founder (`docs/guide-ar.md`).
- Superseded ADRs are never edited — a new ADR replaces them.

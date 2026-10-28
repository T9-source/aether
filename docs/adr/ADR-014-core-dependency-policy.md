# ADR-014 — Core dependency policy: zero infrastructure/protocol dependencies

- **Status:** Accepted
- **Date:** 2026-10-09
- **Owner:** T9-source (founder)

## Context

The README and crate docs previously stated the invariant as *"`regex` is
its only computational dependency"* for `aether-core`. Treating a single
crate name as an absolute invariant is architecturally wrong: it freezes an
implementation detail instead of the actual principle, and it would force
awkward abstractions the first time the domain model legitimately needs a
small correctness library (URL parsing, IDs, time, encoding).

The principle that actually matters: **the domain core must not know how
traffic is transported, stored, or displayed.** What `aether-core` may
depend on must be defined by *category*, not by an allowlist of names.

## Decision

**Accepted.** The invariant is restated as:

> **`aether-core` has zero infrastructure/protocol dependencies.**

It must never depend on TLS, TCP, HTTP codecs, WebSocket, SQLite, Tauri,
React — or any crate whose purpose is to move bytes, terminate protocols,
or persist state. What it *may* use falls into exactly three buckets:

1. **Serialization & type-sharing** — `serde`, `serde_json`, `ts-rs`
   (the model is the contract; ADR-002/ADR-006).
2. **Ergonomics** — `thiserror` (error model), `async-trait` (hook traits).
3. **Computation** — `regex` (the serialized rule layer), `base64`
   (binary-body JSON encoding), and `tokio` **primitives only**.

The `tokio` subtlety, made explicit: the EventBus is a broadcast channel
and hooks run under a timeout budget — both are `tokio` *sync/time
primitives*, not infrastructure. `aether-core` therefore declares
`tokio = { default-features = false, features = ["sync", "time"] }`
deliberately **not** inherited from the workspace dependency, and takes
`rt`/`macros` only in `[dev-dependencies]` for `#[tokio::test]`. An async
runtime, network I/O, and filesystem access are infrastructure — the
library graph of the core must compile without them.

If a feature seems to need more than these three buckets, it belongs in
another crate. That is the invariant working as intended, not a limitation.

## Enforcement

- `crates/aether-core/Cargo.toml` carries the policy as a comment and the
  minimal tokio declaration.
- `scripts/check-core-deps.sh` greps the manifest for forbidden markers
  (`net`, `fs`, `rt-multi-thread`, `hyper`, `rustls`, `sqlx`,
  `tokio-tungstenite`, `tauri`) and fails CI on a hit (`.github/workflows/ci.yml`).

## Consequences

- The rule survives new dependencies: a future `url` or `uuid` in the core
  is a *policy* question (computation? yes → allowed), not a violation.
- The core compiles and its unit tests run on a dependency graph with no
  network stack at all — provable with `cargo tree -p aether-core`.
- The old "regex-only" phrasing is retired from README and crate docs.

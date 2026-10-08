# ADR-005: Storage Engine

- **Status:** Accepted
- **Date:** 2026-10-08
- **Deciders:** Project founder + architecture review

## Context

History is a **traffic database**, not a request log. A pentest session can
generate hundreds of thousands of flows and gigabytes of bodies. Requirements:
local-first (single project file), fast structured filtering (foundation for the
future query language), full-text search, safe concurrent reads while the daemon
writes, and a clean abstraction in case a second engine (Postgres/DuckDB) ever
makes sense.

## Decision

**SQLite via `sqlx` (async), WAL mode, one project = one `.aether` database.**

- **Schema versioning from day 1:** `PRAGMA user_version` + `sqlx` migrations.
  Breaking schema changes are expected pre-1.0; migrations are mandatory.
- **Flows table is designed for the query engine:** indexed columns
  (`ts`, `method`, `host`, `status`, `duration_ms`, `client_proto`, `server_proto`),
  tags in JSON1 columns. No list query ever touches body blobs.
- **Bodies:** small inline (threshold, default 64 KiB), larger spooled to blob
  sidecar files referenced by the row; raw-bytes cap per ADR-003.
- **List view reads a summary table** with **keyset pagination**
  (`WHERE id < ? ORDER BY id DESC LIMIT ?`) — `OFFSET` pagination is forbidden
  (breaks at 100k+ flows).
- **Full-text search:** FTS5 over method/host/path/headers (+ bodies, size-capped)
  — this is the substrate the future query language compiles onto.
- **Retention policies per project** (max size / max age) — pentests fill disks.
- **Abstraction:** a `Storage` trait in the core crate. SQLite is the *only*
  implementation in V0.x. No second backend is promised or built.

## Consequences

- Single-writer via the daemon (ADR-001) keeps SQLite happy; readers (UI list,
  CLI export) run concurrently under WAL.
- Performance targets recorded at M0 (measurement before optimization):
  1M flows stored, 100k flows browsable with interactive UI latency.

## Alternatives Considered

- **sled / redb** — rejected: no SQL query surface; we would rebuild a query engine.
- **Postgres from day 1** — rejected: kills local-first and the single-file project model.
- **DuckDB** — deferred: interesting for analytics over flows later; not for the OLTP history path.

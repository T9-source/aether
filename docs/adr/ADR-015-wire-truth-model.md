# ADR-015 — Wire truth model: Observed, Logical, Forwarded are distinct

- **Status:** Accepted
- **Date:** 2026-10-09
- **Owner:** T9-source (founder)

## Context

ADR-003/ADR-009 said "wire truth": Aether keeps original header order,
casing, duplicates, and framing instead of normalizing like hyper. V0.2
(ADR-013) exposed the missing half of that idea: with match & replace,
replay, capture-limit re-framing, and h2↔h1 translation, **the bytes we
observed are no longer the bytes we forward** — and the model had no way to
say so. Treating raw bytes as the canonical model would also make security
analysis operate on transport accidents (header order quirks) instead of
meaning.

The insight to encode: there are *three* representations, not two:

```text
Observed Wire  →  Parser  →  Logical Traffic Model  →  Forwarded Wire
 (client/origin)              (canonical, analyzed)     (what we sent)
```

## Decision

**Accepted.**

1. **The logical model is canonical — never the raw bytes.** Security
   analysis (rules, hooks, future query language, fuzzing) matches on the
   normalized model. Raw representation is *evidence*, kept beside the
   model, never inside it.
2. **Observed ≠ Forwarded, as named concepts.** `FlowMeta` now records four
   explicit, serializable wire facts (per leg):
   - `observed_req_framing` — the body framing as the client's wire
     delivered it, before any rewrite;
   - `forwarded_req_framing` — what we actually sent upstream (de-chunking,
     CL honesty, h2 may change it);
   - `observed_res_framing` — the origin's own declaration;
   - `forwarded_res_framing` — what the client leg will actually see
     (buffered exact-CL, chunked passthrough, close-delimited per RFC 9110
     §6.3, `101` upgrade, or h2 streams).
   The `Framing` enum (`content_length | chunked | close_delimited |
   upgrade | h2_streams`) lives in the core model; `None` means "no body /
   not applicable" — distinct from an explicit `Content-Length: 0`.
3. **Existing wire truth is preserved, not replaced.** Ordered,
   duplicate-capable headers (ADR-003), truthful framing on serialize
   (ADR-009 §close-delimited), and per-leg protocol records remain exactly
   what they were. The facts *name* what those mechanisms produce.
4. **Storage/UI cost stays bounded**: one JSON sidecar column
   (`wire_facts_json`) with a `pragma_table_info` migration (ADR-005
   pattern); old rows deserialize as all-`None` facts; TS bindings gain
   `Framing`/`WireFacts` types via ts-rs.

## Where it lives in code

- `aether-core/src/model/flow.rs` — `Framing`, `WireFacts`, `FlowMeta` fields.
- `aether-http/src/engine.rs` — fact recording (observed at flow creation,
  forwarded/observed at upstream reply, forwarded-res at persist; the
  mocked path crosses no upstream wire and records accordingly).
- `aether-http/src/{upstream,h2x,server}.rs` — leg protocol facts
  (`UpstreamReply.leg_h2`, `ConnInfo.client_h2`).
- `aether-store-sqlite/src/lib.rs` — `wire_facts_json` column + migration.

## Consequences

- "Preserve original casing/order" now has a precise meaning: it applies to
  the **observed** wire; the **forwarded** wire is governed by our framing
  honesty rules and says so in the record.
- Diffing observed vs forwarded framing becomes a one-liner per flow —
  directly useful for smuggling research and match & replace debugging.
- Plugins (V0.3) inherit the facts for free: they are ordinary serialized
  model fields (ADR-006).

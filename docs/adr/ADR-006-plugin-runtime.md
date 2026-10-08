# ADR-006: Plugin Runtime (Deferred) — Plugin Architecture Early

- **Status:** Accepted
- **Date:** 2026-10-08
- **Deciders:** Project founder + architecture review

## Context

Founder's formal amendment (accepted over the earlier "Extism in V1.x" position):
**no plugin runtime in the MVP — at all.** The MVP already carries Proxy, TLS,
Storage, UI, IPC, History, Replay. Adding plugin ABI, lifecycle, capabilities,
sandboxing, versioning, WASM runtime, and an SDK before proving the core would
be infrastructure outrunning the product. At the same time, the plugin system is
part of Aether's identity — it must not be retrofitted in panic later.

> **Plugin system late. Plugin architecture early.**

## Decision

**No WASM runtime, no public SDK in V0.1/V0.2.**
Ladder: `V0.1` internal hooks only → `V0.2` WASM host (Extism) + migrate dogfood
plugins → `V0.3` public SDK, permissions, docs.

**But the following five design rules bind V0.1 from the first commit — these are
what "plugin architecture early" concretely means:**

1. **Everything routes through a `HookRegistry`.** Pipeline stages invoke
   registered hooks; core features (match & replace, tagger) are themselves
   implemented *as internal hooks*, so the future plugin boundary is exercised
   every single day (dogfooding the cut line).
2. **Hook signatures are serialization-shaped:** owned, `Serialize +
   Deserialize` data or capability handles — never borrows of internal state.
   Example contract: `async fn on_request(&self, ctx: &mut RequestContext, flow:
   &mut Flow) -> HookAction`. If a hook can't be expressed this way, the WASM
   boundary is already broken — fix the model, don't special-case.
3. **EventBus is the second extension surface:** every security-relevant state
   change emits an event (ADR-primitive #3). Future plugins consume events
   exactly like internal components do.
4. **`RequestContext` carries a capability map** (scope, config, storage
   handles). No hook reaches global state. Future sandboxing = replacing ctx
   capabilities with brokered handles — a mechanical change, not a redesign.
5. **Hooks have a timeout budget in the trait contract** (per-stage). The
   intercept queue is the explicit, user-driven exception.

## Consequences

- The eventual WASM cut (V0.2) becomes a serialization + hosting change over an
  already-stable boundary — the highest-risk part of plugins (API shape) is
  resolved *before* any runtime exists.
- Risk accepted: a V0.1 feature built outside the hook path will silently poison
  the architecture — reviewed via a simple rule in PR template: "does this go
  through HookRegistry/EventBus?"

## Alternatives Considered

- **Extism/WASM in V0.1** — rejected (founder's amendment): correct technology,
  wrong time; would add a subsystem bigger than the MVP itself.
- **deno_core JS runtime** — deferred/rejected for MVP: heavyweight embedding
  decision; revisit only if the TS-first story proves stronger than WASM-first
  at V0.3 planning.

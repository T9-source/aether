# ADR-012: Console UI stack, desktop shell, replay, live rules

- **Status:** Accepted
- **Date:** 2026-10-08
- **Author:** smart-jemy
- **Phase:** 2

## Context

Phase 2 of the roadmap calls for a real interface: **history, viewer,
intercept, replay** — a UI that survives 100k flows and proves the control
plane (ADR-001/002) is the *only* path to daemon state, for GUI and CLI
alike. Three questions needed decisions: the web stack, the desktop
packaging, and the missing daemon capabilities the UI exposed.

## Decision

**1. Console UI — `apps/aether-ui` (Vite + React + TypeScript, zero runtime
dependencies beyond React).**

- Pure static client: it owns **no state**. History, intercept queue, rules
  and stats live in the daemon; the browser is a viewport with optimistic
  nothing. Closing a tab never touches a running capture.
- Transport is the control plane itself (ADR-002/010): one `ControlClient`
  with auth handshake, strictly FIFO request matching (the daemon dispatches
  sequentially per connection), server-pushed events, and bounded reconnect
  with backoff. No polling anywhere: events say *when*, keyset pagination
  says *what*.
- Types are the ts-rs bindings generated from Rust (single source of truth),
  with one documented adaptation layer: JSON u64 ids are numbers at runtime,
  so `bigint` annotations are normalized to `number`.
- The history table is a hand-rolled fixed-row virtualizer (pure,
  unit-tested range math + a thin ResizeObserver hook). Live new flows
  prepend under a scroll anchor so the viewport never jumps; "N new — jump"
  pill when the operator scrolled away.
- Viewer: structured headers/bodies, JSON pretty-print, raw HTTP view, hex
  dump for binary, TLS facts (version/ALPN), copy-as-cURL.

**2. Desktop shell — `apps/aether-desktop` (Tauri 2), kept outside the
workspace.** The shell is a bare webview (no shell-side plugins, no shell
state; CSP restricts WebSockets to loopback). Its crate compiles only where
webview system libraries exist, so it is deliberately not a workspace member
and is excluded from CI builds. The daemon's origin check additionally
accepts exactly `tauri://localhost` and `https://tauri.localhost` (no
wildcards, no remote hosts).

**3. `flows_replay` op (the operator's direct voice).** Replay executes a
request upstream with **no client leg, no pipeline, no intercept**, records
it as a flow tagged `replay`, and publishes the same lifecycle events as any
other flow. The op reserves the flow id and returns immediately; execution
continues in a background task so a slow origin never stalls the sequential
control plane. Edited request framing (content-length/chunking) is recomputed
by the wire codec — the editor is never trusted about framing.

**4. Live rules — the registry becomes runtime-mutable.** Building the UI
exposed that rules added over the control plane never reached the engine
(the `HookRegistry` was frozen at boot). `HookRegistry` now holds a published
immutable hook set behind `Arc<RwLock<Arc<Vec<_>>>>`: readers take a cheap
snapshot per stage; writers (rule add/remove/enable) publish a new set via
`RulesStore::sync_into`, which replaces only the `rule:*` namespace (stable
hook names `rule:<id>`). Rule edits therefore hit in-flight traffic with no
daemon restart — the ADR-006 "serialized hook layer" promise, now actually
true end to end.

**5. Captured bodies decode to `Text` exactly when valid UTF-8**
(`wire::body_from_captured`). Stored flows become human-inspectable and
text-only rules (body rewrite) can fire; binary stays binary (base64 on the
control plane). Wire-equivalent for forwarding (`Body::as_bytes`).

## Consequences

- The UI is a *third* client (CLI, control tests, console) proving the
  kill-the-UI test from both directions: the daemon runs with no UI, and the
  UI has no privileged backdoor.
- The virtualizer caps DOM size regardless of history size; deep pagination
  walks keyset cursors only (ADR-005 holds in the UI layer).
- Rule sync replaces the `rule:*` namespace atomically; other subsystems can
  register their own namespaces without interference.
- Webview origins are pinned to two exact strings; anything else remote or
  custom-scheme is rejected at handshake.
- The desktop bundle is built on desktop machines; headless CI still verifies
  the workspace, the UI build, and the UI↔daemon contract end to end.

# Milestone 0 — Acceptance Criteria: "The Birth of Aether"

- **Status:** Active
- **Date:** 2026-10-08

> **Birth moment (founder's definition, adopted verbatim):**
> Run `aether`, point a browser at the proxy, open a real HTTPS website —
> Aether performs TLS interception and the UI shows the request/response with
> **zero certificate errors** — all while the Core daemon runs **independently
> of the UI**.

This milestone proves we have a **real product**, not an architecture diagram.

## M0.1 — The Birth Moment (hard gate)

1. `aether serve --project test.aether` starts headless and stays running.
2. Tauri app connects to the daemon over the control plane (ADR-002) and stays
   connected (auto-reconnect on daemon restart).
3. Browser (Chrome or Firefox, real profile, real internet) → HTTPS site →
   request/response visible in UI; no certificate warnings after CA install.
4. **Kill the UI: proxying continues.** Kill the daemon: UI reports disconnect,
   reconnects when daemon returns. (Proves ADR-001 is real, not decorative.)
5. At least one h3-serving site passes with silent TCP fallback (ADR-007).

## M0.2 — Operational Criteria (hard gate)

6. **Intercept works from both clients:** hold a request in the queue, modify
   it, forward it — once from the UI, once from a CLI command
   (`aether intercept list / edit / forward`). The control plane must prove it
   is a first-class interface, not a UI-only shortcut.
7. **Scope + pass-through:** in-scope hosts intercepted; a certificate-pinned
   domain on the pass-through list passes through untouched and *keeps working*.
8. **Match & Replace rule** (built through the HookRegistry path — ADR-006
   rule #1) modifies a request live, visible in both UI and history.
9. **Persistence:** restart the daemon — history and project settings survive;
   bodies render correctly after restart.
10. **Scale smoke test:** ingest 100k flows; the list view remains interactive
    (keyset pagination per ADR-005); opening a flow shows summary + body from
    blob storage without loading neighbors.
11. **Latency measurement exists:** `aether bench` reports added p50/p99
    overhead for passthrough vs direct. (No target to hit at M0 — *measurement
    before optimization*; targets become a V0.1 exit criterion.)
12. **24h soak:** steady-state memory and file-descriptor count under continuous
    browsing traffic; zero crashes; WAL checkpoint healthy.

## M0 Exit Review

- Benchmarks + known-limitations document committed to the repo.
- Demo script (the birth moment, minus secrets) recorded or scripted for
  reproducibility.
- ADR-008 must be **Accepted** before any ecosystem-facing work starts
  (public SDK docs, marketplace plans, contributor onboarding) — not required
  for M0 itself.

## Explicitly out of scope for M0

HTTP/2, WebSocket inspection, plugins/WASM, query language, fuzzing, AI,
mobile QR setup, team features. Anything found "missing" here that is on this
list goes to the backlog, not the milestone.

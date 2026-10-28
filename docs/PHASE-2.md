# Phase 2 — Console UI: history, viewer, intercept, replay

**Status: Complete** · 2026-10-08 · Author: T9-source

Phase 2 of the roadmap ("Tauri UI + history + viewer + intercept + replay")
delivers the first graphical client: a thin console that talks to the daemon
control plane over WebSocket (ADR-002/010) and a Tauri 2 desktop shell around
it (ADR-012). Per ADR-001 the UI owns nothing — every byte of state lives in
the daemon.

## What shipped

### `apps/aether-ui` — the console (Vite + React + TS, no other runtime deps)

- **Connect screen** — control-plane URL + token (persisted in localStorage;
  `?ws=…&token=…` params enable autoconnect for automation).
- **History** — virtualized table (fixed 30px rows, hand-rolled virtualizer,
  DOM size independent of history size). Live: `flow_completed` /
  `flow_dropped` events trigger keyset catch-up (`flows_list after=<newest>`)
  with a scroll anchor so the viewport never jumps; scrolled-away operators
  get a "N new flows — jump" pill instead. Filters: full-text search (FTS
  via `flows_search`, debounced), method select, host glob. Infinite scroll
  downward fetches older pages via `next_cursor`. 100k flows: rows render in
  a fixed-size window; nothing accumulates client-side.
- **Viewer** — request/response tabs; headers table; body views
  (pretty JSON / raw text / hex dump, auto per content-type); raw whole-HTTP
  message toggle; meta chips (protos, duration, sizes, TLS version + ALPN,
  tags incl. `replay`/`dropped`); copy-as-cURL; one-click "↻ Replay".
- **Intercept** — live held queue driven by `intercept_hold`/`intercept_released`
  events; structured editors for held requests (method/scheme/host/port/path/
  query, header rows add/edit/remove, body empty/text/base64) and responses
  (status/headers/body); raw-HTTP editing mode for requests; Forward (with
  edits) or Drop; "view in history" cross-link.
- **Replay** — full structured request editor + raw mode; sends
  `flows_replay` (executes upstream with no client leg, records flow tagged
  `replay`, returns the id immediately); recent-replays list with history
  cross-links.
- **Rules** — list with enable toggles and action summaries; add-rule form
  for all ten actions (header set/remove, path/body regex rewrites, tag,
  drop, mock respond_with) with live JSON spec preview; delete. Edits apply
  to in-flight traffic immediately (live registry, ADR-012).
- **Status bar** — proxy listener (from `stats_get`), flow counters, held
  count, rule count, uptime, last-event ticker, CA certificate download.

### `apps/aether-desktop` — Tauri 2 shell

Bare webview + CSP (loopback WebSockets only), zero shell-side plugins or
state. Built with `cargo tauri build` on desktop machines (webkit2gtk /
WebView2); kept outside the Cargo workspace deliberately.

### Daemon additions (protocol v1 extensions, ADR-012)

- `flows_replay { request } → { flow_id }` — background execution, honest
  framing, `replay` tag, full event stream.
- `stats_get` now carries `proxy_addr` (the UI shows where to point clients).
- Origin check accepts the Tauri webview origins (exact strings only).
- **Live rules**: `HookRegistry` is runtime-mutable (snapshot semantics);
  `RulesStore::sync_into` re-projects `rule:*` hooks after every mutation —
  the serialized-hook-layer promise of ADR-006 is now true end to end.
- `wire::body_from_captured`: captured bodies become `Text` when valid UTF-8
  (viewer shows pretty JSON; text-only rules can fire), else stay `Binary`.

## Verification (all green)

- Rust: **129 tests** workspace-wide (+7 in this phase: replay validation /
  network-error recording / body append semantics, origin allowlist, registry
  resync, body decode); `clippy -D warnings` clean; fmt clean.
- UI: **29 vitest tests** (WS client FIFO + auth + offline + serde-parity +
  reconnect, virtualizer math incl. clamping, formatters, intercept reducer,
  raw-HTTP parser round-trip); `tsc --noEmit` clean; production build
  ~60 KB gzip.
- **End-to-end, real browser against a real daemon** (screenshots in
  `download/screenshots/`):
  - connect → live history of real MITM traffic (example.com, httpbin.org,
    GitHub API, plain-HTTP neverssl.com, 418/403 status colors);
  - viewer: pretty JSON body of a real POST, TLSv1.3/ALPN facts;
  - intercept ON → real request held (curl blocked) → edited query in the
    console → forwarded → **the origin itself echoed the edited args**
    (`edited=by-console&proof=aether`);
  - replay: flow executed upstream, recorded tagged `replay`, visible live;
  - rules: mock rule authored in the console → `example.com` answered
    **418 + mock JSON** through the proxy while non-matching hosts passed
    through — **without restarting the daemon**; persistence survived a
    restart via `rules.json`.

## Deliberately out of scope (later phases)

- h2/WebSocket interception (roadmap: after V0 dogfoodable).
- WASM plugin API (V0.3) — the live registry is its foundation.
- Query language (HTTPQL-like naming is banned; Caido owns that name).

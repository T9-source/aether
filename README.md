# Aether

**Local-first, open-source, developer-first API security proxy** —
with a long-term ambition to become a programmable security workspace.

> Status: **Phase 2 complete** — the console UI works against the real daemon:
> live history (virtualized, 100k-ready), full flow viewer, intercept with
> structured editing, replay, live match & replace rules — in the browser or
> wrapped in a Tauri desktop window.
>
> See [`docs/PHASE-2.md`](docs/PHASE-2.md) · [`docs/PHASE-1.md`](docs/PHASE-1.md) · [`docs/PHASE-0.md`](docs/PHASE-0.md) · [`docs/adr/`](docs/adr/README.md)

---

## Quickstart (2 minutes)

```bash
# 1) start the daemon (history in RAM for a first look)
cargo run -p aether-cli -- serve --memory

# 2) export & install the CA once (browser → import as trusted root CA)
cargo run -p aether-cli -- ca export --out aether-ca.pem

# 3) point any client at the proxy and browse https://…
curl -x http://127.0.0.1:8888 --cacert aether-ca.pem https://example.com/

# 4) watch history from a second terminal
aether flows list
aether flows show 1
```

### The console (new in Phase 2)

```bash
# serve the console UI, then open http://localhost:5173 in a browser
cd apps/aether-ui && npm install && npm run dev
# → connect with:  ws://127.0.0.1:9099  + the token printed by `aether serve`
```

In the console: live history (virtualized), full request/response viewer
(pretty JSON / raw / hex, TLS facts, copy-as-cURL), intercept hold→edit→
forward/drop with structured editors, one-click replay of any request
(plus a dedicated replay editor), and live match & replace rules with
mock/rewrite/tag actions — all driving the same daemon the CLI talks to.
A Tauri 2 desktop shell wraps the same UI (`apps/aether-desktop`, built on
desktop machines — see its README).

Manual interception (the Burp-style loop, from the CLI only — no UI needed):

```bash
aether intercept set --request on        # hold new requests
aether intercept ls                      # see held flows
aether intercept show 7                  # dump as JSON
aether intercept release 7 --header "X-Reviewed: yes" --body-file req.json
aether intercept drop 7                  # or kill it (client gets 502)
```

Match & replace rules — pure JSON, live over the control plane (edits hit
in-flight traffic immediately, no daemon restart):

```bash
aether rules add --file examples/rule-stamp-header.json
aether rules ls
```

Scale gate (Milestone-0 #10):

```bash
aether bench flows --n 100000
# insert ≈12k/s · list page(50) p50 ≈ 0.5 ms · FTS5 search ≈ 70 ms over 100k
```

Latency baseline & steady-state health (Milestone-0 #11/#12, V0 dogfoodable):

```bash
aether bench proxy          # direct vs plain-proxy vs MITM (p50/p95/p99)
aether soak --secs 60       # continuous traffic + RSS/FD sampling (--secs 86400 = the 24h run)
scripts/demo.sh             # the birth moment, reproducible in ~30s, minus secrets
```

Known limits are documented, not hidden: see `docs/LIMITATIONS.md`.

---

## Structure

```
aether/
├── crates/
│   ├── aether-core/          # domain core: traffic model, pipeline, events,
│   │   └── bindings/         #   rules, storage port — zero protocol knowledge
│   ├── aether-http/          # network engine: hand-rolled h1 (wire truth),
│   │                         #   TLS MITM (rustls+rcgen), CA, intercept, engine
│   ├── aether-store-sqlite/  # ADR-005 backend: WAL, keyset pagination, FTS5
│   └── aether-daemon/        # state owner: proxy+storage+control plane wiring
├── apps/
│   ├── aether-cli/           # `aether` — pure WS client of the daemon (ADR-001)
│   ├── aether-ui/            # console: history/viewer/intercept/replay/rules
│   └── aether-desktop/       # Tauri 2 shell around the console (ADR-012)
├── examples/                 # aether.toml + rule files
├── docs/
│   ├── adr/                  # 12 accepted ADRs, all linked to code
│   ├── PHASE-0.md            # primitives: DoD + proof
│   ├── PHASE-1.md            # real proxy: DoD + scale numbers
│   ├── PHASE-2.md            # console UI: DoD + end-to-end proofs
│   └── guide-ar.md           # quickstart بالعربي
└── .github/workflows/ci.yml  # fmt + clippy -D warnings + tests + TS bindings
```

## Architecture invariants

1. `aether-core` knows nothing about TLS / SQLite / WebSocket / Tauri / React
   / wire codecs. `regex` is its only computational dependency.
2. GUI, CLI and CI are all just clients of the daemon (ADR-001) — killing the
   UI never touches traffic.
3. Every pipeline stage routes through the `HookRegistry`; match & replace
   ships as serialized `RuleSpec`s → `RuleHook`s (ADR-006 #1/#2).
4. Every security-relevant state change is an event on the bus; drops are
   *tagged decisions*, never silent failures.
5. The proxy owns its bytes: hand-rolled h1 codec, original header order and
   casing preserved, ambiguous framings refused loudly (ADR-009).

## Roadmap (frozen ladder)

| Version | Scope | Status |
|---------|-------|--------|
| V0.1 | h1 + TLS MITM + history + intercept + rules + control plane | ✅ shipped (Phase 1) |
| V0.2 | **console UI + Tauri shell + replay + live rules** | ✅ **shipped (Phase 2)** |
| V0.3 | h2 inspection, WebSocket, upstream pooling, spooling | next |
| V0.4 | WASM plugins (Extism) + plugin SDK | planned |
| V1.0 | query language, fuzzing, automation, AI layer (BYO-key, off by default) | planned |

## Founder

Built by **smart-jemy**. Apache-2.0 — see [LICENSE](LICENSE) and
[ADR-008](docs/adr/ADR-008-license-and-business-model.md). Zero telemetry,
ever.

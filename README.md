# Aether

**Local-first, open-source API security proxy built for developers** —
with a long-term ambition to become a programmable security workspace.

> Status: **Phase 0 complete** — architectural primitives proven.
> See [`docs/PHASE-0.md`](docs/PHASE-0.md) and [`docs/adr/`](docs/adr/README.md).

## Structure

```
aether/
├── crates/
│   └── aether-core/        # domain core: traffic model, pipeline, events
│       └── bindings/       # generated TypeScript types (ts-rs, CI-verified)
├── apps/
│   └── aether-cli/         # `aether` — thin client of the core (kill-the-UI invariant)
├── docs/
│   ├── adr/                # architecture decision records (8 ADRs, all Accepted)
│   └── PHASE-0.md          # definition of done + status + proof transcript
└── .github/workflows/ci.yml
```

## Architecture invariants

1. `aether-core` knows nothing about hyper / TLS / SQLite / WebSocket / Tauri / React.
2. GUI, CLI and CI are all just clients of the core daemon (ADR-001).
3. Every pipeline stage routes through the `HookRegistry` (ADR-006 #1).
4. Hook signatures are serialization-shaped — WASM-cut later is a hosting
   change, not a redesign (ADR-006 #2).
5. Every security-relevant state change is an event (primitive #3).

## Try the Phase-0 proof

```bash
cargo run -p aether-cli -- demo   # Request → ctx → hooks → events → modified request
cargo run -p aether-cli -- info   # invariants summary
cargo test --all                  # 60 tests
```

## License

Apache-2.0 — see [ADR-008](docs/adr/ADR-008-license-and-business-model.md)
for the ratified license & business-model decision.

# Contributing to Aether

First: thank you. Aether is a security workspace built in public — every
contribution is reviewed like it will be attacked, because it will be.

## Development environment

- **Rust 1.85+** (edition 2024) — `rustup update stable`
- **Node 20+** for the console UI
- Linux is the primary dev platform; macOS/Windows welcome

```bash
cargo build --workspace
cargo test --workspace
cd apps/aether-ui && npm install && npm test && npm run typecheck
```

## Non-negotiable rules (CI enforces these)

1. `cargo fmt --all -- --check` — clean.
2. `cargo clippy --workspace --all-targets -- -D warnings` — zero warnings.
3. `aether-core` keeps **zero infrastructure/protocol dependencies**
   (ADR-014). `scripts/check-core-deps.sh` fails the build otherwise.
4. TypeScript bindings are generated from Rust (`ts-rs`); if
   `cargo test -p aether-core --test ts_bindings` changes files, commit the
   refreshed bindings.
5. New parsers of untrusted bytes get a fuzz target (`fuzz/`) — no
   exceptions (ADR-018).
6. Supply-chain: `cargo deny check` must pass (`deny.toml`).

## Architecture decisions

Any change that touches invariants (wire truth, event taxonomy, storage
port, control-plane protocol) requires an ADR in `docs/adr/` **before** the
PR that implements it. Copy `docs/adr/ADR-001-core-daemon.md` as the
template: context → decision → consequences → linked code.

## Pull requests

- Small, single-purpose PRs beat large ones.
- Every PR must state which phase/ADR item it advances (the roadmap ladder
  in `README.md` is frozen — see `docs/adr/README.md`).
- Tests prove the behavior, not the implementation: pipeline stages are
  tested against the `HookRegistry`, storage against the port, not against
  SQLite specifics.
- Security-relevant changes (TLS, CA, control plane, rules) require a
  threat-model note in the PR description: what can an attacker on the
  network / with a malicious site do now that they could not before?

## Reporting vulnerabilities

Never in public issues — see [SECURITY.md](.github/SECURITY.md).

## License

By contributing you agree your contributions are licensed under
Apache-2.0 (see [LICENSE](LICENSE) and ADR-008).

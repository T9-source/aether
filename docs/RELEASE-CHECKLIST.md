# Release checklist — v0.2.1

The gate closes when the 24h soak acceptance passes. This is the exact
sequence to cut the release; nothing here is optional.

## 0. Preconditions (all must be green)

- [ ] CI green on `main` (fmt + ADR-014 check + clippy -D warnings + tests +
      TS bindings freshness + cargo-deny + perf budget + fuzz smoke + UI)
- [ ] The soak gate: `aether soak --secs 86400 --assert-mode` on a machine
      that stays up — verdict `pass:true` (ADR-018 item 4, the last open
      item; the nightly CI soak at 5h50m is the drift monitor, not the
      acceptance)
- [ ] `docs/PHASE-5.md` updated: the soak line moves from 🟡 to ✅

## 1. Version bump

```bash
# workspace version 0.2.0 → 0.2.1 (Cargo.toml [workspace.package])
sed -i 's/^version = "0.2.0"/version = "0.2.1"/' Cargo.toml
cargo build --workspace   # refresh Cargo.lock version entries
```

- [ ] CHANGELOG.md: rename `## [Unreleased]` → `## [0.2.1] — YYYY-MM-DD`,
      add a fresh empty `## [Unreleased]` above it

## 2. Branch protection (do this BEFORE tagging — GitHub settings, not code)

Settings → Branches → Add rule for `main`:

- [ ] Require a pull request before merging — **but** allow the founder to
      bypass for now (solo project); the point is the next two lines
- [ ] Require status checks to pass: `test`, `security`, `perf`,
      `fuzz-smoke`, `ui` (the job names from ci.yml)
- [ ] Require branches to be up to date before merging
- [ ] Do not allow bypassing the above (dependabot included)

Dependabot policy (was a review finding: six-major tokio-tungstenite and
friends were merged in minutes):

- [ ] Major bumps never auto-merge — close the PR, open an issue, review the
      changelog of the new major, then bump manually with a benchmark run
- [ ] Consider `@dependabot ignore` labels or reduce `open-pull-requests-limit`

## 3. Tag, release, verify

```bash
git tag -a 0.2.1 -m "v0.2.1 — WS relay backpressure freeze fix, MCP hardening, nightly soak CI"
git push origin 0.2.1
```

- [ ] GitHub Release from the tag: paste the CHANGELOG `## [0.2.1]` section
      as the notes, attach the CHANGELOG link + the AUDIT.md link
- [ ] Verify the release page renders the fix list (this is the first
      release — it is the public record of the stabilization gate)

## 4. Post-release

- [ ] Re-check `docs/comparison.md` quotes (its own rule: re-verify each
      release) — update the "checked" date
- [ ] Announce with the benchmark numbers from README (12k flows/s, 0.5ms
      page p50, MITM +1.15ms p50) — numbers age slower than adjectives

# Changelog

All notable changes to Aether are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **WASM plugin host (V0.3, ADR-019/020).** Third-party plugins compiled to
  `wasm32-wasip1` run in-process, sandboxed (no network, no filesystem, no host
  functions beyond WASI), with static manifest permissions. The hook contract is
  frozen at the serialization shape: request/response *views* in, a declarative
  edit-set out — applied by the host, never by the plugin. Pooled warm call
  ≈ 100 µs (compile once, instance pool per call). Plugins register after the
  built-ins under a stable `plugin/<name>/` prefix.
- New crate `aether-plugins`: manifest parsing (fail-closed `plugin.toml` with
  clamped timeout/memory bounds, capability vocabulary, wasm-path escape guard),
  wire types, Extism-based hook adapter, and the runtime registry (load / list /
  enable / disable).
- **Fail-closed plugin startup**: an enabled plugin that fails to load refuses
  daemon startup — before any listener binds. Disabled plugins are parsed only;
  their failures are recorded and surfaced, not fatal.
- **Checksum pinning**: `plugin.toml` may pin the artifact with `sha256`
  (64 lowercase hex, `sha256sum` output pastes directly); a mismatched byte is a
  refused plugin. Full signing stays deferred (ADR-020).
- `[[plugins]]` config section (relative paths anchor to the config file's
  directory; unknown keys rejected like every other section) + example plugin
  sources (`examples/plugins/{hello,sleepy,mute}`) and
  `scripts/build-spike-plugins.sh`.

## [0.2.1] - 2026-10-12

> **Re-built (2026-10-13):** the tagged 0.2.1 binaries are built from the
> commit that includes this Security section — the original tag briefly
> pointed one commit earlier; assets were replaced the same day.

### Security (audit follow-ups — H1 + M1..M7 + lows)

- **Fixed: the console could leak the daemon token to an attacker-chosen
  server.** A deep link like `?ws=ws://attacker.example` (no `auth` param)
  made the console auto-reconnect using the saved token against the
  attacker's daemon — full traffic read, rule edits, replay. The saved auth
  value is now only ever reused with the exact URL it was saved for
  (`initialConn` hard invariant + attack-shaped tests).
- Fixed: the data directory (`~/.aether`) is now `0700` and the SQLite
  database `0600` — captured traffic (cookies, tokens, bodies) is no longer
  world-readable (WAL/SHM inherit the directory mode).
- Fixed: the h2 client leg now caps concurrent streams at 128 — unlimited
  streams × 8 MB capture bodies was a memory-DoS.
- Fixed: the CA leaf cache is now a bounded LRU (1024) — random-subdomain
  scanners no longer grow it forever.
- Fixed: unknown keys inside a config section are rejected at startup
  (`deny_unknown_fields`); `examples/aether.toml` moved the four engine
  settings (`passthrough`, `upstream_alpn_h2`, `upstream_tls_verify`,
  `capture_max_body`) out of `[intercept]`, where they were silently ignored.
- Fixed: responses no longer launder duplicated origin `Content-Length`
  headers (request-smuggling vector); framing is recomputed from the
  buffered body on every response.
- Fixed: a slow console consumer that falls behind the event bus now gets a
  `missed_events` frame and stays connected, instead of being silently
  disconnected by the old `Lagged → None → "bus closed"` path.
- Fixed: `cargo-deny` is downloaded with a pinned sha256 in CI.
- Fixed: private files (CA key, daemon token) are created with `0600` from
  the first byte (TOCTOU window closed).
- Fixed: blind-tunnel `dial_tcp` now has a 10s connect timeout.
- Changed: the console stores the daemon auth value in `sessionStorage`
  (no longer persists across browser sessions); in-flight requests are
  failed on socket loss instead of crossing reconnects; auth-failed no
  longer triggers 12 reconnect attempts.
- CI: added `concurrency` groups and `timeout-minutes` to every job.


### Fixed

- **WebSocket relay could freeze under full-duplex backpressure.** The relay
  loop used `write_all()` without `flush()`; with tokio-rustls, `write_all`
  only guarantees bytes reached the encrypted internal queue — not the wire.
  When a peer was busy sending while the relay wrote to it (the exact shape
  of a megabyte echo), the tail of the write stayed buffered while the loop
  blocked on the next read — the stream froze until the next client-bound
  frame, which might never come. The relay now flushes after every frame;
  the megabyte acceptance test went from ~50% hangs to 28/28 green, and a
  `FlushSpy` unit test pins the flush contract
  (`relay_leg_flushes_after_every_forwarded_frame`).
- **MCP `initialize` now answers per the MCP spec.** It echoed any
  client-requested `protocolVersion` back, including versions the server
  cannot speak. It now echoes only a supported version and otherwise
  responds with its own latest (`initialize_downgrades_unsupported_protocol_version`).
- **MCP server shutdown is real.** The shutdown flag was dead code — polled
  between blocking stdin reads and never set. The loop is now
  `select!`-driven over a `watch` channel wired to Ctrl-C, so a silent
  stdin can no longer hold the server hostage.
- **MCP page limits are clamped, not truncated.** A huge `limit` used to
  `as u32`-truncate silently (u64::MAX → u32::MAX pages); it now clamps to
  `MAX_PAGE_LIMIT` (200).
- Removed the stale `apps/aether-cli/src/mcp.rs` — an undeclared duplicate
  of `commands/mcp.rs` whose own test copy masked the initialize defect.
- Removed the last three stale `docs/guide-ar.md` references (README tree,
  `docs/adr/README.md` language policy, `MILESTONE-0.md`).

### Added

- Nightly long-soak CI workflow (`.github/workflows/soak.yml`): 5h50m
  `aether soak --assert-mode` under the hosted-runner ceiling, gating on the
  machine-readable `SOAK_JSON` verdict with the log as an artifact. The
  one-time 24h acceptance run remains a scheduled runtime task per ADR-018.

## [0.2.0] — initial public release

Phase 0–3B of the frozen ladder: h1 + TLS MITM + history + intercept +
replay + rules + SQLite + control plane + console UI, HTTP/2 and WebSocket
inspection, the V0 dogfoodable kit (bench/soak/demo/LIMITATIONS), and the
V0.2.x stabilization gate artifacts (AUDIT, fuzz targets, perf budgets,
soak gate). See `docs/PHASE-*.md` and `docs/adr/` for the full record.

[Unreleased]: https://github.com/T9-source/aether/compare/0.2.1...HEAD
[0.2.1]: https://github.com/T9-source/aether/compare/0.2.0...0.2.1
[0.2.0]: https://github.com/T9-source/aether/releases/tag/0.2.0

# ADR-019: V0.3 WASM plugin host — Extism, static permissions, declarative outcomes

- Status: Accepted (spike implemented on `v03-impl`; daemon wiring in ADR-020)
- Date: 2026-10-13
- Drivers: V0.3 goal "AI-native, developer-first extensibility" without giving up the
  security posture the 0.2.x audit established

## Context

Aether's hook architecture (ADR-006) was designed so that cutting hooks over a WASM
boundary is a *hosting change, not an API redesign*: signatures are serialization-shaped,
everything routes through the `HookRegistry`, and the pipeline already enforces per-hook
timeouts. V0.3 makes that real: third-party plugins compiled to `wasm32-wasip1` run
in-process, sandboxed, with no host functions beyond WASI.

Six decisions were settled with the maintainer before any code (V0.3 planning round):
design + spike now on a branch; Extism as the runtime; `on_request` + `on_response`
only for 0.3.0; Rust SDK + accept any compliant wasm; static manifest permissions;
host lives in a new crate `aether-plugins`.

## Decision

### Hosting model — compile once, pool per call

- The wasm module compiles once at plugin load into an `extism::CompiledPlugin`
  (wasmtime codegen is the expensive part — never per request).
- Each call instantiates through an `extism::Pool` (`Plugin::new_from_compiled`).
  Measured on the spike: **pooled warm call ≈ 104 µs median / 122 µs p90 vs ≈ 2111 µs
  for a fresh instantiation — 13×**. Pooling is not optional.
- Subscriptions are **discovered, not declared**: the pool probes
  `function_exists("on_request" / "on_response")` at load (results cached). A plugin
  exporting neither is a load error; exporting one means the other stage continues
  without a call.

### The contract — views in, declarative outcome out

The frozen serialization shape (ADR-006 rule #2):

- **In**: `HttpRequestView` / `HttpResponseView` — method/status, full URL, ordered
  header pairs, body as lossy UTF-8 truncated at 1 MiB with an honest
  `body_truncated` flag. A plugin never silently sees half a body pretending to be
  the whole one.
- **Out**: `PluginOutcome` — `action` (`continue`/`stop`), `set_headers`,
  `remove_headers`, `set_body`. The plugin never mutates traffic directly; the **host
  applies the declared edit-set**. Every modification is auditable host behavior, and
  the semantics mirror the rules engine (`set` replaces all occurrences of a name,
  `remove` deletes all — same as `Headers::insert`/`remove`).
- Dropping traffic must be deliberate: unknown `action` strings **fail open to
  `continue`**; only a literal `"stop"` aborts.

### Permissions v1 — static manifest

`plugin.toml` declares: `name` (`[a-z0-9-]{1,64}`), `version`, `wasm` (relative path),
`timeout_ms` (clamped 10..=5000), `max_pages` (clamped 1..=256, default 256 = 16 MiB),
`capabilities` (subset of the existing `Capabilities` vocabulary:
`modify_traffic`, `read_history`, `write_history`, `network_access`, `emit_findings`),
and `sha256` (optional artifact pin, see ADR-020).

Fail-closed everywhere: unknown keys rejected (`deny_unknown_fields` — same policy as
the daemon config, audit M4), unknown capabilities rejected, and the `wasm` path must
resolve *inside* the plugin directory (canonicalize + prefix check).

Sandbox by default: the extism manifest grants **no** `allowed_hosts` and **no**
`allowed_paths` — no network, no filesystem, no host functions beyond WASI. The v1
hook contract needs none of those; capabilities are declarative vocabulary that the
management plane surfaces (and future capability-gated host functions will enforce).

### Timeouts — one budget, two layers

The manifest's `timeout_ms` is enforced twice: by extism's epoch-interruption timer
(`Error::msg("timeout")` on expiry) and by an outer `tokio::time::timeout` around the
whole pool-wait + call. The pool queue is inside the plugin's allowance, not on top of
it. Both layers map to `AetherError::HookTimeout` — the same error the pipeline
already produces for native hooks (ADR-006 rule #5).

### Statelessness contract

Pooled instances share wasm memory across rotations. Anything a plugin keeps in
memory is **undefined behavior** for a hook host. The example plugins are deliberately
stateless. (A future `plugin_state` host function, if ever, becomes a deliberate
capability — not an accident.)

### Memory — why 256 pages

The first spike build used `max_pages = 16` (1 MiB): the WASI+serde startup path
OOM-killed before `main` ran. 256 pages (16 MiB) is the default and the ceiling; the
manifest may lower it, never raise it.

## Consequences

- Zero pipeline changes: a plugin *is* a hook, registered after the built-ins under
  the stable `plugin/<name>/` prefix (unregister-by-prefix gives unload without
  restart).
- Plugin authors need `extism-pdk` + `wasm32-wasip1` and nothing else. The Rust SDK
  crate (planned 0.3) will wrap the wire types so authors never hand-roll JSON.
- The pipeline's fingerprint-based modification detection (`RequestModified` /
  `ResponseModified`) observes host-applied plugin edits for free.
- Deferred to later versions: hot-reload (edit-then-enable covers the need today),
  plugin signing beyond checksum pinning, capability-gated host functions,
  `on_connect`/`on_flow_complete` stages, marketplace/registry.

## Alternatives considered

- **wasmtime directly (no Extism)**: full control, but we would re-implement
  instance pooling, epoch-timer wiring, WASI bootstrapping, and the PDK conventions
  that Extism already solved. Spike showed Extism's API fits our frozen contract
  without fighting it.
- **wasmtime + Component Model (wasi:0.2)**: the cleaner long-term ABI, but the
  toolchain surface for authors is heavier today; the serialization-shaped contract
  is already component-shaped, so migrating later is a wire-format version bump,
  not a redesign.
- **Out-of-process plugins (RPC)**: strongest isolation, but deployment/latency
  story collapses for a local-first desktop tool; 100 µs pooled calls would become
  milliseconds of IPC.

## Evidence

- Spike tests: 14/14 (5 integration incl. SPIKE_JSON perf output), workspace 197/0,
  fmt + clippy clean.
- Epoch interruption proven by `sleepy` (500 ms of WASI sleep under a 10 ms budget —
  cut off in ~10 ms, surfaced as `HookTimeout`).
- Performance baseline in `SPIKE_JSON` above; artifacts built by
  `scripts/build-spike-plugins.sh` (missing artifacts fail tests, never skip).

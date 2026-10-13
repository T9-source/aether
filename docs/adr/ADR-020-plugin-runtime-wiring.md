# ADR-020: Plugin runtime wiring — lifecycle, fail-closed startup, checksum pinning

- Status: Accepted (implemented on `v03-impl`)
- Date: 2026-10-14
- Drivers: turn the ADR-019 spike into a shipped daemon feature for 0.3.0; decisions
  settled with the maintainer in the implementation-planning round

## Context

ADR-019 landed the plugin host as a library (`aether-plugins`). What 0.3.0 ships is
the *runtime wiring*: how plugins are configured, when they load, what a failed load
means, how enable/disable behaves at runtime, and how the artifacts themselves are
trusted on disk.

Six decisions were confirmed with the maintainer: the spike is re-created as the base
of the implementation branch (environment loss — the code, not the design, was
replayed); an enabled plugin failing to load refuses startup; `aether.toml` is the
single source of truth for persistence; checksum pinning enters 0.3.0 (full signing
stays deferred); the work ships as one PR per phase (wiring → management plane →
SDK); and a practical third example plugin joins 0.3.0.

## Decision

### Configuration — `[[plugins]]`

```toml
[[plugins]]
path = "/home/me/.aether/plugins/hello"   # dir with plugin.toml + plugin.wasm
enabled = true                            # default: true
```

- One entry per plugin directory; relative paths resolve against **the config file's
  directory**, never the daemon's CWD (a config moved between hosts keeps working).
- `deny_unknown_fields` — a typo in a plugin entry fails startup loudly, exactly like
  every other section (audit M4 policy).
- The daemon parses straight into `aether_plugins::PluginEntry` — the vocabulary lives
  in the plugins crate so the daemon stays a consumer.

### Fail-closed startup

If an **enabled** plugin fails to load — manifest missing, artifact missing, sha256
mismatch, wasm that doesn't compile, zero exported hooks — `daemon::run` returns
`Err` **before any listener binds**. A security tool that silently drops its plugins
is worse than one that refuses to run half-configured.

A **disabled** plugin is parsed (bounds, capabilities, path guard) but not compiled;
failures there are recorded in the registry, surfaced as `state = failed` with the
error string, and are non-fatal — the operator opted out.

### Lifecycle — the config is the single source of truth

- `PluginRegistry::load(entries, &HookRegistry)` runs during startup, registering
  plugin hooks **after** the built-ins (stable priority-0 order → core guarantees run
  first; plugins are extensions, not replacements).
- Runtime `enable(name)` re-reads the artifact from disk (fresh bytes, fresh checksum
  verification) and registers. Runtime `disable(name)` calls
  `unregister_prefix("plugin/<name>/")` and drops the compiled artifact (wasm memory
  released). Both are **ephemeral** — they die with the daemon. Persisting a change
  means editing `aether.toml`, which stays trivially auditable and versionable like
  everything else local-first.
- Enable-after-edit is a poor man's reload: no hot-reload feature in 0.3.0, but
  disabling + enabling (or enabling a previously disabled plugin after an edit) always
  runs the bytes that are on disk *now*.

### Integrity — checksum pinning in, signing deferred

`plugin.toml` may pin the artifact:

```toml
sha256 = "aeae8e918f3b…"   # 64 lowercase hex — sha256sum output pastes directly
```

The host verifies the pin **before** compilation; one flipped byte is a refused
plugin (`Config` error naming both digests). Pins are optional — local-first loading
from your own disk is the 0.3.0 threat model — but when present they turn "someone
swapped my plugin.wasm" from silent compromise into a loud startup refusal.

Deferred: ed25519 signatures + keyring. That is a distribution concern (who may
install what), and 0.3.0 deliberately has no installer or marketplace yet.

### Observability

`DaemonInfo` gains `plugins_loaded` (configured entries: loaded + disabled + failed).
The management plane (list / enable / disable over the control plane, MCP tools, and
a UI page with declared-capability badges) is the next phase on top of this registry.

## Consequences

- `aether-daemon` depends on `aether-plugins`; `aether-plugins` depends only on
  `aether-core` — no cycles, the SDK (planned) sits next to `extism-pdk` on the
  author side.
- Tests: registry lifecycle against real wasm (enable → live call → disable →
  re-enable), fail-closed startup through `daemon::run`, config parsing with
  relative-path resolution, checksum pinning (correct/wrong/absent), and an
  end-to-end proxied request stamped by the hello plugin observed at the origin.
- The UI/MCP surface can already rely on `PluginRegistry::list()` — the snapshot is
  serializable and includes declared capabilities, subscription status, and the
  honest failure reason per plugin.

## Alternatives considered

- **Skip-with-warning startup**: rejected — see fail-closed rationale; the control
  plane can still show a `failed` entry for *disabled* plugins where the operator
  already opted out.
- **Runtime state file (~/.aether/plugins-state.json)**: two sources of truth that
  can disagree after a crash; the config file is already the artifact users inspect
  and copy.
- **API writes back to aether.toml**: cleanest UX, but it needs a config writer +
  comment/format preservation; deferred until the management plane phase, where it
  belongs.

## Evidence

- `cargo test -p aether-plugins` — manifest bounds/guards, wire semantics, checksum
  pinning, registry lifecycle against real wasm, SPIKE_JSON perf baseline.
- `cargo test -p aether-daemon` — plugins section parse + fail-loud, fail-closed
  startup, end-to-end stamped traffic (`echo:from-wasm` at the origin).

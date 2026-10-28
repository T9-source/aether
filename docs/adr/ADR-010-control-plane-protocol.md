# ADR-010: Control-plane protocol v1 (WebSocket + JSON)

- **Status:** Accepted
- **Date:** 2026-10-08
- **Author:** T9-source
- **Phase:** 1

## Context

ADR-002 froze the transport (WebSocket + JSON, one codec, auth + origin
check, types generated from Rust). Phase 1 needed the concrete protocol.

## Decision

**Wire shapes** (single tagged JSON objects, UTF-8, one frame per message):

```text
client → server   {"op": "<op>", "params": {…}}
server → client   {"type":"response", "ok": true, "result": …}
                  {"type":"response", "ok": false, "error": {"kind": …, "message": …}}
                  {"type":"event", "event": {…AetherEvent…}}   // unsolicited
```

**Connection lifecycle:**

1. Upgrade with origin check: `Origin` header, when present, must be a
   loopback origin (`localhost` / `127.0.0.1` / `::1`, any port) — else 403
   at handshake. Non-browser clients (no Origin) pass.
2. First frame within 10s MUST be `{"op":"auth","params":{"token":…}}`.
   Mismatch → one error frame, then close. Comparison is constant-time.
3. Post-auth: any number of requests; events are pushed anytime.

**Ops (v1)** (amended by [ADR-012]: `flows_replay` added; `stats_get` carries `proxy_addr`; tauri webview origins allowlisted)**:** `ping`, `flows_list`, `flows_get`, `flows_search`, `stats_get`,
`intercept_list`, `intercept_show`, `intercept_drop`, `intercept_release`,
`intercept_set`, `rules_list`, `rules_add`, `rules_remove`,
`rules_set_enabled`, `ca_export_pem`.

**Type generation (ADR-002):** request/response DTOs derive `ts-rs` and are
exported to `crates/aether-core/bindings/` — the same directory as the
domain types, so the future GUI consumes one npm-shaped folder.

**Error kinds:** the `AetherError::kind_name()` strings
(`config`, `network`, `protocol`, `storage`, `tls`, `hook`, …) — stable,
machine-readable, part of this contract.

## Consequences

- The CLI is a *pure client* of this protocol (kill-the-UI, ADR-001):
  `aether flows list` is a WS round-trip, nothing else.
- Adding ops is additive; v1 clients ignore unknown frames.
- Interception is cooperative-blocking: the daemon parks a flow with a
  oneshot; `intercept_release` resolves it (forward-edited or drop).

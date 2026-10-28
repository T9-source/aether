# Security self-audit — 2026-10-11 (stabilization gate, ADR-018 item 2)

- **Scope:** every surface that parses untrusted bytes, makes a trust
  decision, or stores a secret, on the V0.2 tree (`d96cce5` + gate work).
- **Method:** line-level review of the five security-sensitive areas named in
  ADR-018, each backed by its test evidence; supply-chain scan via
  `cargo-deny` (advisories + licenses + sources). Findings are either **fixed
  in this commit** or **filed with a boundary** in `LIMITATIONS.md` — no
  silent acceptance.
- **Reviewer:** T9-source, with the ADR-018 gate as the forcing function.

## 1. h1 framing decisions (`aether-http/src/wire.rs`)

**Invariants reviewed**

- Head size is capped at `MAX_HEAD_BYTES` (64 KiB), each line at
  `MAX_LINE_BYTES` (16 KiB) — no unbounded read on attacker-controlled
  framing (`read_until_needle` hard-errors at the cap).
- Body framing is decided per-response from *observed* facts: `Content-Length`
  wins, `chunked` wins over CL (RFC 9110 §6.3 conflict rule), `close-delimited`
  only when nothing else delimits, `Upgrade`/`101` never gets a body read.
  A chunked body that exceeds the capture cap degrades to **stream-through +
  close-delimited truth**, never to a fabricated `Content-Length` (regression
  test from the framing-fix commit).
- `parse_request_head` rejects obs-fold and bare-LF line endings; CR/NUL
  injection into the head is a parse error, not a smuggled header.

**Evidence:** `wire` unit tests + `v01_e2e` framing tests.

**Findings:** none open. The chunked→close-delimited fix from the previous
phase holds under re-review; the cap paths are covered by explicit tests.

## 2. CONNECT handling & tunnel peek (`aether-http/src/server.rs`)

**Invariants reviewed**

- CONNECT is rejected on the MITM leg (`CONNECT inside a MITM tunnel is not
  supported`, 400) — no nested-tunnel confusion.
- Pass-through is an explicit, default-empty include list; anything not
  listed gets the MITM path. No wildcard default.
- The tunnel peek reads **3 bytes under `idle_timeout`** — a client that
  never speaks cannot wedge a worker slot; timeout is an error, not a hang.
- TLS detection is the standard 2-byte ClientHello prefix (0x16 0x03);
  misclassification can only degrade a tunnel the client chose to send
  garbage into — it grants no privilege.

**Evidence:** `v01_e2e` CONNECT tests, `v02_e2e` ws-over-CONNECT test.

**Findings:** none open.

## 3. Control-plane origin & token (`aether-daemon/src/control.rs`)

**Invariants reviewed**

- Default binds are loopback-only (`127.0.0.1:8888` proxy,
  `127.0.0.1:9099` control) — remote exposure requires an explicit config
  change by the operator.
- Token: 32 bytes from the OS CSPRNG (`getrandom`), hex-encoded, written
  with `0600` permissions, required as the **first frame** of every
  connection; everything else is rejected with a generic auth error.
- Browser attack surface (CSWSH / DNS-rebinding): `origin_allowed` admits
  only loopback origins (any port) and the two exact Tauri shell origins.
  Lookalikes (`tauri://localhost.evil.com`, `127.0.0.1.evil.com`,
  custom `app://` schemes, `null`) are rejected by tests
  (`origin_allows_tauri_shell_only`, `origin_rejects_remote_and_garbage`).
- The origin check is browser defense-in-depth; the token is the actual
  authentication — a non-browser client bypasses nothing by forging an
  Origin header, because auth still gates every operation.

**Findings**

- **F1 (fixed): `constant_time_eq` leaked the token length.** The old
  implementation returned early on `len` mismatch, so a local process able
  to time auth responses could learn the token's length before guessing
  bytes. Replaced with a max-length loop that folds `0` past the shorter
  slice — runtime depends only on the maximum length, result on the XOR
  accumulator. Regression test
  `constant_time_eq_is_correct_for_all_length_classes` covers equal/unequal
  lengths, empty inputs, and multi-byte UTF-8.
- **F2 (filed): auth failures are not rate-limited.** A local process can
  brute-force the token space offline-fast. Bound accepted as residual risk:
  the token is 256-bit random, the surface is loopback-only by default, and
  rate limiting belongs to the control-plane hardening pass, not V0.2.x.
  Filed in `LIMITATIONS.md`.

## 4. Capture policy (ADR-011) — `wire::read_body`, `FlowMeta` sizing

**Invariants reviewed**

- Capture caps are enforced **inside** the read loop (`take(cap)`, chunked
  accumulator check) — memory per flow is bounded by config, not by attacker
  ambition; oversize bodies stream through with true sizes recorded.
- Recording honesty: sizes counted truly, previews bounded; `body_from_captured`
  promotes to `Text` only on valid UTF-8.

**Evidence:** body-cap unit tests, `ws_relay_forwards_control_frames_and_megabyte_messages`
(1 MB relay byte-for-byte, 128 KB recorded at true size).

**Findings:** none open.

## 5. h2 + WebSocket surface (ADR-013)

**Invariants reviewed**

- Malformed h2 input kills the *connection*, never the daemon
  (`malformed_h2_bytes_are_rejected_and_daemon_survives`).
- RST_STREAM/CANCEL is absorbed and the connection keeps serving; graceful
  GOAWAY completes in-flight work and cuts GOAWAY-ignoring clients at the
  30 s drain deadline (nginx `worker_shutdown_timeout` precedent).
- WS relay bounds staging; the assembler caps fragmentation state
  (ADR-013), so a hostile peer cannot grow memory without sending data.

**Evidence:** the four `v02_e2e` acceptance tests (ADR-018 Part 1).

**Findings:** none open. WS *interception* (editing frames in flight) is
explicitly deferred and listed in `LIMITATIONS.md` — an honesty boundary,
not a vulnerability.

## 6. Supply chain (`cargo-deny`, gate half B)

- `advisories ok, bans ok, licenses ok, sources ok` on 2026-10-11
  (cargo-deny 0.19.0, fresh advisory DB).
- **F3 (fixed): `rustls-pemfile` unmaintained** (RustSec). Migrated to the
  `rustls-pki-types::pem` module (the upstream successor), removing the
  dependency entirely. Behavior covered by existing CA/PEM tests.
- License allow-list is OSI-permissive only; unknown registries/git sources
  are denied.

## Verdict

The gate's audit item is **passed** with F1/F3 fixed in-tree, F2 filed as a
documented residual risk. Re-audit trigger: any change to
`wire.rs`/`server.rs`/`control.rs` framing or auth logic, or a new ADR that
moves the trust boundary.

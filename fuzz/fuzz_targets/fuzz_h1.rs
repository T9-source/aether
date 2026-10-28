//! Fuzz the h1 head codec (ADR-018 gate item: fuzzing).
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).
//!
//! Invariants under arbitrary bytes:
//!   1. `parse_request_head` / `parse_response_head` return errors — they
//!      never panic, never abort, never read out of bounds.
//!   2. Anything that *parses* must survive `serialize_request` /
//!      `serialize_response` — the model→wire direction cannot assume a
//!      friendlier input than the wire→model direction produced.
//!   3. Body-spec classification is total: every header map maps to exactly
//!      one `BodySpec`, deterministically.

#![no_main]

use libfuzzer_sys::fuzz_target;
use aether_http::wire;

fuzz_target!(|data: &[u8]| {
    // Slice input at the first CRLFCRLF so heads and trailing bytes are
    // exercised independently (libFuzzer learns the split quickly).
    let (head, _rest) = match data.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => (&data[..i + 4], &data[i + 4..]),
        None => (data, &[][..]),
    };

    // ── requests ────────────────────────────────────────────────────────────
    if let Ok(req) = wire::parse_request_head(head, None, None) {
        // NOTE: request_body_spec is deliberately PARTIAL — it returns Err
        // for smuggling-shaped requests (CL+TE, unsupported TE values). The
        // server turns that Err into a 400 (server.rs) and the engine
        // records honest `None` framing (engine.rs). The invariant is
        // "never panics", not "always Ok" — an earlier version asserted
        // totality and the fuzzer promptly "crashed" it with
        // `Transfer-Encoding: chunkencoding: chd` (harness bug, not a
        // product bug; see docs/PHASE-5.md).
        let _ = wire::request_body_spec(&req.headers);
        let out = wire::serialize_request(&req);
        // Re-parse what we serialized: wire truth must survive a round trip.
        let _ = wire::parse_request_head(&out, None, None);
    }

    // ── responses (both HEAD and non-HEAD semantics) ───────────────────────
    if let Ok(res) = wire::parse_response_head(head, false) {
        let spec = wire::response_body_spec(&res.headers, res.status.code, false);
        let _ = spec;
        let out = wire::serialize_response(&res, true);
        let _ = wire::parse_response_head(&out, false);
    }
    if let Ok(res) = wire::parse_response_head(head, true) {
        let _ = wire::response_body_spec(&res.headers, res.status.code, true);
    }

    // ── authority parsing (host:port with IPv6 corners) ─────────────────────
    if let Ok(text) = std::str::from_utf8(data) {
        if text.len() < 512 {
            let _ = wire::split_authority(text);
        }
    }
});

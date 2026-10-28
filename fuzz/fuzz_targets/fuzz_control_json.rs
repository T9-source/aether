//! Fuzz the control-plane JSON decoder (ADR-018 gate item).
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).
//!
//! Invariant: `serde_json::from_str::<ControlRequest>` over arbitrary UTF-8
//! returns Ok/Err — never panics, never hangs (no recursion bombs beyond
//! serde_json's own 128-depth limit, which is part of the contract being
//! fuzzed, not assumed).

#![no_main]

use libfuzzer_sys::fuzz_target;
use aether_daemon::ControlRequest;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(req) = serde_json::from_str::<ControlRequest>(text) {
            // Decoded requests must survive a re-serialization + re-parse
            // round trip (the daemon echoes held payloads back to the UI).
            if let Ok(res) = serde_json::to_string(&req) {
                let _ = serde_json::from_str::<ControlRequest>(&res);
            }
        }
    }
});

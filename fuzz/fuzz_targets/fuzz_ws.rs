//! Fuzz the RFC 6455 frame reader + fragment assembler (ADR-018 gate item).
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Invariants under arbitrary bytes:
//!   1. `read_frame` returns `FrameError` (Eof / Io / Protocol / TooLarge) —
//!      never panics, never allocates beyond `MAX_FRAME_BYTES`.
//!   2. `MessageAssembler::feed` is total over any frame sequence; fragmented
//!      state is bounded (cap), so a hostile peer cannot grow memory without
//!      sending corresponding data.

#![no_main]

use libfuzzer_sys::fuzz_target;
use aether_http::ws::{MessageAssembler, read_frame};

fuzz_target!(|data: &[u8]| {
    // One current-thread runtime, built once per process (libFuzzer calls
    // this target millions of times; runtime construction dominates cost).
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    let rt = RT.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("fuzz runtime")
    });

    let mut cursor = std::io::Cursor::new(data.to_vec());
    let mut assembler = MessageAssembler::new();
    let mut frames = 0usize;

    loop {
        match rt.block_on(read_frame(&mut cursor)) {
            Ok(frame) => {
                frames += 1;
                if frames > 64 {
                    break; // keep one iteration bounded; corpus learns long chains anyway
                }
                let _ = assembler.feed(&frame);
            }
            Err(_) => break, // any error ends the stream — same as the relay does
        }
    }
});

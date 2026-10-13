//! sleepy — 500ms of WASI sleep under a 10ms budget (ADR-019 spike).
//!
//! Proves the epoch-interruption path: the host must cut this off and
//! surface `AetherError::HookTimeout`, not hang for the full sleep.

use extism_pdk::{plugin_fn, FnResult, Json};

#[plugin_fn]
pub fn on_request(_view: Json<serde::de::IgnoredAny>) -> FnResult<Json<serde_json::Value>> {
    // 500 × 1ms — small sleeps so wasip1 sched_yield actually returns.
    for _ in 0..500 {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let outcome = serde_json::json!({ "action": "continue" });
    Ok(Json(outcome))
}

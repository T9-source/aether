//! hello — stamps `x-aether-hello: from-wasm` (ADR-019 spike example).
//!
//! Subscribes to BOTH hooks so the host can prove: header edits flow
//! through the declarative outcome, not direct mutation.

use extism_pdk::{plugin_fn, FnResult, Json};
use serde::Deserialize;

#[derive(Deserialize)]
struct RequestView {
    #[allow(dead_code)]
    #[serde(default)]
    method: String,
    #[allow(dead_code)]
    #[serde(default)]
    url: String,
}

#[plugin_fn]
pub fn on_request(view: Json<RequestView>) -> FnResult<Json<serde_json::Value>> {
    let _ = view;
    let outcome = serde_json::json!({
        "action": "continue",
        "set_headers": [
            {"name": "x-aether-hello", "value": "from-wasm"}
        ]
    });
    Ok(Json(outcome))
}

#[plugin_fn]
pub fn on_response(_view: Json<serde::de::IgnoredAny>) -> FnResult<Json<serde_json::Value>> {
    let outcome = serde_json::json!({
        "action": "continue"
    });
    Ok(Json(outcome))
}

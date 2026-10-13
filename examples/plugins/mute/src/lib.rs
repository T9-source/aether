//! mute — a valid wasm module that exports no hook stages (ADR-019 spike).
//!
//! The one exported function is a non-hook helper; `function_exists` probing
//! for `on_request`/`on_response` must find nothing, and `WasmHook::load`
//! must refuse the plugin.

use extism_pdk::{plugin_fn, FnResult, Json};

#[plugin_fn]
pub fn describe(_input: Json<serde::de::IgnoredAny>) -> FnResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({ "plugin": "mute" })))
}

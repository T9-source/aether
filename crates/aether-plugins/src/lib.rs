//! aether-plugins — the V0.3 WASM plugin host (ADR-019/020).
//!
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).
//!
//! Three moving parts, each small on purpose:
//!
//! - [`manifest`] — parses `plugin.toml` (declarative permissions, timeout,
//!   memory budget, optional sha256 pinning). Fail-closed: unknown keys,
//!   unknown capabilities, or a `wasm` path escaping the plugin directory are
//!   all hard errors.
//! - [`wire`] — the serialization-shaped hook contract (ADR-006 rule #2):
//!   request/response *views* go in, a [`wire::PluginOutcome`] comes out.
//!   The plugin never mutates traffic directly — the **host** applies the
//!   declared edit-set, so every modification is auditable host behavior.
//! - [`host`] — [`WasmHook`], the [`aether_core::hooks::Hook`] adapter.
//!   Compile once (`extism::CompiledPlugin`), instantiate through an
//!   `extism::Pool` per call (warm pooled call ≈ 100 µs vs ≈ 2 ms fresh —
//!   pooling is not optional), epoch-interrupted on timeout.
//!
//! Zero pipeline changes: a plugin *is* a hook in the existing registry,
//! registered after the built-ins under the stable `plugin/<name>/` prefix.

pub mod host;
pub mod manifest;
pub mod registry;
pub mod wire;

pub use host::WasmHook;
pub use manifest::PluginManifest;
pub use registry::{PluginEntry, PluginInfo, PluginRegistry, PluginState};
pub use wire::{HttpRequestView, HttpResponseView, PluginOutcome};

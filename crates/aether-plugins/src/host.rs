//! [`WasmHook`] — the [`Hook`] adapter that runs a plugin instance per call.
//!
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).
//!
//! Hosting model (ADR-019):
//! - **compile once**: the wasm module compiles into a
//!   `extism::CompiledPlugin` at load time (wasmtime codegen is the expensive
//!   part — never per request);
//! - **pool per call**: each call instantiates through an `extism::Pool`
//!   (warm pooled call ≈ 100 µs vs ≈ 2 ms fresh — pooling is not optional);
//! - **one budget, two layers**: the manifest's `timeout_ms` is applied by
//!   extism's epoch-interruption timer *and* by an outer `tokio::time::timeout`
//!   around the whole pool-wait + call — a plugin can never exceed its budget
//!   by getting stuck in the pool queue, and epoch cancellation maps to
//!   `AetherError::HookTimeout` like any other hook timeout (ADR-006 #5);
//! - **subscriptions are discovered**, not declared: the pool probes
//!   `function_exists("on_request" / "on_response")` at load. A plugin
//!   exporting neither is a load error; exporting one is fine — the other
//!   stage reports not-subscribed and continues without a call;
//! - **sandbox by default**: the extism manifest grants no `allowed_hosts`
//!   and no `allowed_paths` — no network, no filesystem, no host functions
//!   beyond WASI. Permissions v1 are declarative (see `manifest`).

use aether_core::context::RequestContext;
use aether_core::error::{AetherError, AetherResult};
use aether_core::hooks::{Hook, HookAction, HookResult};
use aether_core::model::{Request, Response};
use async_trait::async_trait;
use extism::{CompiledPlugin, Plugin, PluginBuilder};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::manifest::PluginManifest;
use super::wire::{HttpRequestView, HttpResponseView, PluginOutcome};

/// Timeout for the pool's one-time subscription probe at load.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct WasmHook {
    /// Registry name: `plugin/<name>/hook` — lives under the
    /// `plugin/<name>/` prefix so `unregister_prefix` unloads exactly this
    /// plugin and nothing else (no `hello` vs `hello2` collisions).
    hook_name: String,
    manifest: PluginManifest,
    compiled: Arc<CompiledPlugin>,
    pool: Arc<extism::Pool>,
    subscribed_request: bool,
    subscribed_response: bool,
}

impl std::fmt::Debug for WasmHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmHook")
            .field("hook", &self.hook_name)
            .field("plugin", &self.manifest.name)
            .field("timeout_ms", &self.manifest.timeout())
            .field("subscribed_request", &self.subscribed_request)
            .field("subscribed_response", &self.subscribed_response)
            .finish_non_exhaustive()
    }
}

impl WasmHook {
    /// Loads, verifies, compiles and probes a plugin from its directory.
    ///
    /// Fail-closed: manifest problems, a pinned-sha256 mismatch, wasm
    /// compilation failure, or *zero* exported hooks are all load errors.
    pub fn load(dir: &Path, manifest: PluginManifest) -> AetherResult<Self> {
        // Integrity pin (ADR-020 §integrity): verify before anything runs.
        let wasm = manifest.wasm_bytes(dir)?;
        if let Some(pin) = &manifest.sha256 {
            use sha2::Digest;
            let digest = hex::encode(sha2::Sha256::digest(&wasm));
            if &digest != pin {
                return Err(AetherError::config(format!(
                    "plugin '{}': sha256 mismatch — artifact is {digest}, pinned {pin}",
                    manifest.name
                )));
            }
        }

        // Host manifest: artifact + memory ceiling + per-call timeout, and
        // *no* allowed_hosts / allowed_paths — deny by default (ADR-019).
        let host_manifest = extism::Manifest::new([extism::Wasm::data(wasm)])
            .with_timeout(Duration::from_millis(manifest.timeout()))
            .with_memory_max(manifest.pages());
        let builder = PluginBuilder::new(host_manifest).with_wasi(true);

        let compiled = CompiledPlugin::new(builder).map_err(|e| {
            AetherError::config(format!("plugin '{}': compile failed: {e}", manifest.name))
        })?;
        let compiled = Arc::new(compiled);

        let pool = Arc::new(extism::Pool::new({
            let compiled = Arc::clone(&compiled);
            move || Plugin::new_from_compiled(&compiled)
        }));

        // Discover subscriptions once; the pool caches the probe results.
        let subscribed_request =
            pool.function_exists("on_request", PROBE_TIMEOUT)
                .map_err(|e| {
                    AetherError::config(format!("plugin '{}': probe failed: {e}", manifest.name))
                })?;
        let subscribed_response =
            pool.function_exists("on_response", PROBE_TIMEOUT)
                .map_err(|e| {
                    AetherError::config(format!("plugin '{}': probe failed: {e}", manifest.name))
                })?;
        if !subscribed_request && !subscribed_response {
            return Err(AetherError::config(format!(
                "plugin '{}': exports neither on_request nor on_response — nothing to run",
                manifest.name
            )));
        }

        Ok(Self {
            hook_name: format!("plugin/{}/hook", manifest.name),
            manifest,
            compiled,
            pool,
            subscribed_request,
            subscribed_response,
        })
    }

    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    /// The compiled artifact this hook owns (pool instances derive from it).
    pub fn compiled(&self) -> &Arc<CompiledPlugin> {
        &self.compiled
    }

    pub fn subscribes_request(&self) -> bool {
        self.subscribed_request
    }

    pub fn subscribes_response(&self) -> bool {
        self.subscribed_response
    }

    /// One pooled call with the double-layer budget. `stage` is the export
    /// name; `payload` the serialized view; returns the parsed outcome.
    async fn call_stage(
        &self,
        stage: &'static str,
        payload: String,
    ) -> AetherResult<PluginOutcome> {
        let pool = Arc::clone(&self.pool);
        let budget = Duration::from_millis(self.manifest.timeout());
        let hook_name = self.hook_name.clone();

        // Outer layer: covers pool wait + call with the SAME budget — the
        // pool queue is inside the plugin's allowance, not on top of it.
        let raw = tokio::task::spawn_blocking(move || {
            pool.with_plugin(budget, |plugin| {
                plugin.call::<&str, Vec<u8>>(stage, payload.as_str())
            })
        });

        match tokio::time::timeout(budget, raw).await {
            Err(_elapsed) => Err(AetherError::HookTimeout {
                hook: hook_name,
                timeout_ms: budget.as_millis() as u64,
            }),
            Ok(Err(join_err)) => Err(AetherError::Hook {
                hook: hook_name,
                message: format!("runtime join: {join_err}"),
            }),
            Ok(Ok(Ok(Some(bytes)))) => {
                let outcome: PluginOutcome =
                    serde_json::from_slice(&bytes).map_err(|e| AetherError::Hook {
                        hook: hook_name,
                        message: format!("invalid outcome JSON: {e}"),
                    })?;
                Ok(outcome)
            }
            // Pool exhausted within budget — same shape to the caller as a
            // plain timeout.
            Ok(Ok(Ok(None))) => Err(AetherError::HookTimeout {
                hook: hook_name,
                timeout_ms: budget.as_millis() as u64,
            }),
            Ok(Ok(Err(e))) => {
                // extism's epoch timer surfaces expiry as a bare "timeout";
                // pool and call errors arrive on the same layer here.
                if e.to_string().contains("timeout") {
                    Err(AetherError::HookTimeout {
                        hook: hook_name,
                        timeout_ms: budget.as_millis() as u64,
                    })
                } else {
                    Err(AetherError::Hook {
                        hook: hook_name,
                        message: e.to_string(),
                    })
                }
            }
        }
    }
}

#[async_trait]
impl Hook for WasmHook {
    fn name(&self) -> &str {
        &self.hook_name
    }

    async fn on_request(&self, _ctx: &mut RequestContext, req: &mut Request) -> HookResult {
        if !self.subscribed_request {
            return Ok(HookAction::Continue);
        }
        let view = HttpRequestView::from_request(req);
        let payload = serde_json::to_string(&view).map_err(|e| AetherError::Hook {
            hook: self.hook_name.clone(),
            message: format!("serialize request view: {e}"),
        })?;
        let outcome = self.call_stage("on_request", payload).await?;
        if outcome.action_is_stop() {
            return Ok(HookAction::Stop);
        }
        outcome.apply_to_request(req);
        Ok(HookAction::Continue)
    }

    async fn on_response(&self, _ctx: &mut RequestContext, res: &mut Response) -> HookResult {
        if !self.subscribed_response {
            return Ok(HookAction::Continue);
        }
        let view = HttpResponseView::from_response(res);
        let payload = serde_json::to_string(&view).map_err(|e| AetherError::Hook {
            hook: self.hook_name.clone(),
            message: format!("serialize response view: {e}"),
        })?;
        let outcome = self.call_stage("on_response", payload).await?;
        if outcome.action_is_stop() {
            return Ok(HookAction::Stop);
        }
        outcome.apply_to_response(res);
        Ok(HookAction::Continue)
    }
}

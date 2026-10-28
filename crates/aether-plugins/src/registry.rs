//! [`PluginRegistry`] — load, list, enable, disable (ADR-020 §lifecycle).
//!
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).
//!
//! Lifecycle rules, all settled in the V0.3 planning round:
//!
//! - **the config is the single source of truth**: `aether.toml` says which
//!   plugins exist and whether they start enabled. Runtime enable/disable is
//!   ephemeral — it dies with the daemon, and that is the documented deal.
//! - **fail-closed startup**: if an *enabled* plugin fails to load, the
//!   daemon refuses to start. A security tool that silently drops its
//!   plugins is worse than one that refuses to run half-configured.
//!   Disabled plugins are parsed but not compiled — failures there are
//!   recorded, surfaced, and non-fatal (the operator opted out).
//! - **enable() loads lazily**: a disabled plugin holds no wasm memory; the
//!   moment it is enabled, its artifact is re-read from disk (fresh bytes,
//!   fresh checksum verification) — which also makes edit-then-enable a
//!   poor man's reload, without a hot-reload feature.
//! - **plugins register after built-ins**: same priority (0), later
//!   registration — the pipeline's stable sort keeps core guarantees ahead
//!   of extensions.

use aether_core::error::{AetherError, AetherResult};
use aether_core::hooks::HookRegistry;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use super::host::WasmHook;
use super::manifest::PluginManifest;

/// One `[[plugins]]` entry from the daemon config. Lives here (not in
/// aether-daemon) so the daemon can deserialize straight into it and the
/// registry stays daemon-agnostic.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEntry {
    /// Plugin directory (contains `plugin.toml` + the wasm artifact).
    /// Relative paths resolve against the config file's directory.
    pub path: PathBuf,
    /// Load and register at startup. Default: true.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
    /// Loaded and registered — participating in the pipeline.
    Loaded,
    /// Configured but not running; no wasm memory held.
    Disabled,
    /// Configured, enabled, but load failed — see `error`.
    Failed,
}

/// The management-plane projection of one plugin (serializable snapshot).
#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: Option<String>,
    pub dir: PathBuf,
    pub manifest: PluginManifest,
    pub state: PluginState,
    /// Set when `state == Failed` — the honest reason, shown in the UI.
    pub error: Option<String>,
    /// Declared permissions, echoed for the management plane.
    pub capabilities: Vec<String>,
    /// Live subscription status (false unless `state == Loaded`).
    pub subscribes_request: bool,
    pub subscribes_response: bool,
}

struct Entry {
    info: PluginInfo,
    hook: Option<Arc<WasmHook>>,
}

pub struct PluginRegistry {
    entries: RwLock<Vec<Entry>>,
}

impl std::fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let list = self.list();
        f.debug_struct("PluginRegistry")
            .field("plugins", &list.iter().map(|p| &p.name).collect::<Vec<_>>())
            .finish()
    }
}

impl PluginRegistry {
    /// Loads every configured entry against `hook_registry`.
    ///
    /// Enabled + load failure → `Err` (startup refused, all failures listed).
    /// Disabled → parsed only, `Disabled`, no compile. Enabled + success →
    /// compiled, probed, registered under `plugin/<name>/`.
    pub fn load(entries_cfg: &[PluginEntry], hook_registry: &HookRegistry) -> AetherResult<Self> {
        let mut entries: Vec<Entry> = Vec::new();
        let mut failures: Vec<String> = Vec::new();
        let mut seen: Vec<String> = Vec::new();

        for cfg in entries_cfg {
            match Self::load_one(&cfg.path, cfg.enabled) {
                Ok(entry) => {
                    if seen.contains(&entry.info.name) {
                        failures.push(format!(
                            "plugin '{}' configured more than once",
                            entry.info.name
                        ));
                        continue;
                    }
                    seen.push(entry.info.name.clone());
                    if cfg.enabled {
                        hook_registry
                            .register(Arc::clone(entry.hook.as_ref().expect("enabled has hook"))
                                as Arc<dyn aether_core::hooks::Hook>);
                    }
                    entries.push(entry);
                }
                Err(e) => {
                    if cfg.enabled {
                        failures.push(e.to_string());
                    } else {
                        // Disabled plugins may be broken on disk — recorded,
                        // surfaced, non-fatal. Best-effort identity only.
                        entries.push(Entry {
                            info: PluginInfo {
                                name: dir_name(&cfg.path),
                                version: None,
                                dir: cfg.path.clone(),
                                manifest: stub_manifest(&dir_name(&cfg.path)),
                                state: PluginState::Failed,
                                error: Some(format!("disabled, load failed: {e}")),
                                capabilities: Vec::new(),
                                subscribes_request: false,
                                subscribes_response: false,
                            },
                            hook: None,
                        });
                    }
                }
            }
        }

        if failures.is_empty() {
            Ok(Self {
                entries: RwLock::new(entries),
            })
        } else {
            Err(AetherError::config(format!(
                "plugin load failed (refusing to start, ADR-020): {}",
                failures.join("; ")
            )))
        }
    }

    fn load_one(dir: &std::path::Path, enabled: bool) -> AetherResult<Entry> {
        let manifest = PluginManifest::load(dir)?;
        let mut info = PluginInfo {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            dir: dir.to_path_buf(),
            manifest: manifest.clone(),
            state: PluginState::Disabled,
            error: None,
            capabilities: manifest.capabilities.clone(),
            subscribes_request: false,
            subscribes_response: false,
        };
        if !enabled {
            // Parse-only: manifest validated (bounds, capabilities, escape
            // guard), but no compile, no wasm memory held.
            return Ok(Entry { info, hook: None });
        }
        let hook = Arc::new(WasmHook::load(dir, manifest)?);
        info.state = PluginState::Loaded;
        info.subscribes_request = hook.subscribes_request();
        info.subscribes_response = hook.subscribes_response();
        Ok(Entry {
            info,
            hook: Some(hook),
        })
    }

    /// Snapshot for the management plane (list + declared permissions).
    pub fn list(&self) -> Vec<PluginInfo> {
        self.entries
            .read()
            .expect("plugin registry read")
            .iter()
            .map(|e| e.info.clone())
            .collect()
    }

    /// Runtime enable: (re-)loads from disk and registers. Affects the
    /// running daemon only — persistence is the config file's job (ADR-020).
    pub fn enable(&self, name: &str, hook_registry: &HookRegistry) -> AetherResult<()> {
        let mut entries = self.entries.write().expect("plugin registry write");
        let idx = entries
            .iter()
            .position(|e| e.info.name == name)
            .ok_or_else(|| AetherError::config(format!("no plugin named '{name}'")))?;
        let dir = entries[idx].info.dir.clone();
        let loaded = Self::load_one(&dir, true)
            .map_err(|e| AetherError::config(format!("enable '{name}' failed: {e}")))?;
        hook_registry.register(Arc::clone(loaded.hook.as_ref().expect("enabled has hook"))
            as Arc<dyn aether_core::hooks::Hook>);
        entries[idx] = loaded;
        Ok(())
    }

    /// Runtime disable: unregister exactly this plugin's hooks; the loaded
    /// artifact is dropped (wasm memory released). Manifest knowledge is
    /// kept so `list()` still shows what it *would* run.
    pub fn disable(&self, name: &str, hook_registry: &HookRegistry) -> AetherResult<()> {
        let mut entries = self.entries.write().expect("plugin registry write");
        let idx = entries
            .iter()
            .position(|e| e.info.name == name)
            .ok_or_else(|| AetherError::config(format!("no plugin named '{name}'")))?;
        let prefix = format!("plugin/{name}/");
        hook_registry.unregister_prefix(&prefix);
        entries[idx].info.state = PluginState::Disabled;
        entries[idx].info.subscribes_request = false;
        entries[idx].info.subscribes_response = false;
        entries[idx].info.error = None;
        entries[idx].hook = None;
        Ok(())
    }
}

fn dir_name(dir: &std::path::Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// Placeholder manifest projection for entries we could not parse. Honest:
/// name/dir real, everything else empty — the UI shows the error string.
fn stub_manifest(name: &str) -> PluginManifest {
    PluginManifest {
        name: name.to_string(),
        version: None,
        wasm: String::new(),
        timeout_ms: None,
        max_pages: None,
        capabilities: Vec::new(),
        sha256: None,
    }
}

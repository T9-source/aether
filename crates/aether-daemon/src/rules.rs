//! Aether daemon — wires proxy engine + storage + control plane.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Persistent match & replace rules (ADR-006 #1: core features ship as
//! hooks). Rules live in `<data_dir>/rules.json`, each with a stable id so
//! the control plane can enable/disable/remove them at runtime. Every rule
//! becomes a `RuleHook` in the shared `HookRegistry` — the JSON→hook
//! boundary that a future WASM plugin host will reuse.

use aether_core::error::{AetherError, AetherResult};
use aether_core::hooks::{Hook, HookRegistry};
use aether_core::{RuleHook, RuleSpec};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Prefix reserved by the rules layer inside the shared registry; syncing
/// replaces exactly these hooks, never anything else registered by other
/// subsystems.
const RULE_HOOK_PREFIX: &str = "rule:";

/// Wraps a [`RuleHook`] with a stable registry name (`rule:<id>`) so the sync
/// layer can replace individual rules without touching other hooks. All hook
/// behavior is delegated unchanged.
struct IdHook {
    label: String,
    inner: RuleHook,
}

#[async_trait::async_trait]
impl Hook for IdHook {
    fn name(&self) -> &str {
        &self.label
    }
    fn priority(&self) -> i32 {
        self.inner.priority()
    }
    async fn on_request(
        &self,
        ctx: &mut aether_core::context::RequestContext,
        req: &mut aether_core::model::Request,
    ) -> aether_core::hooks::HookResult {
        self.inner.on_request(ctx, req).await
    }
    async fn on_response(
        &self,
        ctx: &mut aether_core::context::RequestContext,
        res: &mut aether_core::model::Response,
    ) -> aether_core::hooks::HookResult {
        self.inner.on_response(ctx, res).await
    }
    async fn on_flow_complete(
        &self,
        ctx: &aether_core::context::RequestContext,
        flow: &aether_core::model::Flow,
    ) -> aether_core::hooks::HookResult {
        self.inner.on_flow_complete(ctx, flow).await
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleEntry {
    pub id: u64,
    #[serde(flatten)]
    pub spec: RuleSpec,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct RulesFile {
    rules: Vec<RuleEntry>,
}

pub struct RulesStore {
    path: PathBuf,
    next_id: AtomicU64,
    entries: RwLock<Vec<RuleEntry>>,
}

impl RulesStore {
    /// Loads (or initializes) `<dir>/rules.json`.
    pub fn load(dir: &Path) -> AetherResult<Self> {
        std::fs::create_dir_all(dir)
            .map_err(|e| AetherError::storage(format!("create {}: {e}", dir.display())))?;
        let path = dir.join("rules.json");
        let (entries, next) = match std::fs::read_to_string(&path) {
            Ok(text) => {
                let file: RulesFile = serde_json::from_str(&text)
                    .map_err(|e| AetherError::config(format!("parse rules.json: {e}")))?;
                let max = file.rules.iter().map(|r| r.id).max().unwrap_or(0);
                (file.rules, max)
            }
            Err(_) => (Vec::new(), 0),
        };
        Ok(Self {
            path,
            next_id: AtomicU64::new(next),
            entries: RwLock::new(entries),
        })
    }

    pub fn list(&self) -> Vec<RuleEntry> {
        self.entries.read().expect("rules lock").clone()
    }

    pub fn add(&self, spec: RuleSpec) -> AetherResult<u64> {
        // validate by compiling
        RuleHook::from_spec(spec.clone())?;
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        self.entries
            .write()
            .expect("rules lock")
            .push(RuleEntry { id, spec });
        self.persist()?;
        Ok(id)
    }

    pub fn remove(&self, id: u64) -> bool {
        let mut guard = self.entries.write().expect("rules lock");
        let removed = match guard.iter().position(|r| r.id == id) {
            Some(pos) => {
                guard.remove(pos);
                true
            }
            None => false,
        };
        drop(guard);
        if removed {
            let _ = self.persist();
        }
        removed
    }

    pub fn set_enabled(&self, id: u64, enabled: bool) -> bool {
        let mut guard = self.entries.write().expect("rules lock");
        let Some(entry) = guard.iter_mut().find(|r| r.id == id) else {
            return false;
        };
        entry.spec.enabled = enabled;
        drop(guard);
        let _ = self.persist();
        true
    }

    /// Re-projects every enabled rule into the live registry: removes the
    /// previous `rule:*` hooks, registers the current set. Called at boot and
    /// after every control-plane mutation, so rule edits hit in-flight
    /// traffic without a daemon restart (ADR-006 #1).
    pub fn sync_into(&self, registry: &HookRegistry) -> AetherResult<usize> {
        registry.unregister_prefix(RULE_HOOK_PREFIX);
        let entries = self.entries.read().expect("rules lock").clone();
        let mut count = 0;
        for entry in &entries {
            if !entry.spec.enabled {
                continue;
            }
            let inner = RuleHook::from_spec(entry.spec.clone())?;
            registry.register_fn(IdHook {
                label: format!("{RULE_HOOK_PREFIX}{}", entry.id),
                inner,
            });
            count += 1;
        }
        Ok(count)
    }

    /// Registers every enabled rule into the shared registry.
    pub fn register_into(&self, registry: &HookRegistry) -> AetherResult<usize> {
        self.sync_into(registry)
    }

    fn persist(&self) -> AetherResult<()> {
        let file = RulesFile {
            rules: self.entries.read().expect("rules lock").clone(),
        };
        let text =
            serde_json::to_string_pretty(&file).map_err(|e| AetherError::storage(e.to_string()))?;
        std::fs::write(&self.path, text)
            .map_err(|e| AetherError::storage(format!("write {}: {e}", self.path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn spec(name: &str) -> RuleSpec {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "action": {"kind": "set_request_header", "name": "x", "value": "y"}
        }))
        .unwrap()
    }

    #[test]
    fn add_remove_persist_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store = RulesStore::load(dir.path()).unwrap();
        let id = store.add(spec("stamp")).unwrap();
        store.add(spec("second")).unwrap();
        assert_eq!(store.list().len(), 2);
        assert!(store.set_enabled(id, false));
        assert!(!store.set_enabled(999, false));

        let reloaded = RulesStore::load(dir.path()).unwrap();
        assert_eq!(reloaded.list().len(), 2);
        let listed = reloaded.list();
        let first = listed.iter().find(|r| r.id == id).unwrap();
        assert!(!first.spec.enabled);

        assert!(reloaded.remove(id));
        assert_eq!(reloaded.list().len(), 1);
    }

    #[test]
    fn invalid_regex_is_rejected() {
        let dir = TempDir::new().unwrap();
        let store = RulesStore::load(dir.path()).unwrap();
        let bad: RuleSpec = serde_json::from_value(serde_json::json!({
            "name": "bad",
            "action": {"kind": "replace_request_path", "pattern": "((", "replacement": ""}
        }))
        .unwrap();
        assert!(store.add(bad).is_err());
        assert!(store.list().is_empty());
    }

    #[test]
    fn register_only_enabled_and_resync() {
        let dir = TempDir::new().unwrap();
        let store = RulesStore::load(dir.path()).unwrap();
        let on = store.add(spec("on")).unwrap();
        let off = store.add(spec("off")).unwrap();
        assert!(store.set_enabled(off, false));
        let registry = HookRegistry::new();
        let count = store.register_into(&registry).unwrap();
        assert_eq!(count, 1);
        assert_eq!(registry.names(), vec!["rule:1"]);
        let _ = on;

        // disable the last enabled rule → sync leaves the registry empty
        assert!(store.set_enabled(on, false));
        store.sync_into(&registry).unwrap();
        assert!(registry.is_empty());

        // re-enable → back exactly once (no duplicate registration)
        assert!(store.set_enabled(on, true));
        store.sync_into(&registry).unwrap();
        store.sync_into(&registry).unwrap();
        assert_eq!(registry.names(), vec!["rule:1"]);
        assert_eq!(registry.len(), 1);
    }
}

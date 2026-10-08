//! Aether daemon — wires proxy engine + storage + control plane.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Persistent match & replace rules (ADR-006 #1: core features ship as
//! hooks). Rules live in `<data_dir>/rules.json`, each with a stable id so
//! the control plane can enable/disable/remove them at runtime. Every rule
//! becomes a `RuleHook` in the shared `HookRegistry` — the JSON→hook
//! boundary that a future WASM plugin host will reuse.

use aether_core::error::{AetherError, AetherResult};
use aether_core::hooks::HookRegistry;
use aether_core::{RuleHook, RuleSpec};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

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

    /// Registers every enabled rule into the shared registry.
    pub fn register_into(&self, registry: &mut HookRegistry) -> AetherResult<usize> {
        let entries = self.entries.read().expect("rules lock").clone();
        let mut count = 0;
        for entry in &entries {
            if !entry.spec.enabled {
                continue;
            }
            let hook = RuleHook::from_spec(entry.spec.clone())?;
            registry.register_fn(hook);
            count += 1;
        }
        Ok(count)
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
    fn register_only_enabled() {
        let dir = TempDir::new().unwrap();
        let store = RulesStore::load(dir.path()).unwrap();
        let on = store.add(spec("on")).unwrap();
        let off = store.add(spec("off")).unwrap();
        assert!(store.set_enabled(off, false));
        let mut registry = HookRegistry::new();
        let count = store.register_into(&mut registry).unwrap();
        assert_eq!(count, 1);
        assert_eq!(registry.hooks()[0].name(), "on");
        let _ = on;
    }
}

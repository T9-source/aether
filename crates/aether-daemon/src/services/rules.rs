//! RulesService — declarative rule CRUD with immediate re-projection into
//! the live hook registry (ADR-016, ADR-006 #1).
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).

use aether_core::RuleSpec;
use aether_core::error::AetherResult;
use aether_core::hooks::HookRegistry;
use std::sync::Arc;

use crate::rules::{RuleEntry, RulesStore};

/// Rules stay **declarative** (match + action, deterministic, serializable,
/// inspectable — ADR-016): this service can only add/remove/enable whole
/// specs. Arbitrary behavior belongs to plugins (V0.3+), never to rules.
pub struct RulesService {
    store: Arc<RulesStore>,
    registry: Arc<HookRegistry>,
}

impl RulesService {
    pub fn new(store: Arc<RulesStore>, registry: Arc<HookRegistry>) -> Self {
        Self { store, registry }
    }

    pub fn list(&self) -> Vec<RuleEntry> {
        self.store.list()
    }

    /// Add a spec and re-project into the live registry — in-flight traffic
    /// picks the rule up without a restart.
    pub fn add(&self, spec: RuleSpec) -> AetherResult<u64> {
        let id = self.store.add(spec)?;
        self.store.sync_into(&self.registry)?;
        Ok(id)
    }

    /// Remove a rule (no-op when absent); re-projects on success.
    pub fn remove(&self, id: u64) -> AetherResult<bool> {
        let removed = self.store.remove(id);
        if removed {
            self.store.sync_into(&self.registry)?;
        }
        Ok(removed)
    }

    /// Enable/disable a rule (no-op when absent); re-projects on change.
    pub fn set_enabled(&self, id: u64, enabled: bool) -> AetherResult<bool> {
        let changed = self.store.set_enabled(id, enabled);
        if changed {
            self.store.sync_into(&self.registry)?;
        }
        Ok(changed)
    }
}

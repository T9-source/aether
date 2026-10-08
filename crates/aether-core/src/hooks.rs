//! Hooks + HookRegistry (primitive #4 — plugin architecture early, ADR-006).
//!
//! The five binding rules from ADR-006 live here:
//! 1. Everything routes through the registry — core features ship as hooks.
//! 2. Hook signatures are serialization-shaped: owned, serializable data or
//!    capability handles; never borrows of internal state.
//! 3. The EventBus is the second extension surface (see `events`).
//! 4. `RequestContext` is the capability carrier — no global state access.
//! 5. Hooks have a timeout budget (enforced by the pipeline, per stage).

use async_trait::async_trait;
use std::sync::{Arc, RwLock};

use crate::context::RequestContext;
use crate::error::AetherResult;
use crate::model::{Flow, Request, Response};

/// What a hook asks the pipeline to do after its stage ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookAction {
    /// Keep processing.
    Continue,
    /// Drop/abort this traffic (proxy: close the connection; replay: discard).
    /// Meaningless in `on_flow_complete` — the exchange is already over.
    Stop,
}

pub type HookResult = AetherResult<HookAction>;

/// The extension boundary. Every hook is `Send + Sync` and async — and its
/// signature takes only owned/serializable-shaped data, so cutting it over a
/// WASM boundary in V0.2 is a hosting change, not an API redesign.
#[async_trait]
pub trait Hook: Send + Sync {
    fn name(&self) -> &str;

    /// Lower runs earlier. Hooks with equal priority keep registration order.
    fn priority(&self) -> i32 {
        0
    }

    async fn on_request(&self, _ctx: &mut RequestContext, _req: &mut Request) -> HookResult {
        Ok(HookAction::Continue)
    }

    async fn on_response(&self, _ctx: &mut RequestContext, _res: &mut Response) -> HookResult {
        Ok(HookAction::Continue)
    }

    async fn on_flow_complete(&self, _ctx: &RequestContext, _flow: &Flow) -> HookResult {
        Ok(HookAction::Continue)
    }
}

/// A *live* hook registry: hooks can be registered/unregistered at runtime
/// while the pipeline reads a consistent snapshot per stage. Readers clone an
/// `Arc<Vec<_>>` (one short read-lock, then lock-free iteration); writers
/// publish a new vector — no reader is ever blocked by a registration.
/// This is what makes control-plane rule edits apply to in-flight traffic
/// without restarting the daemon (ADR-006: plugin architecture early).
///
/// A published, immutable set of hooks — swapping it is a pointer write.
type HookSet = Arc<Vec<Arc<dyn Hook>>>;

#[derive(Clone, Default)]
pub struct HookRegistry {
    hooks: Arc<RwLock<HookSet>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers keeping hooks sorted by priority (stable for equal priority).
    pub fn register(&self, hook: Arc<dyn Hook>) {
        let mut hooks = self.write_lock();
        let mut next: Vec<Arc<dyn Hook>> = hooks.as_ref().clone();
        let pos = next.partition_point(|h| h.priority() <= hook.priority());
        next.insert(pos, hook);
        *hooks = Arc::new(next);
    }

    pub fn register_fn(&self, hook: impl Hook + 'static) {
        self.register(Arc::new(hook));
    }

    /// Removes every hook whose name starts with `prefix`. Returns the count.
    pub fn unregister_prefix(&self, prefix: &str) -> usize {
        let mut hooks = self.write_lock();
        let kept: Vec<Arc<dyn Hook>> = hooks
            .iter()
            .filter(|h| !h.name().starts_with(prefix))
            .cloned()
            .collect();
        let removed = hooks.len() - kept.len();
        *hooks = Arc::new(kept);
        removed
    }

    /// Consistent, cheap-to-iterate snapshot of the current hook set.
    pub fn snapshot(&self) -> HookSet {
        Arc::clone(&self.read_lock())
    }

    pub fn names(&self) -> Vec<String> {
        self.snapshot()
            .iter()
            .map(|h| h.name().to_string())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.snapshot().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn read_lock(&self) -> HookSet {
        Arc::clone(&self.hooks.read().expect("hook registry read lock"))
    }

    fn write_lock(&self) -> std::sync::RwLockWriteGuard<'_, HookSet> {
        self.hooks.write().expect("hook registry write lock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AetherError;
    use std::time::Duration;
    use tokio::time::sleep;

    struct Noop {
        name: &'static str,
        prio: i32,
    }

    #[async_trait]
    impl Hook for Noop {
        fn name(&self) -> &str {
            self.name
        }
        fn priority(&self) -> i32 {
            self.prio
        }
    }

    struct Sleeper {
        ms: u64,
    }

    #[async_trait]
    impl Hook for Sleeper {
        fn name(&self) -> &str {
            "sleeper"
        }
        async fn on_request(&self, _ctx: &mut RequestContext, _req: &mut Request) -> HookResult {
            sleep(Duration::from_millis(self.ms)).await;
            Ok(HookAction::Continue)
        }
    }

    #[test]
    fn registry_orders_by_priority_stably() {
        let r = HookRegistry::new();
        r.register(Arc::new(Noop {
            name: "b-late",
            prio: 5,
        }));
        r.register(Arc::new(Noop {
            name: "early1",
            prio: -1,
        }));
        r.register(Arc::new(Noop {
            name: "early2",
            prio: -1,
        }));
        r.register(Arc::new(Noop {
            name: "a-late",
            prio: 5,
        }));
        assert_eq!(r.names(), vec!["early1", "early2", "b-late", "a-late"]);

        // live removal by prefix leaves the rest intact
        let removed = r.unregister_prefix("early");
        assert_eq!(removed, 2);
        assert_eq!(r.names(), vec!["b-late", "a-late"]);
    }

    #[tokio::test]
    async fn hook_future_is_directly_awaitable() {
        let mut ctx = RequestContext::new(1, crate::context::TrafficSource::Proxy);
        let mut req = Request::get(crate::model::Url::parse("https://a/").unwrap());
        let out = Sleeper { ms: 1 }
            .on_request(&mut ctx, &mut req)
            .await
            .unwrap();
        assert_eq!(out, HookAction::Continue);
        // error path shapes
        let e = AetherError::HookTimeout {
            hook: "s".into(),
            timeout_ms: 1,
        };
        assert!(e.to_string().contains("timed out"));
    }
}

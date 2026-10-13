//! ReplayService — the operator's direct voice, launched as background work
//! (ADR-016, ADR-012).
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).

use aether_core::EventBus;
use aether_core::error::AetherResult;
use aether_core::model::{FlowId, Request};
use aether_core::storage::Storage;
use aether_http::upstream::UpstreamCfg;
use std::sync::Arc;

use crate::replay::run_replay;

/// Re-executes (possibly edited) requests with no client leg. `start`
/// reserves the flow id immediately and runs the exchange in the background
/// so a slow origin can never stall the control plane (ADR-010).
pub struct ReplayService {
    storage: Arc<dyn Storage>,
    bus: EventBus,
    upstream: UpstreamCfg,
}

impl ReplayService {
    pub fn new(storage: Arc<dyn Storage>, bus: EventBus, upstream: UpstreamCfg) -> Self {
        Self {
            storage,
            bus,
            upstream,
        }
    }

    /// Reserve a flow id, spawn the replay, return the id at once.
    pub async fn start(&self, request: Request) -> AetherResult<FlowId> {
        let flow_id = self.storage.alloc_id().await?;
        let storage = self.storage.clone();
        let bus = self.bus.clone();
        let cfg = self.upstream.clone();
        tokio::spawn(async move {
            if let Err(e) = run_replay(request, flow_id, storage, bus, cfg).await {
                tracing::warn!(%flow_id, error = %e, "replay failed");
            }
        });
        Ok(flow_id)
    }
}

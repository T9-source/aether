//! TrafficService — history is read through one behavior boundary (ADR-016).
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).

use aether_core::error::AetherResult;
use aether_core::model::{Flow, FlowId};
use aether_core::storage::{FlowPage, FlowQuery, FlowStats, FlowSummary, Storage};
use std::sync::Arc;

/// Read-side behavior over the flow history. The control plane serializes
/// whatever this returns; it never touches [`Storage`] directly.
pub struct TrafficService {
    storage: Arc<dyn Storage>,
}

impl TrafficService {
    pub fn new(storage: Arc<dyn Storage>) -> Self {
        Self { storage }
    }

    /// Keyset-paginated history (ADR-005: no OFFSET pagination, ever).
    pub async fn list(&self, query: FlowQuery) -> AetherResult<FlowPage> {
        self.storage.list_flows(query).await
    }

    /// One full flow (request + response + meta + ws session).
    pub async fn get(&self, id: FlowId) -> AetherResult<Option<Flow>> {
        self.storage.get_flow(id).await
    }

    /// Full-text search over hosts/paths/bodies (FTS5, ADR-005).
    pub async fn search(&self, query: &str, limit: u32) -> AetherResult<Vec<FlowSummary>> {
        self.storage.search_flows(query, limit).await
    }

    /// Aggregate counters for the status bar.
    pub async fn stats(&self) -> AetherResult<FlowStats> {
        self.storage.flow_stats().await
    }
}

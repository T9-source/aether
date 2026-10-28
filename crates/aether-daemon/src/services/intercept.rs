//! InterceptService — operator hold/release behavior behind one boundary
//! (ADR-016).
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).

use aether_core::error::AetherResult;
use aether_core::{EventBus, InterceptDirection};
use aether_http::intercept::{HeldDecision, HeldInfo, HeldPayload, InterceptState};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Row shape for `intercept_list` (mirror of the engine's [`HeldInfo`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeldRow {
    pub flow_id: u64,
    pub direction: InterceptDirection,
    pub method: Option<String>,
    pub host: Option<String>,
    pub path: Option<String>,
    pub status: Option<u16>,
}

/// The intercept hold queue: list, inspect, resolve (forward/drop), and
/// toggle what the engine holds. Decisions are published on the bus by the
/// [`InterceptState`] itself, so every client sees them live.
pub struct InterceptService {
    state: Arc<InterceptState>,
    bus: EventBus,
}

impl InterceptService {
    pub fn new(state: Arc<InterceptState>, bus: EventBus) -> Self {
        Self { state, bus }
    }

    /// Every currently-held exchange, one row each.
    pub fn list_rows(&self) -> Vec<HeldRow> {
        self.state
            .list()
            .into_iter()
            .map(|h: HeldInfo| HeldRow {
                flow_id: h.flow_id,
                direction: h.direction,
                method: h.method,
                host: h.host,
                path: h.path,
                status: h.status,
            })
            .collect()
    }

    /// The full payload of a held exchange (request or response).
    pub fn show(&self, id: u64) -> Option<HeldPayload> {
        self.state.payload(id)
    }

    /// Drop a held exchange — nothing reaches the other side.
    pub fn drop_held(&self, id: u64) -> AetherResult<bool> {
        self.state.resolve(id, HeldDecision::Drop, &self.bus)
    }

    /// Resolve a held exchange with an operator decision (possibly edited).
    pub fn release(&self, id: u64, decision: HeldDecision) -> AetherResult<bool> {
        self.state.resolve(id, decision, &self.bus)
    }

    /// Toggle what the engine holds; returns the effective state.
    pub fn set_enabled(&self, request: Option<bool>, response: Option<bool>) -> (bool, bool) {
        let req_now = request.unwrap_or(self.state.request_enabled());
        let res_now = response.unwrap_or(self.state.response_enabled());
        self.state.set_enabled(req_now, res_now);
        (req_now, res_now)
    }

    pub fn held_count(&self) -> usize {
        self.state.held_count()
    }
}

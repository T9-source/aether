//! Aether — network engine.
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).
//!
//! Intercept hold state — the proxy-side half of manual interception.
//!
//! A held flow parks in `held` with a oneshot channel; the control plane
//! lists it, hands the serialized request/response to the operator, and
//! resolves the channel with `Drop` or `Forward(edited)`. The engine awaits
//! the channel — interception is *cooperative blocking*, exactly the Burp
//! mental model, but the hold is visible to every client via `InterceptHold`
//! events (ADR-002).

use aether_core::error::AetherResult;
use aether_core::model::{FlowId, Request, Response};
use aether_core::{AetherEvent, EventBus, InterceptDecision, InterceptDirection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::oneshot;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HeldPayload {
    Request(Request),
    Response(Response),
}

/// What the operator decided for a held flow. (Variant-size asymmetry is
/// fine: these move through a oneshot exactly once, never in bulk.)
#[allow(clippy::large_enum_variant)]
pub enum HeldDecision {
    Drop,
    Forward(HeldPayload),
}

/// Summary row for `intercept ls`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeldInfo {
    pub flow_id: FlowId,
    pub direction: InterceptDirection,
    pub method: Option<String>,
    pub host: Option<String>,
    pub path: Option<String>,
    pub status: Option<u16>,
}

struct HeldEntry {
    direction: InterceptDirection,
    payload: HeldPayload,
    tx: Option<oneshot::Sender<HeldDecision>>,
}

#[derive(Default)]
struct Inner {
    request_enabled: AtomicBool,
    response_enabled: AtomicBool,
    held: Mutex<HashMap<FlowId, HeldEntry>>,
}

/// Shared between the engine (hold/await) and the control plane (list/resolve).
#[derive(Default)]
pub struct InterceptState {
    inner: Inner,
}

impl InterceptState {
    pub fn new(request: bool, response: bool) -> Self {
        let state = Self::default();
        state.inner.request_enabled.store(request, Ordering::SeqCst);
        state
            .inner
            .response_enabled
            .store(response, Ordering::SeqCst);
        state
    }

    pub fn set_enabled(&self, request: bool, response: bool) {
        self.inner.request_enabled.store(request, Ordering::SeqCst);
        self.inner
            .response_enabled
            .store(response, Ordering::SeqCst);
    }

    pub fn request_enabled(&self) -> bool {
        self.inner.request_enabled.load(Ordering::SeqCst)
    }

    pub fn response_enabled(&self) -> bool {
        self.inner.response_enabled.load(Ordering::SeqCst)
    }

    /// Parks the payload and returns the receiver the engine will await.
    pub fn hold(
        &self,
        flow_id: FlowId,
        direction: InterceptDirection,
        payload: HeldPayload,
        bus: &EventBus,
    ) -> oneshot::Receiver<HeldDecision> {
        let (tx, rx) = oneshot::channel();
        self.inner.held.lock().expect("intercept lock").insert(
            flow_id,
            HeldEntry {
                direction,
                payload,
                tx: Some(tx),
            },
        );
        bus.publish(AetherEvent::InterceptHold { flow_id, direction });
        rx
    }

    /// Lists held flows, oldest first.
    pub fn list(&self) -> Vec<HeldInfo> {
        let held = self.inner.held.lock().expect("intercept lock");
        let mut rows: Vec<HeldInfo> = held
            .iter()
            .map(|(id, e)| {
                let (method, host, path, status) = match &e.payload {
                    HeldPayload::Request(r) => (
                        Some(r.method.as_str().to_string()),
                        Some(r.url.host.clone()),
                        Some(r.url.path_and_query()),
                        None,
                    ),
                    HeldPayload::Response(res) => (None, None, None, Some(res.status.code)),
                };
                HeldInfo {
                    flow_id: *id,
                    direction: e.direction,
                    method,
                    host,
                    path,
                    status,
                }
            })
            .collect();
        rows.sort_by_key(|r| r.flow_id);
        rows
    }

    /// Full payload of a held flow (for `intercept show`).
    pub fn payload(&self, flow_id: FlowId) -> Option<HeldPayload> {
        self.inner
            .held
            .lock()
            .expect("intercept lock")
            .get(&flow_id)
            .map(|e| e.payload.clone())
    }

    /// Resolves a held flow. Returns false when the id is unknown.
    pub fn resolve(
        &self,
        flow_id: FlowId,
        decision: HeldDecision,
        bus: &EventBus,
    ) -> AetherResult<bool> {
        // NB: std Mutex is not reentrant — take everything we need from the
        // guard, drop it, then send/remove.
        let (tx, direction) = {
            let mut guard = self.inner.held.lock().expect("intercept lock");
            let Some(entry) = guard.get_mut(&flow_id) else {
                return Ok(false);
            };
            let direction = entry.direction;
            match entry.tx.take() {
                Some(tx) => (tx, direction),
                None => return Ok(false),
            }
        };
        let event_decision = match decision {
            HeldDecision::Drop => InterceptDecision::Drop,
            HeldDecision::Forward(_) => InterceptDecision::Forward,
        };
        let _ = tx.send(decision); // receiver gone ⇒ engine treats as drop
        self.inner
            .held
            .lock()
            .expect("intercept lock")
            .remove(&flow_id);
        bus.publish(AetherEvent::InterceptReleased {
            flow_id,
            direction,
            decision: event_decision,
        });
        Ok(true)
    }

    /// Number of currently held flows (daemon stats).
    pub fn held_count(&self) -> usize {
        self.inner.held.lock().expect("intercept lock").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_core::model::{Method, Url};

    fn req() -> Request {
        Request::new(Method::Get, Url::parse("https://a.com/x").unwrap())
    }

    #[tokio::test]
    async fn hold_forward_cycle() {
        let bus = EventBus::new(64);
        let mut events = bus.subscribe();
        let state = InterceptState::new(true, false);

        let rx = state.hold(
            7,
            InterceptDirection::Request,
            HeldPayload::Request(req()),
            &bus,
        );
        assert_eq!(state.list().len(), 1);
        assert!(matches!(
            events.try_next(),
            Some(AetherEvent::InterceptHold { flow_id: 7, .. })
        ));

        state
            .resolve(7, HeldDecision::Forward(HeldPayload::Request(req())), &bus)
            .unwrap();
        let decision = rx.await.unwrap();
        assert!(matches!(decision, HeldDecision::Forward(_)));
        assert!(state.list().is_empty());
        assert!(matches!(
            events.try_next(),
            Some(AetherEvent::InterceptReleased {
                decision: InterceptDecision::Forward,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn unknown_id_returns_false() {
        let bus = EventBus::new(8);
        let state = InterceptState::new(true, false);
        assert!(!state.resolve(42, HeldDecision::Drop, &bus).unwrap());
    }
}

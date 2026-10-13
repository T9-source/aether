//! EventBus (primitive #3) — every security-relevant state change is an event.
//!
//! These are the same events a future plugin will consume across the WASM
//! boundary and the same events the control plane will push to UI/CLI
//! clients (ADR-002), which is why they are `Clone + Serialize` and shaped as
//! one tagged enum.
//!
//! Event taxonomy (ADR-017): every event belongs to exactly one of four
//! categories — [`EventCategory::Transport`], [`EventCategory::Pipeline`],
//! [`EventCategory::Security`], [`EventCategory::Lifecycle`]. Low-level
//! networking telemetry (bytes read/written, socket churn, timer ticks) is
//! deliberately **not** here: it belongs to tracing/metrics, so the bus can
//! never explode into millions of micro-events.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use ts_rs::TS;

use crate::model::{FlowId, WsDirection, WsKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum HookStage {
    OnRequest,
    OnResponse,
    OnFlowComplete,
}

/// Which leg of the exchange is held for manual inspection (Phase 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum InterceptDirection {
    Request,
    Response,
}

/// What the operator decided for a held flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum InterceptDecision {
    Forward,
    Drop,
}

/// Which slice of the system an event belongs to (ADR-017). Consumers (UI
/// filters, plugins, automation) subscribe by category instead of matching
/// every variant — and reviewers use it as a checklist: anything that is not
/// security-relevant state must not become an `AetherEvent` at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum EventCategory {
    /// Connection-level decisions: tunneling, pass-through, handshake
    /// policy. Reserved for the coarse transport facts an operator must
    /// see — never per-byte telemetry.
    Transport,
    /// The request/response pipeline: received, modified, forwarded,
    /// hook execution, WebSocket message flow.
    Pipeline,
    /// Security-relevant operator/analysis outcomes: findings, intercept
    /// holds and releases.
    Security,
    /// Flow lifecycle and daemon health: started, completed, dropped,
    /// non-fatal errors.
    Lifecycle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum AetherEvent {
    FlowStarted {
        flow_id: FlowId,
    },
    RequestReceived {
        flow_id: FlowId,
    },
    RequestModified {
        flow_id: FlowId,
        by: String,
    },
    RequestForwarded {
        flow_id: FlowId,
    },
    ResponseReceived {
        flow_id: FlowId,
        status: u16,
    },
    ResponseModified {
        flow_id: FlowId,
        by: String,
    },
    ResponseForwarded {
        flow_id: FlowId,
    },
    FlowCompleted {
        flow_id: FlowId,
    },
    HookExecuted {
        hook: String,
        stage: HookStage,
        duration_ms: u64,
    },
    FindingCreated {
        flow_id: FlowId,
        title: String,
    },
    // ── Phase 1 (proxy engine) ──────────────────────────────────────────
    /// A flow is parked awaiting an operator decision.
    InterceptHold {
        flow_id: FlowId,
        direction: InterceptDirection,
    },
    InterceptReleased {
        flow_id: FlowId,
        direction: InterceptDirection,
        decision: InterceptDecision,
    },
    /// The exchange was dropped (rule, hook, or operator) — no response served.
    FlowDropped {
        flow_id: FlowId,
        reason: String,
    },
    /// A CONNECT was tunneled blindly (pass-through list, ADR-004) — not
    /// recorded, not parsed, by design.
    PassThrough {
        authority: String,
    },
    /// Non-fatal daemon/engine problem worth surfacing in the UI.
    ErrorOccurred {
        context: String,
        message: String,
    },
    // ── V0.2 (WebSocket inspection, ADR-013) ────────────────────────────
    /// An h1 exchange upgraded to a WebSocket connection; the relay is live.
    WsOpened {
        flow_id: FlowId,
    },
    /// One assembled message crossed the relay (recorded or counted-only).
    WsMessage {
        flow_id: FlowId,
        direction: WsDirection,
        kind: WsKind,
        size: u64,
    },
    /// The WebSocket relay ended; the bounded session record was persisted.
    WsClosed {
        flow_id: FlowId,
        total_messages: u64,
        total_bytes: u64,
    },
}

impl AetherEvent {
    /// The taxonomy slot of this event (ADR-017). Exhaustive by construction:
    /// adding a variant without classifying it is a compile error.
    pub fn category(&self) -> EventCategory {
        match self {
            // ── transport ────────────────────────────────────────────────
            AetherEvent::PassThrough { .. } => EventCategory::Transport,
            // ── pipeline ───────────────────────────────────────────────
            AetherEvent::RequestReceived { .. }
            | AetherEvent::RequestModified { .. }
            | AetherEvent::RequestForwarded { .. }
            | AetherEvent::ResponseReceived { .. }
            | AetherEvent::ResponseModified { .. }
            | AetherEvent::ResponseForwarded { .. }
            | AetherEvent::HookExecuted { .. }
            | AetherEvent::WsOpened { .. }
            | AetherEvent::WsMessage { .. }
            | AetherEvent::WsClosed { .. } => EventCategory::Pipeline,
            // ── security ───────────────────────────────────────────────
            AetherEvent::FindingCreated { .. }
            | AetherEvent::InterceptHold { .. }
            | AetherEvent::InterceptReleased { .. } => EventCategory::Security,
            // ── lifecycle ─────────────────────────────────────────────
            AetherEvent::FlowStarted { .. }
            | AetherEvent::FlowCompleted { .. }
            | AetherEvent::FlowDropped { .. }
            | AetherEvent::ErrorOccurred { .. } => EventCategory::Lifecycle,
        }
    }
}

/// Broadcast bus: any number of consumers (GUI, CLI, automation, future
/// plugins) may subscribe; each receives every event published after its
/// subscription. A consumer that falls behind loses *its own* copy with a
/// `Lagged` signal — storage, not the bus, is the source of truth.
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<AetherEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    /// Publish an event. Zero subscribers is a normal state, not an error.
    pub fn publish(&self, event: AetherEvent) {
        let _ = self.sender.send(event);
    }

    pub fn subscribe(&self) -> EventSubscription {
        EventSubscription {
            receiver: self.sender.subscribe(),
        }
    }

    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

pub struct EventSubscription {
    receiver: broadcast::Receiver<AetherEvent>,
}

/// Outcome of awaiting the bus (audit M6): `Lagged` is a *recovery*
/// signal, not a shutdown. Storage remains the source of truth — the
/// consumer should tell its user it missed events and keep going.
#[derive(Debug)]
pub enum EventTick {
    /// The next event in order.
    Event(AetherEvent),
    /// This subscriber fell behind by `n` events; they are gone for this
    /// subscriber only. Continue consuming from here on.
    Lagged(u64),
    /// The bus is closed (daemon shutting down).
    Closed,
}

impl EventSubscription {
    /// Next outcome: an event, a lag notice, or bus closure.
    pub async fn next_tick(&mut self) -> EventTick {
        match self.receiver.recv().await {
            Ok(event) => EventTick::Event(event),
            Err(broadcast::error::RecvError::Lagged(n)) => EventTick::Lagged(n),
            Err(broadcast::error::RecvError::Closed) => EventTick::Closed,
        }
    }

    /// Back-compat wrapper: next event, or `None` when the bus is closed.
    /// Lagged is swallowed here by design (see `next_tick` for the loud path).
    pub async fn next(&mut self) -> Option<AetherEvent> {
        loop {
            match self.next_tick().await {
                EventTick::Event(e) => return Some(e),
                EventTick::Lagged(_) => continue, // skip the gap, keep serving
                EventTick::Closed => return None,
            }
        }
    }

    /// Non-blocking drain (used by tests and the CLI demo).
    pub fn try_next(&mut self) -> Option<AetherEvent> {
        self.receiver.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_has_exactly_one_taxonomy_slot() {
        // ADR-017: spot-check each category through representative variants.
        let cases = [
            (
                AetherEvent::PassThrough {
                    authority: "a".into(),
                },
                EventCategory::Transport,
            ),
            (
                AetherEvent::RequestReceived { flow_id: 1 },
                EventCategory::Pipeline,
            ),
            (
                AetherEvent::WsClosed {
                    flow_id: 1,
                    total_messages: 0,
                    total_bytes: 0,
                },
                EventCategory::Pipeline,
            ),
            (
                AetherEvent::InterceptHold {
                    flow_id: 1,
                    direction: InterceptDirection::Request,
                },
                EventCategory::Security,
            ),
            (
                AetherEvent::FlowCompleted { flow_id: 1 },
                EventCategory::Lifecycle,
            ),
            (
                AetherEvent::ErrorOccurred {
                    context: "c".into(),
                    message: "m".into(),
                },
                EventCategory::Lifecycle,
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(event.category(), expected, "wrong slot for {event:?}");
        }
        // The category itself serializes snake_case for the control plane.
        assert_eq!(
            serde_json::to_value(EventCategory::Security).unwrap(),
            serde_json::json!("security")
        );
    }

    #[tokio::test]
    async fn subscribers_get_events_published_after_subscription() {
        let bus = EventBus::new(64);
        let mut sub = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);

        bus.publish(AetherEvent::FlowStarted { flow_id: 7 });
        let got = sub.next().await.unwrap();
        assert_eq!(got, AetherEvent::FlowStarted { flow_id: 7 });
    }

    #[tokio::test]
    async fn publish_without_subscribers_is_ok() {
        let bus = EventBus::new(8);
        bus.publish(AetherEvent::FlowCompleted { flow_id: 1 });
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn events_roundtrip_through_json() {
        let e = AetherEvent::HookExecuted {
            hook: "t".into(),
            stage: HookStage::OnRequest,
            duration_ms: 3,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"type\":\"hook_executed\""), "{json}");
        assert_eq!(serde_json::from_str::<AetherEvent>(&json).unwrap(), e);
    }
}

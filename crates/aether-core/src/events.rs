//! EventBus (primitive #3) — every security-relevant state change is an event.
//!
//! These are the same events a future plugin will consume across the WASM
//! boundary and the same events the control plane will push to UI/CLI
//! clients (ADR-002), which is why they are `Clone + Serialize` and shaped as
//! one tagged enum.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use ts_rs::TS;

use crate::model::FlowId;

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

impl EventSubscription {
    /// Next event, or `None` when the bus is closed.
    pub async fn next(&mut self) -> Option<AetherEvent> {
        self.receiver.recv().await.ok()
    }

    /// Non-blocking drain (used by tests and the CLI demo).
    pub fn try_next(&mut self) -> Option<AetherEvent> {
        self.receiver.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

//! Pipeline (primitive #2) — the single unified path every piece of traffic
//! takes through Aether, from model to events (ADR-006 rule #1: every stage
//! routes through the HookRegistry; nothing bypasses it).
//!
//! The pipeline enforces the two invariants that make the plugin architecture
//! real *before* the plugin runtime exists:
//! - **timeout budget per hook** (ADR-006 rule #5),
//! - **modification detection via fingerprints** — hooks mutate in place, the
//!   pipeline observes and emits `RequestModified` / `ResponseModified`.

use std::time::{Duration, Instant};

use crate::context::RequestContext;
use crate::error::{AetherError, AetherResult};
use crate::events::{AetherEvent, EventBus, HookStage};
use crate::hooks::{HookAction, HookRegistry};
use crate::model::{Flow, FlowId, Request, Response};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineVerdict {
    /// All stages said continue — traffic may be forwarded.
    Forwarded,
    /// A hook stopped this exchange (drop / close).
    Stopped,
}

#[derive(Clone)]
pub struct Pipeline {
    registry: HookRegistry,
    bus: EventBus,
    hook_timeout: Duration,
}

impl Pipeline {
    pub fn new(registry: HookRegistry, bus: EventBus) -> Self {
        Self {
            registry,
            bus,
            hook_timeout: Duration::from_secs(5),
        }
    }

    pub fn with_hook_timeout(mut self, timeout: Duration) -> Self {
        self.hook_timeout = timeout;
        self
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn registry(&self) -> &HookRegistry {
        &self.registry
    }

    /// Request stage: `RequestReceived → hooks (RequestModified when touched)
    /// → RequestForwarded`, or `Stopped`.
    pub async fn run_request(
        &self,
        ctx: &mut RequestContext,
        req: &mut Request,
    ) -> AetherResult<PipelineVerdict> {
        self.bus.publish(AetherEvent::RequestReceived {
            flow_id: ctx.flow_id,
        });

        for hook in self.registry.hooks() {
            let before = req.fingerprint();
            let started = Instant::now();
            let fut = hook.on_request(ctx, req);
            match tokio::time::timeout(self.hook_timeout, fut).await {
                Err(_elapsed) => {
                    return Err(AetherError::HookTimeout {
                        hook: hook.name().to_string(),
                        timeout_ms: self.hook_timeout.as_millis() as u64,
                    });
                }
                Ok(Err(e)) => {
                    return Err(AetherError::Hook {
                        hook: hook.name().to_string(),
                        message: e.to_string(),
                    });
                }
                Ok(Ok(HookAction::Stop)) => {
                    self.emit_hook_executed(hook.name(), HookStage::OnRequest, started);
                    return Ok(PipelineVerdict::Stopped);
                }
                Ok(Ok(HookAction::Continue)) => {
                    if req.fingerprint() != before {
                        self.bus.publish(AetherEvent::RequestModified {
                            flow_id: ctx.flow_id,
                            by: hook.name().to_string(),
                        });
                    }
                    self.emit_hook_executed(hook.name(), HookStage::OnRequest, started);
                }
            }
        }

        self.bus.publish(AetherEvent::RequestForwarded {
            flow_id: ctx.flow_id,
        });
        Ok(PipelineVerdict::Forwarded)
    }

    /// Response stage: `ResponseReceived → hooks (ResponseModified when
    /// touched) → ResponseForwarded`, or `Stopped`.
    pub async fn run_response(
        &self,
        ctx: &mut RequestContext,
        res: &mut Response,
    ) -> AetherResult<PipelineVerdict> {
        self.bus.publish(AetherEvent::ResponseReceived {
            flow_id: ctx.flow_id,
            status: res.status.code,
        });

        for hook in self.registry.hooks() {
            let before = res.fingerprint();
            let started = Instant::now();
            let fut = hook.on_response(ctx, res);
            match tokio::time::timeout(self.hook_timeout, fut).await {
                Err(_elapsed) => {
                    return Err(AetherError::HookTimeout {
                        hook: hook.name().to_string(),
                        timeout_ms: self.hook_timeout.as_millis() as u64,
                    });
                }
                Ok(Err(e)) => {
                    return Err(AetherError::Hook {
                        hook: hook.name().to_string(),
                        message: e.to_string(),
                    });
                }
                Ok(Ok(HookAction::Stop)) => {
                    self.emit_hook_executed(hook.name(), HookStage::OnResponse, started);
                    return Ok(PipelineVerdict::Stopped);
                }
                Ok(Ok(HookAction::Continue)) => {
                    if res.fingerprint() != before {
                        self.bus.publish(AetherEvent::ResponseModified {
                            flow_id: ctx.flow_id,
                            by: hook.name().to_string(),
                        });
                    }
                    self.emit_hook_executed(hook.name(), HookStage::OnResponse, started);
                }
            }
        }

        self.bus.publish(AetherEvent::ResponseForwarded {
            flow_id: ctx.flow_id,
        });
        Ok(PipelineVerdict::Forwarded)
    }

    /// Completion stage: read-only hooks run over the finished flow, then
    /// `FlowCompleted` is emitted. `Stop` is ignored here by definition —
    /// the exchange is already over (documented in `HookAction::Stop`).
    pub async fn complete_flow(&self, ctx: &RequestContext, flow: &Flow) -> AetherResult<()> {
        for hook in self.registry.hooks() {
            let started = Instant::now();
            let fut = hook.on_flow_complete(ctx, flow);
            match tokio::time::timeout(self.hook_timeout, fut).await {
                Err(_elapsed) => {
                    return Err(AetherError::HookTimeout {
                        hook: hook.name().to_string(),
                        timeout_ms: self.hook_timeout.as_millis() as u64,
                    });
                }
                Ok(Err(e)) => {
                    return Err(AetherError::Hook {
                        hook: hook.name().to_string(),
                        message: e.to_string(),
                    });
                }
                Ok(Ok(_action)) => {
                    self.emit_hook_executed(hook.name(), HookStage::OnFlowComplete, started);
                }
            }
        }
        self.bus.publish(AetherEvent::FlowCompleted {
            flow_id: ctx.flow_id,
        });
        Ok(())
    }

    /// Findings are just events (primitive #3) — anything may report one.
    pub fn report_finding(&self, flow_id: FlowId, title: impl Into<String>) {
        self.bus.publish(AetherEvent::FindingCreated {
            flow_id,
            title: title.into(),
        });
    }

    fn emit_hook_executed(&self, hook: &str, stage: HookStage, started: Instant) {
        self.bus.publish(AetherEvent::HookExecuted {
            hook: hook.to_string(),
            stage,
            duration_ms: started.elapsed().as_millis() as u64,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::TrafficSource;
    use crate::hooks::{Hook, HookResult};
    use crate::model::Url;
    use async_trait::async_trait;
    use tokio::time::sleep;

    struct UaRewrite;
    struct Tagger;

    #[async_trait]
    impl Hook for UaRewrite {
        fn name(&self) -> &str {
            "ua-rewrite"
        }
        async fn on_request(&self, _ctx: &mut RequestContext, req: &mut Request) -> HookResult {
            req.set_header("user-agent", "aether/0.1");
            Ok(HookAction::Continue)
        }
    }

    #[async_trait]
    impl Hook for Tagger {
        fn name(&self) -> &str {
            "tagger"
        }
        async fn on_request(&self, ctx: &mut RequestContext, _req: &mut Request) -> HookResult {
            ctx.set_data("classified", serde_json::json!("api"));
            Ok(HookAction::Continue)
        }
    }

    struct Stopper;
    #[async_trait]
    impl Hook for Stopper {
        fn name(&self) -> &str {
            "stopper"
        }
        async fn on_request(&self, _ctx: &mut RequestContext, _req: &mut Request) -> HookResult {
            Ok(HookAction::Stop)
        }
    }

    struct Slow;
    #[async_trait]
    impl Hook for Slow {
        fn name(&self) -> &str {
            "slow"
        }
        async fn on_request(&self, _ctx: &mut RequestContext, _req: &mut Request) -> HookResult {
            sleep(Duration::from_millis(200)).await;
            Ok(HookAction::Continue)
        }
    }

    fn sample_request() -> Request {
        Request::get(Url::parse("https://api.example.com/v1/users/42").unwrap())
    }

    #[tokio::test]
    async fn stop_halts_and_reports_stopped() {
        let bus = EventBus::new(64);
        let mut reg = HookRegistry::new();
        reg.register_fn(Stopper);
        reg.register_fn(UaRewrite); // must NOT run
        let pipe = Pipeline::new(reg, bus.clone());

        let mut sub = bus.subscribe();
        let mut ctx = RequestContext::new(3, TrafficSource::Proxy);
        let mut req = sample_request();
        let verdict = pipe.run_request(&mut ctx, &mut req).await.unwrap();
        assert_eq!(verdict, PipelineVerdict::Stopped);

        let mut seen = Vec::new();
        while let Some(e) = sub.try_next() {
            seen.push(e);
        }
        assert!(matches!(seen[0], AetherEvent::RequestReceived { .. }));
        assert!(matches!(
            seen.last().unwrap(),
            AetherEvent::HookExecuted { .. }
        ));
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, AetherEvent::RequestForwarded { .. }))
        );
        assert!(
            req.header("user-agent").is_none(),
            "hook after Stopper must not run"
        );
    }

    #[tokio::test]
    async fn timeout_is_enforced() {
        let bus = EventBus::new(16);
        let mut reg = HookRegistry::new();
        reg.register_fn(Slow);
        let pipe = Pipeline::new(reg, bus).with_hook_timeout(Duration::from_millis(20));

        let mut ctx = RequestContext::new(4, TrafficSource::Proxy);
        let mut req = sample_request();
        let err = pipe.run_request(&mut ctx, &mut req).await.unwrap_err();
        assert!(matches!(err, AetherError::HookTimeout { .. }), "{err}");
    }

    #[tokio::test]
    async fn modified_event_carries_hook_name() {
        let bus = EventBus::new(64);
        let mut reg = HookRegistry::new();
        reg.register_fn(Tagger); // touches ctx only → no modified event
        reg.register_fn(UaRewrite); // touches request → modified event
        let pipe = Pipeline::new(reg, bus.clone());

        let mut sub = bus.subscribe();
        let mut ctx = RequestContext::new(5, TrafficSource::Proxy);
        let mut req = sample_request();
        pipe.run_request(&mut ctx, &mut req).await.unwrap();

        let events: Vec<_> = std::iter::from_fn(|| sub.try_next()).collect();
        let modified: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AetherEvent::RequestModified { by, .. } => Some(by.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(modified, vec!["ua-rewrite"]);
        assert!(ctx.get_data("classified").is_some());
    }

    #[tokio::test]
    async fn full_cycle_and_finding() {
        let bus = EventBus::new(64);
        let pipe = Pipeline::new(HookRegistry::new(), bus.clone());

        let mut sub = bus.subscribe();
        let mut ctx = RequestContext::new(11, TrafficSource::Proxy);
        let mut req = sample_request();
        let mut res = Response::new(200).unwrap();

        assert_eq!(
            pipe.run_request(&mut ctx, &mut req).await.unwrap(),
            PipelineVerdict::Forwarded
        );
        assert_eq!(
            pipe.run_response(&mut ctx, &mut res).await.unwrap(),
            PipelineVerdict::Forwarded
        );
        pipe.report_finding(ctx.flow_id, "interesting: idor candidate");

        let mut flow = Flow::new(ctx.flow_id, req.clone());
        flow.response = Some(res.clone());
        pipe.complete_flow(&ctx, &flow).await.unwrap();

        let events: Vec<_> = std::iter::from_fn(|| sub.try_next()).collect();
        let kinds: Vec<String> = events
            .iter()
            .map(|e| {
                serde_json::to_value(e).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "request_received",
                "request_forwarded",
                "response_received",
                "response_forwarded",
                "finding_created",
                "flow_completed"
            ]
        );
    }
}

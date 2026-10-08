//! THE Phase-0 architectural test (founder's definition):
//!
//! ```text
//! Request → RequestContext → Hook A → Hook B → Event → Modified Request
//! ```
//!
//! then the same core is consumed from a second, independent client that
//! only listens on the event bus (what the GUI / automation will be).

use aether_core::{
    EventBus, Hook, HookAction, HookRegistry, HookResult, Pipeline, PipelineVerdict,
    RequestContext, Response, TrafficSource,
};
use async_trait::async_trait;
use std::time::Duration;

use aether_core::{Body, Method, Request, Url};

/// Hook A: lowest priority — classifies and tags the *context* only.
struct HookA;

#[async_trait]
impl Hook for HookA {
    fn name(&self) -> &str {
        "hook-a"
    }
    fn priority(&self) -> i32 {
        -10
    }
    async fn on_request(&self, ctx: &mut RequestContext, _req: &mut Request) -> HookResult {
        ctx.set_data("classified", serde_json::json!("api"));
        Ok(HookAction::Continue)
    }
}

/// Hook B: stamps the request — must produce a `RequestModified` event.
struct HookB;

#[async_trait]
impl Hook for HookB {
    fn name(&self) -> &str {
        "hook-b"
    }
    async fn on_request(&self, _ctx: &mut RequestContext, req: &mut Request) -> HookResult {
        req.set_header("x-aether-touched", "hook-b");
        Ok(HookAction::Continue)
    }
}

/// Hook C: annotates responses (order checks across stages).
struct HookC;

#[async_trait]
impl Hook for HookC {
    fn name(&self) -> &str {
        "hook-c"
    }
    async fn on_response(&self, _ctx: &mut RequestContext, res: &mut Response) -> HookResult {
        res.set_header("x-aether-inspected", "hook-c");
        Ok(HookAction::Continue)
    }
}

fn sample() -> Request {
    let mut r = Request::new(
        Method::Post,
        Url::parse("https://api.example.com/v1/users/42?id=7").unwrap(),
    );
    r.body = Body::text(r#"{"id": 42}"#);
    r
}

#[tokio::test]
async fn the_phase_zero_architectural_proof() {
    let bus = EventBus::new(128);
    let mut registry = HookRegistry::new();
    registry.register_fn(HookA);
    registry.register_fn(HookB);
    registry.register_fn(HookC);
    let pipeline = Pipeline::new(registry, bus.clone());

    // an independent consumer — the future GUI/CLI/automation shape
    let mut client = bus.subscribe();

    let mut ctx = RequestContext::new(18342, TrafficSource::Proxy);
    let mut req = sample();
    let before = req.fingerprint();

    // 1) request through hooks
    assert_eq!(
        pipeline.run_request(&mut ctx, &mut req).await.unwrap(),
        PipelineVerdict::Forwarded
    );

    // 2) the request was actually modified
    assert_eq!(req.header("x-aether-touched"), Some("hook-b"));
    assert_ne!(before, req.fingerprint());
    assert_eq!(
        ctx.get_data("classified").and_then(|v| v.as_str()),
        Some("api")
    );

    // 3) response through hooks
    let mut res = Response::new(200).unwrap();
    assert_eq!(
        pipeline.run_response(&mut ctx, &mut res).await.unwrap(),
        PipelineVerdict::Forwarded
    );
    assert_eq!(res.header("x-aether-inspected"), Some("hook-c"));

    // 4) completion
    let mut flow = aether_core::Flow::new(ctx.flow_id, req.clone());
    flow.response = Some(res.clone());
    pipeline.complete_flow(&ctx, &flow).await.unwrap();

    // 5) the independent client saw the whole story, in order
    // (HookExecuted is observability telemetry — filtered from the story)
    let mut kinds: Vec<String> = Vec::new();
    while let Some(ev) = client.try_next() {
        let v = serde_json::to_value(&ev).unwrap();
        if v["type"] == "hook_executed" {
            continue;
        }
        kinds.push(v["type"].as_str().unwrap().to_string());
    }
    assert_eq!(
        kinds,
        vec![
            "request_received",
            "request_modified", // by hook-b
            "request_forwarded",
            "response_received",
            "response_modified", // by hook-c
            "response_forwarded",
            "flow_completed",
        ]
    );
}

#[tokio::test]
async fn same_core_serves_two_clients_without_ui() {
    // Two pipelines sharing one bus = the daemon contract: any client can
    // drive traffic, every client sees the events.
    let bus = EventBus::new(128);
    let mk = |name: &'static str| {
        struct P(&'static str);
        #[async_trait]
        impl Hook for P {
            fn name(&self) -> &str {
                self.0
            }
            async fn on_request(&self, _ctx: &mut RequestContext, req: &mut Request) -> HookResult {
                req.set_header("x-driven-by", self.0);
                Ok(HookAction::Continue)
            }
        }
        let mut reg = HookRegistry::new();
        reg.register_fn(P(name));
        Pipeline::new(reg, bus.clone())
    };
    let cli_pipeline = mk("cli-driver");
    let automation_pipeline = mk("automation-driver");

    let mut watcher = bus.subscribe();

    let mut ctx1 = RequestContext::new(1, TrafficSource::Replay);
    let mut req1 = sample();
    cli_pipeline
        .run_request(&mut ctx1, &mut req1)
        .await
        .unwrap();

    let mut ctx2 = RequestContext::new(2, TrafficSource::Automation);
    let mut req2 = sample();
    automation_pipeline
        .run_request(&mut ctx2, &mut req2)
        .await
        .unwrap();

    let mut drivers = Vec::new();
    while let Some(ev) = watcher.try_next() {
        if let aether_core::AetherEvent::RequestModified { by, .. } = ev {
            drivers.push(by);
        }
    }
    assert_eq!(drivers, vec!["cli-driver", "automation-driver"]);
    assert_eq!(req1.header("x-driven-by"), Some("cli-driver"));
    assert_eq!(req2.header("x-driven-by"), Some("automation-driver"));
}

#[tokio::test]
async fn stop_action_drops_the_exchange() {
    struct Firewall;
    #[async_trait]
    impl Hook for Firewall {
        fn name(&self) -> &str {
            "firewall"
        }
        async fn on_request(&self, ctx: &mut RequestContext, _req: &mut Request) -> HookResult {
            if !ctx.in_scope("evil.example.com") {
                return Ok(HookAction::Stop);
            }
            Ok(HookAction::Continue)
        }
    }

    let mut reg = HookRegistry::new();
    reg.register_fn(Firewall);
    let pipe = Pipeline::new(reg, EventBus::new(16)).with_hook_timeout(Duration::from_secs(1));

    let mut ctx = RequestContext::new(7, TrafficSource::Proxy);
    ctx.scope = aether_core::Scope::include(&["api.example.com"]); // evil is out
    let mut req = Request::get(Url::parse("https://evil.example.com/x").unwrap());
    assert_eq!(
        pipe.run_request(&mut ctx, &mut req).await.unwrap(),
        PipelineVerdict::Stopped
    );
}

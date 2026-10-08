//! `aether demo|info|about` — the Phase-0 proof and project meta.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).

use aether_core::error::AetherResult;
use aether_core::{
    Body, Capabilities, EventBus, Hook, HookAction, HookRegistry, HookResult, Pipeline, Request,
    RequestContext, RuleAction, RuleHook, RuleSpec, Scope, TrafficSource, Url,
};
use async_trait::async_trait;

pub async fn run() -> AetherResult<()> {
    println!("── Phase 0 architectural proof ─────────────────────────────");

    // The same core a GUI/automation client would drive — no UI in sight.
    let bus = EventBus::new(64);
    let mut registry = HookRegistry::new();

    // Hook A: a compiled hook (what the daemon ships internally).
    struct Classifier;
    #[async_trait]
    impl Hook for Classifier {
        fn name(&self) -> &str {
            "classifier"
        }
        fn priority(&self) -> i32 {
            -10
        }
        async fn on_request(&self, ctx: &mut RequestContext, req: &mut Request) -> HookResult {
            let kind = if req.url.host.contains("api") {
                "api"
            } else {
                "web"
            };
            ctx.set_data("classified", serde_json::json!(kind));
            Ok(HookAction::Continue)
        }
    }
    registry.register_fn(Classifier);

    // Hook B: a *serialized* rule (what the UI/control plane ships) — proof
    // that JSON from a remote client becomes live traffic logic (ADR-006 #2).
    let stamp: RuleSpec = serde_json::from_value(serde_json::json!({
        "name": "stamp-demo",
        "action": {"kind": "set_request_header", "name": "x-aether-touched", "value": "rule-from-json"}
    }))
    .map_err(|e| aether_core::AetherError::protocol(e.to_string()))?;
    registry.register_fn(RuleHook::from_spec(stamp)?);

    let pipeline = Pipeline::new(registry, bus.clone());
    let mut events = bus.subscribe();

    let mut ctx = RequestContext::new(18342, TrafficSource::Proxy);
    ctx.scope = Scope::include(&["*.example.com"]);
    ctx.capabilities = Capabilities::proxy_defaults();

    let mut req = Request::post(Url::parse("https://api.example.com/v1/users/42?id=7")?);
    req.set_header("content-type", "application/json");
    req.body = Body::text(r#"{"id": 42}"#);

    println!("initial request : {} {}", req.method.as_str(), req.url);
    println!("scope match     : {}", ctx.in_scope(&req.url.host));

    let verdict = pipeline.run_request(&mut ctx, &mut req).await?;

    let mut res = aether_core::Response::new(200)?;
    res.body = Body::text(r#"{"id": 42}"#);
    let verdict_r = pipeline.run_response(&mut ctx, &mut res).await?;

    let mut flow = aether_core::Flow::new(ctx.flow_id, req.clone());
    flow.response = Some(res.clone());
    pipeline.complete_flow(&ctx, &flow).await?;

    println!("verdict         : req={verdict:?} res={verdict_r:?}");
    println!(
        "final header    : {}",
        req.header("x-aether-touched").unwrap_or("MISSING")
    );
    println!("── events (as the control plane would deliver them) ────────");
    while let Some(ev) = events.try_next() {
        println!(
            "  {}",
            serde_json::to_string(&ev).expect("events are serializable")
        );
    }
    println!("── proof complete: compiled hook + JSON rule, zero UI crates ──");
    Ok(())
}

pub fn info() -> AetherResult<()> {
    println!("Aether — architecture invariants:");
    println!("  1. aether-core knows nothing about TLS/SQLite/WebSocket/Tauri/React/wire codecs");
    println!("  2. GUI, CLI and CI are all just clients of the daemon (ADR-001)");
    println!("  3. every pipeline stage routes through the HookRegistry (ADR-006 #1)");
    println!("  4. hook signatures are serialization-shaped — JSON rules prove it (ADR-006 #2)");
    println!("  5. hooks carry a timeout budget, enforced by the pipeline (ADR-006 #5)");
    println!("  6. every security-relevant state change is an event (primitive #3)");
    println!("Run `aether demo` to watch the proof execute.");
    Ok(())
}

pub fn about() -> AetherResult<()> {
    println!(
        "Aether {} — local-first, open-source, developer-first, AI-native API security proxy",
        aether_daemon::VERSION
    );
    println!();
    println!("  founder   : smart-jemy");
    println!("  license   : Apache-2.0 (core) — see LICENSE");
    println!("  telemetry : zero, by design (ADR-008)");
    println!("  ai        : BYO-key, off by default (ADR-008)");
    println!();
    println!("  decisions : docs/adr/ADR-001 … ADR-011 (all Accepted)");
    println!(
        "  roadmap   : V0.1 h1+MITM (you are here) → V0.2 h2/WS → V0.3 plugins → V1.0 query+AI"
    );
    let _ = RuleAction::AddTag { tag: String::new() }; // keep rules import used
    Ok(())
}

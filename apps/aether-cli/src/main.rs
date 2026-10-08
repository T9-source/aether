//! `aether` CLI — a thin client of the same core the GUI will use.
//!
//! Architecture invariant (kill-the-UI, ADR-001): everything below works with
//! **zero** UI crates in the picture. At Phase 0 the `demo` command runs the
//! first architectural proof:
//!
//! ```text
//! Request → RequestContext → Hook A → Hook B → Events → Modified Request
//! ```

use aether_core::{
    AetherResult, Capabilities, Hook, HookAction, HookRegistry, HookResult, Pipeline,
    RequestContext, Scope, TrafficSource, Url,
};
use async_trait::async_trait;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "aether",
    version,
    about = "Aether — local-first, open-source API security proxy (Phase 0 core)"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the Phase-0 architectural proof through the real pipeline.
    Demo,
    /// Print the architecture invariants this binary guarantees.
    Info,
}

/// Hook A (priority -10): classify into the context (ctx-only touch →
/// deliberately produces no `RequestModified`).
struct Classifier;

#[async_trait]
impl Hook for Classifier {
    fn name(&self) -> &str {
        "classifier"
    }
    fn priority(&self) -> i32 {
        -10
    }
    async fn on_request(
        &self,
        ctx: &mut RequestContext,
        req: &mut aether_core::Request,
    ) -> HookResult {
        let classified = if req.url.host.contains("api") {
            "api"
        } else {
            "web"
        };
        ctx.set_data("classified", serde_json::json!(classified));
        Ok(HookAction::Continue)
    }
}

/// Hook B (priority 0): stamp the request (model touch → `RequestModified`).
struct TouchStamp;

#[async_trait]
impl Hook for TouchStamp {
    fn name(&self) -> &str {
        "touch-stamp"
    }
    async fn on_request(
        &self,
        _ctx: &mut RequestContext,
        req: &mut aether_core::Request,
    ) -> HookResult {
        req.set_header("x-aether-touched", "hook-b");
        Ok(HookAction::Continue)
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Demo => demo().await,
        Commands::Info => info(),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn info() -> AetherResult<()> {
    println!("Aether Phase 0 — architecture invariants:");
    println!("  1. aether-core knows nothing about hyper/TLS/SQLite/WebSocket/Tauri/React");
    println!("  2. GUI, CLI and CI are all just clients of the core (ADR-001)");
    println!("  3. every pipeline stage routes through the HookRegistry (ADR-006 #1)");
    println!("  4. hook signatures are serialization-shaped (ADR-006 #2)");
    println!("  5. hooks carry a timeout budget, enforced by the pipeline (ADR-006 #5)");
    println!("  6. every security-relevant state change is an event (primitive #3)");
    println!("Run `aether demo` to watch the proof execute.");
    Ok(())
}

async fn demo() -> AetherResult<()> {
    println!("── Phase 0 architectural proof ─────────────────────────────");

    // The same core a GUI/automation client would drive — no UI in sight.
    let bus = aether_core::EventBus::new(64);
    let mut registry = HookRegistry::new();
    registry.register_fn(Classifier);
    registry.register_fn(TouchStamp);
    let pipeline = Pipeline::new(registry, bus.clone());

    // Subscribe BEFORE running: an event consumer is just another client.
    let mut events = bus.subscribe();

    let mut ctx = RequestContext::new(18342, TrafficSource::Proxy);
    ctx.scope = Scope::include(&["*.example.com"]);
    ctx.capabilities = Capabilities::proxy_defaults();

    let mut req =
        aether_core::Request::post(Url::parse("https://api.example.com/v1/users/42?id=7")?);
    req.set_header("content-type", "application/json");
    req.set_header("user-agent", "demo-client/1.0");
    req.body = aether_core::Body::text(r#"{"id": 42}"#);

    println!("initial request : {} {}", req.method.as_str(), req.url);
    println!("scope match     : {}", ctx.in_scope(&req.url.host));

    let verdict = pipeline.run_request(&mut ctx, &mut req).await?;

    let mut res = aether_core::Response::new(200)?;
    res.set_header("content-type", "application/json");
    res.body = aether_core::Body::text(r#"{"id": 42, "email": "u42@example.com"}"#);
    let verdict_r = pipeline.run_response(&mut ctx, &mut res).await?;

    let mut flow = aether_core::Flow::new(ctx.flow_id, req.clone());
    flow.response = Some(res.clone());
    flow.add_tag(
        ctx.get_data("classified")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown"),
    );
    pipeline.complete_flow(&ctx, &flow).await?;

    pipeline.report_finding(ctx.flow_id, "demo: object id looks user-controlled");

    println!("verdict         : req={verdict:?} res={verdict_r:?}");
    println!("ctx.classified  : {}", ctx.get_data("classified").unwrap());
    println!("flow tags       : {:?}", flow.meta.tags);
    println!(
        "final header    : x-aether-touched={}",
        req.header("x-aether-touched").unwrap_or("MISSING")
    );
    println!("── events (as the control plane would deliver them) ────────");
    while let Some(ev) = events.try_next() {
        println!(
            "  {}",
            serde_json::to_string(&ev).expect("events are serializable")
        );
    }
    println!("── proof complete: core ran standalone, zero UI crates ─────");
    Ok(())
}

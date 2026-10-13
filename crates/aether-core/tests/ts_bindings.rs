//! TypeScript bindings strategy (Milestone-0 DoD item):
//!
//! **Single source of truth = Rust.** Every type that crosses the control
//! plane derives `ts_rs::TS` and exports to `crates/aether-core/bindings/`.
//! Hand-written TS types are forbidden (ADR-002 guardrail #2). CI runs this
//! test; generated `.d.ts` files are consumed by the Tauri app workspace.

use aether_core::{
    AetherError, AetherEvent, Body, Capabilities, Flow, Header, Headers, HookStage, HttpVersion,
    Method, Request, Response, Scope, StatusCode, TrafficSource, TransportProto, Url,
};
use ts_rs::TS;

macro_rules! export_all {
    ($($t:ty),+ $(,)?) => {
        $( <$t as TS>::export_all(&ts_rs::Config::default())
            .expect(concat!("export failed for ", stringify!($t))); )+
    };
}

#[test]
fn generate_typescript_bindings() {
    std::fs::create_dir_all("bindings").unwrap();
    export_all!(
        Body,
        Flow,
        Header,
        Headers,
        HttpVersion,
        Method,
        Request,
        Response,
        Scope,
        StatusCode,
        TrafficSource,
        TransportProto,
        Url,
        AetherError,
        AetherEvent,
        HookStage,
        Capabilities,
        // RequestContext's scratch map uses a custom TS type override;
        // export it last so failures point at the exotic member first.
        aether_core::RequestContext,
    );
}

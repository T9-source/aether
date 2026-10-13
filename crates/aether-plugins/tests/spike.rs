//! ADR-019 spike integration tests — they exercise REAL compiled wasm
//! artifacts (examples/plugins/*/plugin.wasm).
//!
//! The artifacts are built by `scripts/build-spike-plugins.sh` (needs the
//! wasm32-wasip1 target). Missing artifacts are a HARD FAILURE, not a skip:
//! silently green tests that never touched wasm would be worse than red.

use aether_core::context::{RequestContext, TrafficSource};
use aether_core::error::AetherError;
use aether_core::hooks::{Hook, HookAction};
use aether_core::model::{Body, Request, Url};
use aether_plugins::{PluginManifest, WasmHook};
use std::path::PathBuf;
use std::time::Instant;

fn plugin_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/plugins")
        .join(name)
}

fn require_artifact(name: &str) -> (PathBuf, PluginManifest) {
    let dir = plugin_dir(name);
    let wasm = dir.join("plugin.wasm");
    assert!(
        wasm.exists(),
        "missing {wasm:?} — run scripts/build-spike-plugins.sh first (tests must touch real wasm)"
    );
    let manifest = PluginManifest::load(&dir).expect("manifest parses");
    (dir, manifest)
}

fn request() -> Request {
    let mut req = Request::get(Url::parse("https://example.com/").unwrap());
    req.body = Body::text("hello-body");
    req
}

fn ctx() -> RequestContext {
    RequestContext::new(1, TrafficSource::Proxy)
}

#[tokio::test]
async fn hello_stamps_header_through_pooled_hook() {
    let (dir, manifest) = require_artifact("hello");
    let hook = WasmHook::load(&dir, manifest).unwrap();

    assert!(hook.subscribes_request());
    assert!(hook.subscribes_response());
    assert_eq!(hook.name(), "plugin/hello/hook");

    let mut req = request();
    let out = hook.on_request(&mut ctx(), &mut req).await.unwrap();
    assert_eq!(out, HookAction::Continue);
    assert_eq!(
        req.headers.get("x-aether-hello"),
        Some("from-wasm"),
        "the declarative outcome must be applied by the host"
    );
}

#[tokio::test]
async fn sleepy_is_epoch_interrupted_within_budget() {
    let (dir, manifest) = require_artifact("sleepy");
    assert_eq!(manifest.timeout(), 10, "sleepy pins a 10ms budget");
    let hook = WasmHook::load(&dir, manifest).unwrap();
    assert!(hook.subscribes_request());
    assert!(!hook.subscribes_response(), "single-stage subscription");

    let mut req = request();
    let started = Instant::now();
    let err = hook.on_request(&mut ctx(), &mut req).await.unwrap_err();
    let elapsed = started.elapsed();

    match err {
        AetherError::HookTimeout { hook, timeout_ms } => {
            assert!(hook.starts_with("plugin/sleepy/"));
            assert_eq!(timeout_ms, 10);
        }
        other => panic!("expected HookTimeout, got {other:?}"),
    }
    // The plugin wants 500ms; interruption must be ~10ms, not the full sleep.
    assert!(
        elapsed < std::time::Duration::from_millis(400),
        "epoch interruption took {elapsed:?} — interrupt is not real"
    );
}

#[tokio::test]
async fn mute_plugin_is_refused_at_load() {
    let (dir, manifest) = require_artifact("mute");
    let err = WasmHook::load(&dir, manifest).unwrap_err();
    assert!(
        err.to_string()
            .contains("neither on_request nor on_response"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn manifest_guards_block_escapes_and_missing_artifacts() {
    let tmp = tempfile::tempdir().unwrap();

    // (a) wasm path escaping the plugin directory — refused by the guard.
    let outside = tmp.path().join("outside.wasm");
    std::fs::write(&outside, b"not wasm").unwrap();
    let dir = tmp.path().join("evil");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!("name = \"evil\"\nwasm = \"{}\"\n", outside.display()),
    )
    .unwrap();
    let err = PluginManifest::load(&dir).unwrap_err();
    assert!(err.to_string().contains("escapes"), "unexpected: {err}");

    // (b) manifest referencing a missing artifact — refused at load.
    let dir = tmp.path().join("ghost");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "name = \"ghost\"\nwasm = \"plugin.wasm\"\n",
    )
    .unwrap();
    let err = PluginManifest::load(&dir).unwrap_err();
    assert!(err.to_string().contains("plugin.wasm"), "unexpected: {err}");
}

#[tokio::test]
async fn pooled_warm_baseline_reports_spike_json() {
    let (dir, manifest) = require_artifact("hello");
    let hook = WasmHook::load(&dir, manifest).unwrap();

    // Warm-up: fill the pool, JIT caches, allocator arenas.
    for _ in 0..50 {
        let mut req = request();
        hook.on_request(&mut ctx(), &mut req).await.unwrap();
    }

    // 1000 pooled warm calls — the median/p90 that ADR-019 reports.
    let mut samples: Vec<u128> = Vec::with_capacity(1000);
    for _ in 0..1000 {
        let mut req = request();
        let started = Instant::now();
        hook.on_request(&mut ctx(), &mut req).await.unwrap();
        samples.push(started.elapsed().as_micros());
    }
    samples.sort_unstable();
    let median = samples[samples.len() / 2];
    let p90 = samples[samples.len() * 9 / 10];

    // Fresh-instance cost, once, for the report (compile is NOT redone —
    // only instantiation; that is the whole point of the pool).
    let fresh_started = Instant::now();
    let fresh = extism::Plugin::new_from_compiled(&hook_debug_compiled()).unwrap();
    drop(fresh);
    let fresh_us = fresh_started.elapsed().as_micros();

    // Sanity bounds — never flaky, just catching gross regressions.
    assert!(
        median < 5_000,
        "pooled warm median {median}µs — pooling collapsed?"
    );
    assert!(fresh_us > 0);

    // Machine-readable output for the ADR evidence table.
    println!(
        "SPIKE_JSON: {{\"pooled_warm_median_us\": {}, \"pooled_warm_p90_us\": {}, \"fresh_instance_us\": {}, \"n\": 1000}}",
        median, p90, fresh_us
    );
}

/// Test-only helper: compile the hello artifact again for the fresh-instance
/// datapoint (the pool owns its own CompiledPlugin privately).
fn hook_debug_compiled() -> extism::CompiledPlugin {
    let (dir, manifest) = require_artifact("hello");
    let wasm = manifest.wasm_bytes(&dir).unwrap();
    let m = extism::Manifest::new([extism::Wasm::data(wasm)])
        .with_timeout(std::time::Duration::from_millis(manifest.timeout()))
        .with_memory_max(manifest.pages());
    extism::CompiledPlugin::new(extism::PluginBuilder::new(m).with_wasi(true)).unwrap()
}

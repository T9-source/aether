//! PluginRegistry lifecycle (ADR-020 §lifecycle) — fail-closed startup,
//! config-as-truth, lazy enable, clean disable — against REAL wasm.

use aether_core::context::{RequestContext, TrafficSource};
use aether_core::hooks::HookRegistry;
use aether_core::model::{Body, Request, Url};
use aether_plugins::{PluginEntry, PluginRegistry, PluginState};
use std::path::PathBuf;

fn plugin_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/plugins")
        .join(name)
}

fn require_artifact(name: &str) {
    let wasm = plugin_path(name).join("plugin.wasm");
    assert!(
        wasm.exists(),
        "missing {wasm:?} — run scripts/build-spike-plugins.sh first"
    );
}

fn entry(name: &str, enabled: bool) -> PluginEntry {
    PluginEntry {
        path: plugin_path(name),
        enabled,
    }
}

fn request() -> Request {
    let mut req = Request::get(Url::parse("https://example.com/").unwrap());
    req.body = Body::text("hi");
    req
}

#[test]
fn enabled_plugin_loads_and_registers() {
    require_artifact("hello");
    let registry = HookRegistry::new();
    let plugins = PluginRegistry::load(&[entry("hello", true)], &registry).expect("loads");
    let list = plugins.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].state, PluginState::Loaded);
    assert!(list[0].subscribes_request);
    assert!(list[0].subscribes_response);
    assert_eq!(list[0].capabilities, vec!["modify_traffic"]);
    assert!(
        registry
            .names()
            .iter()
            .any(|n| n.starts_with("plugin/hello/"))
    );
}

#[test]
fn enabled_plugin_with_missing_wasm_refuses_startup() {
    let registry = HookRegistry::new();
    let ghost = PluginEntry {
        path: plugin_path("does-not-exist"),
        enabled: true,
    };
    let err = PluginRegistry::load(&[ghost], &registry).unwrap_err();
    assert!(
        err.to_string().contains("refusing to start"),
        "unexpected: {err}"
    );
}

#[test]
fn disabled_plugin_is_parsed_but_not_registered() {
    require_artifact("hello");
    let registry = HookRegistry::new();
    let plugins = PluginRegistry::load(&[entry("hello", false)], &registry).expect("loads");
    assert_eq!(plugins.list()[0].state, PluginState::Disabled);
    assert!(!plugins.list()[0].subscribes_request);
    assert!(
        registry.names().iter().all(|n| !n.starts_with("plugin/")),
        "disabled plugin must not register hooks"
    );
}

#[test]
fn disabled_broken_plugin_is_nonfatal_but_recorded() {
    let registry = HookRegistry::new();
    let ghost = PluginEntry {
        path: plugin_path("does-not-exist"),
        enabled: false,
    };
    let plugins = PluginRegistry::load(&[ghost], &registry).expect("disabled failure is nonfatal");
    let list = plugins.list();
    assert_eq!(list[0].state, PluginState::Failed);
    assert!(
        list[0]
            .error
            .as_deref()
            .unwrap_or("")
            .contains("load failed")
    );
}

#[test]
fn duplicate_names_are_refused() {
    require_artifact("hello");
    let registry = HookRegistry::new();
    let err =
        PluginRegistry::load(&[entry("hello", true), entry("hello", true)], &registry).unwrap_err();
    assert!(
        err.to_string().contains("more than once"),
        "unexpected: {err}"
    );
}

#[tokio::test]
async fn enable_disable_lifecycle_and_live_calls() {
    require_artifact("hello");
    let registry = HookRegistry::new();
    let plugins =
        PluginRegistry::load(&[entry("hello", false)], &registry).expect("starts disabled");

    // disabled → no hook in the pipeline
    assert!(registry.names().iter().all(|n| !n.starts_with("plugin/")));

    // enable → registers, and the hook actually stamps traffic
    plugins.enable("hello", &registry).unwrap();
    assert!(
        registry
            .names()
            .iter()
            .any(|n| n.starts_with("plugin/hello/"))
    );
    let hook_names = registry.names();
    let list = plugins.list();
    assert_eq!(list[0].state, PluginState::Loaded);

    // run the pipeline's hook through the registry to prove wiring, not
    // just bookkeeping
    let hooks = registry.snapshot();
    let mut req = request();
    let mut ctx = RequestContext::new(1, TrafficSource::Proxy);
    for h in hooks.iter() {
        h.on_request(&mut ctx, &mut req).await.unwrap();
    }
    assert_eq!(req.headers.get("x-aether-hello"), Some("from-wasm"));
    drop(hook_names);

    // disable → unregisters exactly this plugin
    plugins.disable("hello", &registry).unwrap();
    assert!(
        registry
            .names()
            .iter()
            .all(|n| !n.starts_with("plugin/hello/"))
    );
    let list = plugins.list();
    assert_eq!(list[0].state, PluginState::Disabled);

    // re-enable works (fresh compile from disk)
    plugins.enable("hello", &registry).unwrap();
    assert!(
        registry
            .names()
            .iter()
            .any(|n| n.starts_with("plugin/hello/"))
    );

    // unknown names error cleanly
    assert!(plugins.enable("nope", &registry).is_err());
    assert!(plugins.disable("nope", &registry).is_err());
}

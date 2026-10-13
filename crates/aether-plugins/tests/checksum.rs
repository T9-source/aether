//! Checksum pinning (ADR-020 §integrity) — the artifact is verified against
//! the manifest's `sha256` before anything compiles. One flipped byte is a
//! refused plugin.

use aether_core::error::AetherError;
use aether_plugins::{PluginManifest, WasmHook};
use std::path::PathBuf;

fn hello_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/hello")
}

fn require_artifact() {
    let wasm = hello_dir().join("plugin.wasm");
    assert!(
        wasm.exists(),
        "missing {wasm:?} — run scripts/build-spike-plugins.sh first"
    );
}

fn manifest_with_pin(pin: Option<String>) -> (tempfile::TempDir, PluginManifest) {
    // Copy the real plugin into a temp dir so we can rewrite its manifest
    // without touching the examples tree.
    let src = hello_dir();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::copy(src.join("plugin.wasm"), tmp.path().join("plugin.wasm")).unwrap();
    let manifest = PluginManifest::load(&src).unwrap();
    let mut manifest = manifest;
    manifest.sha256 = pin;
    (tmp, manifest)
}

#[tokio::test]
async fn correct_pin_loads() {
    require_artifact();
    use sha2::Digest as _;
    let wasm = std::fs::read(hello_dir().join("plugin.wasm")).unwrap();
    let digest = hex::encode(sha2::Sha256::digest(&wasm));
    let (tmp, manifest) = manifest_with_pin(Some(digest));
    let hook = WasmHook::load(tmp.path(), manifest);
    assert!(hook.is_ok(), "pinned artifact must load: {hook:?}");
}

#[tokio::test]
async fn wrong_pin_is_refused_before_compile() {
    require_artifact();
    let wrong = "0".repeat(64);
    let (tmp, manifest) = manifest_with_pin(Some(wrong));
    let err = WasmHook::load(tmp.path(), manifest).unwrap_err();
    match err {
        AetherError::Config(msg) => {
            assert!(msg.contains("sha256 mismatch"), "unexpected: {msg}");
        }
        other => panic!("expected Config error, got {other:?}"),
    }
}

#[tokio::test]
async fn unpinned_still_loads() {
    require_artifact();
    let (tmp, manifest) = manifest_with_pin(None);
    let hook = WasmHook::load(tmp.path(), manifest);
    assert!(hook.is_ok(), "unpinned artifact must load: {hook:?}");
}

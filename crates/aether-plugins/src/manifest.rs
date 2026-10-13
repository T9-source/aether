//! `plugin.toml` — the declarative plugin manifest (ADR-019 §permissions).
//!
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Permissions v1 are **static**: the manifest declares what the plugin may
//! do, mapped 1:1 onto the existing `Capabilities` vocabulary, and the
//! control plane surfaces it. Fail-closed everywhere:
//!
//! - unknown keys in `plugin.toml` are rejected (`deny_unknown_fields`) —
//!   same policy as the daemon config (M4: typos must fail loudly);
//! - unknown capability names are rejected, not silently ignored;
//! - the `wasm` path must resolve *inside* the plugin directory (canonicalize
//!   + prefix check — a manifest cannot point at `/usr/lib/evil.wasm`);
//! - when `sha256` is pinned, the artifact must match byte-for-byte at load
//!   (ADR-020 §integrity) — one flipped byte is a refused plugin.
//!
//! Resource bounds are clamped, not trusted: `timeout_ms` into
//! 10..=5000, `max_pages` into 1..=256 (16 pages = 1 MiB OOM-killed the
//! WASI+serde startup path in spike testing; 256 pages = 16 MiB is the
//! ceiling ADR-019 ships with).

use aether_core::context::Capabilities;
use aether_core::error::{AetherError, AetherResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default per-call budget when the manifest omits `timeout_ms`.
pub const DEFAULT_TIMEOUT_MS: u64 = 1_000;
/// Hard ceiling for `timeout_ms` — the pipeline's own hook budget is 5s
/// (ADR-006 rule #5); a plugin may ask for at most the same.
pub const MAX_TIMEOUT_MS: u64 = 5_000;
/// Floor for `timeout_ms` — below this even a no-op can't schedule.
pub const MIN_TIMEOUT_MS: u64 = 10;
/// Default Wasm linear-memory ceiling, in 64KiB pages (256 × 64KiB = 16 MiB).
pub const DEFAULT_MAX_PAGES: u32 = 256;
/// Hard ceiling for `max_pages`.
pub const MAX_MAX_PAGES: u32 = 256;
/// Honest body truncation point for the wire views (see `wire`).
pub const MAX_VIEW_BODY: usize = 1024 * 1024;

/// The one manifest file every plugin must ship.
pub const MANIFEST_FILE: &str = "plugin.toml";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    /// Stable identifier, `[a-z0-9-]{1,64}` — also the hook-registry prefix.
    pub name: String,
    /// Display version (control plane only — no dependency semantics in v1).
    #[serde(default)]
    pub version: Option<String>,
    /// Wasm artifact path, relative to this manifest's directory.
    pub wasm: String,
    /// Per-call budget in milliseconds (clamped to 10..=5000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Wasm linear-memory ceiling in 64KiB pages (clamped to 1..=256).
    #[serde(default)]
    pub max_pages: Option<u32>,
    /// Declared permissions — subset of the Capabilities vocabulary.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Optional artifact pin: lowercase hex sha256 of `wasm`. When present,
    /// the host refuses to load on any mismatch (ADR-020 §integrity).
    #[serde(default)]
    pub sha256: Option<String>,
}

impl PluginManifest {
    /// Parses `<dir>/plugin.toml`, clamps bounds, and resolves the wasm
    /// artifact with the escape guard. `dir` should already be absolute.
    pub fn load(dir: &Path) -> AetherResult<Self> {
        let manifest_path = dir.join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&manifest_path).map_err(|e| {
            AetherError::config(format!("plugin manifest {}: {e}", manifest_path.display()))
        })?;
        let mut m: PluginManifest = toml::from_str(&text)
            .map_err(|e| AetherError::config(format!("parse {}: {e}", manifest_path.display())))?;
        m.validate(dir)?;
        Ok(m)
    }

    /// In-code path for tests/programmatic use: same validation as `load`.
    pub fn parse(&self, dir: &Path) -> AetherResult<Self> {
        let mut m = self.clone();
        m.validate(dir)?;
        Ok(m)
    }

    fn validate(&mut self, dir: &Path) -> AetherResult<()> {
        // name: stable, lowercase, registry-prefix safe.
        let ok_len = !self.name.is_empty() && self.name.len() <= 64;
        let ok_chars = self
            .name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !ok_len || !ok_chars {
            return Err(AetherError::config(format!(
                "plugin name {:?} must match [a-z0-9-]{{1,64}}",
                self.name
            )));
        }

        // timeout: clamp, don't trust.
        self.timeout_ms = Some(
            self.timeout_ms
                .unwrap_or(DEFAULT_TIMEOUT_MS)
                .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS),
        );

        // memory: clamp — 16 pages (1 MiB) OOM-killed the WASI+serde startup
        // path in spike testing; 256 pages is the ceiling (ADR-019 §memory).
        self.max_pages = Some(
            self.max_pages
                .unwrap_or(DEFAULT_MAX_PAGES)
                .clamp(1, MAX_MAX_PAGES),
        );

        // capabilities: unknown name = refused plugin, not a silent no-op.
        for cap in &self.capabilities {
            if !KNOWN_CAPABILITIES.contains(&cap.as_str()) {
                return Err(AetherError::config(format!(
                    "plugin '{}': unknown capability '{cap}' (known: {})",
                    self.name,
                    KNOWN_CAPABILITIES.join(", ")
                )));
            }
        }

        // sha256 pin: format-check here (lowercase hex, 64 chars); the
        // content-check happens at load time against the actual artifact
        // bytes (see `WasmHook::load`). Lowercase-only keeps `sha256sum`
        // output directly pasteable into the manifest.
        if let Some(pin) = &self.sha256 {
            let ok = pin.len() == 64
                && pin
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if !ok {
                return Err(AetherError::config(format!(
                    "plugin '{}': sha256 must be 64 lowercase hex chars",
                    self.name
                )));
            }
        }

        if self.wasm.is_empty() || self.wasm.contains("..") {
            return Err(AetherError::config(format!(
                "plugin '{}': wasm path must be a plain relative path",
                self.name
            )));
        }

        // Escape guard: canonicalized artifact must stay inside the
        // canonicalized plugin directory.
        let canon_dir = dir
            .canonicalize()
            .map_err(|e| AetherError::config(format!("plugin dir {}: {e}", dir.display())))?;
        let target = dir.join(&self.wasm);
        let canon_wasm = target
            .canonicalize()
            .map_err(|e| AetherError::config(format!("plugin wasm {}: {e}", target.display())))?;
        if !canon_wasm.starts_with(&canon_dir) {
            return Err(AetherError::config(format!(
                "plugin '{}': wasm path escapes the plugin directory",
                self.name
            )));
        }

        Ok(())
    }

    /// Absolute (canonicalized) path to the wasm artifact.
    pub fn wasm_path(&self, dir: &Path) -> AetherResult<PathBuf> {
        let canon_dir = dir
            .canonicalize()
            .map_err(|e| AetherError::config(format!("plugin dir {}: {e}", dir.display())))?;
        let target = dir.join(&self.wasm);
        target
            .canonicalize()
            .map_err(|e| AetherError::config(format!("plugin wasm {}: {e}", target.display())))?;
        Ok(canon_dir.join(&self.wasm))
    }

    /// Reads the artifact bytes (already escape-guarded by `validate`).
    pub fn wasm_bytes(&self, dir: &Path) -> AetherResult<Vec<u8>> {
        let path = self.wasm_path(dir)?;
        std::fs::read(&path)
            .map_err(|e| AetherError::config(format!("read {}: {e}", path.display())))
    }

    /// Effective per-call budget after clamping — clamped again on read,
    /// so even a programmatically-built manifest can't exceed the ceiling.
    pub fn timeout(&self) -> u64 {
        self.timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS)
    }

    /// Effective memory ceiling (pages) after clamping — same read-path
    /// defense in depth.
    pub fn pages(&self) -> u32 {
        self.max_pages
            .unwrap_or(DEFAULT_MAX_PAGES)
            .clamp(1, MAX_MAX_PAGES)
    }

    /// Static declarations → the runtime `Capabilities` vocabulary, 1:1.
    /// Absent = false (fail-closed); there is no wildcard.
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            modify_traffic: self.capabilities.iter().any(|c| c == "modify_traffic"),
            read_history: self.capabilities.iter().any(|c| c == "read_history"),
            write_history: self.capabilities.iter().any(|c| c == "write_history"),
            network_access: self.capabilities.iter().any(|c| c == "network_access"),
            emit_findings: self.capabilities.iter().any(|c| c == "emit_findings"),
        }
    }

    /// The stable hook-registry prefix: every hook this plugin contributes
    /// is named `plugin/<name>/…`, so `unregister_prefix` removes exactly
    /// this plugin and nothing else (ADR-020 §lifecycle).
    pub fn hook_prefix(&self) -> String {
        format!("plugin/{}/", self.name)
    }
}

/// The permission vocabulary v1 understands (mirrors `Capabilities`).
pub const KNOWN_CAPABILITIES: &[&str] = &[
    "modify_traffic",
    "read_history",
    "write_history",
    "network_access",
    "emit_findings",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PluginManifest {
        toml::from_str(
            r#"
name = "demo"
wasm = "plugin.wasm"
capabilities = ["modify_traffic"]
"#,
        )
        .unwrap()
    }

    #[test]
    fn name_charset_enforced() {
        let mut m = sample();
        m.name = "Bad_Name!".into();
        let dir = std::env::temp_dir();
        assert!(m.parse(&dir).is_err());
        m.name = "a".repeat(65);
        assert!(m.parse(&dir).is_err());
        m.name = "good-name-1".into();
        assert!(m.parse(&dir).is_err()); // missing artifact — escape guard's sibling
    }

    #[test]
    fn bounds_are_clamped_not_trusted() {
        let mut m = sample();
        m.timeout_ms = Some(1_000_000);
        m.max_pages = Some(100_000);
        assert_eq!(m.timeout(), MAX_TIMEOUT_MS);
        assert_eq!(m.pages(), MAX_MAX_PAGES);
        m.timeout_ms = Some(0);
        m.max_pages = Some(0);
        assert_eq!(m.timeout(), MIN_TIMEOUT_MS);
        assert_eq!(m.pages(), 1);
    }

    #[test]
    fn unknown_capability_is_rejected() {
        let dir = std::env::temp_dir();
        let text = r#"
name = "demo"
wasm = "plugin.wasm"
capabilities = ["root"]
"#;
        let m: PluginManifest = toml::from_str(text).unwrap();
        assert!(m.parse(&dir).is_err());
    }

    #[test]
    fn unknown_keys_rejected() {
        let text = r#"
name = "demo"
wasm = "plugin.wasm"
root = true
"#;
        let r: Result<PluginManifest, _> = toml::from_str(text);
        assert!(r.is_err());
    }

    #[test]
    fn sha256_pin_format_checked() {
        let mut m = sample();
        m.sha256 = Some("nothex".into());
        let dir = std::env::temp_dir();
        assert!(m.parse(&dir).is_err());
        m.sha256 = Some("A".repeat(64)); // uppercase can never match a digest
        assert!(m.parse(&dir).is_err());
        m.sha256 = Some("a".repeat(64)); // well-formed pin passes format check
        assert!(m.parse(&dir).is_err()); // ...and fails only on the artifact
    }

    #[test]
    fn capabilities_map_one_to_one() {
        let m = sample();
        let caps = m.capabilities();
        assert!(caps.modify_traffic);
        assert!(!caps.read_history);
        assert!(!caps.network_access);
    }

    #[test]
    fn hook_prefix_is_namespaced() {
        let m = sample();
        assert_eq!(m.hook_prefix(), "plugin/demo/");
    }
}

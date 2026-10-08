//! Aether daemon configuration (TOML).
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).

use aether_core::error::{AetherError, AetherResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AetherConfig {
    pub proxy: ProxySection,
    pub control: ControlSection,
    pub storage: StorageSection,
    pub intercept: InterceptSection,
    /// Host globs tunneled blindly (ADR-004 pass-through list).
    pub passthrough: Vec<String>,
    /// `"host"`/`"host:port"` → `"socket://host:port"` dial overrides.
    pub host_overrides: BTreeMap<String, String>,
    /// Validate upstream certificates (turn off only for labs).
    pub upstream_tls_verify: bool,
    /// Per-body capture limit in bytes (ADR-011).
    pub capture_max_body: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxySection {
    /// Explicit-proxy listen address (ADR-007).
    pub listen: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlSection {
    pub listen: String,
    /// Fixed token; generated per-run when absent.
    pub token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageSection {
    /// `"sqlite"` (default) or `"memory"`.
    pub backend: String,
    /// SQLite path; defaults to `<data_dir>/aether.db`.
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterceptSection {
    pub request: bool,
    pub response: bool,
}

impl Default for AetherConfig {
    fn default() -> Self {
        Self {
            proxy: ProxySection {
                listen: "127.0.0.1:8888".into(),
            },
            control: ControlSection {
                listen: "127.0.0.1:9099".into(),
                token: None,
            },
            storage: StorageSection {
                backend: "sqlite".into(),
                path: None,
            },
            intercept: InterceptSection {
                request: false,
                response: false,
            },
            passthrough: Vec::new(),
            host_overrides: BTreeMap::new(),
            upstream_tls_verify: true,
            capture_max_body: 8 * 1024 * 1024,
        }
    }
}

impl AetherConfig {
    pub fn load(path: &std::path::Path) -> AetherResult<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| AetherError::config(format!("read {}: {e}", path.display())))?;
        toml::from_str(&text)
            .map_err(|e| AetherError::config(format!("parse {}: {e}", path.display())))
    }

    pub fn validate(&self) -> AetherResult<()> {
        use std::net::ToSocketAddrs as _;
        self.proxy
            .listen
            .to_socket_addrs()
            .map_err(|e| AetherError::config(format!("proxy.listen: {e}")))?
            .next()
            .ok_or_else(|| AetherError::config("proxy.listen did not resolve"))?;
        self.control
            .listen
            .to_socket_addrs()
            .map_err(|e| AetherError::config(format!("control.listen: {e}")))?
            .next()
            .ok_or_else(|| AetherError::config("control.listen did not resolve"))?;
        match self.storage.backend.as_str() {
            "sqlite" | "memory" => {}
            other => {
                return Err(AetherError::config(format!(
                    "unknown storage backend '{other}'"
                )));
            }
        }
        if self.capture_max_body == 0 {
            return Err(AetherError::config("capture_max_body must be > 0"));
        }
        Ok(())
    }
}

/// `~/.aether` by default.
pub fn default_data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".aether")
}

//! Unified error model for Aether. One error type crosses every boundary —
//! domain, hooks, pipeline — and will later cross the control plane and the
//! WASM plugin boundary, which is why it is `Clone + Serialize`-shaped (stringly
//! payloads only).

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;

pub type AetherResult<T> = Result<T, AetherError>;

#[derive(Debug, Error, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum AetherError {
    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("hook '{hook}' failed: {message}")]
    Hook { hook: String, message: String },

    #[error(
        "hook '{hook}' timed out after {timeout_ms}ms (ADR-006 rule #5: hooks have a timeout budget)"
    )]
    HookTimeout { hook: String, timeout_ms: u64 },

    #[error("pipeline aborted by hook '{hook}': {reason}")]
    Aborted { hook: String, reason: String },

    #[error("unsupported operation: {0}")]
    Unsupported(String),

    // Phase 1+ carriers — the unified model starts now so later subsystems
    // (network, storage) never invent their own incompatible error types.
    #[error("network error: {0}")]
    Network(String),

    #[error("tls error: {0}")]
    Tls(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("storage error: {0}")]
    Storage(String),
}

impl AetherError {
    pub fn invalid_input(msg: impl Into<String>) -> Self {
        Self::InvalidInput(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    pub fn network(msg: impl Into<String>) -> Self {
        Self::Network(msg.into())
    }

    pub fn tls(msg: impl Into<String>) -> Self {
        Self::Tls(msg.into())
    }

    pub fn protocol(msg: impl Into<String>) -> Self {
        Self::Protocol(msg.into())
    }

    pub fn storage(msg: impl Into<String>) -> Self {
        Self::Storage(msg.into())
    }

    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    /// Stable machine-readable kind name (control-plane error shape, ADR-010).
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "invalid_input",
            Self::Config(_) => "config",
            Self::Serialization(_) => "serialization",
            Self::Hook { .. } => "hook",
            Self::HookTimeout { .. } => "hook_timeout",
            Self::Aborted { .. } => "aborted",
            Self::Unsupported(_) => "unsupported",
            Self::Network(_) => "network",
            Self::Tls(_) => "tls",
            Self::Protocol(_) => "protocol",
            Self::Storage(_) => "storage",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_stable_and_informative() {
        let e = AetherError::HookTimeout {
            hook: "tagger".into(),
            timeout_ms: 500,
        };
        let msg = e.to_string();
        assert!(msg.contains("tagger") && msg.contains("500ms"));
    }

    #[test]
    fn roundtrips_through_json() {
        let e = AetherError::Hook {
            hook: "h".into(),
            message: "boom".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<AetherError>(&json).unwrap(), e);
    }
}

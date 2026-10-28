//! Command implementations.
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).

pub mod bench;
pub mod ca;
pub mod demo;
pub mod flows;
pub mod intercept;
pub(crate) mod lab;
pub mod mcp;
pub mod rules;
pub mod serve;
pub mod soak;

use crate::client::ControlClient;
use aether_core::error::AetherResult;
use serde_json::Value;

/// Opens a control connection (helper shared by all remote commands).
pub(crate) async fn connect(
    control: Option<&String>,
    token: Option<&String>,
) -> AetherResult<ControlClient> {
    let conn = crate::Conn {
        control: control.cloned(),
        token: token.cloned(),
    };
    crate::connect(&conn).await
}

pub(crate) fn print_json(v: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
    );
}

/// `POST /x?y=1` — one summary line for flow tables.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

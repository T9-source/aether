//! `aether rules` — match & replace over the control plane.
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).

use super::{connect, print_json, truncate};
use aether_core::error::{AetherError, AetherResult};
use aether_daemon::ControlRequest;
use serde_json::Value;
use std::path::PathBuf;

pub async fn ls(json: bool, control: Option<String>, token: Option<String>) -> AetherResult<()> {
    let mut client = connect(control.as_ref(), token.as_ref()).await?;
    let result = client.request(ControlRequest::RulesList).await?;
    if json {
        print_json(&result);
        return Ok(());
    }
    let rows = result.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("no rules. add one: aether rules add --file rule.json");
        return Ok(());
    }
    println!(
        "{:>4}  {:<7} {:<20} {:<9} ACTION",
        "ID", "STAGE", "NAME", "ENABLED"
    );
    for row in &rows {
        let action = row.get("action").cloned().unwrap_or(Value::Null);
        let kind = action.get("kind").and_then(Value::as_str).unwrap_or("?");
        println!(
            "{:>4}  {:<7} {:<20} {:<9} {}",
            row.get("id").and_then(Value::as_u64).unwrap_or(0),
            row.get("stage")
                .and_then(Value::as_str)
                .unwrap_or("request"),
            truncate(row.get("name").and_then(Value::as_str).unwrap_or("?"), 20),
            row.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            kind
        );
    }
    Ok(())
}

pub async fn add(
    file: PathBuf,
    control: Option<String>,
    token: Option<String>,
) -> AetherResult<()> {
    let text = if file.as_os_str() == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| AetherError::config(format!("stdin: {e}")))?;
        buf
    } else {
        std::fs::read_to_string(&file)
            .map_err(|e| AetherError::config(format!("read {}: {e}", file.display())))?
    };
    let spec: Value =
        serde_json::from_str(&text).map_err(|e| AetherError::config(format!("rule json: {e}")))?;
    let mut client = connect(control.as_ref(), token.as_ref()).await?;
    let result = client
        .request(ControlRequest::RulesAdd {
            spec: serde_json::from_value(spec)
                .map_err(|e| AetherError::config(format!("rule shape: {e}")))?,
        })
        .await?;
    println!(
        "added rule #{}",
        result.get("id").and_then(Value::as_u64).unwrap_or(0)
    );
    Ok(())
}

pub async fn rm(id: u64, control: Option<String>, token: Option<String>) -> AetherResult<()> {
    let mut client = connect(control.as_ref(), token.as_ref()).await?;
    let result = client.request(ControlRequest::RulesRemove { id }).await?;
    if result.get("removed").and_then(Value::as_bool) == Some(true) {
        println!("rule {id} removed");
    } else {
        println!("no rule {id}");
    }
    Ok(())
}

pub async fn set_enabled(
    id: u64,
    enabled: bool,
    control: Option<String>,
    token: Option<String>,
) -> AetherResult<()> {
    let mut client = connect(control.as_ref(), token.as_ref()).await?;
    let result = client
        .request(ControlRequest::RulesSetEnabled { id, enabled })
        .await?;
    if result.get("changed").and_then(Value::as_bool) == Some(true) {
        println!("rule {id} {}", if enabled { "enabled" } else { "disabled" });
    } else {
        println!("no rule {id}");
    }
    Ok(())
}

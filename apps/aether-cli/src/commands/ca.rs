//! `aether ca` — the MITM root certificate (ADR-004).
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).

use super::{connect, print_json};
use aether_core::error::AetherResult;
use aether_daemon::ControlRequest;
use serde_json::Value;
use std::path::PathBuf;

pub async fn export(
    out: Option<PathBuf>,
    control: Option<String>,
    token: Option<String>,
) -> AetherResult<()> {
    let mut client = connect(control.as_ref(), token.as_ref()).await?;
    let result = client.request(ControlRequest::CaExportPem).await?;
    let pem = result
        .get("pem")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match out {
        Some(path) => {
            std::fs::write(&path, pem).map_err(|e| {
                aether_core::AetherError::storage(format!("write {}: {e}", path.display()))
            })?;
            println!("CA written to {}", path.display());
            println!(
                "Install it in your browser/OS trust store, then MITM works with zero warnings."
            );
        }
        None => print!("{pem}"),
    }
    Ok(())
}

pub fn path(data_dir: Option<PathBuf>) -> AetherResult<()> {
    let dd = crate::data_dir(data_dir.as_ref());
    let mut v = serde_json::Map::new();
    v.insert("data_dir".into(), Value::String(dd.display().to_string()));
    v.insert(
        "ca_dir".into(),
        Value::String(dd.join("ca").display().to_string()),
    );
    v.insert(
        "ca_cert".into(),
        Value::String(dd.join("ca").join("ca.cert.pem").display().to_string()),
    );
    v.insert(
        "ca_key".into(),
        Value::String(dd.join("ca").join("ca.key.pem").display().to_string()),
    );
    print_json(&Value::Object(v));
    Ok(())
}

//! `aether intercept` — manual interception (the Milestone-0 gate).
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).

use super::{connect, print_json};
use aether_core::error::{AetherError, AetherResult};
use aether_daemon::{ControlRequest, DecisionDto, HeldPayloadDto};
use serde_json::Value;
use std::path::PathBuf;

pub async fn ls(json: bool, control: Option<String>, token: Option<String>) -> AetherResult<()> {
    let mut client = connect(control.as_ref(), token.as_ref()).await?;
    let result = client.request(ControlRequest::InterceptList).await?;
    if json {
        print_json(&result);
        return Ok(());
    }
    let rows = result.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("nothing held. (turn on with `aether intercept set --request on`)");
        return Ok(());
    }
    println!(
        "{:>6}  {:<9} {:<7} {:<24} PATH/STATUS",
        "ID", "DIR", "METHOD", "HOST"
    );
    for row in &rows {
        println!(
            "{:>6}  {:<9} {:<7} {:<24} {}",
            row.get("flow_id").and_then(Value::as_u64).unwrap_or(0),
            row.get("direction").and_then(Value::as_str).unwrap_or("?"),
            row.get("method").and_then(Value::as_str).unwrap_or("—"),
            row.get("host").and_then(Value::as_str).unwrap_or("—"),
            row.get("path")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| row
                    .get("status")
                    .and_then(Value::as_u64)
                    .map(|s| format!("HTTP {s}")))
                .unwrap_or_else(|| "—".into())
        );
    }
    Ok(())
}

pub async fn show(id: u64, control: Option<String>, token: Option<String>) -> AetherResult<()> {
    let mut client = connect(control.as_ref(), token.as_ref()).await?;
    let result = client.request(ControlRequest::InterceptShow { id }).await?;
    print_json(&result);
    println!();
    println!("edit and apply: aether intercept release {id} --from-file <file.json>");
    println!("quick edits   : aether intercept release {id} --header \"X: y\"");
    Ok(())
}

pub async fn drop(id: u64, control: Option<String>, token: Option<String>) -> AetherResult<()> {
    let mut client = connect(control.as_ref(), token.as_ref()).await?;
    let result = client.request(ControlRequest::InterceptDrop { id }).await?;
    if result.get("removed").and_then(Value::as_bool) == Some(true) {
        println!("flow {id} dropped (client leg gets a 502).");
    } else {
        println!("no held flow {id}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // mirrors the CLI flag set 1:1
pub async fn release(
    id: u64,
    from_file: Option<PathBuf>,
    headers: Vec<String>,
    remove_headers: Vec<String>,
    path: Option<String>,
    body_file: Option<PathBuf>,
    control: Option<String>,
    token: Option<String>,
) -> AetherResult<()> {
    let mut client = connect(control.as_ref(), token.as_ref()).await?;

    // Start from the daemon-held payload (or a full edited file).
    let payload: Value = if let Some(file) = &from_file {
        let text = std::fs::read_to_string(file)
            .map_err(|e| AetherError::config(format!("read {}: {e}", file.display())))?;
        serde_json::from_str(&text)
            .map_err(|e| AetherError::config(format!("payload json: {e}")))?
    } else {
        client.request(ControlRequest::InterceptShow { id }).await?
    };

    // Quick edits apply to request payloads only (V0.1 contract).
    let payload = if !headers.is_empty()
        || !remove_headers.is_empty()
        || path.is_some()
        || body_file.is_some()
    {
        apply_request_edits(
            payload,
            &headers,
            &remove_headers,
            path.as_deref(),
            body_file.as_deref(),
        )?
    } else {
        payload
    };

    let dto_payload: HeldPayloadDto = serde_json::from_value(payload)
        .map_err(|e| AetherError::config(format!("payload shape: {e}")))?;
    let decision = DecisionDto::Forward {
        payload: dto_payload,
    };
    let result = client
        .request(ControlRequest::InterceptRelease { id, decision })
        .await?;
    if result.get("removed").and_then(Value::as_bool) == Some(true) {
        println!("flow {id} released → forwarded upstream.");
    } else {
        println!("no held flow {id}");
    }
    Ok(())
}

pub async fn set(
    request: Option<bool>,
    response: Option<bool>,
    control: Option<String>,
    token: Option<String>,
) -> AetherResult<()> {
    let mut client = connect(control.as_ref(), token.as_ref()).await?;
    let result = client
        .request(ControlRequest::InterceptSet { request, response })
        .await?;
    println!(
        "intercept: request={} response={}",
        result
            .get("request")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        result
            .get("response")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    Ok(())
}

fn apply_request_edits(
    mut payload: Value,
    set_headers: &[String],
    remove_headers: &[String],
    path: Option<&str>,
    body_file: Option<&std::path::Path>,
) -> AetherResult<Value> {
    if payload.get("request").is_none() {
        return Err(AetherError::config(
            "quick edits apply to held REQUESTS only; use --from-file for full control",
        ));
    }
    let req = payload.get_mut("request").unwrap();
    if let Some(p) = path {
        let url = req
            .get_mut("url")
            .ok_or_else(|| AetherError::config("payload missing url"))?;
        let (new_path, new_query) = match p.split_once('?') {
            Some((a, b)) => (a, Some(b.to_string())),
            None => (p, None),
        };
        url["path"] = Value::String(new_path.to_string());
        url["query"] = new_query.map(Value::String).unwrap_or(Value::Null);
    }
    if let Some(headers) = req.get_mut("headers").and_then(Value::as_array_mut) {
        for spec in set_headers {
            let (name, value) = spec.split_once(':').ok_or_else(|| {
                AetherError::config(format!("--header expects \"Name: value\", got {spec:?}"))
            })?;
            let name = name.trim();
            let value = value.trim();
            headers.retain(|h| {
                !(h.get(0)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .eq_ignore_ascii_case(name))
            });
            headers.push(Value::Array(vec![
                Value::String(name.to_string()),
                Value::String(value.to_string()),
            ]));
        }
        for name in remove_headers {
            headers.retain(|h| {
                !(h.get(0)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .eq_ignore_ascii_case(name.trim()))
            });
        }
    }
    if let Some(file) = body_file {
        let text = std::fs::read_to_string(file)
            .map_err(|e| AetherError::config(format!("read {}: {e}", file.display())))?;
        req["body"] = serde_json::json!({"kind": "text", "text": text});
    }
    Ok(payload)
}

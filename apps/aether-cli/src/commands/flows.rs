//! `aether flows` — history inspection.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).

use super::{connect, print_json, truncate};
use aether_core::error::AetherResult;
use aether_daemon::ControlRequest;
use serde_json::Value;

pub async fn list(
    limit: u32,
    host: Option<String>,
    method: Option<String>,
    before: Option<u64>,
    json: bool,
    control: Option<String>,
    token: Option<String>,
) -> AetherResult<()> {
    let mut client = connect(control.as_ref(), token.as_ref()).await?;
    let result = client
        .request(ControlRequest::FlowsList {
            limit: Some(limit),
            before,
            after: None,
            host,
            method,
        })
        .await?;
    if json {
        print_json(&result);
        return Ok(());
    }
    let items = result
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    println!(
        "{:>6}  {:<7} {:>4}  {:<28} {:<34} TAGS",
        "ID", "METHOD", "CODE", "HOST", "PATH"
    );
    for item in &items {
        let id = item.get("id").and_then(Value::as_u64).unwrap_or(0);
        let m = item.get("method").and_then(Value::as_str).unwrap_or("?");
        let status = item
            .get("status")
            .and_then(Value::as_u64)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "—".into());
        let host = item.get("host").and_then(Value::as_str).unwrap_or("?");
        let path = item.get("path").and_then(Value::as_str).unwrap_or("?");
        let tags: Vec<String> = item
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        println!(
            "{:>6}  {:<7} {:>4}  {:<28} {:<34} {}",
            id,
            truncate(m, 7),
            truncate(&status, 4),
            truncate(host, 28),
            truncate(path, 34),
            tags.join(",")
        );
    }
    if let Some(next) = result.get("next_cursor").and_then(Value::as_u64) {
        println!("… older: `aether flows list --before {next}`");
    }
    Ok(())
}

pub async fn show(
    id: u64,
    json: bool,
    control: Option<String>,
    token: Option<String>,
) -> AetherResult<()> {
    let mut client = connect(control.as_ref(), token.as_ref()).await?;
    let result = client.request(ControlRequest::FlowsGet { id }).await?;
    if result.is_null() {
        println!("no flow {id}");
        return Ok(());
    }
    if json {
        print_json(&result);
        return Ok(());
    }
    let req = result.get("request");
    let res = result.get("response");
    if let Some(r) = req {
        println!(
            "{} {}",
            r.get("method").and_then(Value::as_str).unwrap_or("?"),
            r.get("url")
                .map(|u| u.get("scheme").and_then(Value::as_str).unwrap_or(""))
                .unwrap_or("")
        );
        let url = r.get("url").cloned().unwrap_or(Value::Null);
        println!(
            "  {}:{}{}",
            url.get("host").and_then(Value::as_str).unwrap_or("?"),
            url.get("port")
                .and_then(Value::as_u64)
                .map(|p| p.to_string())
                .unwrap_or_default(),
            url.get("path").and_then(Value::as_str).unwrap_or("?")
        );
        print_headers(r.get("headers"));
        print_body(r.get("body"));
    }
    if let Some(r) = res {
        println!("──────────────────────────────────────────────");
        println!(
            "status: {}",
            r.get("status")
                .map(|s| s.get("code").and_then(Value::as_u64).unwrap_or(0))
                .unwrap_or(0)
        );
        print_headers(r.get("headers"));
        print_body(r.get("body"));
    }
    Ok(())
}

pub async fn search(
    query: String,
    limit: u32,
    json: bool,
    control: Option<String>,
    token: Option<String>,
) -> AetherResult<()> {
    let mut client = connect(control.as_ref(), token.as_ref()).await?;
    let result = client
        .request(ControlRequest::FlowsSearch {
            query: query.clone(),
            limit: Some(limit),
        })
        .await?;
    if json {
        print_json(&result);
        return Ok(());
    }
    let items = result.as_array().cloned().unwrap_or_default();
    println!("{} hits for {query:?}:", items.len());
    for item in &items {
        println!(
            "  {:>6}  {:<7} {:<28} {}",
            item.get("id").and_then(Value::as_u64).unwrap_or(0),
            item.get("method").and_then(Value::as_str).unwrap_or("?"),
            item.get("host").and_then(Value::as_str).unwrap_or("?"),
            item.get("path").and_then(Value::as_str).unwrap_or("?")
        );
    }
    Ok(())
}

fn print_headers(headers: Option<&Value>) {
    if let Some(Value::Array(headers)) = headers {
        for h in headers {
            if let (Some(n), Some(v)) = (h.get(0), h.get(1)) {
                println!(
                    "  {}: {}",
                    n.as_str().unwrap_or("?"),
                    v.as_str().unwrap_or("?")
                );
            }
        }
    }
}

fn print_body(body: Option<&Value>) {
    let Some(body) = body else { return };
    match body.get("kind").and_then(Value::as_str) {
        Some("empty") => println!("  (empty body)"),
        Some("text") => {
            if let Some(t) = body.get("text").and_then(Value::as_str) {
                println!("  ── body ──\n{}", truncate(t, 2000));
            }
        }
        Some("binary") => println!("  (binary body, base64 in JSON output)"),
        _ => {}
    }
}

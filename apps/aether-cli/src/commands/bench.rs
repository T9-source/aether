//! `aether bench flows` — the Milestone-0 scale gate.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Inserts N synthetic flows into SQLite (the exact storage path the daemon
//! uses), then times keyset pagination and search. The historical target:
//! 100k flows with page-of-50 reads staying in single-digit milliseconds.

use aether_core::error::{AetherError, AetherResult};
use aether_core::model::{Body, Flow, FlowId, Method, Request, Response, Url};
use aether_core::storage::{FlowQuery, Storage};
use aether_store_sqlite::SqliteStore;
use std::path::PathBuf;
use std::time::Instant;

pub async fn flows(n: u64, db: Option<PathBuf>, keep: bool) -> AetherResult<()> {
    let (_dir_guard, path): (Option<tempfile::TempDir>, PathBuf) = match &db {
        Some(p) => (None, p.clone()),
        None => {
            let dir = tempfile::TempDir::new()
                .map_err(|e| AetherError::storage(format!("tempdir: {e}")))?;
            let p = dir.path().join("bench.db");
            (Some(dir), p)
        }
    };

    let store = SqliteStore::open(&path).await?;
    println!("Aether storage bench — {n} flows into {}", path.display());
    println!("  backend: sqlx/sqlite (FTS5 probed at startup, ADR-005)");

    let hosts = [
        "api.example.com",
        "web.example.com",
        "cdn.example.net",
        "auth.example.org",
    ];
    let methods = [Method::Get, Method::Post, Method::Put, Method::Delete];
    let t0 = Instant::now();
    const BATCH: u64 = 1000;
    let mut batch: Vec<Flow> = Vec::with_capacity(BATCH as usize);
    for i in 1..=n {
        let id: FlowId = i;
        let host = hosts[(i as usize) % hosts.len()];
        let method = &methods[(i as usize) % methods.len()];
        let mut flow = Flow::new(
            id,
            Request::new(
                method.clone(),
                Url::parse(&format!("https://{host}/v1/items/{i}?page=1&q=bench")).unwrap(),
            ),
        );
        flow.request
            .as_mut()
            .unwrap()
            .set_header("user-agent", "aether-bench/0.1");
        flow.request.as_mut().unwrap().body =
            Body::text(format!("{{\"item\": {i}, \"payload\": \"synthetic\"}}"));
        let mut res = Response::new((200 + (i % 4) * 100 % 300) as u16)
            .unwrap_or_else(|_| Response::new(200).unwrap());
        res.body = Body::text(format!("{{\"ok\": true, \"echo\": {i}}}"));
        flow.response = Some(res);
        flow.meta.duration_ms = Some(5 + i % 40);
        if i % 97 == 0 {
            flow.add_tag("interesting");
        }
        batch.push(flow);
        if batch.len() as u64 == BATCH || i == n {
            store.put_flows(&batch).await?;
            batch.clear();
        }
        if i % 10_000 == 0 {
            let rate = i as f64 / t0.elapsed().as_secs_f64();
            println!("  … {i} inserted ({rate:.0}/s)");
        }
    }
    let insert_secs = t0.elapsed().as_secs_f64();
    println!(
        "insert: {:.2}s ({:.0} flows/s)",
        insert_secs,
        n as f64 / insert_secs
    );

    // ── keyset pagination timing (page of 50, 20 pages deep) ────────────
    let mut cursor = None;
    let mut timings = Vec::new();
    for _ in 0..20 {
        let t = Instant::now();
        let page = store
            .list_flows(FlowQuery {
                before: cursor,
                limit: 50,
                ..Default::default()
            })
            .await?;
        let d = t.elapsed();
        timings.push(d.as_secs_f64() * 1000.0);
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    timings.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = timings[timings.len() / 2];
    let p95 = timings[(timings.len() as f32 * 0.95) as usize % timings.len()];
    println!(
        "list_flows page(50): p50={p50:.2}ms p95={p95:.2}ms over {} pages",
        timings.len()
    );

    // ── search timing ───────────────────────────────────────────────────
    let t = Instant::now();
    let hits = store.search_flows("synthetic", 50).await?;
    let search_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!(
        "search(\"synthetic\"): {} hits in {search_ms:.2}ms",
        hits.len()
    );

    let stats = store.flow_stats().await?;
    println!(
        "stats: total={} completed={} dropped={}",
        stats.total, stats.completed, stats.dropped
    );

    // The tempdir guard (when present) cleans the database up at scope end;
    // with --db/--keep the file stays for inspection.
    let _ = keep;
    println!("done.");
    Ok(())
}

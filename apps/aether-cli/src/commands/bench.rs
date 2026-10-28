//! `aether bench flows` — the Milestone-0 scale gate.
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).
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

use super::lab::{self, LabOrigin, LabProxy, Path};

// ---------------------------------------------------------------------------
// `aether bench proxy` — the Milestone-0 latency gate (criterion 11).
//
// Measures *added* latency of every proxy path against a local origin:
//   direct (TLS)      → the floor: TCP + TLS + h1 exchange
//   via proxy (plain) → + one proxy hop
//   via proxy (MITM)  → + CONNECT, two TLS legs, the full engine path
// "Measurement before optimization" (MILESTONE-0): no target at M0 — these
// numbers are the baseline the V0.1 exit criteria refer to.
// ---------------------------------------------------------------------------

enum BenchPath {
    Direct,
    ProxyPlain,
    ProxyMitm,
}

async fn measure(
    proxy: &LabProxy,
    origin: &LabOrigin,
    plain_origin: Option<std::net::SocketAddr>,
    via: BenchPath,
    warmup: u32,
    n: u32,
) -> AetherResult<Vec<f64>> {
    let mut samples = Vec::with_capacity(n as usize);
    for i in 0..warmup + n {
        let d = match via {
            BenchPath::Direct => lab::timed_request(proxy, origin, &Path::Direct).await?,
            BenchPath::ProxyPlain => {
                lab::timed_plain(proxy, plain_origin.expect("plain origin")).await?
            }
            BenchPath::ProxyMitm => lab::timed_request(proxy, origin, &Path::ProxyMitm).await?,
        };
        if i >= warmup {
            samples.push(d.as_secs_f64() * 1000.0);
        }
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Ok(samples)
}

fn report(name: &str, samples: &[f64]) {
    let mean = samples.iter().sum::<f64>() / samples.len().max(1) as f64;
    println!(
        "  {name:<22} p50={:>7.2}ms  p95={:>7.2}ms  p99={:>7.2}ms  mean={:>7.2}ms  (n={})",
        lab::percentile(samples, 0.50),
        lab::percentile(samples, 0.95),
        lab::percentile(samples, 0.99),
        mean,
        samples.len()
    );
}

pub async fn proxy(requests: u32, json: Option<PathBuf>) -> AetherResult<()> {
    println!("Aether proxy latency bench — {requests} requests per path");
    println!("  origin : local TLS echo (self-signed), fresh connection per request");
    println!("  proxy  : embedded engine — real pipeline + SQLite storage (WAL)");

    let origin = lab::spawn_tls_origin("bench.local").await?;
    let plain_origin = lab::spawn_plain_origin().await?;
    let lab_proxy = lab::spawn_proxy(None).await?;
    let warmup = (requests / 10).clamp(5, 50);

    let direct = measure(
        &lab_proxy,
        &origin,
        None,
        BenchPath::Direct,
        warmup,
        requests,
    )
    .await?;
    let plain = measure(
        &lab_proxy,
        &origin,
        Some(plain_origin),
        BenchPath::ProxyPlain,
        warmup,
        requests,
    )
    .await?;
    let mitm = measure(
        &lab_proxy,
        &origin,
        None,
        BenchPath::ProxyMitm,
        warmup,
        requests,
    )
    .await?;

    println!("\nresults (lower is better):");
    report("direct (TLS)", &direct);
    report("via proxy (plain)", &plain);
    report("via proxy (MITM)", &mitm);

    let p50 = |v: &[f64]| lab::percentile(v, 0.50);
    println!("\nadded overhead vs direct (p50):");
    println!("  plain proxy hop : +{:.2}ms", p50(&plain) - p50(&direct));
    println!(
        "  MITM tunnel     : +{:.2}ms (includes a 2nd TLS handshake per request)",
        p50(&mitm) - p50(&direct)
    );

    // ── machine-readable results (ADR-018 item 5: perf regression gate) ────
    let stats = |name: &str, v: &[f64]| {
        serde_json::json!({
            "p50_ms": (p50(v) * 1000.0).round() / 1000.0,
            "p95_ms": (lab::percentile(v, 0.95) * 1000.0).round() / 1000.0,
            "p99_ms": (lab::percentile(v, 0.99) * 1000.0).round() / 1000.0,
            "mean_ms": ((v.iter().sum::<f64>() / v.len().max(1) as f64) * 1000.0).round() / 1000.0,
            "n": v.len(),
            "_name": name,
        })
    };
    let results = serde_json::json!({
        "schema": "aether-bench-proxy/1",
        "requests_per_path": requests,
        "direct_tls": stats("direct_tls", &direct),
        "via_proxy_plain": stats("via_proxy_plain", &plain),
        "via_proxy_mitm": stats("via_proxy_mitm", &mitm),
        "overhead_p50_ms": {
            "plain": ((p50(&plain) - p50(&direct)) * 1000.0).round() / 1000.0,
            "mitm": ((p50(&mitm) - p50(&direct)) * 1000.0).round() / 1000.0,
        },
    });
    println!("BENCH_JSON: {results}");
    if let Some(path) = &json {
        std::fs::write(
            path,
            serde_json::to_string_pretty(&results).map_err(|e| {
                aether_core::error::AetherError::storage(format!("serialize bench json: {e}"))
            })?,
        )
        .map_err(|e| {
            aether_core::error::AetherError::storage(format!("write {}: {e}", path.display()))
        })?;
        println!("baseline written to {}", path.display());
    }

    println!("\nbaseline recorded for MILESTONE-0 criterion 11 (measurement before optimization).");
    Ok(())
}

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

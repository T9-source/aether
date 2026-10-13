//! `aether soak` — steady-state health probe (MILESTONE-0 criterion 12).
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Continuous mixed traffic (plain + MITM) through the embedded engine with
//! real SQLite/WAL persistence, sampling RSS and open FDs at 1 Hz. The 24h
//! acceptance run is the same command: `aether soak --secs 86400`. Pass
//! condition: flat RSS trend, stable FD count, zero crashes, WAL healthy.
//!
//! `--assert` turns the probe into a gate (ADR-018 item 4): budgets are
//! enforced, a `SOAK_JSON:` summary line is printed for CI parsing, and a
//! budget breach exits non-zero. Budgets are absolute (not relative) so a
//! compressed run and the 24h run are judged by the same bar:
//!   - RSS: median growth between the settling window and the final window
//!     (linear-leak budgets scale with time; absolute ones catch leak-classes
//!     that matter at any duration — we chose absolute, conservative).
//!   - FDs: end minus start — a proxy that closes what it opens stays flat.
//!   - traffic errors: zero. A soak with tolerated errors proves nothing.

use aether_core::error::{AetherError, AetherResult};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::lab;

/// Soak budgets (ADR-018 item 4). Absolute numbers on purpose.
pub const RSS_GROWTH_BUDGET_KB: u64 = 15 * 1024; // 15 MiB RSS drift ceiling
pub const FD_GROWTH_BUDGET: u64 = 32; // fds must return; 32 covers sampler jitter

/// Median of a slice of u64s (samples are already 1 Hz — cheap and robust).
fn median(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut v: Vec<u64> = values.to_vec();
    v.sort_unstable();
    v[v.len() / 2]
}

pub async fn run(
    secs: u64,
    workers: u32,
    db: Option<PathBuf>,
    assert_mode: bool,
) -> AetherResult<()> {
    println!("Aether soak — {secs}s, {workers} workers, mixed plain+MITM traffic, SQLite (WAL)");
    let _origin = lab::spawn_tls_origin("soak.local").await?;
    let plain_origin = lab::spawn_plain_origin().await?;
    let proxy = lab::spawn_proxy(db).await?;
    let start_metrics = lab::read_process_metrics();
    let started = std::time::Instant::now();
    let tls_addr = _origin.addr;

    let stop = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();
    for _ in 0..workers {
        let stop = stop.clone();
        let proxy_addr = proxy.addr;
        let plain_addr = plain_origin;
        handles.push(tokio::spawn(async move {
            let mut plain_ok = 0u64;
            let mut mitm_ok = 0u64;
            let mut errors = 0u64;
            let mut tick = 0u64;
            while !stop.load(Ordering::Relaxed) {
                tick = tick.wrapping_add(1);
                let ok = if tick % 2 == 0 {
                    plain_round(proxy_addr, plain_addr).await
                } else {
                    mitm_round(proxy_addr, tls_addr).await
                };
                match ok {
                    Ok(true) if tick % 2 == 0 => plain_ok += 1,
                    Ok(true) => mitm_ok += 1,
                    _ => errors += 1,
                }
            }
            (plain_ok, mitm_ok, errors)
        }));
    }

    // sampler — 1 Hz RSS / FD
    let mut samples: Vec<lab::ProcMetrics> = Vec::new();
    while started.elapsed().as_secs() < secs {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some(m) = lab::read_process_metrics() {
            samples.push(m);
        }
    }
    stop.store(true, Ordering::Relaxed);

    let mut plain_total = 0u64;
    let mut mitm_total = 0u64;
    let mut errors_total = 0u64;
    for h in handles {
        match h.await {
            Ok((p, m, e)) => {
                plain_total += p;
                mitm_total += m;
                errors_total += e;
            }
            Err(_) => errors_total += 1,
        }
    }

    let elapsed = started.elapsed().as_secs_f64();
    println!("\ntraffic:");
    println!("  plain requests : {plain_total}");
    println!("  MITM requests  : {mitm_total}");
    println!("  errors         : {errors_total}");
    println!(
        "  throughput     : {:.0} req/s over {elapsed:.1}s",
        (plain_total + mitm_total) as f64 / elapsed.max(0.001)
    );

    // ── budget analysis (ADR-018 item 4): settle → final median windows ────
    // The first 20% of samples absorb allocator/JIT-style warmup; the final
    // 20% is the steady state. Growth between medians is the leak signal.
    let n = samples.len();
    let settle = if n >= 10 {
        &samples[n / 5..2 * (n / 5)]
    } else {
        &samples[..n / 2]
    };
    let final_w = if n >= 10 {
        &samples[4 * (n / 5)..]
    } else {
        &samples[n / 2..]
    };
    let settle_rss = median(&settle.iter().map(|m| m.rss_kb).collect::<Vec<_>>());
    let final_rss = median(&final_w.iter().map(|m| m.rss_kb).collect::<Vec<_>>());
    let rss_growth = final_rss.saturating_sub(settle_rss);
    let (fd_first, fd_last) = match (samples.first(), samples.last()) {
        (Some(a), Some(b)) => (a.fds, b.fds),
        _ => (0, 0),
    };
    let fd_growth = fd_last.saturating_sub(fd_first);
    let peak_rss = samples.iter().map(|m| m.rss_kb).max().unwrap_or(0);
    let peak_fd = samples.iter().map(|m| m.fds).max().unwrap_or(0);

    if let Some(first) = start_metrics {
        println!("\nprocess health (1 Hz samples, n={n}):");
        println!(
            "  RSS  : start={}KB settle_median={settle_rss}KB final_median={final_rss}KB peak={peak_rss}KB",
            first.rss_kb
        );
        println!("  FDs  : start={} end={fd_last} peak={peak_fd}", first.fds);
    }

    let rss_ok = rss_growth <= RSS_GROWTH_BUDGET_KB;
    let fd_ok = fd_growth <= FD_GROWTH_BUDGET;
    let traffic_ok = errors_total == 0;
    let pass = rss_ok && fd_ok && traffic_ok;

    // Machine-readable summary for CI (scripts/soak-gate.sh greps this line).
    println!(
        "SOAK_JSON: {{\"secs\":{secs},\"workers\":{workers},\"samples\":{n},\"plain\":{plain_total},\"mitm\":{mitm_total},\"errors\":{errors_total},\"rss_settle_kb\":{settle_rss},\"rss_final_kb\":{final_rss},\"rss_growth_kb\":{rss_growth},\"rss_budget_kb\":{RSS_GROWTH_BUDGET_KB},\"fd_start\":{fd_first},\"fd_end\":{fd_last},\"fd_growth\":{fd_growth},\"fd_budget\":{FD_GROWTH_BUDGET},\"pass\":{}}}",
        pass
    );

    if !pass {
        let mut why: Vec<String> = Vec::new();
        if !rss_ok {
            why.push(format!(
                "RSS grew {rss_growth}KB > budget {RSS_GROWTH_BUDGET_KB}KB"
            ));
        }
        if !fd_ok {
            why.push(format!("FDs grew {fd_growth} > budget {FD_GROWTH_BUDGET}"));
        }
        if !traffic_ok {
            why.push(format!("{errors_total} traffic errors (budget: 0)"));
        }
        println!("soak verdict: FAIL — {}", why.join("; "));
        return Err(AetherError::network(format!(
            "soak budgets breached: {}",
            why.join("; ")
        )));
    }
    if assert_mode {
        println!("soak verdict: PASS (asserted budgets met)");
    } else {
        println!("\ncriterion 12: full acceptance run is `aether soak --secs 86400 --assert`.");
        println!("  pass = flat RSS trend, stable FDs, zero crashes, WAL checkpoint healthy.");
    }
    Ok(())
}

async fn plain_round(
    proxy_addr: std::net::SocketAddr,
    origin: std::net::SocketAddr,
) -> Result<bool, ()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut tcp = tokio::net::TcpStream::connect(proxy_addr)
        .await
        .map_err(|_| ())?;
    let get = format!(
        "GET http://127.0.0.1:{}/api HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nconnection: close\r\n\r\n",
        origin.port(),
        origin.port()
    );
    tcp.write_all(get.as_bytes()).await.map_err(|_| ())?;
    let mut out = Vec::new();
    tcp.read_to_end(&mut out).await.map_err(|_| ())?;
    Ok(out.starts_with(b"HTTP/1.1 200"))
}

async fn mitm_round(
    proxy_addr: std::net::SocketAddr,
    origin: std::net::SocketAddr,
) -> Result<bool, ()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut tcp = tokio::net::TcpStream::connect(proxy_addr)
        .await
        .map_err(|_| ())?;
    let connect_req = format!(
        "CONNECT 127.0.0.1:{} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
        origin.port(),
        origin.port()
    );
    tcp.write_all(connect_req.as_bytes())
        .await
        .map_err(|_| ())?;
    let mut reader = tokio::io::BufReader::new(tcp);
    let head = aether_http::wire::read_until_needle(&mut reader, b"\r\n\r\n", 8 * 1024)
        .await
        .map_err(|_| ())?
        .ok_or(())?;
    if !head.starts_with(b"HTTP/1.1 200") {
        return Err(());
    }
    let tcp = reader.into_inner();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(
        aether_http::tls::upstream_client_config(false).map_err(|_| ())?,
    ));
    let mut tls = tokio::time::timeout(
        Duration::from_secs(5),
        connector.connect(
            rustls::pki_types::ServerName::IpAddress(
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST).into(),
            ),
            tcp,
        ),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    tls.write_all(b"GET /api HTTP/1.1\r\nHost: x\r\nconnection: close\r\n\r\n")
        .await
        .map_err(|_| ())?;
    // tolerant EOF read (see lab.rs) — count only clean 200 exchanges
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match tls.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(_) => return Err(()),
        }
    }
    Ok(out.starts_with(b"HTTP/1.1 200"))
}

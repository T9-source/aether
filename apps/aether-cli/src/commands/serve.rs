//! `aether serve` — start the daemon in this terminal.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).

use aether_core::error::AetherResult;
use aether_daemon::{AetherConfig, DaemonOptions, run as daemon_run};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::watch;

pub async fn run(
    config: Option<PathBuf>,
    listen: Option<String>,
    control: Option<String>,
    memory: bool,
    data_dir: Option<PathBuf>,
) -> AetherResult<()> {
    let mut cfg = match &config {
        Some(path) => AetherConfig::load(path)?,
        None => AetherConfig::default(),
    };
    if let Some(l) = listen {
        cfg.proxy.listen = l;
    }
    if let Some(c) = control {
        cfg.control.listen = c;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let (tx, rx) = watch::channel(false);
    let flag = Arc::new(AtomicBool::new(false));
    let flag_for_signal = flag.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        flag_for_signal.store(true, Ordering::SeqCst);
        let _ = tx.send(true);
    });

    let data_dir = data_dir.or_else(|| {
        std::env::var("AETHER_DATA_DIR")
            .ok()
            .map(std::path::PathBuf::from)
    });
    let info = daemon_run(DaemonOptions {
        config: cfg,
        memory,
        data_dir,
        shutdown: rx,
    })
    .await?;

    println!(
        "Aether {} — by smart-jemy (Apache-2.0)",
        aether_daemon::VERSION
    );
    println!("──────────────────────────────────────────────────────");
    println!(
        "  proxy      : http://{}  (explicit proxy, ADR-007)",
        info.proxy_addr
    );
    println!("  control    : ws://{}", info.control_addr);
    println!();
    println!(
        "  console    : http://localhost:5173/?ws=ws://{}&auth={}",
        info.control_addr, info.token
    );
    println!("             ↑ one-click link — opens the console already connected");
    println!(
        "  auth       : {} (also in {:?})",
        info.token,
        info.data_dir.join("token")
    );
    println!(
        "  ca         : {}  (`aether ca export --out aether.pem`)",
        info.ca_pem_path.display()
    );
    println!("  storage    : {}", info.storage_mode);
    println!("  rules      : {} active", info.rules_loaded);
    println!("  intercept  : `aether intercept set --request on`");
    println!("──────────────────────────────────────────────────────");

    // wait for shutdown
    loop {
        if flag.load(Ordering::SeqCst) {
            println!("shutting down — history saved.");
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

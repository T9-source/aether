//! Aether daemon — wires proxy engine + storage + control plane.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! The daemon is the only state owner (ADR-001): proxy listener, control
//! plane, CA, rules, storage all live here; every client (CLI, GUI, CI) is
//! a remote consumer over the WebSocket control plane.

use crate::config::{AetherConfig, default_data_dir};
use crate::rules::RulesStore;
use aether_core::error::{AetherError, AetherResult};
use aether_core::hooks::HookRegistry;
use aether_core::storage::Storage;
use aether_core::{EventBus, MemoryStorage, Pipeline};
use aether_http::ca::CaManager;
use aether_http::engine::FlowEngine;
use aether_http::intercept::InterceptState;
use aether_http::server::{ProxyCfg, ProxyServer};
use aether_store_sqlite::SqliteStore;
use serde::Serialize;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::net::TcpListener;
use tokio::sync::watch;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What `run` reports back once every subsystem is listening.
#[derive(Debug, Clone, Serialize)]
pub struct DaemonInfo {
    pub proxy_addr: SocketAddr,
    pub control_addr: SocketAddr,
    pub token: String,
    pub data_dir: PathBuf,
    pub ca_pem_path: PathBuf,
    pub storage_mode: String,
    pub rules_loaded: usize,
}

pub struct DaemonOptions {
    pub config: AetherConfig,
    /// Force in-memory storage (overrides the config; used by `--memory`).
    pub memory: bool,
    pub data_dir: Option<PathBuf>,
    /// Watch flips to `true` on ctrl-c / parent shutdown.
    pub shutdown: watch::Receiver<bool>,
}

pub async fn run(opts: DaemonOptions) -> AetherResult<DaemonInfo> {
    opts.config.validate()?;
    let data_dir = opts.data_dir.unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| AetherError::storage(format!("create data dir: {e}")))?;

    // ── storage ─────────────────────────────────────────────────────────
    let (storage, storage_mode): (Arc<dyn Storage>, String) =
        if opts.memory || opts.config.storage.backend == "memory" {
            (Arc::new(MemoryStorage::new()), "memory".into())
        } else {
            let path = match &opts.config.storage.path {
                Some(p) => PathBuf::from(p),
                None => data_dir.join("aether.db"),
            };
            let store = SqliteStore::open(&path).await?;
            (Arc::new(store), format!("sqlite:{}", path.display()))
        };

    // ── ca + rules + pipeline ───────────────────────────────────────────
    // One live HookRegistry shared by the pipeline and the rules store:
    // control-plane rule mutations re-project into the same registry the
    // engine reads, so they apply to in-flight traffic (ADR-006 #1).
    let ca = Arc::new(CaManager::load_or_create(data_dir.join("ca"))?);
    let rules = Arc::new(RulesStore::load(&data_dir)?);
    let registry = Arc::new(HookRegistry::new());
    let rules_loaded = rules.register_into(&registry)?;
    let bus = EventBus::new(4096);
    let pipeline = Pipeline::new((*registry).clone(), bus.clone());

    // ── intercept + engine ──────────────────────────────────────────────
    let intercept = Arc::new(InterceptState::new(
        opts.config.intercept.request,
        opts.config.intercept.response,
    ));
    let engine = Arc::new(FlowEngine {
        bus: bus.clone(),
        pipeline,
        storage: storage.clone(),
        intercept: intercept.clone(),
        upstream_cfg: opts.config.upstream_cfg(),
    });

    // ── proxy listener ──────────────────────────────────────────────────
    let proxy_listener = TcpListener::bind(&opts.config.proxy.listen)
        .await
        .map_err(|e| {
            AetherError::config(format!("bind proxy {}: {e}", opts.config.proxy.listen))
        })?;
    let proxy_addr = proxy_listener
        .local_addr()
        .map_err(|e| AetherError::config(e.to_string()))?;
    let server = Arc::new(ProxyServer {
        engine,
        ca: ca.clone(),
        cfg: ProxyCfg {
            passthrough: opts.config.passthrough.clone(),
            host_overrides: opts.config.host_overrides.clone(),
            upstream_tls_verify: opts.config.upstream_tls_verify,
            capture_max_body: opts.config.capture_max_body,
            idle_timeout: std::time::Duration::from_secs(65),
        },
        shutdown: opts.shutdown.clone(),
    });
    tokio::spawn(server.clone().serve(proxy_listener));

    // ── control plane ───────────────────────────────────────────────────
    let token = match &opts.config.control.token {
        Some(t) => t.clone(),
        None => generate_token()?,
    };
    let control_listener = TcpListener::bind(&opts.config.control.listen)
        .await
        .map_err(|e| {
            AetherError::config(format!("bind control {}: {e}", opts.config.control.listen))
        })?;
    let control_addr = control_listener
        .local_addr()
        .map_err(|e| AetherError::config(e.to_string()))?;
    let shared = Arc::new(crate::control::ControlShared {
        token: token.clone(),
        bus,
        storage,
        intercept,
        rules,
        registry: registry.clone(),
        ca: ca.clone(),
        upstream: opts.config.upstream_cfg(),
        proxy_addr: proxy_addr.to_string(),
        version: VERSION,
        started_at_unix_ms: aether_core::model::unix_millis_now(),
    });
    tokio::spawn(crate::control::run(
        control_listener,
        shared,
        opts.shutdown.clone(),
    ));

    // ── connection hints for CLI clients ────────────────────────────────
    write_private_file(&data_dir.join("token"), token.as_bytes().to_vec())?;
    std::fs::write(data_dir.join("control.addr"), control_addr.to_string())
        .map_err(|e| AetherError::storage(format!("write control.addr: {e}")))?;
    std::fs::write(data_dir.join("proxy.addr"), proxy_addr.to_string())
        .map_err(|e| AetherError::storage(format!("write proxy.addr: {e}")))?;

    Ok(DaemonInfo {
        proxy_addr,
        control_addr,
        token,
        data_dir,
        ca_pem_path: ca.dir().join("ca.cert.pem"),
        storage_mode,
        rules_loaded,
    })
}

fn generate_token() -> AetherResult<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| AetherError::storage(format!("token entropy: {e}")))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn write_private_file(path: &PathBuf, bytes: Vec<u8>) -> AetherResult<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)
        .map_err(|e| AetherError::storage(format!("create {path:?}: {e}")))?;
    f.write_all(&bytes)
        .map_err(|e| AetherError::storage(format!("write {path:?}: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Ctrl-c / SIGTERM → shutdown watch flip.
pub async fn install_ctrl_c(shutdown_tx: watch::Sender<bool>, flag: Arc<AtomicBool>) {
    let _ = tokio::signal::ctrl_c().await;
    flag.store(true, Ordering::SeqCst);
    let _ = shutdown_tx.send(true);
}

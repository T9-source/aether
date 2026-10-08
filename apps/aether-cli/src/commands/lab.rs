//! `lab` — an embedded single-node Aether stack for benchmarks and soak runs.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! The daemon is a thin wiring of `aether-http` + storage; the lab builds the
//! same wiring in-process so `bench proxy` and `aether soak` measure the real
//! engine (real TLS, real pipeline, real SQLite) with zero external setup.
//! This is measurement tooling, not a second daemon implementation.

use aether_core::error::{AetherError, AetherResult};
use aether_core::hooks::HookRegistry;
use aether_core::storage::Storage;
use aether_core::{EventBus, Pipeline};
use aether_http::ca::CaManager;
use aether_http::engine::FlowEngine;
use aether_http::intercept::InterceptState;
use aether_http::server::{ProxyCfg, ProxyServer};
use aether_http::upstream::UpstreamCfg;
use aether_http::wire;
use aether_store_sqlite::SqliteStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

fn io(e: std::io::Error) -> AetherError {
    AetherError::network(format!("lab io: {e}"))
}

/// Reads a (TLS) response to EOF, tolerating a peer that drops the socket
/// without `close_notify` — rustls surfaces that as UnexpectedEof; local
/// test origins and the engine's `Connection: close` path both do it.
async fn read_response_to_eof<S: tokio::io::AsyncRead + Unpin>(
    stream: &mut S,
) -> AetherResult<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(io(e)),
        }
    }
    Ok(out)
}

/// A TLS echo origin (self-signed) — the same request/response shape the
/// e2e suite uses, so benchmark numbers describe the tested behavior.
pub struct LabOrigin {
    pub addr: std::net::SocketAddr,
    #[allow(dead_code)] // exposed for future bench legs that pin the origin cert
    pub cert: CertificateDer<'static>,
}

pub async fn spawn_tls_origin(name: &str) -> AetherResult<LabOrigin> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| AetherError::network(format!("bind origin: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| AetherError::network(e.to_string()))?;
    let key = rcgen_key()?;
    let mut params = rcgen::CertificateParams::new(vec![name.to_string(), "127.0.0.1".to_string()])
        .map_err(|e| AetherError::tls(format!("origin cert params: {e}")))?;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, name);
    let cert = params
        .self_signed(&key)
        .map_err(|e| AetherError::tls(format!("origin self-sign: {e}")))?;
    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
        key.serialize_der(),
    ));
    let mut server_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .map_err(|e| AetherError::tls(format!("origin cert: {e}")))?;
    server_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_cfg));

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(tls) = acceptor.accept(stream).await {
                    let _ = origin_serve(tls).await;
                }
            });
        }
    });

    Ok(LabOrigin {
        addr,
        cert: cert_der,
    })
}

fn rcgen_key() -> AetherResult<rcgen::KeyPair> {
    rcgen::KeyPair::generate().map_err(|e| AetherError::tls(format!("keygen: {e}")))
}

async fn origin_serve<S>(stream: S) -> std::io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut reader = tokio::io::BufReader::new(stream);
    let Some(head) = wire::read_until_needle(&mut reader, b"\r\n\r\n", 64 * 1024)
        .await
        .ok()
        .flatten()
    else {
        return Ok(());
    };
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or("?");
    let target = parts.next().unwrap_or("?");
    let mut content_length = 0usize;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("content-length:") {
            content_length = lower
                .split(':')
                .nth(1)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
        }
        if line.is_empty() {
            break;
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).await?;
    let echo = format!(
        "{{\"method\":\"{method}\",\"target\":\"{target}\",\"body_len\":{}}}",
        body.len()
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nx-origin: aether-lab\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{echo}",
        echo.len()
    );
    reader.get_mut().write_all(response.as_bytes()).await?;
    Ok(())
}

/// The embedded proxy: real engine + real SQLite (WAL) in a tempdir unless
/// a path is given, so soak runs exercise the exact persistence path.
pub struct LabProxy {
    pub addr: std::net::SocketAddr,
    #[allow(dead_code)] // kept for soak reporting (flow counts)
    pub storage: Arc<dyn Storage>,
    #[allow(dead_code)] // keeps the CA dir alive for the process lifetime
    pub ca_dir: tempfile::TempDir,
    _shutdown_tx: watch::Sender<bool>,
}

pub async fn spawn_proxy(db_path: Option<PathBuf>) -> AetherResult<LabProxy> {
    let ca_dir =
        tempfile::TempDir::new().map_err(|e| AetherError::storage(format!("tempdir: {e}")))?;
    let ca = Arc::new(CaManager::load_or_create(ca_dir.path())?);
    let bus = EventBus::new(4096);
    let storage: Arc<dyn Storage> = match &db_path {
        Some(p) => Arc::new(SqliteStore::open(p).await?),
        None => {
            let dir = tempfile::TempDir::new()
                .map_err(|e| AetherError::storage(format!("tempdir: {e}")))?;
            // keep the guard alive for the process lifetime: the lab is a
            // short-lived child process, leaking one tempdir is safe and
            // keeps the db alive for the whole run
            let path = dir.path().join("lab.db");
            let store = SqliteStore::open(&path).await?;
            std::mem::forget(dir);
            Arc::new(store)
        }
    };
    let intercept = Arc::new(InterceptState::new(false, false));
    let engine = Arc::new(FlowEngine {
        bus: bus.clone(),
        pipeline: Pipeline::new(HookRegistry::new(), bus.clone()),
        storage: storage.clone(),
        intercept,
        upstream_cfg: UpstreamCfg {
            upstream_tls_verify: false, // lab origins are self-signed
            ..Default::default()
        },
    });
    let (tx, rx) = watch::channel(false);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| AetherError::network(format!("bind proxy: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| AetherError::network(e.to_string()))?;
    let server = Arc::new(ProxyServer {
        engine,
        ca,
        cfg: ProxyCfg::default(),
        shutdown: rx,
    });
    tokio::spawn(server.serve(listener));
    Ok(LabProxy {
        addr,
        storage,
        ca_dir,
        _shutdown_tx: tx,
    })
}

/// One timed h1 exchange. `via` selects the path being measured.
pub enum Path {
    /// TLS straight to the origin.
    Direct,
    /// CONNECT + TLS MITM through the proxy.
    ProxyMitm,
}

pub async fn timed_request(
    proxy: &LabProxy,
    origin: &LabOrigin,
    via: &Path,
) -> AetherResult<Duration> {
    let t0 = std::time::Instant::now();
    run_request(proxy, origin, via).await?;
    Ok(t0.elapsed())
}

async fn run_request(proxy: &LabProxy, origin: &LabOrigin, via: &Path) -> AetherResult<()> {
    let get = format!(
        "GET /api HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nuser-agent: aether-lab\r\nconnection: close\r\n\r\n",
        origin.addr.port()
    );
    match via {
        Path::Direct => {
            let mut tls = lab_tls_stream(origin.addr).await?;
            tls.write_all(get.as_bytes()).await.map_err(io)?;
            let _ = read_response_to_eof(&mut tls).await?;
        }
        Path::ProxyMitm => {
            let mut tcp = TcpStream::connect(proxy.addr)
                .await
                .map_err(|e| AetherError::network(format!("connect proxy: {e}")))?;
            let connect_req = format!(
                "CONNECT 127.0.0.1:{} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                origin.addr.port(),
                origin.addr.port()
            );
            tcp.write_all(connect_req.as_bytes()).await.map_err(io)?;
            let mut reader = tokio::io::BufReader::new(tcp);
            let head = wire::read_until_needle(&mut reader, b"\r\n\r\n", 8 * 1024)
                .await?
                .ok_or_else(|| AetherError::network("proxy closed during CONNECT"))?;
            if !head.starts_with(b"HTTP/1.1 200") {
                return Err(AetherError::network(format!(
                    "CONNECT refused: {}",
                    String::from_utf8_lossy(&head)
                )));
            }
            let tcp = reader.into_inner();
            let mut tls = tokio::time::timeout(
                Duration::from_secs(5),
                tokio_rustls::TlsConnector::from(Arc::new(
                    aether_http::tls::upstream_client_config(false)?,
                ))
                .connect(server_name(), tcp),
            )
            .await
            .map_err(|_| AetherError::tls("lab tls handshake timeout"))?
            .map_err(|e| AetherError::tls(format!("lab tls handshake: {e}")))?;
            tls.write_all(get.as_bytes()).await.map_err(io)?;
            let _ = read_response_to_eof(&mut tls).await?;
        }
    }
    Ok(())
}

/// Plaintext leg: absolute-form request through the proxy to a plain origin.
pub async fn spawn_plain_origin() -> AetherResult<std::net::SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| AetherError::network(format!("bind plain origin: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| AetherError::network(e.to_string()))?;
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let _ = origin_serve(stream).await;
            });
        }
    });
    Ok(addr)
}

pub async fn timed_plain(
    proxy: &LabProxy,
    origin_addr: std::net::SocketAddr,
) -> AetherResult<Duration> {
    let t0 = std::time::Instant::now();
    let mut tcp = TcpStream::connect(proxy.addr)
        .await
        .map_err(|e| AetherError::network(format!("connect proxy: {e}")))?;
    let get = format!(
        "GET http://127.0.0.1:{}/api HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nuser-agent: aether-lab\r\nconnection: close\r\n\r\n",
        origin_addr.port(),
        origin_addr.port()
    );
    tcp.write_all(get.as_bytes()).await.map_err(io)?;
    let mut out = Vec::new();
    tcp.read_to_end(&mut out).await.map_err(io)?;
    if !out.starts_with(b"HTTP/1.1 200") {
        return Err(AetherError::network("plain leg: non-200 response"));
    }
    Ok(t0.elapsed())
}

pub async fn lab_tls_stream(
    addr: std::net::SocketAddr,
) -> AetherResult<tokio_rustls::client::TlsStream<TcpStream>> {
    let tcp = TcpStream::connect(addr)
        .await
        .map_err(|e| AetherError::network(format!("connect origin: {e}")))?;
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio_rustls::TlsConnector::from(Arc::new(aether_http::tls::upstream_client_config(
            false,
        )?))
        .connect(server_name(), tcp),
    )
    .await
    .map_err(|_| AetherError::tls("lab tls handshake timeout"))?
    .map_err(|e| AetherError::tls(format!("lab tls handshake: {e}")))
}

fn server_name() -> rustls::pki_types::ServerName<'static> {
    rustls::pki_types::ServerName::IpAddress(
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST).into(),
    )
}

pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

// ── process metrics (Linux; graceful elsewhere) ─────────────────────────────

pub struct ProcMetrics {
    pub rss_kb: u64,
    pub fds: u64,
}

pub fn read_process_metrics() -> Option<ProcMetrics> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let rss_kb = status
        .lines()
        .find(|l| l.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let fds = std::fs::read_dir("/proc/self/fd").ok()?.count() as u64;
    Some(ProcMetrics { rss_kb, fds })
}

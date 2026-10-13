//! Aether — network engine.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! The proxy listener (ADR-007: explicit proxy only):
//! - plain HTTP: absolute-form requests on the proxy port,
//! - CONNECT: either a blind pass-through tunnel (ADR-004 pass-through list)
//!   or a TLS MITM with a fixed leaf minted for the authority,
//! - keep-alive honored on the client leg; upstream is one connection per
//!   request with `Connection: close` (V0.1, ADR-009).

use aether_core::error::{AetherError, AetherResult};
use aether_core::model::{HttpVersion, Method, Request, TransportProto, WsDirection};
use aether_core::{AetherEvent, Scope};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use crate::ca::CaManager;
use crate::engine::{ConnInfo, FlowEngine};
use crate::tls::{self, TlsFacts};
use crate::wire;
use crate::ws::WsRecorder;

/// Type-erased bidirectional stream — the recursion boundary for the
/// CONNECT → MITM → serve loop. Without erasure the compiler must prove
/// `Send` for an infinite family `TlsStream<TlsStream<…<TcpStream>>>`
/// (E0275); with it, every leg of the loop has exactly one type.
pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl AsyncReadWrite for TcpStream {}
impl<IO> AsyncReadWrite for tokio_rustls::server::TlsStream<IO> where IO: AsyncReadWrite {}
impl AsyncReadWrite for Box<dyn AsyncReadWrite> {}
impl<T> AsyncReadWrite for Box<T> where T: AsyncReadWrite {}

#[derive(Debug, Clone)]
pub struct ProxyCfg {
    /// Host globs tunneled blindly (never MITM'd, never recorded).
    pub passthrough: Vec<String>,
    /// Dial overrides: `"host"`/`"host:port"` → `"socket://host:port"`.
    pub host_overrides: BTreeMap<String, String>,
    /// Validate upstream certificates (default true).
    pub upstream_tls_verify: bool,
    /// Per-body capture limit (ADR-011). 8 MiB default.
    pub capture_max_body: usize,
    /// Client-leg idle timeout while waiting for the next request head.
    pub idle_timeout: Duration,
}

impl Default for ProxyCfg {
    fn default() -> Self {
        Self {
            passthrough: Vec::new(),
            host_overrides: Default::default(),
            upstream_tls_verify: true,
            capture_max_body: 8 * 1024 * 1024,
            idle_timeout: Duration::from_secs(65),
        }
    }
}

pub struct ProxyServer {
    pub engine: Arc<FlowEngine>,
    pub ca: Arc<CaManager>,
    pub cfg: ProxyCfg,
    pub shutdown: watch::Receiver<bool>,
}

impl ProxyServer {
    /// Accept loop; returns when the shutdown watch flips.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) {
        loop {
            let mut shutdown = self.shutdown.clone();
            tokio::select! {
                _ = shutdown.changed() => {
                    tracing::info!("proxy listener shutting down");
                    return;
                }
                accepted = listener.accept() => {
                    let (stream, peer) = match accepted {
                        Ok(x) => x,
                        Err(e) => {
                            tracing::warn!(error = %e, "accept failed");
                            continue;
                        }
                    };
                    let srv = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = srv.handle_conn(stream, peer).await {
                            tracing::debug!(%peer, error = %e, "connection ended with error");
                        }
                    });
                }
            }
        }
    }

    async fn handle_conn(&self, stream: TcpStream, peer: SocketAddr) -> AetherResult<()> {
        let _ = stream.set_nodelay(true);
        self.serve_h1(Box::new(stream), "http", None, None, peer)
            .await
    }

    /// One h1 request loop over an owned stream. Recursion happens exactly
    /// once (CONNECT → TLS accept → this again), so the continuation is
    /// boxed to keep the future finite.
    fn serve_h1<'a>(
        &'a self,
        stream: Box<dyn AsyncReadWrite>,
        scheme: &'static str,
        authority_fallback: Option<String>,
        tls_facts: Option<TlsFacts>,
        peer: SocketAddr,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AetherResult<()>> + Send + 'a>> {
        Box::pin(self.serve_h1_inner(stream, scheme, authority_fallback, tls_facts, peer))
    }

    async fn serve_h1_inner(
        &self,
        stream: Box<dyn AsyncReadWrite>,
        scheme: &'static str,
        authority_fallback: Option<String>,
        tls_facts: Option<TlsFacts>,
        peer: SocketAddr,
    ) -> AetherResult<()> {
        let authority_fallback = authority_fallback.as_deref();
        let mut reader = BufReader::new(stream);
        loop {
            let head = match tokio::time::timeout(
                self.cfg.idle_timeout,
                wire::read_until_needle(&mut reader, b"\r\n\r\n", wire::MAX_HEAD_BYTES),
            )
            .await
            {
                Err(_) => return Ok(()),       // idle timeout: close quietly
                Ok(Ok(None)) => return Ok(()), // clean close
                Ok(Ok(Some(head))) => head,
                Ok(Err(e)) => {
                    tracing::debug!(%peer, error = %e, "head read failed");
                    return Ok(());
                }
            };

            let parsed = wire::parse_request_head(&head, Some(scheme), authority_fallback);
            let request = match parsed {
                Ok(req) => req,
                Err(e) => {
                    let res = wire::error_response(400, &e.to_string())?;
                    reader
                        .get_mut()
                        .write_all(&wire::serialize_response(&res, false))
                        .await
                        .ok();
                    return Ok(());
                }
            };

            // CONNECT is only meaningful on the plain proxy leg
            if request.method == Method::Connect {
                if authority_fallback.is_some() {
                    let res =
                        wire::error_response(400, "CONNECT inside a MITM tunnel is not supported")?;
                    reader
                        .get_mut()
                        .write_all(&wire::serialize_response(&res, false))
                        .await
                        .ok();
                    return Ok(());
                }
                return self.handle_connect(request, reader, peer).await;
            }

            // WebSocket upgrade (RFC 6455, V0.2 ADR-013): detected on the
            // wire, run through the same engine, then relayed as frames.
            if request.method == Method::Get && is_websocket_upgrade(&request.headers) {
                return self
                    .handle_websocket(request, reader, peer, scheme, tls_facts.clone())
                    .await;
            }

            // ── request body (bounded) ───────────────────────────────────
            let spec = match wire::request_body_spec(&request.headers) {
                Ok(s) => s,
                Err(e) => {
                    let res = wire::error_response(400, &e.to_string())?;
                    reader
                        .get_mut()
                        .write_all(&wire::serialize_response(&res, false))
                        .await
                        .ok();
                    return Ok(());
                }
            };
            let mut request = request;
            if spec != wire::BodySpec::Empty {
                if request
                    .headers
                    .get("expect")
                    .map(|v| v.to_ascii_lowercase().contains("100-continue"))
                    .unwrap_or(false)
                {
                    reader
                        .get_mut()
                        .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                        .await
                        .ok();
                }
                match wire::read_body(&mut reader, &spec, self.cfg.capture_max_body).await {
                    Ok(bytes) => request.body = wire::body_from_captured(bytes),
                    Err(e) if e.to_string().contains("capture limit") => {
                        let res = wire::error_response(413, &e.to_string())?;
                        reader
                            .get_mut()
                            .write_all(&wire::serialize_response(&res, false))
                            .await
                            .ok();
                        return Ok(());
                    }
                    Err(_) => return Ok(()),
                }
            }

            let keep_alive = client_keep_alive(&request);
            let conn_info = ConnInfo {
                client_addr: peer,
                client_proto: if scheme == "https" {
                    TransportProto::Tls
                } else {
                    TransportProto::Plaintext
                },
                client_h2: false,
                tls: tls_facts.clone(),
            };

            match self.engine.handle_request(request, conn_info).await {
                crate::engine::EngineOutcome::Serve {
                    response,
                    remainder,
                } => {
                    let bytes = match &remainder {
                        // chunked origins beyond the capture cap declare no
                        // length — the only honest h1 framing left is EOF
                        // (RFC 9110 §6.3); inventing a CL would lie
                        Some(_) if !response.headers.contains("content-length") => {
                            wire::serialize_response_close_delimited(&response)
                        }
                        _ => wire::serialize_response(&response, keep_alive),
                    };
                    if reader.get_mut().write_all(&bytes).await.is_err() {
                        return Ok(());
                    }
                    if let Some(mut rem) = remainder {
                        let mut sink = reader.get_mut();
                        if tokio::io::copy(&mut rem, &mut sink).await.is_err() {
                            return Ok(());
                        }
                    }
                    if !keep_alive {
                        return Ok(());
                    }
                }
                crate::engine::EngineOutcome::Dropped { reason } => {
                    let res = wire::error_response(502, &format!("exchange dropped: {reason}"))?;
                    reader
                        .get_mut()
                        .write_all(&wire::serialize_response(&res, false))
                        .await
                        .ok();
                    return Ok(());
                }
            }
        }
    }

    fn is_passthrough(&self, host: &str) -> bool {
        let scope = Scope {
            include: if self.cfg.passthrough.is_empty() {
                Vec::new() // empty list = nothing passes through
            } else {
                self.cfg.passthrough.clone()
            },
            exclude: Vec::new(),
        };
        scope.matches_host(host)
    }

    /// CONNECT: pass-through tunnel or MITM (ADR-004).
    async fn handle_connect(
        &self,
        request: Request,
        reader: BufReader<Box<dyn AsyncReadWrite>>,
        peer: SocketAddr,
    ) -> AetherResult<()> {
        let host = request.url.host.clone();
        let port = request.url.effective_port();
        let authority = format!("{host}:{port}");

        if self.is_passthrough(&host) {
            self.engine
                .bus
                .publish(AetherEvent::PassThrough { authority });
            let mut raw = reader.into_inner();
            let mut target = dial_tcp(&host, port).await?;
            raw.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .ok();
            let _ = tokio::io::copy_bidirectional(&mut raw, &mut target).await;
            return Ok(());
        }

        let leaf = self.ca.leaf_for(&host)?;
        let server_cfg = tls::mitm_server_config(leaf)?;
        let mut raw = reader.into_inner();
        raw.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .map_err(|e| AetherError::network(format!("client gone during CONNECT: {e}")))?;

        // ── peek the first bytes of the tunnel (wire truth over guesswork):
        // TLS ClientHello (0x16 0x03 …) → MITM as usual. Anything else is a
        // PLAINTEXT h1 exchange inside the tunnel — real clients do this for
        // `ws://` (CONNECT, then an unencrypted RFC 6455 upgrade). Serving
        // plaintext here keeps WebSockets on http origins working instead of
        // dying with a TLS handshake error.
        let mut peeked = vec![0u8; 3];
        let n = tokio::time::timeout(self.cfg.idle_timeout, raw.read(&mut peeked))
            .await
            .map_err(|_| AetherError::network("tunnel peek timeout"))?
            .map_err(|e| AetherError::network(format!("tunnel read: {e}")))?;
        peeked.truncate(n);
        if n == 0 {
            return Ok(()); // client went away mid-tunnel
        }
        let is_tls = peeked[0] == 0x16 && peeked[1] == 0x03;

        if is_tls {
            let client = PrefixStream {
                prefix: peeked,
                pos: 0,
                inner: raw,
            };
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_cfg));
            let tls_stream: tokio_rustls::server::TlsStream<PrefixStream> =
                acceptor.accept(client).await.map_err(|e| {
                    AetherError::tls(format!("MITM handshake with client failed for {host}: {e}"))
                })?;
            let facts = tls_facts_of(&tls_stream);
            let negotiated_h2 = tls_stream.get_ref().1.alpn_protocol() == Some(b"h2");
            if negotiated_h2 {
                // V0.2 (ADR-013): the client speaks HTTP/2 — serve streams
                // through the h2 leg codec; same engine, same history.
                tracing::debug!(%host, "MITM tunnel established (h2 via ALPN)");
                crate::h2x::serve_connection(
                    self.engine.clone(),
                    Box::new(tls_stream),
                    peer,
                    Some(facts),
                    self.shutdown.clone(),
                )
                .await;
                return Ok(());
            }
            tracing::debug!(%host, "MITM tunnel established (h1)");
            self.serve_h1(
                Box::new(tls_stream),
                "https",
                Some(authority),
                Some(facts),
                peer,
            )
            .await
        } else {
            // plaintext h1 inside the CONNECT tunnel (ws:// over proxies)
            tracing::debug!(%host, "tunnel established (plaintext h1 inside CONNECT)");
            let client = PrefixStream {
                prefix: peeked,
                pos: 0,
                inner: raw,
            };
            self.serve_h1(Box::new(client), "http", Some(authority), None, peer)
                .await
        }
    }
}

fn tls_facts_of<IO: AsyncRead + AsyncWrite + Unpin>(
    stream: &tokio_rustls::server::TlsStream<IO>,
) -> TlsFacts {
    let (_, conn) = stream.get_ref();
    TlsFacts {
        version: conn.protocol_version().map(|v| format!("{v:?}")),
        cipher: conn
            .negotiated_cipher_suite()
            .map(|cs| format!("{:?}", cs.suite())),
        alpn: conn
            .alpn_protocol()
            .map(|p| String::from_utf8_lossy(p).into_owned()),
    }
}

// ---------------------------------------------------------------------------
// WebSocket relay (V0.2, ADR-013)
// ---------------------------------------------------------------------------

/// RFC 6455 §4.2.1 upgrade markers (case-insensitive per RFC 9110 §7.6).
fn is_websocket_upgrade(headers: &aether_core::model::Headers) -> bool {
    let upgrade = headers
        .get("upgrade")
        .map(|v| v.to_ascii_lowercase().contains("websocket"))
        .unwrap_or(false);
    let connection = headers
        .get("connection")
        .map(|v| v.to_ascii_lowercase().contains("upgrade"))
        .unwrap_or(false);
    upgrade && connection
}

/// Client-leg socket with bytes already buffered by the h1 head reader —
/// the first WS frames often arrive with the handshake, and losing them
/// would corrupt the stream.
struct PrefixStream {
    prefix: Vec<u8>,
    pos: usize,
    inner: Box<dyn AsyncReadWrite>,
}

impl AsyncRead for PrefixStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.pos < this.prefix.len() {
            let n = buf.remaining().min(this.prefix.len() - this.pos);
            buf.put_slice(&this.prefix[this.pos..this.pos + n]);
            this.pos += n;
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for PrefixStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Re-erased so the MITM accept result boxes back into the recursion
/// boundary (`serve_h1`/`h2x::serve_connection` take `Box<dyn AsyncReadWrite>`).
impl AsyncReadWrite for PrefixStream {}

impl ProxyServer {
    /// The full WebSocket lifecycle: upgrade through the engine, serve the
    /// 101, then relay frames byte-for-byte while the recorder assembles
    /// the bounded session (persisted on the flow when the socket ends).
    async fn handle_websocket(
        &self,
        request: Request,
        mut reader: BufReader<Box<dyn AsyncReadWrite>>,
        peer: SocketAddr,
        scheme: &'static str,
        tls_facts: Option<TlsFacts>,
    ) -> AetherResult<()> {
        let conn_info = ConnInfo {
            client_addr: peer,
            client_proto: if scheme == "https" {
                TransportProto::Tls
            } else {
                TransportProto::Plaintext
            },
            client_h2: false,
            tls: tls_facts,
        };

        match self.engine.handle_upgrade(request, conn_info).await {
            crate::engine::UpgradeOutcome::Upgraded {
                flow_id,
                response,
                upstream,
            } => {
                let bytes = wire::serialize_upgrade_response(&response);
                if reader.get_mut().write_all(&bytes).await.is_err() {
                    return Ok(());
                }
                self.engine.bus.publish(AetherEvent::WsOpened { flow_id });

                // reclaim anything the head reader already swallowed
                let prefix = reader.buffer().to_vec();
                let raw = reader.into_inner();
                let client = PrefixStream {
                    prefix,
                    pos: 0,
                    inner: raw,
                };

                let recorder = WsRecorder::new(flow_id, self.engine.bus.clone());
                let rec_a = recorder.clone();
                let rec_b = recorder.clone();
                let (client_r, client_w) = tokio::io::split(client);
                let (up_r, up_w) = tokio::io::split(upstream);
                let mut c2s = tokio::spawn(async move {
                    crate::ws::relay_leg(client_r, up_w, WsDirection::ToServer, &rec_a).await
                });
                let mut s2c = tokio::spawn(async move {
                    crate::ws::relay_leg(up_r, client_w, WsDirection::ToClient, &rec_b).await
                });

                // First leg to end wins: abort the sibling — both socket
                // halves drop, the peers see FIN, the session is over.
                tokio::select! {
                    r = &mut c2s => {
                        if let Err(e) = r {
                            tracing::debug!(%flow_id, error = %e, "ws c2s leg ended");
                        }
                        s2c.abort();
                    }
                    r = &mut s2c => {
                        if let Err(e) = r {
                            tracing::debug!(%flow_id, error = %e, "ws s2c leg ended");
                        }
                        c2s.abort();
                    }
                }

                // persist the bounded session on the flow (primitive #5)
                let session = recorder.finish();
                let (total_messages, total_bytes) = (session.total_messages, session.total_bytes);
                match self.engine.storage.get_flow(flow_id).await {
                    Ok(Some(mut flow)) => {
                        flow.ws = Some(session);
                        if let Err(e) = self.engine.storage.put_flow(&flow).await {
                            tracing::warn!(%flow_id, error = %e, "ws session persist failed");
                        }
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!(%flow_id, error = %e, "ws flow reload failed"),
                }
                self.engine.bus.publish(AetherEvent::WsClosed {
                    flow_id,
                    total_messages,
                    total_bytes,
                });
                tracing::debug!(%flow_id, total_messages, total_bytes, "ws relay closed");
                Ok(())
            }
            crate::engine::UpgradeOutcome::Replied {
                response,
                remainder,
            } => {
                // the origin refused the upgrade — a normal response
                let bytes = match &remainder {
                    Some(_) if !response.headers.contains("content-length") => {
                        wire::serialize_response_close_delimited(&response)
                    }
                    _ => wire::serialize_response(&response, false),
                };
                reader.get_mut().write_all(&bytes).await.ok();
                if let Some(mut rem) = remainder {
                    let mut sink = reader.get_mut();
                    let _ = tokio::io::copy(&mut rem, &mut sink).await;
                }
                Ok(())
            }
            crate::engine::UpgradeOutcome::Dropped { reason } => {
                let res = wire::error_response(502, &format!("exchange dropped: {reason}"))?;
                reader
                    .get_mut()
                    .write_all(&wire::serialize_response(&res, false))
                    .await
                    .ok();
                Ok(())
            }
        }
    }
}

fn client_keep_alive(req: &Request) -> bool {
    let conn_hdr = req
        .headers
        .get("connection")
        .map(|s| s.to_ascii_lowercase());
    match req.version {
        HttpVersion::Http10 => conn_hdr.map(|c| c.contains("keep-alive")).unwrap_or(false),
        _ => !conn_hdr.map(|c| c.contains("close")).unwrap_or(false),
    }
}

async fn dial_tcp(host: &str, port: u16) -> AetherResult<TcpStream> {
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
    let addr = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| AetherError::network(format!("dns lookup {host}: {e}")))?
        .next()
        .ok_or_else(|| AetherError::network(format!("no addresses for {host}")))?;
    // Bounded connect (audit LOW): a host that swallows SYN must not pin a
    // tunnel task forever — same budget as the proxied upstream leg.
    tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| AetherError::network(format!("connect {addr}: timed out after 10s")))?
        .map_err(|e| AetherError::network(format!("connect {addr}: {e}")))
}

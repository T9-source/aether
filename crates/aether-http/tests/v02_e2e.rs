//! Aether — V0.2 end-to-end tests (ADR-013).
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Real sockets, real TLS, real h2 handshakes, real WebSocket frames:
//! - h2 client leg ↔ h2 upstream (the browser-to-modern-origin path)
//! - h2 client leg downgraded to an h1 upstream
//! - h1 client leg upgraded to an h2 upstream
//! - a `wss://` WebSocket upgraded through the engine, relayed frame by
//!   frame, and recorded on the flow
//!
//! The rule under test: the protocol of the legs never changes what the
//! engine sees — flows, events, rules and intercept behave identically.

use aether_core::storage::{FlowQuery, MemoryStorage, Storage};
use aether_core::{EventBus, HookRegistry, Pipeline};
use aether_http::ca::CaManager;
use aether_http::engine::FlowEngine;
use aether_http::intercept::InterceptState;
use aether_http::server::{ProxyCfg, ProxyServer};
use aether_http::upstream::UpstreamCfg;
use aether_http::wire;
use futures_util::{SinkExt, StreamExt};
use rustls::ClientConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;

// ── harness (mirrors proxy_e2e.rs) ──────────────────────────────────────────

#[allow(dead_code)] // bus kept for symmetry with the phase-1 harness
struct TestProxy {
    addr: SocketAddr,
    storage: Arc<MemoryStorage>,
    bus: EventBus,
    _shutdown_tx: watch::Sender<bool>,
}

async fn spawn_proxy(alpn_h2: bool) -> TestProxy {
    let ca_dir = tempfile::TempDir::new().unwrap();
    let ca = Arc::new(CaManager::load_or_create(ca_dir.path()).unwrap());
    let bus = EventBus::new(1024);
    let storage = Arc::new(MemoryStorage::new());
    let engine = Arc::new(FlowEngine {
        bus: bus.clone(),
        pipeline: Pipeline::new(HookRegistry::new(), bus.clone()),
        storage: storage.clone(),
        intercept: Arc::new(InterceptState::new(false, false)),
        upstream_cfg: UpstreamCfg {
            upstream_tls_verify: false, // test origins are self-signed
            alpn_h2,
            ..Default::default()
        },
    });
    let (tx, rx) = watch::channel(false);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = Arc::new(ProxyServer {
        engine,
        ca,
        cfg: ProxyCfg {
            idle_timeout: Duration::from_secs(30),
            ..Default::default()
        },
        shutdown: rx,
    });
    tokio::spawn(server.serve(listener));
    TestProxy {
        addr,
        storage,
        bus,
        _shutdown_tx: tx,
    }
}

/// Lab client: accepts any origin cert (the test origins are self-signed).
/// ALPN is explicit per test — it decides which client leg the proxy serves.
fn client_config_alpn(alpn: &[&[u8]], _insecure: bool) -> Arc<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let versions = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap();
    let cfg = versions
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(aether_http::tls::NoVerify))
        .with_no_client_auth();
    let mut cfg = cfg;
    cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    Arc::new(cfg)
}

/// CONNECT → returns the raw tunnel.
async fn connect_tunnel(proxy: SocketAddr, authority: &str) -> TcpStream {
    let mut stream = TcpStream::connect(proxy).await.unwrap();
    let connect_req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    stream.write_all(connect_req.as_bytes()).await.unwrap();
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).await.unwrap();
    assert!(
        String::from_utf8_lossy(&buf[..n]).contains("200"),
        "CONNECT must succeed: {}",
        String::from_utf8_lossy(&buf[..n])
    );
    stream
}

fn server_name_for(authority: &str) -> ServerName<'static> {
    let host = authority
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(authority);
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        ServerName::IpAddress(ip.into())
    } else {
        ServerName::try_from(host.to_string()).unwrap()
    }
}

async fn tls_connect(
    stream: TcpStream,
    authority: &str,
    cfg: Arc<ClientConfig>,
) -> tokio_rustls::client::TlsStream<TcpStream> {
    tokio_rustls::TlsConnector::from(cfg)
        .connect(server_name_for(authority), stream)
        .await
        .unwrap()
}

/// Latest recorded flow (or None while the engine is still writing it).
async fn latest_flow(proxy: &TestProxy) -> Option<aether_core::model::Flow> {
    let page = proxy
        .storage
        .list_flows(FlowQuery {
            limit: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    let id = page.items.first().map(|s| s.id)?;
    proxy.storage.get_flow(id).await.unwrap()
}

// ── origins ────────────────────────────────────────────────────────────────

fn self_signed(name: &str) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params =
        rcgen::CertificateParams::new(vec![name.to_string(), "127.0.0.1".to_string()]).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, name);
    let cert = params.self_signed(&key).unwrap();
    (
        CertificateDer::from(cert.der().to_vec()),
        PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
            key.serialize_der(),
        )),
    )
}

fn tls_acceptor_for(name: &str, alpn: &[&[u8]]) -> tokio_rustls::TlsAcceptor {
    let (cert, key) = self_signed(name);
    let mut cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    tokio_rustls::TlsAcceptor::from(Arc::new(cfg))
}

/// An HTTP/2 origin: answers every request stream with a JSON echo.
async fn spawn_h2_origin(name: &str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = tls_acceptor_for(name, &[b"h2"]);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(stream).await else {
                    return;
                };
                let mut conn = match h2::server::handshake(tls).await {
                    Ok(c) => c,
                    Err(_) => return,
                };
                while let Some(Ok((req, mut respond))) = conn.accept().await {
                    let (parts, _) = req.into_parts();
                    let body = format!(
                        "{{\"method\":\"{}\",\"path\":\"{}\",\"origin\":\"h2\"}}",
                        parts.method,
                        parts.uri.path()
                    );
                    let resp = http::Response::builder()
                        .status(200)
                        .header("x-h2-origin", "yes")
                        .header("content-type", "application/json")
                        .body(())
                        .unwrap();
                    let Ok(mut send) = respond.send_response(resp, false) else {
                        continue;
                    };
                    let _ = send.send_data(body.into(), true);
                }
            });
        }
    });
    addr
}

/// An HTTP/1.1 origin over TLS (ALPN h1) — the h1 side of downgrade tests.
async fn spawn_h1_tls_origin(name: &str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = tls_acceptor_for(name, &[b"http/1.1"]);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(stream).await else {
                    return;
                };
                let mut reader = tokio::io::BufReader::new(tls);
                let Some(head) = wire::read_until_needle(&mut reader, b"\r\n\r\n", 64 * 1024)
                    .await
                    .ok()
                    .flatten()
                else {
                    return;
                };
                let text = String::from_utf8_lossy(&head);
                let path = text.split(' ').nth(1).unwrap_or("/").to_string();
                let body = format!("{{\"origin\":\"h1\",\"path\":\"{path}\"}}");
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nx-origin: h1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                reader.get_mut().write_all(response.as_bytes()).await.ok();
            });
        }
    });
    addr
}

/// A WebSocket origin over TLS (wss): echoes text/binary messages.
async fn spawn_wss_origin(name: &str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = tls_acceptor_for(name, &[b"http/1.1"]);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(stream).await else {
                    return;
                };
                let Ok(ws) = tokio_tungstenite::accept_async(tls).await else {
                    return;
                };
                let (mut sink, mut source) = ws.split();
                while let Some(Ok(msg)) = source.next().await {
                    match msg {
                        Message::Text(_) | Message::Binary(_) => {
                            if sink.send(msg).await.is_err() {
                                return;
                            }
                        }
                        Message::Close(_) => {
                            let _ = sink.send(Message::Close(None)).await;
                            return;
                        }
                        _ => {}
                    }
                }
            });
        }
    });
    addr
}

// ── the tests ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn h2_client_to_h2_origin_is_transparent() {
    let origin = spawn_h2_origin("h2.local").await;
    let proxy = spawn_proxy(true).await;

    let authority = format!("127.0.0.1:{}", origin.port());
    let tunnel = connect_tunnel(proxy.addr, &authority).await;
    let tls = tls_connect(tunnel, &authority, client_config_alpn(&[b"h2"], true)).await;

    let (send_request, connection) = h2::client::handshake(tls).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut send_request = send_request.ready().await.unwrap();
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://{authority}/api/thing?q=1"))
        .body(())
        .unwrap();
    let (response_fut, _send) = send_request.send_request(request, true).unwrap();
    let resp = response_fut.await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers().get("x-h2-origin").map(|v| v.as_bytes()),
        Some(&b"yes"[..])
    );
    let mut recv = resp.into_body();
    let mut body = Vec::new();
    while let Some(data) = std::future::poll_fn(|cx| recv.poll_data(cx)).await {
        let chunk = data.unwrap();
        let _ = recv.flow_control().release_capacity(chunk.len());
        body.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&body).into_owned();
    assert!(text.contains("/api/thing"), "{text}");
    assert!(text.contains("\"origin\":\"h2\""), "{text}");

    // the flow must be recorded with h2 semantics on the client leg
    let mut deadline = 30;
    let flow = loop {
        if let Some(f) = latest_flow(&proxy).await {
            if f.response.is_some() {
                break f;
            }
        }
        deadline -= 1;
        assert!(deadline > 0, "flow never recorded");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let req = flow.request.as_ref().unwrap();
    assert_eq!(req.version, aether_core::model::HttpVersion::Http2);
    assert_eq!(req.url.host, "127.0.0.1");
    assert_eq!(req.url.path, "/api/thing");
    assert_eq!(req.url.query.as_deref(), Some("q=1"));
    assert_eq!(flow.response.as_ref().unwrap().status.code, 200);
    assert_eq!(
        flow.meta.tls.as_ref().and_then(|t| t.alpn.as_deref()),
        Some("h2"),
        "client-leg ALPN recorded"
    );
}

#[tokio::test]
async fn h2_client_leg_downgrades_to_h1_upstream() {
    let origin = spawn_h1_tls_origin("h1origin.local").await;
    let proxy = spawn_proxy(true).await;

    let authority = format!("127.0.0.1:{}", origin.port());
    let tunnel = connect_tunnel(proxy.addr, &authority).await;
    let tls = tls_connect(tunnel, &authority, client_config_alpn(&[b"h2"], true)).await;

    let (send_request, connection) = h2::client::handshake(tls).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut send_request = send_request.ready().await.unwrap();
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://{authority}/down?x=2"))
        .body(())
        .unwrap();
    let (response_fut, _send) = send_request.send_request(request, true).unwrap();
    let resp = response_fut.await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers().get("x-origin").map(|v| v.as_bytes()),
        Some(&b"h1"[..])
    );
    let mut recv = resp.into_body();
    let mut body = Vec::new();
    while let Some(data) = std::future::poll_fn(|cx| recv.poll_data(cx)).await {
        let chunk = data.unwrap();
        let _ = recv.flow_control().release_capacity(chunk.len());
        body.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&body).into_owned();
    assert!(text.contains("\"origin\":\"h1\""), "{text}");

    let mut deadline = 30;
    loop {
        if let Some(f) = latest_flow(&proxy).await {
            if f.response.is_some() {
                assert_eq!(
                    f.request.unwrap().version,
                    aether_core::model::HttpVersion::Http2,
                    "client leg was h2"
                );
                return;
            }
        }
        deadline -= 1;
        assert!(deadline > 0, "flow never recorded");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn h1_client_leg_gets_h2_upstream() {
    let origin = spawn_h2_origin("h2only.local").await;
    let proxy = spawn_proxy(true).await;

    let authority = format!("127.0.0.1:{}", origin.port());
    let tunnel = connect_tunnel(proxy.addr, &authority).await;
    let mut tls = tls_connect(tunnel, &authority, client_config_alpn(&[b"http/1.1"], true)).await;

    // plain h1 request inside the tunnel (origin-form, MITM leg)
    tls.write_all(
        format!("GET /upgraded HTTP/1.1\r\nHost: {authority}\r\nconnection: close\r\n\r\n")
            .as_bytes(),
    )
    .await
    .unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), tls.read_to_end(&mut out))
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&out);
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.contains("\"origin\":\"h2\""), "{text}");

    // engine recorded the upstream leg as h2
    let mut deadline = 30;
    loop {
        if let Some(f) = latest_flow(&proxy).await {
            if let Some(res) = &f.response {
                assert_eq!(
                    res.version,
                    aether_core::model::HttpVersion::Http2,
                    "upstream leg was h2"
                );
                return;
            }
        }
        deadline -= 1;
        assert!(deadline > 0, "flow never recorded");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn websocket_upgrade_relays_and_records_session() {
    let origin = spawn_wss_origin("ws.local").await;
    let proxy = spawn_proxy(true).await;

    let authority = format!("127.0.0.1:{}", origin.port());
    let tunnel = connect_tunnel(proxy.addr, &authority).await;
    let tls = tls_connect(tunnel, &authority, client_config_alpn(&[b"http/1.1"], true)).await;

    // tungstenite performs the RFC 6455 handshake over the (already TLS)
    // tunnel — the proxy sees a normal wss upgrade on its MITM leg
    let (mut ws, resp) = tokio_tungstenite::client_async(format!("ws://{authority}/socket"), tls)
        .await
        .unwrap();
    assert_eq!(resp.status(), 101, "expected Switching Protocols");

    // text round-trip
    ws.send(Message::text("hello-from-client")).await.unwrap();
    let echo = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(echo, Message::text("hello-from-client"));

    // binary round-trip
    ws.send(Message::binary(vec![0xFFu8, 0x00, 0x42]))
        .await
        .unwrap();
    let echo = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(echo, Message::binary(vec![0xFFu8, 0x00, 0x42]));

    // close both directions; the relay ends and the session is persisted
    ws.close(None).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), ws.next()).await;

    // find the 101 flow and wait for its session record
    let mut deadline = 60;
    loop {
        let page = proxy
            .storage
            .list_flows(FlowQuery {
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        let mut done = false;
        for summary in &page.items {
            if summary.status == Some(101) {
                let flow = proxy.storage.get_flow(summary.id).await.unwrap().unwrap();
                if let Some(session) = &flow.ws {
                    assert!(flow.meta.tags.iter().any(|t| t == "websocket"));
                    assert!(session.total_messages >= 4, "{:?}", session);
                    let kinds: Vec<_> = session
                        .messages
                        .iter()
                        .map(|m| (m.direction, m.kind))
                        .collect();
                    assert!(kinds.contains(&(
                        aether_core::model::WsDirection::ToServer,
                        aether_core::model::WsKind::Text
                    )));
                    assert!(kinds.contains(&(
                        aether_core::model::WsDirection::ToClient,
                        aether_core::model::WsKind::Text
                    )));
                    assert!(kinds.contains(&(
                        aether_core::model::WsDirection::ToServer,
                        aether_core::model::WsKind::Binary
                    )));
                    let hello = session
                        .messages
                        .iter()
                        .find(|m| m.text.as_deref() == Some("hello-from-client"))
                        .expect("recorded text preview");
                    assert_eq!(hello.size, 17);
                    done = true;
                }
            }
        }
        if done {
            return;
        }
        deadline -= 1;
        assert!(deadline > 0, "ws session never persisted");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn h2_connection_survives_multiple_streams() {
    // two requests on ONE h2 client connection — proves the per-connection
    // serve loop multiplexes instead of treating the tunnel as one shot
    let origin = spawn_h2_origin("multi.local").await;
    let proxy = spawn_proxy(true).await;

    let authority = format!("127.0.0.1:{}", origin.port());
    let tunnel = connect_tunnel(proxy.addr, &authority).await;
    let tls = tls_connect(tunnel, &authority, client_config_alpn(&[b"h2"], true)).await;

    let (send_request, connection) = h2::client::handshake(tls).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut send_request = send_request.ready().await.unwrap();

    for path in ["/one", "/two"] {
        let request = http::Request::builder()
            .method("GET")
            .uri(format!("https://{authority}{path}"))
            .body(())
            .unwrap();
        let (response_fut, _send) = send_request.send_request(request, true).unwrap();
        let resp = response_fut.await.unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let mut recv = resp.into_body();
        let mut body = Vec::new();
        while let Some(data) = std::future::poll_fn(|cx| recv.poll_data(cx)).await {
            let chunk = data.unwrap();
            let _ = recv.flow_control().release_capacity(chunk.len());
            body.extend_from_slice(&chunk);
        }
        assert!(String::from_utf8_lossy(&body).contains(path), "{path}");
    }

    let mut deadline = 40;
    loop {
        let page = proxy
            .storage
            .list_flows(FlowQuery {
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap();
        if page.items.len() >= 2 {
            let paths: Vec<String> = page.items.iter().map(|s| s.path.clone()).collect();
            assert!(paths.iter().any(|p| p == "/one"), "{paths:?}");
            assert!(paths.iter().any(|p| p == "/two"), "{paths:?}");
            return;
        }
        deadline -= 1;
        assert!(deadline > 0, "both flows never recorded");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn connect_peek_serves_plaintext_upgrade() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // a minimal plaintext WS echo origin (hand-rolled handshake)
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut stream = stream;
                let mut buf = vec![0u8; 4096];
                let n = stream.peek(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                if head.contains("Sec-WebSocket-Key:") {
                    let key = head
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("sec-websocket-key:"))
                        .and_then(|l| l.split(':').nth(1))
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    // fixed GUID concat without a SHA-1 impl: respond 101 with
                    // a static accept — tungstenite-style clients verify it,
                    // but here we only assert the proxy relayed the handshake
                    let _ = key;
                    let resp = String::from(
                        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: x\r\n\r\n",
                    );
                    let _ = stream.try_write(resp.as_bytes());
                    // then echo raw frames byte-for-byte (server side)
                    loop {
                        match tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => {
                                let _ = stream.try_write(&buf[..n]);
                            }
                        }
                    }
                }
            });
        }
    });

    let proxy = spawn_proxy(true).await;
    let authority = format!("127.0.0.1:{}", origin.port());

    // CONNECT, then a plaintext upgrade inside the tunnel (Chrome-style)
    let mut tunnel = connect_tunnel(proxy.addr, &authority).await;
    tunnel
        .write_all(format!("GET /ws HTTP/1.1\r\nHost: {authority}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut resp = vec![0u8; 1024];
    let n = tunnel.read(&mut resp).await.unwrap();
    let text = String::from_utf8_lossy(&resp[..n]).into_owned();
    assert!(
        text.starts_with("HTTP/1.1 101"),
        "proxy must relay the plaintext upgrade: {text}"
    );

    // the flow is recorded: 101 + websocket tag on the plaintext leg
    let mut deadline = 30;
    loop {
        let page = proxy
            .storage
            .list_flows(FlowQuery {
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        for summary in &page.items {
            if summary.status == Some(101) {
                let flow = proxy.storage.get_flow(summary.id).await.unwrap().unwrap();
                assert!(flow.meta.tags.iter().any(|t| t == "websocket"));
                assert_eq!(
                    flow.meta.client_proto,
                    aether_core::model::TransportProto::Plaintext
                );
                return;
            }
        }
        deadline -= 1;
        assert!(deadline > 0, "upgrade flow never recorded");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ── V0.2.x acceptance tests (ADR-018) ──────────────────────────────────────
// The word "shipped" must have a test suite behind it: stream cancellation,
// graceful GOAWAY, malformed-frame resilience, and control-frame/megabyte
// relay honesty.

/// Waits for the latest *completed* flow (response recorded).
async fn completed_flow(proxy: &TestProxy) -> aether_core::model::Flow {
    let mut deadline = 60;
    loop {
        let page = proxy
            .storage
            .list_flows(FlowQuery {
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        for summary in &page.items {
            if summary.status.is_some() {
                let flow = proxy.storage.get_flow(summary.id).await.unwrap().unwrap();
                if flow.response.is_some() {
                    return flow;
                }
            }
        }
        deadline -= 1;
        assert!(deadline > 0, "flow never completed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn h2_stream_reset_is_handled_and_connection_survives() {
    let origin = spawn_h2_origin("reset.local").await;
    let proxy = spawn_proxy(true).await;

    let authority = format!("127.0.0.1:{}", origin.port());
    let tunnel = connect_tunnel(proxy.addr, &authority).await;
    let tls = tls_connect(tunnel, &authority, client_config_alpn(&[b"h2"], true)).await;

    let (send_request, connection) = h2::client::handshake(tls).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });

    // stream 1: a healthy request through the proxy
    let mut send_request = send_request.ready().await.unwrap();
    let req1 = http::Request::builder()
        .method("GET")
        .uri(format!("https://{authority}/first"))
        .body(())
        .unwrap();
    let (resp1, _stream) = send_request.send_request(req1, true).unwrap();
    assert_eq!(resp1.await.unwrap().status().as_u16(), 200);

    // stream 2: the client cancels mid-flight — dropping the response future
    // and its send stream makes h2 emit RST_STREAM(CANCEL); the daemon must
    // survive the abandoned stream without poisoning anything
    let mut send_request = send_request.ready().await.unwrap();
    let req2 = http::Request::builder()
        .method("GET")
        .uri(format!("https://{authority}/cancelled"))
        .body(())
        .unwrap();
    let (resp2, send_stream) = send_request.send_request(req2, true).unwrap();
    drop(resp2);
    drop(send_stream);
    // give the reset a moment to travel; its outcome must not matter
    tokio::time::sleep(Duration::from_millis(300)).await;

    // stream 3: the same connection still serves a full exchange
    let mut send_request = send_request.ready().await.unwrap();
    let req3 = http::Request::builder()
        .method("GET")
        .uri(format!("https://{authority}/after-reset"))
        .body(())
        .unwrap();
    let (resp3, _stream) = send_request.send_request(req3, true).unwrap();
    let resp3 = resp3.await.unwrap();
    assert_eq!(resp3.status().as_u16(), 200, "connection survived a reset");
    let mut recv = resp3.into_body();
    while let Some(data) = std::future::poll_fn(|cx| recv.poll_data(cx)).await {
        let _ = data.unwrap();
    }

    // the clean exchange is recorded with h2 wire facts on both legs (ADR-015)
    let flow = completed_flow(&proxy).await;
    assert_eq!(flow.request.as_ref().unwrap().url.path, "/after-reset");
    let m = &flow.meta;
    assert_eq!(
        m.forwarded_req_framing,
        Some(aether_core::model::Framing::H2Streams)
    );
    assert_eq!(
        m.observed_res_framing,
        Some(aether_core::model::Framing::H2Streams)
    );
    assert_eq!(
        m.forwarded_res_framing,
        Some(aether_core::model::Framing::H2Streams)
    );
    assert_eq!(
        m.observed_req_framing, None,
        "bare GET carries no body framing"
    );
}

#[tokio::test]
async fn h2_shutdown_sends_graceful_goaway() {
    let origin = spawn_h2_origin("goaway.local").await;
    let proxy = spawn_proxy(true).await;

    let authority = format!("127.0.0.1:{}", origin.port());
    let tunnel = connect_tunnel(proxy.addr, &authority).await;
    let tls = tls_connect(tunnel, &authority, client_config_alpn(&[b"h2"], true)).await;

    let (send_request, connection) = h2::client::handshake(tls).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });

    // start a request and trigger shutdown WHILE IT IS IN FLIGHT — the whole
    // point of graceful GOAWAY: in-flight work finishes, new work is refused
    let mut send_request = send_request.ready().await.unwrap();
    let req1 = http::Request::builder()
        .method("GET")
        .uri(format!("https://{authority}/in-flight"))
        .body(())
        .unwrap();
    let (resp1, _stream) = send_request.send_request(req1, true).unwrap();
    proxy._shutdown_tx.send(true).unwrap();
    let resp1 = tokio::time::timeout(Duration::from_secs(5), resp1)
        .await
        .expect("in-flight stream died on shutdown — shutdown is not graceful")
        .unwrap();
    assert_eq!(
        resp1.status().as_u16(),
        200,
        "an in-flight stream must complete across a graceful shutdown"
    );

    // after the drain the connection must terminate: any subsequent attempt
    // resolves without ever being served a fresh 200
    let outcome = tokio::time::timeout(Duration::from_secs(6), async {
        match send_request.ready().await {
            Err(_) => (), // connection closed — the graceful outcome
            Ok(mut sr) => {
                let req = http::Request::builder()
                    .method("GET")
                    .uri(format!("https://{authority}/after-goaway"))
                    .body(())
                    .unwrap();
                match sr.send_request(req, true) {
                    Err(_) => (), // refused by the going-away connection
                    Ok((fut, _)) => {
                        if let Ok(resp) = fut.await {
                            assert_ne!(
                                resp.status().as_u16(),
                                200,
                                "a new stream was served after graceful shutdown"
                            );
                        }
                    }
                }
            }
        }
    })
    .await;
    assert!(outcome.is_ok(), "client hung after graceful shutdown");
}

#[tokio::test]
async fn malformed_h2_bytes_are_rejected_and_daemon_survives() {
    let origin = spawn_h2_origin("fuzz.local").await;
    let proxy = spawn_proxy(true).await;
    let authority = format!("127.0.0.1:{}", origin.port());

    // connection 1: an HTTP/1.1 request where an h2 preface belongs — the
    // server's h2 handshake fails, the connection dies, the daemon lives
    let tunnel = connect_tunnel(proxy.addr, &authority).await;
    let mut tls = tls_connect(tunnel, &authority, client_config_alpn(&[b"h2"], true)).await;
    use tokio::io::AsyncWriteExt as _;
    tls.write_all(b"GET / HTTP/1.1\r\nHost: fuzz.local\r\n\r\n")
        .await
        .unwrap();
    let _ = tls.shutdown().await;
    let _ = tls.read_to_end(&mut Vec::new()).await; // EOF, not a daemon crash

    // connection 2: a completely normal exchange afterwards
    let tunnel = connect_tunnel(proxy.addr, &authority).await;
    let tls = tls_connect(tunnel, &authority, client_config_alpn(&[b"h2"], true)).await;
    let (send_request, connection) = h2::client::handshake(tls).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut send_request = send_request.ready().await.unwrap();
    let req = http::Request::builder()
        .method("GET")
        .uri(format!("https://{authority}/still-alive"))
        .body(())
        .unwrap();
    let (resp, _stream) = send_request.send_request(req, true).unwrap();
    assert_eq!(
        resp.await.unwrap().status().as_u16(),
        200,
        "daemon survived garbage bytes"
    );

    let flow = completed_flow(&proxy).await;
    assert_eq!(flow.request.as_ref().unwrap().url.path, "/still-alive");
}

#[tokio::test]
async fn ws_relay_forwards_control_frames_and_megabyte_messages() {
    let origin = spawn_wss_origin("bigws.local").await;
    let proxy = spawn_proxy(true).await;

    let authority = format!("127.0.0.1:{}", origin.port());
    let tunnel = connect_tunnel(proxy.addr, &authority).await;
    let tls = tls_connect(tunnel, &authority, client_config_alpn(&[b"http/1.1"], true)).await;
    let (mut ws, resp) = tokio_tungstenite::client_async(format!("ws://{authority}/big"), tls)
        .await
        .unwrap();
    assert_eq!(resp.status(), 101);

    // control frames cross the relay untouched (ping → origin auto-pong)
    ws.send(Message::Ping(vec![7, 7, 7].into())).await.unwrap();
    ws.send(Message::text("after-ping")).await.unwrap(); // flushes the queued pong
    let mut got_pong = false;
    let mut got_text = false;
    for _ in 0..2 {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("ping/text round trip hung")
            .unwrap()
            .unwrap();
        match msg {
            Message::Pong(p) => {
                assert_eq!(p, vec![7, 7, 7], "pong payload must survive the relay");
                got_pong = true;
            }
            Message::Text(t) => {
                assert_eq!(t, "after-ping");
                got_text = true;
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert!(got_pong && got_text);

    // a 1 MiB binary message relays byte-for-byte — proof the relay streams
    // instead of staging the whole message in our process (backpressure)
    let megabyte: Vec<u8> = (0..=255u8).cycle().take(1024 * 1024).collect();
    ws.send(Message::binary(megabyte.clone())).await.unwrap();
    let echo = tokio::time::timeout(Duration::from_secs(30), ws.next())
        .await
        .expect("1MB echo hung — relay is staging, not streaming")
        .unwrap()
        .unwrap();
    match echo {
        Message::Binary(b) => assert_eq!(b, megabyte, "1MB relay corrupted bytes"),
        other => panic!("expected binary echo, got {other:?}"),
    }

    // a 128 KiB message lands under the 256 KiB record cap: the session must
    // count its TRUE size while the preview stays bounded (ADR-011 honesty)
    let kib128: Vec<u8> = (0..=255u8).cycle().take(128 * 1024).collect();
    ws.send(Message::binary(kib128.clone())).await.unwrap();
    let echo = tokio::time::timeout(Duration::from_secs(30), ws.next())
        .await
        .expect("128KB echo hung")
        .unwrap()
        .unwrap();
    match echo {
        Message::Binary(b) => assert_eq!(b, kib128),
        other => panic!("expected binary echo, got {other:?}"),
    }

    ws.close(None).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), ws.next()).await;

    // the recorded session: true sizes counted, 101 framing recorded
    let mut deadline = 60;
    loop {
        let page = proxy
            .storage
            .list_flows(FlowQuery {
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        let mut done = false;
        for summary in &page.items {
            if summary.status == Some(101) {
                let flow = proxy.storage.get_flow(summary.id).await.unwrap().unwrap();
                assert_eq!(
                    flow.meta.forwarded_res_framing,
                    Some(aether_core::model::Framing::Upgrade),
                    "101 Switching Protocols is the client-leg framing (ADR-015)"
                );
                if let Some(session) = &flow.ws {
                    // the 128KB message: recorded at its TRUE size, preview bounded
                    let kib128 = session.messages.iter().find(|m| {
                        m.kind == aether_core::model::WsKind::Binary
                            && m.size == kib128.len() as u64
                    });
                    assert!(
                        kib128.is_some(),
                        "128KB message counted at true size: {:?}",
                        session.messages
                    );
                    let kib128 = kib128.unwrap();
                    assert!(
                        kib128.truncated || kib128.base64.as_ref().is_none_or(|b| b.len() <= 4096),
                        "preview must stay bounded (ADR-011)"
                    );
                    // the 1MB message crossed the relay and was counted
                    assert!(
                        session.total_messages >= 6,
                        "control + text + 1MB + 128KB messages counted: {:?}",
                        session
                    );
                    done = true;
                }
            }
        }
        if done {
            return;
        }
        deadline -= 1;
        assert!(deadline > 0, "ws session never persisted");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

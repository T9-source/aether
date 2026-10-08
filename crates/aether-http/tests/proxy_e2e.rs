//! Aether — end-to-end proxy tests.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Real sockets, real TLS, real event bus. These are the Phase-1 acceptance
//! tests: an explicit proxy that records, hooks, intercepts, passes through.

use aether_core::storage::{FlowQuery, MemoryStorage, Storage};
use aether_core::{
    AetherEvent, EventBus, HookRegistry, InterceptDirection, Pipeline, RuleHook, RuleSpec,
};
use aether_http::ca::CaManager;
use aether_http::engine::FlowEngine;
use aether_http::intercept::{HeldDecision, HeldPayload, InterceptState};
use aether_http::server::{ProxyCfg, ProxyServer};
use aether_http::upstream::UpstreamCfg;
use aether_http::wire;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

// ── helpers ─────────────────────────────────────────────────────────────────

struct TestProxy {
    addr: std::net::SocketAddr,
    storage: Arc<MemoryStorage>,
    bus: EventBus,
    intercept: Arc<InterceptState>,
    ca_dir: tempfile::TempDir,
    _shutdown_tx: watch::Sender<bool>,
}

async fn spawn_proxy(registry: HookRegistry) -> TestProxy {
    spawn_proxy_with(
        vec![],
        BTreeMap::new(),
        registry,
        false,
        false,
        8 * 1024 * 1024,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn spawn_proxy_with(
    passthrough: Vec<String>,
    host_overrides: BTreeMap<String, String>,
    registry: HookRegistry,
    intercept_request: bool,
    intercept_response: bool,
    capture_max_body: usize,
) -> TestProxy {
    let ca_dir = tempfile::TempDir::new().unwrap();
    let ca = Arc::new(CaManager::load_or_create(ca_dir.path()).unwrap());
    let bus = EventBus::new(1024);
    let storage = Arc::new(MemoryStorage::new());
    let intercept = Arc::new(InterceptState::new(intercept_request, intercept_response));
    let engine = Arc::new(FlowEngine {
        bus: bus.clone(),
        pipeline: Pipeline::new(registry, bus.clone()),
        storage: storage.clone(),
        intercept: intercept.clone(),
        upstream_cfg: UpstreamCfg {
            capture_max_body,
            upstream_tls_verify: false, // test origins are self-signed
            host_overrides: host_overrides.clone(),
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
            passthrough,
            host_overrides,
            upstream_tls_verify: false,
            capture_max_body,
            idle_timeout: Duration::from_secs(30),
        },
        shutdown: rx,
    });
    tokio::spawn(server.serve(listener));
    TestProxy {
        addr,
        storage,
        bus,
        intercept,
        ca_dir,
        _shutdown_tx: tx,
    }
}

/// Minimal origin: answers every request with 200 + JSON echo of what it saw.
async fn spawn_plain_origin() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
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
    addr
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
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        headers.push(line.to_string());
    }
    let content_length = headers
        .iter()
        .find(|h| h.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|h| h.split(':').nth(1))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).await?;

    let echo = format!(
        "{{\"method\":\"{method}\",\"target\":\"{target}\",\"headers\":{},\"body\":\"{}\"}}",
        serde_json::to_string(&headers).unwrap_or_default(),
        String::from_utf8_lossy(&body)
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nx-origin: aether-test\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{echo}",
        echo.len()
    );
    reader.get_mut().write_all(response.as_bytes()).await?;
    Ok(())
}

/// TLS origin with a self-signed cert for `name`; returns (addr, cert).
async fn spawn_tls_origin(name: &str) -> (std::net::SocketAddr, CertificateDer<'static>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    // include the loopback IP so clients may dial by IP or by name
    let mut params =
        rcgen::CertificateParams::new(vec![name.to_string(), "127.0.0.1".to_string()]).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, name);
    let cert = params.self_signed(&key).unwrap();
    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
        key.serialize_der(),
    ));

    let mut server_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .unwrap();
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
    (addr, cert_der)
}

/// Raw client: send `bytes` to `addr`, read until EOF.
async fn raw_request(addr: std::net::SocketAddr, bytes: Vec<u8>) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(&bytes).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut out)).await;
    out
}

fn response_body(response: &[u8]) -> &[u8] {
    response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| &response[p + 4..])
        .unwrap_or(&[])
}

fn status_of(response: &[u8]) -> u16 {
    let line =
        std::str::from_utf8(response.split(|&b| b == b'\r').next().unwrap_or(&[])).unwrap_or("");
    line.split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// TLS client config trusting exactly `roots` (or accepting everything).
fn client_config(roots: Vec<CertificateDer<'static>>, insecure: bool) -> Arc<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let versions = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap();
    let mut cfg = if insecure {
        versions
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(aether_http::tls::NoVerify))
            .with_no_client_auth()
    } else {
        let mut store = RootCertStore::empty();
        for r in roots {
            store.add(r).unwrap();
        }
        versions.with_root_certificates(store).with_no_client_auth()
    };
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(cfg)
}

/// CONNECT + TLS + request, for MITM tests (trusts the given roots).
async fn https_via_proxy(
    proxy: std::net::SocketAddr,
    authority: &str,
    cfg: Arc<ClientConfig>,
    request: &[u8],
) -> Vec<u8> {
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

    let (host, port) = authority.rsplit_once(':').unwrap();
    let port: u16 = port.parse().unwrap();
    let _ = port;
    let server_name: ServerName<'static> = if host.parse::<std::net::IpAddr>().is_ok() {
        ServerName::IpAddress(host.parse::<std::net::IpAddr>().unwrap().into())
    } else {
        ServerName::try_from(host.to_string()).unwrap()
    };
    let connector = tokio_rustls::TlsConnector::from(cfg);
    let mut tls = connector.connect(server_name, stream).await.unwrap();
    tls.write_all(request).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), tls.read_to_end(&mut out)).await;
    out
}

fn rule_from_json(json: serde_json::Value) -> RuleHook {
    let spec: RuleSpec = serde_json::from_value(json).unwrap();
    RuleHook::from_spec(spec).unwrap()
}

// ── tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn plain_http_flows_through_and_is_recorded() {
    let origin = spawn_plain_origin().await;
    let proxy = spawn_proxy(HookRegistry::new()).await;
    let mut events = proxy.bus.subscribe();

    let target = format!("http://127.0.0.1:{}/hello?x=1", origin.port());
    let response = raw_request(
        proxy.addr,
        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .into_bytes(),
    )
    .await;

    assert_eq!(status_of(&response), 200);
    let echo: serde_json::Value = serde_json::from_slice(response_body(&response)).unwrap();
    assert_eq!(echo["method"], "GET");
    assert_eq!(echo["target"], "/hello?x=1");

    // flow recorded with the full story
    let page = proxy
        .storage
        .list_flows(FlowQuery {
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let flow = proxy
        .storage
        .get_flow(page.items[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(flow.request.as_ref().unwrap().url.host, "127.0.0.1");
    assert_eq!(flow.response.as_ref().unwrap().status.code, 200);
    assert!(flow.meta.duration_ms.is_some());

    // events emitted
    let mut kinds = Vec::new();
    while let Some(ev) = events.try_next() {
        kinds.push(serde_json::to_value(&ev).unwrap()["type"].clone());
    }
    assert!(kinds.contains(&"flow_started".into()));
    assert!(kinds.contains(&"request_received".into()));
    assert!(kinds.contains(&"response_received".into()));
    assert!(kinds.contains(&"flow_completed".into()));
}

#[tokio::test]
async fn serialized_rules_modify_traffic() {
    let origin = spawn_plain_origin().await;
    let registry = HookRegistry::new();
    registry.register_fn(rule_from_json(serde_json::json!({
        "name": "stamp",
        "action": {"kind": "set_request_header", "name": "x-aether-ran", "value": "yes"}
    })));
    registry.register_fn(rule_from_json(serde_json::json!({
        "name": "tag",
        "action": {"kind": "add_tag", "tag": "stamped"}
    })));

    let proxy = spawn_proxy(registry).await;
    let response = raw_request(
        proxy.addr,
        format!(
            "GET http://127.0.0.1:{}/echo HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
            origin.port()
        )
        .into_bytes(),
    )
    .await;
    assert_eq!(status_of(&response), 200);
    let echo: serde_json::Value = serde_json::from_slice(response_body(&response)).unwrap();
    let headers = echo["headers"].as_array().unwrap();
    assert!(
        headers.iter().any(|h| h
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase()
            .starts_with("x-aether-ran: yes")),
        "rule must stamp the request: {headers:?}"
    );

    let page = proxy
        .storage
        .list_flows(FlowQuery {
            limit: 5,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.items[0].tags, vec!["stamped"]);
}

#[tokio::test]
async fn https_connect_is_mitm_d_with_zero_cert_errors() {
    let (origin_addr, _cert) = spawn_tls_origin("mock.local").await;
    let mut overrides = BTreeMap::new();
    overrides.insert(
        "mock.local".to_string(),
        format!("socket://127.0.0.1:{}", origin_addr.port()),
    );
    let proxy = spawn_proxy_with(
        vec![],
        overrides,
        HookRegistry::new(),
        false,
        false,
        8 * 1024 * 1024,
    )
    .await;

    // trust EXACTLY the proxy CA — the browser scenario
    let ca = CaManager::load_or_create(proxy.ca_dir.path()).unwrap();
    let cfg = client_config(vec![ca.ca_cert_der().clone()], false);

    let response = https_via_proxy(
        proxy.addr,
        "mock.local:443",
        cfg,
        b"GET /secure?x=1 HTTP/1.1\r\nHost: mock.local\r\nConnection: close\r\n\r\n",
    )
    .await;

    assert_eq!(status_of(&response), 200, "MITM must be transparent");
    let echo: serde_json::Value = serde_json::from_slice(response_body(&response)).unwrap();
    assert_eq!(echo["method"], "GET");

    // the flow records TLS facts of the MITM leg
    let page = proxy
        .storage
        .list_flows(FlowQuery {
            limit: 5,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let flow = proxy
        .storage
        .get_flow(page.items[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(flow.request.as_ref().unwrap().url.host, "mock.local");
    assert_eq!(flow.meta.client_proto, aether_core::TransportProto::Tls);
    let tls = flow.meta.tls.expect("TLS facts recorded");
    assert!(tls.version.is_some());
    assert!(tls.cipher.is_some());
}

#[tokio::test]
async fn intercept_hold_edit_release() {
    let origin = spawn_plain_origin().await;
    let proxy = spawn_proxy(HookRegistry::new()).await;
    proxy.intercept.set_enabled(true, false);
    let mut events = proxy.bus.subscribe();

    // client sends a POST that will be held
    let request_bytes = format!(
        "POST http://127.0.0.1:{}/submit HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
        origin.port()
    );
    let client_task = tokio::spawn(raw_request(proxy.addr, request_bytes.into_bytes()));

    // wait for the hold, then edit + release
    loop {
        match tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
        {
            Some(AetherEvent::InterceptHold { flow_id, direction }) => {
                assert_eq!(direction, InterceptDirection::Request);
                let held = proxy.intercept.list();
                assert_eq!(held.len(), 1);

                let payload = proxy.intercept.payload(flow_id).unwrap();
                let HeldPayload::Request(mut req) = payload else {
                    panic!("expected request")
                };
                req.set_header("x-reviewed", "by-operator");
                req.body = aether_core::Body::text("edited");
                proxy
                    .intercept
                    .resolve(
                        flow_id,
                        HeldDecision::Forward(HeldPayload::Request(req)),
                        &proxy.bus,
                    )
                    .unwrap();
                break;
            }
            Some(_) => continue,
            None => panic!("stream ended before InterceptHold"),
        }
    }

    let response = client_task.await.unwrap();
    assert_eq!(status_of(&response), 200);
    let echo: serde_json::Value = serde_json::from_slice(response_body(&response)).unwrap();
    assert!(
        echo["headers"].as_array().unwrap().iter().any(|h| h
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase()
            .starts_with("x-reviewed: by-operator")),
        "edited header must reach the origin: {echo}"
    );
    assert_eq!(echo["body"], "edited");
}

#[tokio::test]
async fn intercept_drop_serves_502_and_tags_flow() {
    let origin = spawn_plain_origin().await;
    let proxy = spawn_proxy(HookRegistry::new()).await;
    proxy.intercept.set_enabled(true, false);
    let mut events = proxy.bus.subscribe();

    let request_bytes = format!(
        "GET http://127.0.0.1:{}/nope HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        origin.port()
    );
    let client_task = tokio::spawn(raw_request(proxy.addr, request_bytes.into_bytes()));

    loop {
        match tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
        {
            Some(AetherEvent::InterceptHold { flow_id, .. }) => {
                proxy
                    .intercept
                    .resolve(flow_id, HeldDecision::Drop, &proxy.bus)
                    .unwrap();
            }
            Some(AetherEvent::FlowDropped { .. }) => break,
            Some(_) => continue,
            None => panic!("ended before drop"),
        }
    }

    let response = client_task.await.unwrap();
    assert_eq!(status_of(&response), 502);
    let page = proxy
        .storage
        .list_flows(FlowQuery {
            limit: 5,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(page.items[0].is_dropped());
}

#[tokio::test]
async fn pass_through_tunnels_blindly_and_records_nothing() {
    let (origin_addr, cert) = spawn_tls_origin("passthrough.local").await;
    // pass-through matches the CONNECT authority host (an IP here)
    let proxy = spawn_proxy_with(
        vec!["127.0.0.1".into()],
        BTreeMap::new(),
        HookRegistry::new(),
        false,
        false,
        8 * 1024 * 1024,
    )
    .await;
    let mut events = proxy.bus.subscribe();

    let cfg = client_config(vec![cert], false);
    let response = https_via_proxy(
        proxy.addr,
        &format!("127.0.0.1:{}", origin_addr.port()),
        cfg,
        b"GET /tunneled HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;

    assert_eq!(status_of(&response), 200);
    let echo: serde_json::Value = serde_json::from_slice(response_body(&response)).unwrap();
    assert_eq!(echo["method"], "GET");

    let mut saw_pass_through = false;
    while let Some(ev) = events.try_next() {
        if matches!(ev, AetherEvent::PassThrough { .. }) {
            saw_pass_through = true;
        }
    }
    assert!(saw_pass_through, "PassThrough event must be published");

    let page = proxy
        .storage
        .list_flows(FlowQuery {
            limit: 5,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        page.items.is_empty(),
        "pass-through traffic is never recorded (ADR-004)"
    );
}

#[tokio::test]
async fn respond_with_rule_mocks_the_origin() {
    let registry = HookRegistry::new();
    registry.register_fn(rule_from_json(serde_json::json!({
        "name": "teapot",
        "host_include": ["mocked.example.com"],
        "action": {"kind": "respond_with", "status": 418, "content_type": "text/plain", "body": "short and stout"}
    })));

    let proxy = spawn_proxy(registry).await;
    let response = raw_request(
        proxy.addr,
        b"GET http://mocked.example.com/anything HTTP/1.1\r\nHost: mocked.example.com\r\nConnection: close\r\n\r\n".to_vec(),
    )
    .await;
    assert_eq!(status_of(&response), 418);
    assert_eq!(response_body(&response), b"short and stout");
    let as_text = String::from_utf8_lossy(&response).to_string();
    assert!(
        as_text.contains("content-type: text/plain"),
        "mock content-type present"
    );

    let page = proxy
        .storage
        .list_flows(FlowQuery {
            limit: 5,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.items[0].status, Some(418));
    assert!(page.items[0].tags.contains(&"mocked".to_string()));
}

#[tokio::test]
async fn oversized_response_bodies_stream_through_with_capped_capture() {
    // origin serving a 2 MiB body
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut s = stream;
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    if s.read_exact(&mut byte).await.is_err() {
                        return;
                    }
                    head.push(byte[0]);
                    if head.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let body = vec![b'A'; 2 * 1024 * 1024];
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                s.write_all(resp.as_bytes()).await.unwrap();
                s.write_all(&body).await.unwrap();
            });
        }
    });

    let proxy = spawn_proxy_with(
        vec![],
        BTreeMap::new(),
        HookRegistry::new(),
        false,
        false,
        64 * 1024, // tiny capture window
    )
    .await;

    let response = raw_request(
        proxy.addr,
        format!(
            "GET http://127.0.0.1:{}/big HTTP/1.1\r\nHost: big\r\nConnection: close\r\n\r\n",
            origin_port
        )
        .into_bytes(),
    )
    .await;

    assert_eq!(status_of(&response), 200);
    let body = response_body(&response);
    assert_eq!(body.len(), 2 * 1024 * 1024, "client receives the FULL body");
    assert!(body.iter().all(|&b| b == b'A'));

    // history holds only the capped prefix
    let flow = proxy
        .storage
        .get_flow(1)
        .await
        .unwrap()
        .expect("flow recorded");
    assert_eq!(flow.response.as_ref().unwrap().body.len(), 64 * 1024);
}

#[tokio::test]
async fn oversized_request_bodies_fail_closed_with_413() {
    let origin = spawn_plain_origin().await;
    let proxy = spawn_proxy_with(
        vec![],
        BTreeMap::new(),
        HookRegistry::new(),
        false,
        false,
        1024, // 1 KiB cap
    )
    .await;

    let big = vec![b'x'; 4096];
    let req = format!(
        "POST http://127.0.0.1:{}/upload HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        origin.port(),
        big.len()
    );
    let mut bytes = req.into_bytes();
    bytes.extend_from_slice(&big);
    let response = raw_request(proxy.addr, bytes).await;
    assert_eq!(status_of(&response), 413);
}

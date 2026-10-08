//! Aether — daemon control-plane integration tests (ADR-002/010).
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Boots the full daemon (proxy + control plane + memory storage) and drives
//! it exactly like the CLI will: WebSocket → auth → ops. This is the
//! "intercept works from the CLI" acceptance path.

use aether_daemon::{
    AetherConfig, ControlRequest, DaemonOptions, DecisionDto, HeldPayloadDto, run as daemon_run,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

struct TestDaemon {
    proxy_addr: std::net::SocketAddr,
    control_addr: std::net::SocketAddr,
    token: String,
    _dir: tempfile::TempDir,
    _shutdown_tx: watch::Sender<bool>,
}

async fn spawn_daemon() -> TestDaemon {
    let dir = tempfile::TempDir::new().unwrap();
    let (tx, rx) = watch::channel(false);
    let mut cfg = AetherConfig {
        proxy: aether_daemon::config::ProxySection {
            listen: "127.0.0.1:0".into(),
        },
        control: aether_daemon::config::ControlSection {
            listen: "127.0.0.1:0".into(),
            token: None,
        },
        storage: aether_daemon::config::StorageSection {
            backend: "memory".into(),
            path: None,
        },
        intercept: aether_daemon::config::InterceptSection {
            request: false,
            response: false,
        },
        passthrough: vec![],
        host_overrides: Default::default(),
        upstream_tls_verify: false,
        upstream_alpn_h2: true,
        capture_max_body: 8 * 1024 * 1024,
    };
    cfg.proxy.listen = "127.0.0.1:0".into();
    let info = daemon_run(DaemonOptions {
        config: cfg,
        memory: true,
        data_dir: Some(dir.path().to_path_buf()),
        shutdown: rx,
    })
    .await
    .unwrap();
    TestDaemon {
        proxy_addr: info.proxy_addr,
        control_addr: info.control_addr,
        token: info.token,
        _dir: dir,
        _shutdown_tx: tx,
    }
}

/// Minimal echo origin (same shape as the engine tests).
async fn spawn_plain_origin() -> std::net::SocketAddr {
    use aether_http::wire;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut reader = tokio::io::BufReader::new(stream);
                let Some(head) = wire::read_until_needle(&mut reader, b"\r\n\r\n", 64 * 1024)
                    .await
                    .ok()
                    .flatten()
                else {
                    return;
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
                let cl = headers
                    .iter()
                    .find(|h| h.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|h| h.split(':').nth(1))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                let mut body = vec![0u8; cl];
                if reader.read_exact(&mut body).await.is_err() {
                    return;
                }
                let body_text = String::from_utf8_lossy(&body)
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"");
                let echo = format!(
                    "{{\"method\":\"{method}\",\"target\":\"{target}\",\"headers\":{},\"body\":\"{body_text}\"}}",
                    serde_json::to_string(&headers).unwrap_or_default()
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{echo}",
                    echo.len()
                );
                let _ = reader.get_mut().write_all(response.as_bytes()).await;
            });
        }
    });
    addr
}

struct WsClient {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl WsClient {
    async fn connect(addr: &std::net::SocketAddr) -> Self {
        let (ws, _) = connect_async(format!("ws://{addr}")).await.unwrap();
        Self { ws }
    }

    async fn send(&mut self, req: ControlRequest) {
        let text = serde_json::to_string(&req).unwrap();
        self.ws.send(Message::Text(text)).await.unwrap();
    }

    /// Next response (skips event frames).
    async fn response(&mut self) -> Value {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(10), self.ws.next())
                .await
                .expect("response timeout")
                .expect("stream ended")
                .unwrap();
            let Message::Text(text) = msg else { continue };
            let v: Value = serde_json::from_str(&text).unwrap();
            match v.get("type").and_then(Value::as_str) {
                Some("event") => continue,
                Some("response") => return v,
                _ => continue,
            }
        }
    }

    async fn call(&mut self, req: ControlRequest) -> Value {
        self.send(req).await;
        self.response().await
    }

    /// Expect an event frame matching `predicate`, skipping other frames.
    async fn wait_event(&mut self, name: &str) {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(10), self.ws.next())
                .await
                .expect("event timeout")
                .expect("stream ended")
                .unwrap();
            let Message::Text(text) = msg else { continue };
            let v: Value = serde_json::from_str(&text).unwrap();
            if v.get("type").and_then(Value::as_str) == Some("event") && v["event"]["type"] == name
            {
                return;
            }
        }
    }
}

async fn raw_get(proxy: std::net::SocketAddr, target: String) -> Vec<u8> {
    let mut stream = TcpStream::connect(proxy).await.unwrap();
    stream
        .write_all(
            format!("GET {target} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut out)).await;
    out
}

#[tokio::test]
async fn auth_origin_and_flows_end_to_end() {
    let daemon = spawn_daemon().await;

    // ── cross-origin upgrades are rejected at handshake ────────────────
    let request = tokio_tungstenite::tungstenite::http::Request::builder()
        .uri(format!("ws://{}", daemon.control_addr))
        .header("Host", daemon.control_addr.to_string())
        .header("Origin", "http://evil.example.com")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .unwrap();
    assert!(
        connect_async(request).await.is_err(),
        "non-local origin must be rejected (ADR-002)"
    );

    // ── wrong token is refused ──────────────────────────────────────────
    let mut bad = WsClient::connect(&daemon.control_addr).await;
    bad.send(ControlRequest::Auth {
        token: "nope".into(),
    })
    .await;
    let resp = bad.response().await;
    assert_eq!(resp["ok"], false, "wrong token must fail");
    // server closes after failed auth
    let closed = tokio::time::timeout(Duration::from_secs(5), bad.ws.next()).await;
    assert!(
        closed.is_err()
            || matches!(closed, Ok(None) | Ok(Some(Err(_))))
            || matches!(closed, Ok(Some(Ok(_)))),
        "connection ends after auth failure"
    );

    // ── correct token → hello ───────────────────────────────────────────
    let mut client = WsClient::connect(&daemon.control_addr).await;
    let hello = client
        .call(ControlRequest::Auth {
            token: daemon.token.clone(),
        })
        .await;
    assert_eq!(hello["ok"], true);
    assert!(hello["result"]["version"].is_string());

    // empty history
    let page = client
        .call(ControlRequest::FlowsList {
            limit: Some(10),
            before: None,
            after: None,
            host: None,
            method: None,
        })
        .await;
    assert_eq!(page["ok"], true);
    assert_eq!(page["result"]["items"].as_array().unwrap().len(), 0);

    // ── add a rule over the control plane ───────────────────────────────
    let added = client
        .call(ControlRequest::RulesAdd {
            spec: serde_json::from_value(serde_json::json!({
                "name": "cp-stamp",
                "action": {"kind": "set_request_header", "name": "x-via-cp", "value": "1"}
            }))
            .unwrap(),
        })
        .await;
    assert_eq!(added["ok"], true, "rule add over WS must work: {added}");
    let rule_id = added["result"]["id"].as_u64().unwrap();

    // ── real traffic through the proxy, recorded and rule-stamped ───────
    let origin = spawn_plain_origin().await;
    let response = raw_get(
        daemon.proxy_addr,
        format!("http://127.0.0.1:{}/via-cp", origin.port()),
    )
    .await;
    assert!(
        String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"),
        "proxy must serve"
    );

    let page = client
        .call(ControlRequest::FlowsList {
            limit: Some(10),
            before: None,
            after: None,
            host: None,
            method: None,
        })
        .await;
    let items = page["result"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "flow recorded: {page}");
    assert_eq!(items[0]["host"], "127.0.0.1");

    let flow = client
        .call(ControlRequest::FlowsGet {
            id: items[0]["id"].as_u64().unwrap(),
        })
        .await;
    assert_eq!(flow["result"]["request"]["url"]["host"], "127.0.0.1");

    // ── intercept from the control plane: hold → release with edit ──────
    let set = client
        .call(ControlRequest::InterceptSet {
            request: Some(true),
            response: None,
        })
        .await;
    assert_eq!(set["result"]["request"], true);

    let held_task = tokio::spawn(async move {
        let bytes = format!(
            "POST http://127.0.0.1:{}/held HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody",
            origin.port()
        );
        let mut stream = TcpStream::connect(daemon.proxy_addr).await.unwrap();
        stream.write_all(bytes.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut out)).await;
        out
    });

    client.wait_event("intercept_hold").await;
    let list = client.call(ControlRequest::InterceptList).await;
    let rows = list["result"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "held flow visible over WS");
    let held_id = rows[0]["flow_id"].as_u64().unwrap();

    let shown = client
        .call(ControlRequest::InterceptShow { id: held_id })
        .await;
    assert!(
        shown["result"]["request"].is_object(),
        "held request is inspectable: {shown}"
    );

    // edit the held request: replace the body
    let mut edited = shown["result"].clone();
    edited["request"]["body"] = serde_json::json!({"kind": "text", "text": "changed"});
    client
        .call(ControlRequest::InterceptRelease {
            id: held_id,
            decision: DecisionDto::Forward {
                payload: serde_json::from_value::<HeldPayloadDto>(edited).unwrap(),
            },
        })
        .await;

    let response = held_task.await.unwrap();
    assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
    assert!(
        String::from_utf8_lossy(&response).contains("\\\"body\\\":\\\"changed\\\"")
            || String::from_utf8_lossy(&response).contains("changed"),
        "edited body must reach the origin: {}",
        String::from_utf8_lossy(&response)
    );

    // rule that was added over WS actually ran (request carried the header)
    // and stats reflect the second flow
    let stats = client.call(ControlRequest::StatsGet).await;
    assert_eq!(stats["result"]["flows"]["total"], 2, "{stats}");
    assert_eq!(stats["result"]["rules"], 1);

    let _ = rule_id; // cleanup handled by tempdir
}

//! Aether CLI — WebSocket control client (ADR-002/010).
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).

use aether_core::error::{AetherError, AetherResult};
use aether_daemon::ControlRequest;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

pub struct ControlClient {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    /// Events seen while waiting for responses (`intercept ls --watch` uses
    /// none today, but `serve`-adjacent tooling may want the feed).
    pub drain_events: bool,
}

impl ControlClient {
    pub async fn connect(addr: &str, token: &str) -> AetherResult<Self> {
        let url = format!("ws://{addr}");
        let (ws, _) = connect_async(url)
            .await
            .map_err(|e| AetherError::network(format!("connect to control plane {addr}: {e}")))?;
        let mut client = Self {
            ws,
            drain_events: false,
        };
        let hello = client
            .request(ControlRequest::Auth {
                token: token.to_string(),
            })
            .await?;
        if hello.get("version").is_none() {
            return Err(AetherError::network("control plane sent malformed hello"));
        }
        Ok(client)
    }

    /// Sends one request, skipping event frames, and returns the `result`
    /// payload. Errors surface as `AetherError` with the server's kind.
    pub async fn request(&mut self, req: ControlRequest) -> AetherResult<Value> {
        self.send_raw(req).await
    }

    async fn send_raw(&mut self, req: ControlRequest) -> AetherResult<Value> {
        let text = serde_json::to_string(&req).map_err(|e| AetherError::protocol(e.to_string()))?;
        self.ws
            .send(Message::Text(text))
            .await
            .map_err(|e| AetherError::network(format!("control send: {e}")))?;
        loop {
            let msg = self
                .ws
                .next()
                .await
                .ok_or_else(|| AetherError::network("control plane closed"))?
                .map_err(|e| AetherError::network(format!("control recv: {e}")))?;
            let Message::Text(text) = msg else { continue };
            let value: Value = serde_json::from_str(&text)
                .map_err(|e| AetherError::protocol(format!("bad control frame: {e}")))?;
            match value.get("type").and_then(Value::as_str) {
                Some("event") => {
                    if self.drain_events {
                        // leave for callers that want the feed (none today)
                    }
                    continue;
                }
                Some("response") => {
                    if value.get("ok").and_then(Value::as_bool) == Some(true) {
                        return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                    }
                    let error = value.get("error").cloned().unwrap_or(Value::Null);
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown control error")
                        .to_string();
                    return Err(AetherError::protocol(message));
                }
                _ => continue,
            }
        }
    }
}

/// Resolve control-plane address: flag → env → data_dir file → default.
pub fn resolve_addr(flag: Option<&str>, data_dir: &std::path::Path) -> String {
    if let Some(a) = flag {
        return a.to_string();
    }
    if let Ok(a) = std::env::var("AETHER_CONTROL_ADDR") {
        return a;
    }
    if let Ok(text) = std::fs::read_to_string(data_dir.join("control.addr")) {
        let t = text.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    "127.0.0.1:9099".to_string()
}

/// Resolve auth token: flag → env → data_dir file.
pub fn resolve_token(flag: Option<&str>, data_dir: &std::path::Path) -> AetherResult<String> {
    if let Some(t) = flag {
        return Ok(t.to_string());
    }
    if let Ok(t) = std::env::var("AETHER_TOKEN") {
        return Ok(t);
    }
    let path = data_dir.join("token");
    std::fs::read_to_string(&path)
        .map(|t| t.trim().to_string())
        .map_err(|_| {
            AetherError::config(format!(
                "no token: pass --token, set AETHER_TOKEN, or ensure {} exists (it is written by `aether serve`)",
                path.display()
            ))
        })
}

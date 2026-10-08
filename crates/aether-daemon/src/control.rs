//! Aether daemon — control plane (ADR-002: WebSocket + JSON, single codec).
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Protocol v1 (ADR-010):
//! - browser-style origin check at upgrade time (reject non-local origins),
//! - the first frame MUST be `{"op":"auth","params":{"token":...}}`,
//! - then `{"op": "<op>", "params": {...}}` requests get
//!   `{"type":"response","ok":true,"result":...}` / `{"ok":false,"error":...}`,
//! - server-pushed frames are `{"type":"event","event":{...AetherEvent...}}`.

use aether_core::error::{AetherError, AetherResult};
use aether_core::hooks::HookRegistry;
use aether_core::storage::{FlowQuery, FlowStats, Storage};
use aether_core::{EventBus, InterceptDirection, RuleSpec};
use aether_http::ca::CaManager;
use aether_http::intercept::{HeldDecision, HeldPayload, InterceptState};
use aether_http::upstream::UpstreamCfg;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};

use crate::rules::RulesStore;

/// Everything an op handler may touch.
pub struct ControlShared {
    pub token: String,
    pub bus: EventBus,
    pub storage: Arc<dyn Storage>,
    pub intercept: Arc<InterceptState>,
    pub rules: Arc<RulesStore>,
    /// The live hook registry the engine reads; rule mutations re-project
    /// into it via `RulesStore::sync_into` (ADR-006 #1).
    pub registry: Arc<HookRegistry>,
    pub ca: Arc<CaManager>,
    pub upstream: UpstreamCfg,
    /// Human-readable proxy listener (the UI status bar shows it — users
    /// need to know where to point their clients).
    pub proxy_addr: String,
    pub version: &'static str,
    pub started_at_unix_ms: u64,
}

/// Requests. `Auth` must be the first frame on every connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", content = "params", rename_all = "snake_case")]
pub enum ControlRequest {
    Auth {
        token: String,
    },
    Ping,
    FlowsList {
        limit: Option<u32>,
        before: Option<u64>,
        after: Option<u64>,
        host: Option<String>,
        method: Option<String>,
    },
    FlowsGet {
        id: u64,
    },
    FlowsSearch {
        query: String,
        limit: Option<u32>,
    },
    StatsGet,
    InterceptList,
    InterceptShow {
        id: u64,
    },
    InterceptDrop {
        id: u64,
    },
    InterceptRelease {
        id: u64,
        decision: DecisionDto,
    },
    InterceptSet {
        request: Option<bool>,
        response: Option<bool>,
    },
    RulesList,
    RulesAdd {
        spec: RuleSpec,
    },
    RulesRemove {
        id: u64,
    },
    RulesSetEnabled {
        id: u64,
        enabled: bool,
    },
    CaExportPem,
    /// Re-execute a (possibly edited) request with no client leg (ADR-012).
    /// The flow id is reserved and returned immediately; execution continues
    /// in the background so a slow origin never stalls the control plane.
    FlowsReplay {
        request: aether_core::model::Request,
    },
}

/// Operator decision for a held flow: forward (possibly edited) or drop.
/// Operator decision for a held flow: forward (possibly edited) or drop.
/// (Large payload variant is intentional: the DTO exists only to cross the
/// WS boundary and is dropped immediately after deserialization.)
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum DecisionDto {
    Drop,
    Forward { payload: HeldPayloadDto },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldPayloadDto {
    Request(aether_core::model::Request),
    Response(aether_core::model::Response),
}

impl From<HeldPayload> for HeldPayloadDto {
    fn from(p: HeldPayload) -> Self {
        match p {
            HeldPayload::Request(r) => Self::Request(r),
            HeldPayload::Response(r) => Self::Response(r),
        }
    }
}

impl From<HeldPayloadDto> for HeldPayload {
    fn from(p: HeldPayloadDto) -> Self {
        match p {
            HeldPayloadDto::Request(r) => Self::Request(r),
            HeldPayloadDto::Response(r) => Self::Response(r),
        }
    }
}

/// Row shape for `intercept_list` (mirror of the engine's HeldInfo).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeldRow {
    pub flow_id: u64,
    pub direction: InterceptDirection,
    pub method: Option<String>,
    pub host: Option<String>,
    pub path: Option<String>,
    pub status: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsInfo {
    pub flows: FlowStats,
    pub held: usize,
    pub rules: usize,
    pub uptime_ms: u64,
    pub version: String,
    /// The explicit-proxy listener, e.g. "127.0.0.1:8888".
    #[serde(default)]
    pub proxy_addr: String,
}

pub async fn run(
    listener: TcpListener,
    shared: Arc<ControlShared>,
    shutdown: watch::Receiver<bool>,
) {
    loop {
        let mut sd = shutdown.clone();
        tokio::select! {
            _ = sd.changed() => {
                tracing::info!("control plane shutting down");
                return;
            }
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(x) => x,
                    Err(e) => { tracing::warn!(error = %e, "control accept failed"); continue }
                };
                let sh = shared.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, peer, sh).await {
                        tracing::debug!(%peer, error = %e, "control client ended");
                    }
                });
            }
        }
    }
}

fn origin_allowed(origin: &str) -> bool {
    // Loopback origins (any port) — browser dev tooling.
    let rest = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .or_else(|| origin.strip_prefix("ws://"))
        .or_else(|| origin.strip_prefix("wss://"));
    if let Some(host_port) = rest {
        let host = host_port
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host_port);
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1" {
            return true;
        }
    }
    // Tauri desktop shell (ADR-012): the packaged UI's webview origin.
    // Exact match only — no wildcard schemes, no remote hosts.
    origin == "tauri://localhost" || origin == "https://tauri.localhost"
}

#[allow(clippy::result_large_err)] // tungstenite's own ErrorResponse shape
async fn handle_client(
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    shared: Arc<ControlShared>,
) -> AetherResult<()> {
    let _ = peer;
    let callback = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        match req.headers().get("origin").and_then(|v| v.to_str().ok()) {
            Some(origin) if !origin_allowed(origin) => {
                tracing::warn!(%origin, "control plane: rejected cross-origin upgrade");
                let err: ErrorResponse = tokio_tungstenite::tungstenite::http::Response::builder()
                    .status(403)
                    .body(Some("origin not allowed".to_string()))
                    .unwrap_or_else(|_| ErrorResponse::new(None));
                Err(err)
            }
            _ => Ok(resp),
        }
    };
    let ws: WebSocketStream<tokio::net::TcpStream> =
        tokio_tungstenite::accept_hdr_async(stream, callback)
            .await
            .map_err(|e| AetherError::network(format!("ws handshake: {e}")))?;

    let (mut sink, mut source) = ws.split();

    // ── auth handshake (first frame, 10s budget) ────────────────────────
    let auth = tokio::time::timeout(std::time::Duration::from_secs(10), source.next()).await;
    let authenticated = match auth {
        Ok(Some(Ok(Message::Text(text)))) => {
            matches!(
                serde_json::from_str::<ControlRequest>(&text),
                Ok(ControlRequest::Auth { token }) if constant_time_eq(&token, &shared.token)
            )
        }
        _ => false,
    };
    if !authenticated {
        let err = serde_json::json!({"type":"response","ok":false,"error":{"kind":"auth","message":"authentication required or token mismatch"}});
        let _ = sink.send(Message::Text(err.to_string())).await;
        return Err(AetherError::network("control plane auth failed"));
    }
    let hello = serde_json::json!({
        "type":"response","ok":true,
        "result":{"version": shared.version, "server_time_unix_ms": aether_core::model::unix_millis_now()}
    });
    sink.send(Message::Text(hello.to_string()))
        .await
        .map_err(|e| AetherError::network(format!("ws send: {e}")))?;

    // ── main loop: client requests ∥ bus events ─────────────────────────
    let mut events = shared.bus.subscribe();
    loop {
        tokio::select! {
            incoming = source.next() => {
                let Some(msg) = incoming else { return Ok(()) };
                let msg = msg.map_err(|e| AetherError::network(format!("ws recv: {e}")))?;
                let Message::Text(text) = msg else { continue };
                let reply = match serde_json::from_str::<ControlRequest>(&text) {
                    Ok(req) => match dispatch(req, &shared).await {
                        Ok(result) => serde_json::json!({"type":"response","ok":true,"result":result}),
                        Err(e) => serde_json::json!({"type":"response","ok":false,"error":{"kind":e.kind_name(),"message":e.to_string()}}),
                    },
                    Err(e) => serde_json::json!({"type":"response","ok":false,"error":{"kind":"protocol","message":format!("bad request: {e}")}}),
                };
                sink.send(Message::Text(reply.to_string()))
                    .await
                    .map_err(|e| AetherError::network(format!("ws send: {e}")))?;
            }
            event = events.next() => {
                let Some(event) = event else { return Ok(()) };
                let frame = serde_json::json!({"type":"event","event":event});
                sink.send(Message::Text(frame.to_string()))
                    .await
                    .map_err(|e| AetherError::network(format!("ws send: {e}")))?;
            }
        }
    }
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

async fn dispatch(
    req: ControlRequest,
    shared: &Arc<ControlShared>,
) -> AetherResult<serde_json::Value> {
    use ControlRequest::*;
    let value = match req {
        Auth { .. } => return Err(AetherError::protocol("already authenticated")),
        Ping => serde_json::json!({"pong": true}),
        FlowsList {
            limit,
            before,
            after,
            host,
            method,
        } => {
            let page = shared
                .storage
                .list_flows(FlowQuery {
                    limit: limit.unwrap_or(50),
                    before,
                    after,
                    host,
                    method,
                })
                .await?;
            serde_json::to_value(page).map_err(|e| AetherError::protocol(e.to_string()))?
        }
        FlowsGet { id } => {
            let flow = shared.storage.get_flow(id).await?;
            serde_json::to_value(flow).map_err(|e| AetherError::protocol(e.to_string()))?
        }
        FlowsSearch { query, limit } => {
            let hits = shared
                .storage
                .search_flows(&query, limit.unwrap_or(50))
                .await?;
            serde_json::to_value(hits).map_err(|e| AetherError::protocol(e.to_string()))?
        }
        StatsGet => {
            let stats = StatsInfo {
                flows: shared.storage.flow_stats().await?,
                held: shared.intercept.held_count(),
                rules: shared.rules.list().len(),
                uptime_ms: aether_core::model::unix_millis_now()
                    .saturating_sub(shared.started_at_unix_ms),
                version: shared.version.to_string(),
                proxy_addr: shared.proxy_addr.clone(),
            };
            serde_json::to_value(stats).map_err(|e| AetherError::protocol(e.to_string()))?
        }
        InterceptList => {
            let rows: Vec<HeldRow> = shared
                .intercept
                .list()
                .into_iter()
                .map(|h| HeldRow {
                    flow_id: h.flow_id,
                    direction: h.direction,
                    method: h.method,
                    host: h.host,
                    path: h.path,
                    status: h.status,
                })
                .collect();
            serde_json::to_value(rows).map_err(|e| AetherError::protocol(e.to_string()))?
        }
        InterceptShow { id } => {
            let payload = shared.intercept.payload(id);
            match payload {
                Some(p) => serde_json::to_value(HeldPayloadDto::from(p))
                    .map_err(|e| AetherError::protocol(e.to_string()))?,
                None => return Err(AetherError::protocol(format!("no held flow {id}"))),
            }
        }
        InterceptDrop { id } => {
            serde_json::json!({"removed": shared.intercept.resolve(id, HeldDecision::Drop, &shared.bus)?})
        }
        InterceptRelease { id, decision } => {
            let held = match decision {
                DecisionDto::Drop => HeldDecision::Drop,
                DecisionDto::Forward { payload } => {
                    HeldDecision::Forward(HeldPayload::from(payload))
                }
            };
            serde_json::json!({"removed": shared.intercept.resolve(id, held, &shared.bus)?})
        }
        InterceptSet { request, response } => {
            let req_now = request.unwrap_or(shared.intercept.request_enabled());
            let res_now = response.unwrap_or(shared.intercept.response_enabled());
            shared.intercept.set_enabled(req_now, res_now);
            serde_json::json!({"request": req_now, "response": res_now})
        }
        RulesList => serde_json::to_value(shared.rules.list())
            .map_err(|e| AetherError::protocol(e.to_string()))?,
        RulesAdd { spec } => {
            let id = shared.rules.add(spec)?;
            shared.rules.sync_into(&shared.registry)?;
            serde_json::json!({"id": id})
        }
        RulesRemove { id } => {
            let removed = shared.rules.remove(id);
            if removed {
                shared.rules.sync_into(&shared.registry)?;
            }
            serde_json::json!({"removed": removed})
        }
        RulesSetEnabled { id, enabled } => {
            let changed = shared.rules.set_enabled(id, enabled);
            if changed {
                shared.rules.sync_into(&shared.registry)?;
            }
            serde_json::json!({"changed": changed})
        }
        CaExportPem => serde_json::json!({"pem": shared.ca.ca_cert_pem()}),
        FlowsReplay { request } => {
            let flow_id = shared.storage.alloc_id().await?;
            let storage = shared.storage.clone();
            let bus = shared.bus.clone();
            let cfg = shared.upstream.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::replay::run_replay(request, flow_id, storage, bus, cfg).await
                {
                    tracing::warn!(%flow_id, error = %e, "replay failed");
                }
            });
            serde_json::json!({"flow_id": flow_id})
        }
    };
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_allows_loopback_any_port() {
        assert!(origin_allowed("http://localhost:5173"));
        assert!(origin_allowed("http://127.0.0.1:3000"));
        assert!(origin_allowed("ws://localhost:9099"));
        assert!(origin_allowed("http://[::1]:8080"));
        assert!(origin_allowed("http://localhost"));
    }

    #[test]
    fn origin_allows_tauri_shell_only() {
        assert!(origin_allowed("tauri://localhost"));
        assert!(origin_allowed("https://tauri.localhost"));
        // lookalikes must fail
        assert!(!origin_allowed("tauri://evil.com"));
        assert!(!origin_allowed("tauri://localhost.evil.com"));
        assert!(!origin_allowed("https://tauri.localhost.evil.com"));
    }

    #[test]
    fn origin_rejects_remote_and_garbage() {
        assert!(!origin_allowed("http://evil.com"));
        assert!(!origin_allowed("http://localhost.evil.com"));
        assert!(!origin_allowed("https://127.0.0.1.evil.com:9099"));
        assert!(!origin_allowed("file:///home/user"));
        assert!(!origin_allowed(""));
        // custom app-scheme origins (Electron-style) are not trusted
        assert!(!origin_allowed("app://aether"));
    }
}

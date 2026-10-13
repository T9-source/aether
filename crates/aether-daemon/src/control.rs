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
use aether_core::events::EventTick;
use aether_core::storage::{FlowQuery, FlowStats};
use aether_core::{EventBus, RuleSpec};
use aether_http::intercept::{HeldDecision, HeldPayload};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};

/// Everything one control connection may touch. Protocol state only — all
/// behavior lives in [`Services`] (ADR-016: the control plane is a wire
/// codec that delegates, never a behavior owner).
pub struct ControlShared {
    pub token: String,
    pub services: Arc<crate::services::Services>,
    /// The event stream tap: the control plane *forwards* bus events to
    /// clients — reading the stream is protocol plumbing, not behavior
    /// (ADR-016); publishing stays with the engine/services.
    pub bus: EventBus,
    pub version: &'static str,
    pub started_at_unix_ms: u64,
    /// Human-readable proxy listener (the UI status bar shows it — users
    /// need to know where to point their clients).
    pub proxy_addr: String,
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

/// Row shape for `intercept_list` lives with its behavior
/// (`services::intercept::HeldRow`, ADR-016).

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
            let text = text.as_str();
            matches!(
                serde_json::from_str::<ControlRequest>(text),
                Ok(ControlRequest::Auth { token }) if constant_time_eq(&token, &shared.token)
            )
        }
        _ => false,
    };
    if !authenticated {
        let err = serde_json::json!({"type":"response","ok":false,"error":{"kind":"auth","message":"authentication required or token mismatch"}});
        let _ = sink.send(Message::Text(err.to_string().into())).await;
        return Err(AetherError::network("control plane auth failed"));
    }
    let hello = serde_json::json!({
        "type":"response","ok":true,
        "result":{"version": shared.version, "server_time_unix_ms": aether_core::model::unix_millis_now()}
    });
    sink.send(Message::Text(hello.to_string().into()))
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
                let text = text.as_str();
                let reply = match serde_json::from_str::<ControlRequest>(text) {
                    Ok(req) => match dispatch(req, &shared).await {
                        Ok(result) => serde_json::json!({"type":"response","ok":true,"result":result}),
                        Err(e) => serde_json::json!({"type":"response","ok":false,"error":{"kind":e.kind_name(),"message":e.to_string()}}),
                    },
                    Err(e) => serde_json::json!({"type":"response","ok":false,"error":{"kind":"protocol","message":format!("bad request: {e}")}}),
                };
                sink.send(Message::Text(reply.to_string().into()))
                    .await
                    .map_err(|e| AetherError::network(format!("ws send: {e}")))?;
            }
            tick = events.next_tick() => {
                // Audit M6: a slow console consumer that falls behind gets a
                // `missed_events` notice and stays connected — losing events
                // is recoverable (storage is the source of truth, ADR-017),
                // while the old silent `None` path looked like "bus closed"
                // and tore the socket down.
                let frame = match tick {
                    EventTick::Event(event) => {
                        // ADR-017: the taxonomy rides on every pushed frame so UI
                        // filters and future plugins can subscribe by category.
                        let category = event.category();
                        serde_json::json!({"type":"event","event":event,"category":category})
                    }
                    EventTick::Lagged(n) => {
                        tracing::warn!(missed = n, "slow control client resyncing from storage");
                        serde_json::json!({"type":"missed_events","missed":n})
                    }
                    EventTick::Closed => return Ok(()),
                };
                sink.send(Message::Text(frame.to_string().into()))
                    .await
                    .map_err(|e| AetherError::network(format!("ws send: {e}")))?;
            }
        }
    }
}

/// Token comparison that leaks nothing — not the bytes, and not the length
/// (a naive `a == b` or an early `len()` return lets a local process probe
/// the token by timing; AUDIT-2026-10-11 finding F1, fixed).
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut acc = 0u8;
    // Always loop over the longer length, reading 0 past the end of the
    // shorter one: runtime is a function of the *maximum* length only.
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        acc |= x ^ y;
    }
    acc == 0
}

async fn dispatch(
    req: ControlRequest,
    shared: &Arc<ControlShared>,
) -> AetherResult<serde_json::Value> {
    use ControlRequest::*;
    // ADR-016: every arm is a one-line delegation — the control plane owns
    // the wire shape, the services own the behavior.
    let sv = &shared.services;
    let value = match req {
        Auth { .. } => return Err(AetherError::protocol("already authenticated")),
        Ping => serde_json::json!({"pong": true}),
        FlowsList {
            limit,
            before,
            after,
            host,
            method,
        } => serde_json::to_value(
            sv.traffic
                .list(FlowQuery {
                    limit: limit.unwrap_or(50),
                    before,
                    after,
                    host,
                    method,
                })
                .await?,
        )
        .map_err(|e| AetherError::protocol(e.to_string()))?,
        FlowsGet { id } => serde_json::to_value(sv.traffic.get(id).await?)
            .map_err(|e| AetherError::protocol(e.to_string()))?,
        FlowsSearch { query, limit } => {
            serde_json::to_value(sv.traffic.search(&query, limit.unwrap_or(50)).await?)
                .map_err(|e| AetherError::protocol(e.to_string()))?
        }
        StatsGet => {
            let stats = StatsInfo {
                flows: sv.traffic.stats().await?,
                held: sv.intercept.held_count(),
                rules: sv.rules.list().len(),
                uptime_ms: aether_core::model::unix_millis_now()
                    .saturating_sub(shared.started_at_unix_ms),
                version: shared.version.to_string(),
                proxy_addr: shared.proxy_addr.clone(),
            };
            serde_json::to_value(stats).map_err(|e| AetherError::protocol(e.to_string()))?
        }
        InterceptList => serde_json::to_value(sv.intercept.list_rows())
            .map_err(|e| AetherError::protocol(e.to_string()))?,
        InterceptShow { id } => match sv.intercept.show(id) {
            Some(p) => serde_json::to_value(HeldPayloadDto::from(p))
                .map_err(|e| AetherError::protocol(e.to_string()))?,
            None => return Err(AetherError::protocol(format!("no held flow {id}"))),
        },
        InterceptDrop { id } => {
            serde_json::json!({"removed": sv.intercept.drop_held(id)?})
        }
        InterceptRelease { id, decision } => {
            let held = match decision {
                DecisionDto::Drop => HeldDecision::Drop,
                DecisionDto::Forward { payload } => {
                    HeldDecision::Forward(HeldPayload::from(payload))
                }
            };
            serde_json::json!({"removed": sv.intercept.release(id, held)?})
        }
        InterceptSet { request, response } => {
            let (req_now, res_now) = sv.intercept.set_enabled(request, response);
            serde_json::json!({"request": req_now, "response": res_now})
        }
        RulesList => serde_json::to_value(sv.rules.list())
            .map_err(|e| AetherError::protocol(e.to_string()))?,
        RulesAdd { spec } => serde_json::json!({"id": sv.rules.add(spec)?}),
        RulesRemove { id } => serde_json::json!({"removed": sv.rules.remove(id)?}),
        RulesSetEnabled { id, enabled } => {
            serde_json::json!({"changed": sv.rules.set_enabled(id, enabled)?})
        }
        CaExportPem => serde_json::json!({"pem": sv.ca.export_pem()}),
        FlowsReplay { request } => {
            serde_json::json!({"flow_id": sv.replay.start(request).await?})
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
    #[test]
    fn constant_time_eq_is_correct_for_all_length_classes() {
        // AUDIT-2026-10-11 F1 regression: length must not leak, and the
        // result must be correct across mixed lengths (loop runs max len).
        assert!(super::constant_time_eq("", ""));
        assert!(super::constant_time_eq("abc", "abc"));
        assert!(super::constant_time_eq(
            "a".repeat(64).as_str(),
            "a".repeat(64).as_str()
        ));
        // wrong content, same length
        assert!(!super::constant_time_eq("abc", "abd"));
        // wrong content, different lengths (pre-fix this returned via len check)
        assert!(!super::constant_time_eq("abc", "abcd"));
        assert!(!super::constant_time_eq("abcd", "abc"));
        // one side empty
        assert!(!super::constant_time_eq("", "x"));
        assert!(!super::constant_time_eq("x", ""));
        // unicode multi-byte must compare bytes correctly
        assert!(super::constant_time_eq(
            "\u{627}\u{644}\u{62b}\u{644}",
            "\u{627}\u{644}\u{62b}\u{644}"
        ));
        assert!(!super::constant_time_eq(
            "\u{627}\u{644}\u{62b}\u{644}",
            "\u{627}\u{644}\u{62b}"
        ));
    }
}

//! Aether — network engine.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! The flow engine — the single path every exchange takes (ADR-006 #1):
//!
//! ```text
//! wire request → Flow record → [intercept hold] → request pipeline
//!              → upstream → wire response → [intercept hold]
//!              → response pipeline → persist → FlowCompleted
//! ```
//!
//! Every stage is observable on the EventBus; every mutation lands in
//! storage; every drop is a *decision* (rule, hook, or operator) that gets
//! tagged on the flow, never a silent failure.

use aether_core::model::{Body, Flow, FlowId, Request, Response, TlsMeta, TransportProto};
use aether_core::storage::Storage;
use aether_core::{
    AetherEvent, EventBus, InterceptDirection, Pipeline, PipelineVerdict, RequestContext,
    TrafficSource, drain_ctx_tags, take_respond_with,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use crate::intercept::{HeldDecision, HeldPayload, InterceptState};
use crate::tls::TlsFacts;
use crate::upstream::{self, Remainder, UpstreamCfg};

/// Connection facts captured by the listener before the model is built.
#[derive(Debug, Clone)]
pub struct ConnInfo {
    pub client_addr: SocketAddr,
    pub client_proto: TransportProto,
    pub tls: Option<TlsFacts>,
}

/// What the server layer should do with the client socket after the engine ran.
pub enum EngineOutcome {
    /// Serve this response (possibly followed by the streaming remainder).
    Serve {
        response: Response,
        remainder: Option<Remainder>,
    },
    /// The exchange was dropped — the server replies 502 and closes.
    Dropped { reason: String },
}

pub struct FlowEngine {
    pub bus: EventBus,
    pub pipeline: Pipeline,
    pub storage: Arc<dyn Storage>,
    pub intercept: Arc<InterceptState>,
    pub upstream_cfg: UpstreamCfg,
}

/// What the server layer should do with the client socket after an upgrade
/// attempt (RFC 6455, V0.2 ADR-013).
#[allow(clippy::large_enum_variant)]
pub enum UpgradeOutcome {
    /// The origin answered `101` — serve it, then relay frames between the
    /// two sockets while recording the session (`crate::ws`).
    Upgraded {
        flow_id: FlowId,
        response: Response,
        upstream: crate::upstream::UpstreamStream,
    },
    /// The origin refused the upgrade — serve this normal response instead.
    Replied {
        response: Response,
        remainder: Option<Remainder>,
    },
    /// The exchange was dropped before any socket could be served.
    Dropped { reason: String },
}

impl FlowEngine {
    pub async fn handle_request(&self, mut request: Request, conn: ConnInfo) -> EngineOutcome {
        let flow_id: FlowId = match self.storage.alloc_id().await {
            Ok(id) => id,
            Err(e) => {
                self.bus.publish(AetherEvent::ErrorOccurred {
                    context: "storage.alloc_id".into(),
                    message: e.to_string(),
                });
                return EngineOutcome::Dropped {
                    reason: e.to_string(),
                };
            }
        };

        let mut flow = Flow::new(flow_id, request.clone());
        flow.meta.client_proto = conn.client_proto;
        flow.meta.server_proto = if request.url.scheme.eq_ignore_ascii_case("https") {
            TransportProto::Tls
        } else {
            TransportProto::Plaintext
        };
        flow.meta.tls = conn.tls.map(|t| TlsMeta {
            version: t.version,
            cipher: t.cipher,
            alpn: t.alpn,
        });
        self.bus.publish(AetherEvent::FlowStarted { flow_id });
        if let Err(e) = self.storage.put_flow(&flow).await {
            self.bus.publish(AetherEvent::ErrorOccurred {
                context: "storage.put_flow".into(),
                message: e.to_string(),
            });
        }

        let started = Instant::now();
        let mut ctx = RequestContext::new(flow_id, TrafficSource::Proxy);

        // ── intercept: request phase ─────────────────────────────────────
        if self.intercept.request_enabled() {
            let rx = self.intercept.hold(
                flow_id,
                InterceptDirection::Request,
                HeldPayload::Request(request.clone()),
                &self.bus,
            );
            match rx.await {
                Ok(HeldDecision::Forward(HeldPayload::Request(edited))) => {
                    request = edited;
                    flow.request = Some(request.clone());
                }
                Ok(HeldDecision::Forward(_)) | Err(_) => {
                    return self.drop_flow(&mut flow, "intercept abandoned").await;
                }
                Ok(HeldDecision::Drop) => {
                    return self
                        .drop_flow(&mut flow, "dropped by operator (intercept)")
                        .await;
                }
            }
        }

        // ── request pipeline (hooks / rules) ─────────────────────────────
        let mut mocked: Option<Response> = None;
        let mut req_for_pipeline = request.clone();
        match self
            .pipeline
            .run_request(&mut ctx, &mut req_for_pipeline)
            .await
        {
            Ok(PipelineVerdict::Forwarded) => {
                request = req_for_pipeline;
                flow.request = Some(request.clone());
            }
            Ok(PipelineVerdict::Stopped) => {
                if let Some(rw) = take_respond_with(&mut ctx) {
                    let mut res =
                        Response::new(rw.status).unwrap_or_else(|_| Response::new(500).unwrap());
                    if let Some(ct) = rw.content_type {
                        res.set_header("content-type", ct);
                    }
                    res.body = Body::text(rw.body);
                    mocked = Some(res);
                    flow.add_tag("mocked");
                } else {
                    flow.request = Some(req_for_pipeline.clone());
                    return self.drop_flow(&mut flow, "stopped by hook pipeline").await;
                }
            }
            Err(e) => {
                flow.add_tag("pipeline-error");
                return self
                    .drop_flow(&mut flow, &format!("request pipeline: {e}"))
                    .await;
            }
        }
        for tag in drain_ctx_tags(&mut ctx) {
            flow.add_tag(&tag);
        }

        // ── upstream (or mock short-circuit) ─────────────────────────────
        let upstream_result = match &mocked {
            Some(res) => Ok(upstream::UpstreamReply {
                response: res.clone(),
                remainder: None,
            }),
            None => upstream::send_request(&request, &self.upstream_cfg).await,
        };
        let reply = match upstream_result {
            Ok(r) => r,
            Err(e) => {
                flow.add_tag("network-error");
                self.bus.publish(AetherEvent::ErrorOccurred {
                    context: format!(
                        "upstream {}:{} flow {flow_id}",
                        request.url.host,
                        request.url.effective_port()
                    ),
                    message: e.to_string(),
                });
                return self.drop_flow(&mut flow, &format!("upstream: {e}")).await;
            }
        };

        let mut response = reply.response;
        self.bus.publish(AetherEvent::ResponseReceived {
            flow_id,
            status: response.status.code,
        });

        // ── intercept: response phase (fully-captured bodies only — edited
        // bodies cannot be reconciled with a streaming remainder) ─────────
        if self.intercept.response_enabled() && mocked.is_none() && reply.remainder.is_none() {
            let rx = self.intercept.hold(
                flow_id,
                InterceptDirection::Response,
                HeldPayload::Response(response.clone()),
                &self.bus,
            );
            match rx.await {
                Ok(HeldDecision::Forward(HeldPayload::Response(edited))) => {
                    response = edited;
                }
                Ok(HeldDecision::Forward(_)) | Err(_) => {
                    return self.drop_flow(&mut flow, "intercept abandoned").await;
                }
                Ok(HeldDecision::Drop) => {
                    return self
                        .drop_flow(&mut flow, "response dropped by operator (intercept)")
                        .await;
                }
            }
        }

        // ── response pipeline ────────────────────────────────────────────
        match self.pipeline.run_response(&mut ctx, &mut response).await {
            Ok(PipelineVerdict::Forwarded) => {}
            Ok(PipelineVerdict::Stopped) => {
                return self
                    .drop_flow(&mut flow, "response stopped by hook pipeline")
                    .await;
            }
            Err(e) => {
                flow.add_tag("pipeline-error");
                return self
                    .drop_flow(&mut flow, &format!("response pipeline: {e}"))
                    .await;
            }
        }
        for tag in drain_ctx_tags(&mut ctx) {
            flow.add_tag(&tag);
        }

        // ── content-length honesty on the client leg ─────────────────────
        // When the body is fully in memory (edited, mocked, decoded, or
        // EOF-framed), the declared length must match the served bytes.
        if reply.remainder.is_none() {
            response.headers.remove("content-length");
            response.headers.remove("transfer-encoding");
            if !response.body.is_empty() {
                response
                    .headers
                    .insert("content-length", response.body.len().to_string());
            }
        }

        // hop-by-hop headers from the client must not leak upstream either
        // (serialize_request adds its own `Connection: close`, ADR-009)
        aether_http_strip_request_hop_by_hop(&mut request);

        // ── persist + complete ───────────────────────────────────────────
        flow.meta.duration_ms = Some(started.elapsed().as_millis() as u64);
        flow.response = Some(response.clone());
        if let Err(e) = self.storage.put_flow(&flow).await {
            self.bus.publish(AetherEvent::ErrorOccurred {
                context: "storage.put_flow".into(),
                message: e.to_string(),
            });
        }
        let _ = self.pipeline.complete_flow(&ctx, &flow).await;

        EngineOutcome::Serve {
            response,
            remainder: reply.remainder,
        }
    }

    /// The WebSocket-upgrade path (RFC 6455, V0.2 ADR-013): the exact same
    /// pipeline as [`FlowEngine::handle_request`] — flow record, intercept,
    /// hooks, rules — except the upstream call keeps the socket on `101` so
    /// the caller can relay frames. One path, one history, one set of events.
    pub async fn handle_upgrade(&self, mut request: Request, conn: ConnInfo) -> UpgradeOutcome {
        let flow_id: FlowId = match self.storage.alloc_id().await {
            Ok(id) => id,
            Err(e) => {
                self.bus.publish(AetherEvent::ErrorOccurred {
                    context: "storage.alloc_id".into(),
                    message: e.to_string(),
                });
                return UpgradeOutcome::Dropped {
                    reason: e.to_string(),
                };
            }
        };

        let mut flow = Flow::new(flow_id, request.clone());
        flow.meta.client_proto = conn.client_proto;
        flow.meta.server_proto = if request.url.scheme.eq_ignore_ascii_case("https") {
            TransportProto::Tls
        } else {
            TransportProto::Plaintext
        };
        flow.meta.tls = conn.tls.map(|t| TlsMeta {
            version: t.version,
            cipher: t.cipher,
            alpn: t.alpn,
        });
        self.bus.publish(AetherEvent::FlowStarted { flow_id });
        if let Err(e) = self.storage.put_flow(&flow).await {
            self.bus.publish(AetherEvent::ErrorOccurred {
                context: "storage.put_flow".into(),
                message: e.to_string(),
            });
        }

        let started = Instant::now();
        let mut ctx = RequestContext::new(flow_id, TrafficSource::Proxy);

        // ── intercept: request phase ─────────────────────────────────────
        if self.intercept.request_enabled() {
            let rx = self.intercept.hold(
                flow_id,
                InterceptDirection::Request,
                HeldPayload::Request(request.clone()),
                &self.bus,
            );
            match rx.await {
                Ok(HeldDecision::Forward(HeldPayload::Request(edited))) => {
                    request = edited;
                    flow.request = Some(request.clone());
                }
                Ok(HeldDecision::Forward(_)) | Err(_) => {
                    return self.drop_upgrade(&mut flow, "intercept abandoned").await;
                }
                Ok(HeldDecision::Drop) => {
                    return self
                        .drop_upgrade(&mut flow, "dropped by operator (intercept)")
                        .await;
                }
            }
        }

        // ── request pipeline (hooks / rules) ─────────────────────────────
        // Note: the hop-by-hop strip is skipped here ON PURPOSE — the
        // `Connection: Upgrade` / `Upgrade: websocket` headers ARE the
        // protocol; stripping them would abort the handshake. Everything
        // else (mocking, rewriting, tagging) works as usual.
        let mut mocked: Option<Response> = None;
        let mut req_for_pipeline = request.clone();
        match self
            .pipeline
            .run_request(&mut ctx, &mut req_for_pipeline)
            .await
        {
            Ok(PipelineVerdict::Forwarded) => {
                request = req_for_pipeline;
                flow.request = Some(request.clone());
            }
            Ok(PipelineVerdict::Stopped) => {
                if let Some(rw) = take_respond_with(&mut ctx) {
                    let mut res =
                        Response::new(rw.status).unwrap_or_else(|_| Response::new(500).unwrap());
                    if let Some(ct) = rw.content_type {
                        res.set_header("content-type", ct);
                    }
                    res.body = Body::text(rw.body);
                    mocked = Some(res);
                    flow.add_tag("mocked");
                } else {
                    flow.request = Some(req_for_pipeline.clone());
                    return self
                        .drop_upgrade(&mut flow, "stopped by hook pipeline")
                        .await;
                }
            }
            Err(e) => {
                flow.add_tag("pipeline-error");
                return self
                    .drop_upgrade(&mut flow, &format!("request pipeline: {e}"))
                    .await;
            }
        }
        for tag in drain_ctx_tags(&mut ctx) {
            flow.add_tag(&tag);
        }

        // ── inspection hint: strip permessage-deflate so frames arrive in
        // plaintext (mitmproxy-proven default; ADR-013) ────────────────────
        if request
            .headers
            .get("sec-websocket-extensions")
            .map(|v| v.to_ascii_lowercase().contains("permessage-deflate"))
            .unwrap_or(false)
        {
            request.headers.remove("sec-websocket-extensions");
            flow.add_tag("ws-deflate-stripped");
        }

        // ── upstream upgrade (or mock short-circuit) ─────────────────────
        if let Some(res) = &mocked {
            return match self
                .finish_upgrade_flow(&mut flow, res.clone(), started)
                .await
            {
                Ok(()) => UpgradeOutcome::Replied {
                    response: res.clone(),
                    remainder: None,
                },
                Err(e) => UpgradeOutcome::Dropped { reason: e },
            };
        }

        let open = crate::upstream::open_upgrade(&request, &self.upstream_cfg).await;
        let (mut response, upgraded_stream, remainder) = match open {
            Ok(crate::upstream::UpgradeReply::Upgraded { response, stream }) => {
                flow.add_tag("websocket");
                (response, Some(stream), None)
            }
            Ok(crate::upstream::UpgradeReply::Rejected { reply }) => {
                self.bus.publish(AetherEvent::ResponseReceived {
                    flow_id,
                    status: reply.response.status.code,
                });
                (reply.response, None, reply.remainder)
            }
            Err(e) => {
                flow.add_tag("network-error");
                self.bus.publish(AetherEvent::ErrorOccurred {
                    context: format!(
                        "upstream {}:{} flow {flow_id}",
                        request.url.host,
                        request.url.effective_port()
                    ),
                    message: e.to_string(),
                });
                return self
                    .drop_upgrade(&mut flow, &format!("upstream: {e}"))
                    .await;
            }
        };

        // ── intercept: response phase (101 bodies are empty by definition) ─
        if self.intercept.response_enabled() {
            let rx = self.intercept.hold(
                flow_id,
                InterceptDirection::Response,
                HeldPayload::Response(response.clone()),
                &self.bus,
            );
            match rx.await {
                Ok(HeldDecision::Forward(HeldPayload::Response(edited))) => {
                    response = edited;
                }
                Ok(HeldDecision::Forward(_)) | Err(_) => {
                    return self.drop_upgrade(&mut flow, "intercept abandoned").await;
                }
                Ok(HeldDecision::Drop) => {
                    return self
                        .drop_upgrade(&mut flow, "response dropped by operator (intercept)")
                        .await;
                }
            }
        }

        // ── response pipeline ────────────────────────────────────────────
        match self.pipeline.run_response(&mut ctx, &mut response).await {
            Ok(PipelineVerdict::Forwarded) => {}
            Ok(PipelineVerdict::Stopped) => {
                return self
                    .drop_upgrade(&mut flow, "response stopped by hook pipeline")
                    .await;
            }
            Err(e) => {
                flow.add_tag("pipeline-error");
                return self
                    .drop_upgrade(&mut flow, &format!("response pipeline: {e}"))
                    .await;
            }
        }
        for tag in drain_ctx_tags(&mut ctx) {
            flow.add_tag(&tag);
        }

        flow.meta.duration_ms = Some(started.elapsed().as_millis() as u64);
        flow.response = Some(response.clone());
        if let Err(e) = self.storage.put_flow(&flow).await {
            self.bus.publish(AetherEvent::ErrorOccurred {
                context: "storage.put_flow".into(),
                message: e.to_string(),
            });
        }
        let _ = self.pipeline.complete_flow(&ctx, &flow).await;

        match upgraded_stream {
            Some(upstream) => UpgradeOutcome::Upgraded {
                flow_id,
                response,
                upstream,
            },
            None => UpgradeOutcome::Replied {
                response,
                remainder,
            },
        }
    }

    /// Persists a completed (non-upgraded) upgrade-attempt flow.
    async fn finish_upgrade_flow(
        &self,
        flow: &mut Flow,
        response: Response,
        started: Instant,
    ) -> Result<(), String> {
        flow.meta.duration_ms = Some(started.elapsed().as_millis() as u64);
        flow.response = Some(response);
        if let Err(e) = self.storage.put_flow(flow).await {
            self.bus.publish(AetherEvent::ErrorOccurred {
                context: "storage.put_flow".into(),
                message: e.to_string(),
            });
        }
        Ok(())
    }

    async fn drop_flow(&self, flow: &mut Flow, reason: &str) -> EngineOutcome {
        flow.add_tag("dropped");
        if let Err(e) = self.storage.put_flow(flow).await {
            self.bus.publish(AetherEvent::ErrorOccurred {
                context: "storage.put_flow".into(),
                message: e.to_string(),
            });
        }
        self.bus.publish(AetherEvent::FlowDropped {
            flow_id: flow.id,
            reason: reason.to_string(),
        });
        EngineOutcome::Dropped {
            reason: reason.to_string(),
        }
    }

    /// Same bookkeeping as [`FlowEngine::drop_flow`], for the upgrade path.
    async fn drop_upgrade(&self, flow: &mut Flow, reason: &str) -> UpgradeOutcome {
        flow.add_tag("dropped");
        if let Err(e) = self.storage.put_flow(flow).await {
            self.bus.publish(AetherEvent::ErrorOccurred {
                context: "storage.put_flow".into(),
                message: e.to_string(),
            });
        }
        self.bus.publish(AetherEvent::FlowDropped {
            flow_id: flow.id,
            reason: reason.to_string(),
        });
        UpgradeOutcome::Dropped {
            reason: reason.to_string(),
        }
    }
}

/// Requests forwarded upstream speak our own framing: client hop-by-hop
/// headers are dropped and `serialize_request` re-adds what it needs.
fn aether_http_strip_request_hop_by_hop(request: &mut Request) {
    crate::wire::strip_hop_by_hop(&mut request.headers);
}

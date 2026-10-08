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
}

/// Requests forwarded upstream speak our own framing: client hop-by-hop
/// headers are dropped and `serialize_request` re-adds what it needs.
fn aether_http_strip_request_hop_by_hop(request: &mut Request) {
    crate::wire::strip_hop_by_hop(&mut request.headers);
}

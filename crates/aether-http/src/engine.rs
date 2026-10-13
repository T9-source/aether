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

use aether_core::model::{Body, Flow, FlowId, Framing, Request, Response, TlsMeta, TransportProto};
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
use crate::wire::BodySpec;

/// Connection facts captured by the listener before the model is built.
#[derive(Debug, Clone)]
pub struct ConnInfo {
    pub client_addr: SocketAddr,
    pub client_proto: TransportProto,
    /// Wire fact (ADR-015): did the *client* leg negotiate HTTP/2? Framing
    /// facts served to the client are decided from this.
    pub client_h2: bool,
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
        // Wire fact (ADR-015): the request as the client's wire delivered it
        // — before intercept/hooks/rules can reshape it.
        flow.meta.observed_req_framing = framing_of_request(&request);
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
                leg_h2: false,
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

        // Wire facts (ADR-015): what crossed the upstream wire. `observed`
        // is the origin's own framing; `forwarded` is ours. A mocked response
        // crosses no upstream wire — both stay `None` by design.
        if mocked.is_none() {
            if reply.leg_h2 {
                flow.meta.forwarded_req_framing = Some(Framing::H2Streams);
                flow.meta.observed_res_framing = Some(Framing::H2Streams);
            } else {
                flow.meta.forwarded_req_framing = forwarded_h1_req_framing(&request);
                flow.meta.observed_res_framing = framing_of_response(&response);
            }
        }

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
        // Wire fact (ADR-015): the framing the client leg will actually see.
        flow.meta.forwarded_res_framing = if conn.client_h2 {
            Some(Framing::H2Streams)
        } else if reply.remainder.is_some() {
            // Streaming passthrough: an origin-declared CL travels verbatim;
            // an undeclared length can only be honestly served EOF-delimited
            // (RFC 9110 §6.3 — server.rs picks exactly this framing).
            if response.headers.contains("content-length") {
                Some(Framing::ContentLength)
            } else {
                Some(Framing::CloseDelimited)
            }
        } else if response.body.is_empty() {
            None
        } else {
            // Fully-buffered bodies are always re-served with an exact CL.
            Some(Framing::ContentLength)
        };
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
        // Wire fact (ADR-015): the request as the client's wire delivered it
        // — before intercept/hooks/rules can reshape it.
        flow.meta.observed_req_framing = framing_of_request(&request);
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
            // Wire fact (ADR-015): the mock crosses no upstream wire, but the
            // client still receives it — buffered, with an exact CL.
            flow.meta.forwarded_res_framing = if res.body.is_empty() {
                None
            } else {
                Some(Framing::ContentLength)
            };
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

        // Wire facts (ADR-015): the upgrade handshake crossed the upstream
        // h1 wire; a 101 carries no body by definition (maps to `None`).
        flow.meta.observed_res_framing = framing_of_response(&response);
        flow.meta.forwarded_req_framing = forwarded_h1_req_framing(&request);

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

        // Wire fact (ADR-015): a live `101` turns the wire into a tunnel —
        // that (and only that) is the response framing served to the client.
        flow.meta.forwarded_res_framing = if upgraded_stream.is_some() {
            Some(Framing::Upgrade)
        } else if conn.client_h2 {
            Some(Framing::H2Streams)
        } else if remainder.is_some() {
            if response.headers.contains("content-length") {
                Some(Framing::ContentLength)
            } else {
                Some(Framing::CloseDelimited)
            }
        } else if response.body.is_empty() {
            None
        } else {
            Some(Framing::ContentLength)
        };
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

// ---------------------------------------------------------------------------
// wire facts (ADR-015) — observed vs forwarded, kept as explicit model facts
// ---------------------------------------------------------------------------

/// Observed request framing — from the client's own headers, before any
/// rewrite by intercept/hooks/rules.
fn framing_of_request(req: &Request) -> Option<Framing> {
    match crate::wire::request_body_spec(&req.headers) {
        Ok(BodySpec::Empty) => None,
        Ok(BodySpec::ContentLength(_)) => Some(Framing::ContentLength),
        Ok(BodySpec::Chunked) => Some(Framing::Chunked),
        Ok(BodySpec::ReadToEnd) => Some(Framing::CloseDelimited),
        // CL+TE smuggling refusals never reach persistence; `None` is honest
        // for anything that somehow does.
        Err(_) => None,
    }
}

/// Observed response framing — the origin's own declaration (HEAD, 204/304
/// and 1xx all map to `Empty` → `None`).
fn framing_of_response(res: &Response) -> Option<Framing> {
    match crate::wire::response_body_spec(&res.headers, res.status.code, false) {
        BodySpec::Empty => None,
        BodySpec::ContentLength(_) => Some(Framing::ContentLength),
        BodySpec::Chunked => Some(Framing::Chunked),
        BodySpec::ReadToEnd => Some(Framing::CloseDelimited),
    }
}

/// Forwarded request framing — mirrors `wire::serialize_request`: a real
/// body, or a client-declared zero-length body, gets an explicit CL; a bare
/// GET-style request carries no framing at all.
fn forwarded_h1_req_framing(req: &Request) -> Option<Framing> {
    if !req.body.is_empty() || req.headers.contains("content-length") {
        Some(Framing::ContentLength)
    } else {
        None
    }
}

#[cfg(test)]
mod wire_facts_tests {
    use super::*;
    use aether_core::model::{Body, Method, Url};

    #[test]
    fn observed_request_framing_reads_only_declared_framing() {
        let mut req = Request::new(Method::Post, Url::parse("http://a/b").unwrap());
        assert_eq!(framing_of_request(&req), None, "no framing declared yet");
        req.body = Body::text("hello");
        assert_eq!(
            framing_of_request(&req),
            None,
            "a body alone declares no wire framing"
        );
        req.headers.insert("content-length", "5");
        assert_eq!(framing_of_request(&req), Some(Framing::ContentLength));
        req.headers.remove("content-length");
        req.headers.insert("transfer-encoding", "chunked");
        assert_eq!(framing_of_request(&req), Some(Framing::Chunked));
    }

    #[test]
    fn observed_response_framing_follows_rfc9110_cases() {
        let no_body = Response::new(204).unwrap();
        assert_eq!(framing_of_response(&no_body), None, "204 has no body");

        let mut res = Response::new(200).unwrap();
        res.body = Body::text("abc");
        res.headers.insert("content-length", "3");
        assert_eq!(framing_of_response(&res), Some(Framing::ContentLength));
        res.headers.insert("transfer-encoding", "chunked");
        assert_eq!(framing_of_response(&res), Some(Framing::Chunked));
    }

    #[test]
    fn forwarded_request_framing_mirrors_serialize_request() {
        let mut req = Request::new(Method::Get, Url::parse("http://a/").unwrap());
        assert_eq!(forwarded_h1_req_framing(&req), None, "bare GET: no framing");
        req.headers.insert("content-length", "0");
        assert_eq!(
            forwarded_h1_req_framing(&req),
            Some(Framing::ContentLength),
            "explicit zero-length body keeps its declared framing"
        );
        let mut post = Request::new(Method::Post, Url::parse("http://a/").unwrap());
        post.body = Body::text("x");
        assert_eq!(
            forwarded_h1_req_framing(&post),
            Some(Framing::ContentLength)
        );
    }
}

//! Replay (Phase 2, ADR-012) — re-execute a request with no client leg.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Replay is the operator's *direct voice*: the request goes upstream exactly
//! as submitted — no pipeline, no hooks, no intercept — but it is still a
//! first-class flow: it gets an id, is stored in history tagged `replay`, and
//! publishes the same lifecycle events as a proxied exchange so every client
//! (CLI, UI, future plugins) sees it live.
//!
//! Control-plane shape: `flows_replay` returns the flow id immediately and the
//! execution continues in the background — the control plane is sequential per
//! connection (ADR-010), so a slow origin must never stall the operator.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aether_core::EventBus;
use aether_core::error::{AetherError, AetherResult};
use aether_core::events::AetherEvent;
use aether_core::model::{Body, Flow, FlowId, Request, TransportProto};
use aether_core::storage::Storage;
use aether_http::upstream::{self, UpstreamCfg};
use tokio::io::AsyncReadExt;

/// Extra bytes drained from a streaming remainder (bodies beyond the capture
/// limit). Replay has no client leg to stream to, so the remainder is drained
/// into the stored body up to this cap; anything further tags the flow
/// `truncated` honestly instead of pretending completeness.
const REMAINDER_DRAIN_CAP: usize = 8 * 1024 * 1024;
const REMAINDER_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Structural sanity for a replay target before any socket is opened.
pub fn validate_replay_request(req: &Request) -> AetherResult<()> {
    let scheme_ok =
        req.url.scheme.eq_ignore_ascii_case("http") || req.url.scheme.eq_ignore_ascii_case("https");
    if !scheme_ok {
        return Err(AetherError::protocol(format!(
            "replay: unsupported scheme '{}' (http/https only)",
            req.url.scheme
        )));
    }
    if req.url.host.trim().is_empty() {
        return Err(AetherError::protocol("replay: empty host"));
    }
    Ok(())
}

/// Executes the request upstream, records the flow, publishes events.
/// The flow id is supplied by the caller (the control plane reserves it first
/// so it can answer the operator immediately while the replay runs in the
/// background) and is persisted in-flight first, so a crash mid-replay still
/// leaves a trace in history.
pub async fn run_replay(
    request: Request,
    flow_id: FlowId,
    storage: Arc<dyn Storage>,
    bus: EventBus,
    cfg: UpstreamCfg,
) -> AetherResult<()> {
    validate_replay_request(&request)?;
    let mut flow = Flow::new(flow_id, request.clone());
    flow.add_tag("replay");
    flow.meta.server_proto = if request.url.scheme.eq_ignore_ascii_case("https") {
        TransportProto::Tls
    } else {
        TransportProto::Plaintext
    };
    bus.publish(AetherEvent::FlowStarted { flow_id });
    if let Err(e) = storage.put_flow(&flow).await {
        bus.publish(AetherEvent::ErrorOccurred {
            context: "replay.put_flow".into(),
            message: e.to_string(),
        });
    }

    let started = Instant::now();

    // Same rule as the engine: client-side hop-by-hop headers never cross the
    // proxy boundary upstream (RFC 9110 §7.6.1).
    let mut wire_req = request.clone();
    aether_http::wire::strip_hop_by_hop(&mut wire_req.headers);

    match upstream::send_request(&wire_req, &cfg).await {
        Ok(mut reply) => {
            if let Some(remainder) = reply.remainder.as_mut() {
                match drain_remainder(remainder).await {
                    Ok((extra, more)) => {
                        if more {
                            flow.add_tag("truncated");
                        }
                        append_to_body(&mut reply.response.body, &extra);
                    }
                    Err(e) => {
                        flow.add_tag("truncated");
                        bus.publish(AetherEvent::ErrorOccurred {
                            context: "replay.drain_remainder".into(),
                            message: e.to_string(),
                        });
                    }
                }
            }
            flow.response = Some(reply.response);
            flow.meta.duration_ms = Some(started.elapsed().as_millis() as u64);
            let status = flow.response.as_ref().map(|r| r.status.code).unwrap_or(0);
            bus.publish(AetherEvent::ResponseReceived { flow_id, status });
            bus.publish(AetherEvent::FlowCompleted { flow_id });
            if let Err(e) = storage.put_flow(&flow).await {
                bus.publish(AetherEvent::ErrorOccurred {
                    context: "replay.put_flow".into(),
                    message: e.to_string(),
                });
            }
            Ok(())
        }
        Err(e) => {
            flow.add_tag("network-error");
            flow.meta.duration_ms = Some(started.elapsed().as_millis() as u64);
            let _ = storage.put_flow(&flow).await;
            bus.publish(AetherEvent::ErrorOccurred {
                context: "replay.upstream".into(),
                message: e.to_string(),
            });
            Err(e)
        }
    }
}

/// Bounded, time-boxed drain of a streaming remainder.
async fn drain_remainder(
    remainder: &mut (dyn tokio::io::AsyncRead + Unpin + Send),
) -> AetherResult<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let deadline = tokio::time::sleep(REMAINDER_DRAIN_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => {
                return Err(AetherError::network("replay: remainder drain timeout"));
            }
            read = remainder.read(&mut chunk) => {
                match read {
                    Ok(0) => return Ok((buf, false)),
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.len() >= REMAINDER_DRAIN_CAP {
                            return Ok((buf, true));
                        }
                    }
                    Err(e) => return Err(AetherError::network(format!("replay remainder: {e}"))),
                }
            }
        }
    }
}

/// Appends drained bytes, preserving text bodies when the result is valid
/// UTF-8 (binary stays binary — no lossy reinterpretation).
fn append_to_body(body: &mut Body, extra: &[u8]) {
    if extra.is_empty() {
        return;
    }
    let mut all = body.as_bytes().to_vec();
    all.extend_from_slice(extra);
    *body = match body {
        Body::Text { .. } => match String::from_utf8(all) {
            Ok(text) => Body::Text { text },
            Err(e) => Body::Binary {
                bytes: e.into_bytes(),
            },
        },
        _ => Body::Binary { bytes: all },
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_core::model::{Method, Url};
    use aether_core::storage::MemoryStorage;

    fn req(url: &str) -> Request {
        let mut r = Request::new(Method::Get, Url::parse(url).unwrap());
        r.set_header("host", "example.com");
        r
    }

    #[test]
    fn validation_rejects_bad_scheme_and_empty_host() {
        assert!(validate_replay_request(&req("https://a.com/")).is_ok());
        assert!(validate_replay_request(&req("ftp://a.com/")).is_err());
        let mut empty = req("https://a.com/");
        empty.url.host.clear();
        assert!(validate_replay_request(&empty).is_err());
    }

    #[tokio::test]
    async fn network_error_records_flow_tagged_replay() {
        let storage: Arc<dyn Storage> = Arc::new(MemoryStorage::new());
        let bus = EventBus::new(16);
        let mut sub = bus.subscribe();

        // unroutable link-local address, tiny timeouts → fast failure
        let cfg = UpstreamCfg {
            connect_timeout: Duration::from_millis(300),
            response_timeout: Duration::from_millis(300),
            ..Default::default()
        };
        let flow_id = storage.alloc_id().await.unwrap();
        let r = run_replay(
            req("http://203.0.113.1:9/x"),
            flow_id,
            storage.clone(),
            bus,
            cfg,
        )
        .await;
        assert!(r.is_err(), "unroutable target must fail");

        // the failed attempt is still in history, tagged replay + network-error
        let page = storage
            .list_flows(aether_core::storage::FlowQuery {
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(
            page.items[0]
                .tags
                .iter()
                .any(|t| t.eq_ignore_ascii_case("replay"))
        );

        // FlowStarted was published before the failure
        assert!(matches!(
            sub.try_next(),
            Some(AetherEvent::FlowStarted { .. })
        ));
    }

    #[test]
    fn append_to_body_preserves_text_and_upgrades_binary() {
        let mut text = Body::text("hello ");
        append_to_body(&mut text, b"world");
        assert_eq!(text, Body::text("hello world"));

        let mut broken = Body::text("héllo ".to_string());
        append_to_body(&mut broken, &[0xff, 0xfe]);
        assert!(matches!(broken, Body::Binary { .. }));

        let mut bin = Body::binary(vec![1, 2]);
        append_to_body(&mut bin, &[3]);
        assert_eq!(bin, Body::binary(vec![1, 2, 3]));

        let mut empty = Body::Empty;
        append_to_body(&mut empty, b"");
        assert_eq!(empty, Body::Empty);
    }
}

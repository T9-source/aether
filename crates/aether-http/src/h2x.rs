//! Aether — HTTP/2 leg codec (V0.2, ADR-013).
//! Copyright 2026 T9-source. Licensed under Apache-2.0 (see LICENSE).
//!
//! The h1 wire codec is hand-rolled because a security proxy must own its
//! bytes. HTTP/2 is a different animal: HPACK state machines, flow-control
//! windows and frame encoders are exactly the kind of code where a subtle
//! bug becomes an interoperability black hole — so this module delegates the
//! **connection machinery** to the `h2` crate (the hyper team's, fuzzed by
//! the ecosystem) and keeps for itself everything ADR-003 demands:
//!
//! - the Traffic Model stays the single representation (`Request`/`Response`
//!   with ordered, duplicate-capable headers — h2 semantics were designed in
//!   from day 1),
//! - the engine remains the single path (`serve_connection` re-runs
//!   `FlowEngine::handle_request` per stream; nothing h2-specific reaches
//!   the pipeline),
//! - bounded capture (ADR-011) on every body, with the excess exposed as a
//!   streaming remainder exactly like the h1 path,
//! - hop-by-hop headers are stripped in both directions (h2 is stricter than
//!   h1 here: the `h2` crate *refuses* to send them),
//! - known trade-offs are documented in `docs/LIMITATIONS.md` (header order
//!   best-effort via `HeaderMap`, no trailers, no h2c).
//!
//! RFC 8441 (WebSocket over h2 `CONNECT`) is deliberately out of scope; WS
//! inspection rides the h1 upgrade path (`ws`).

use aether_core::error::{AetherError, AetherResult};
use aether_core::model::{Body, Headers, HttpVersion, Method, Request, Response, StatusCode, Url};
use bytes::{Buf, Bytes};
use http::header::{CONTENT_LENGTH, HeaderName, HeaderValue};
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

use crate::engine::{ConnInfo, FlowEngine};
use crate::tls::TlsFacts;
use crate::upstream::{Remainder, UpstreamCfg, UpstreamReply};
use crate::wire;

// ---------------------------------------------------------------------------
// model ⇄ http types (wire truth lives here)
// ---------------------------------------------------------------------------

/// Model `Request` → `http::Request` for the h2 upstream leg. Content-Length
/// is recomputed (never trusted — hooks may have edited the body), hop-by-hop
/// headers are stripped, and values the h2 layer cannot carry are dropped
/// loudly (counted in the log, never silently mangled).
pub fn to_http_request(req: &Request) -> AetherResult<http::Request<()>> {
    let method = http::Method::from_bytes(req.method.as_str().as_bytes()).map_err(|_| {
        AetherError::protocol(format!(
            "method not carryable on h2: {}",
            req.method.as_str()
        ))
    })?;

    let scheme = http::uri::Scheme::from_str(&req.url.scheme.to_ascii_lowercase())
        .map_err(|_| AetherError::protocol(format!("bad scheme '{}'", req.url.scheme)))?;
    let authority = http::uri::Authority::from_str(&req.url.authority())
        .map_err(|_| AetherError::protocol(format!("bad authority '{}'", req.url.authority())))?;
    let uri = http::Uri::builder()
        .scheme(scheme)
        .authority(authority)
        .path_and_query(req.url.path_and_query())
        .build()
        .map_err(|e| AetherError::protocol(format!("bad h2 uri: {e}")))?;

    let mut headers = req.headers.clone();
    wire::strip_hop_by_hop(&mut headers);
    let had_cl = headers.contains("content-length");

    let mut builder = http::Request::builder().method(method).uri(uri);
    for h in headers.iter() {
        let name = match HeaderName::from_bytes(h.name.as_bytes()) {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(header = %h.name, "h2: dropping header with invalid name");
                continue;
            }
        };
        let value = match HeaderValue::from_bytes(h.value.as_bytes()) {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(header = %h.name, "h2: dropping header with invalid value bytes");
                continue;
            }
        };
        builder = builder.header(name, value);
    }

    let body = Bytes::copy_from_slice(req.body.as_bytes());
    if !body.is_empty() || had_cl {
        builder = builder.header(CONTENT_LENGTH, body.len().to_string());
    }

    builder
        .body(())
        .map_err(|e| AetherError::protocol(format!("h2 request build: {e}")))
}

/// The exact body bytes the h2 leg will send for `req` (kept separate from
/// [`to_http_request`] because `http::Request` carries an empty body — the
/// `h2` crate streams DATA through `SendStream` instead).
pub fn request_body_bytes(req: &Request) -> Bytes {
    Bytes::copy_from_slice(req.body.as_bytes())
}

/// `http::Response` → model `Response` (h2 upstream leg).
pub fn from_http_response(resp: http::Response<()>) -> AetherResult<Response> {
    let (parts, _) = resp.into_parts();
    let mut headers = Headers::new();
    for (name, value) in parts.headers.iter() {
        headers.append(
            name.as_str(),
            String::from_utf8_lossy(value.as_bytes()).into_owned(),
        );
    }
    wire::strip_hop_by_hop(&mut headers);
    Ok(Response {
        status: StatusCode::new(parts.status.as_u16())?,
        version: HttpVersion::Http2,
        headers,
        body: Body::Empty,
    })
}

/// Model `Response` → `http::Response` head for the h2 client leg.
pub fn to_http_response(res: &Response) -> AetherResult<http::Response<()>> {
    let mut headers = res.headers.clone();
    headers.remove("transfer-encoding"); // illegal on h2; framing is DATA frames
    let mut builder = http::Response::builder().status(res.status.code);
    for h in headers.iter() {
        let name = match HeaderName::from_bytes(h.name.as_bytes()) {
            Ok(n) => n,
            Err(_) => continue,
        };
        let value = match HeaderValue::from_bytes(h.value.as_bytes()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        builder = builder.header(name, value);
    }
    builder
        .body(())
        .map_err(|e| AetherError::protocol(format!("h2 response build: {e}")))
}

// ---------------------------------------------------------------------------
// body plumbing: bounded capture + streaming remainder over h2 RecvStream
// ---------------------------------------------------------------------------

/// Collects the (already flow-control-released) DATA payload of a
/// `RecvStream` up to `cap`; anything beyond stays available on the
/// [`H2Remainder`] for streaming to the client (ADR-011).
async fn capture_body(
    mut recv: h2::RecvStream,
    cap: usize,
) -> AetherResult<(Vec<u8>, Option<Remainder>)> {
    let mut captured: Vec<u8> = Vec::new();
    loop {
        let data = match std::future::poll_fn(|cx| recv.poll_data(cx)).await {
            Some(Ok(d)) => d,
            Some(Err(e)) => return Err(AetherError::network(format!("h2 body read: {e}"))),
            None => return Ok((captured, None)), // END_STREAM within cap
        };
        let _ = recv.flow_control().release_capacity(data.len());
        if captured.len() + data.len() <= cap {
            captured.extend_from_slice(&data);
        } else {
            let keep = cap - captured.len();
            captured.extend_from_slice(&data[..keep]);
            let over = data.slice(keep..);
            return Ok((
                captured,
                Some(Box::new(H2Remainder {
                    recv,
                    buf: over,
                    eof: false,
                })),
            ));
        }
    }
}

/// `AsyncRead` adapter over the unconsumed tail of an h2 `RecvStream`.
/// Releases flow-control capacity as bytes are handed out so the origin
/// can keep sending (a stalled window is a deadlock, not a safety feature).
pub struct H2Remainder {
    recv: h2::RecvStream,
    buf: Bytes,
    eof: bool,
}

impl AsyncRead for H2Remainder {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        loop {
            if !this.buf.is_empty() {
                let n = buf.remaining().min(this.buf.len());
                buf.put_slice(&this.buf[..n]);
                this.buf.advance(n);
                return std::task::Poll::Ready(Ok(()));
            }
            if this.eof {
                return std::task::Poll::Ready(Ok(()));
            }
            match this.recv.poll_data(cx) {
                std::task::Poll::Ready(Some(Ok(data))) => {
                    let _ = this.recv.flow_control().release_capacity(data.len());
                    this.buf = data;
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    return std::task::Poll::Ready(Err(std::io::Error::other(e.to_string())));
                }
                std::task::Poll::Ready(None) => {
                    this.eof = true;
                }
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}

/// Sends a full payload through a `SendStream`, honoring flow control
/// (reserve → wait for granted capacity → emit ≤16 KiB frames).
async fn send_data_all(send: &mut h2::SendStream<Bytes>, mut data: Bytes) -> AetherResult<()> {
    while !data.is_empty() {
        send.reserve_capacity(data.len().min(16 * 1024));
        match std::future::poll_fn(|cx| send.poll_capacity(cx)).await {
            Some(Ok(granted)) if granted > 0 => {
                let chunk = data.split_to(granted.min(data.len()));
                send.send_data(chunk, false)
                    .map_err(|e| AetherError::network(format!("h2 data send: {e}")))?;
            }
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(AetherError::network(format!("h2 capacity: {e}"))),
            None => return Err(AetherError::network("h2 stream capacity closed mid-body")),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// upstream leg: h2 client exchange
// ---------------------------------------------------------------------------

/// Runs one request/response exchange over an established h2 connection
/// (the connection is created here and dies with the exchange — upstream
/// pooling per client-connection arrives with the performance phase).
pub async fn client_exchange(
    io: tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    req: &Request,
    cfg: &UpstreamCfg,
) -> AetherResult<UpstreamReply> {
    let handshake = tokio::time::timeout(cfg.connect_timeout, h2::client::handshake(io)).await;
    let handshake = match handshake {
        Ok(h) => h,
        Err(_) => return Err(AetherError::network("h2 upstream handshake timeout")),
    };
    let (send_request, connection) =
        handshake.map_err(|e| AetherError::network(format!("h2 upstream handshake: {e}")))?;
    tokio::spawn(async move {
        let _ = connection.await; // drives PING/settings/flow control
    });

    let mut send_request = send_request
        .ready()
        .await
        .map_err(|e| AetherError::network(format!("h2 upstream not ready: {e}")))?;

    let http_req = to_http_request(req)?;
    let (response_fut, mut send_stream) = send_request
        .send_request(http_req, false)
        .map_err(|e| AetherError::network(format!("h2 send_request: {e}")))?;

    let body = request_body_bytes(req);
    if !body.is_empty() {
        send_data_all(&mut send_stream, body).await?;
    }
    let _ = send_stream.send_data(Bytes::new(), true);

    let response = tokio::time::timeout(cfg.response_timeout, response_fut).await;
    let response = match response {
        Ok(r) => r.map_err(|e| AetherError::network(format!("h2 upstream response: {e}")))?,
        Err(_) => return Err(AetherError::network("h2 upstream response timeout")),
    };
    let (parts, recv) = response.into_parts();
    let mut response = from_http_response(http::Response::from_parts(parts, ()))?;

    let head_request = req.method == Method::Head;
    let (body_bytes, remainder) = capture_body(recv, cfg.capture_max_body).await?;
    response.body = wire::body_from_captured(body_bytes);
    if head_request {
        response.body = Body::Empty;
    }
    Ok(UpstreamReply {
        response,
        remainder,
        leg_h2: true,
    })
}

// ---------------------------------------------------------------------------
// client leg: h2 server per MITM connection
// ---------------------------------------------------------------------------

/// Serves one MITM'd connection whose client negotiated `h2` via ALPN.
/// Each request stream runs through the **same** engine path as h1 traffic;
/// the console, CLI, rules and intercept cannot tell the legs apart (and
/// must not have to — ADR-006 #1).
///
/// Shutdown drains per ADR-018: GOAWAY first, in-flight streams finish, and
/// a client that ignores GOAWAY is cut after [`H2_DRAIN_DEADLINE`].
pub const H2_DRAIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

pub async fn serve_connection(
    engine: Arc<FlowEngine>,
    io: Box<dyn crate::server::AsyncReadWrite>,
    peer: SocketAddr,
    tls_facts: Option<TlsFacts>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut builder = h2::server::Builder::new();
    builder
        .initial_window_size(1024 * 1024)
        .initial_connection_window_size(2 * 1024 * 1024)
        // Bounded concurrency: without this the h2 server accepts unlimited
        // concurrent streams; each spawns a task with up to 8MB capture —
        // 1000 streams ≈ OOM (audit M2). 128 is generous for real clients.
        .max_concurrent_streams(128);

    let handshake = builder.handshake::<_, Bytes>(io).await;
    let mut connection = match handshake {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(%peer, error = %e, "h2 handshake failed");
            return;
        }
    };

    loop {
        tokio::select! {
            accepted = connection.accept() => {
                let Some(accepted) = accepted else { break };
                let (http_req, respond) = match accepted {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::debug!(%peer, error = %e, "h2 accept stream failed");
                        break;
                    }
                };
                spawn_stream(&engine, &tls_facts, peer, http_req, respond);
            }
            _ = shutdown.changed() => {
                // ADR-018 (graceful GOAWAY): refuse *new* streams, let
                // in-flight streams finish. Per RFC 7540 §6.8 the CLIENT
                // owns the close after GOAWAY — a client that never closes
                // must not hold the daemon hostage, hence the drain
                // deadline (nginx's worker_shutdown_timeout, same idea).
                tracing::debug!(%peer, "h2 client leg: graceful GOAWAY (shutdown)");
                connection.graceful_shutdown();
                let deadline = tokio::time::sleep(H2_DRAIN_DEADLINE);
                tokio::pin!(deadline);
                loop {
                    tokio::select! {
                        _ = &mut deadline => {
                            tracing::debug!(%peer, "h2 drain deadline elapsed — cutting connection");
                            break;
                        }
                        accepted = connection.accept() => {
                            let Some(accepted) = accepted else { break }; // connection done
                            match accepted {
                                Ok((http_req, respond)) => {
                                    spawn_stream(&engine, &tls_facts, peer, http_req, respond);
                                }
                                Err(e) => {
                                    tracing::debug!(%peer, error = %e, "h2 accept failed during drain");
                                    break;
                                }
                            }
                        }
                    }
                }
                break;
            }
        }
    }
}

fn spawn_stream(
    engine: &Arc<FlowEngine>,
    tls_facts: &Option<TlsFacts>,
    peer: SocketAddr,
    http_req: http::Request<h2::RecvStream>,
    respond: h2::server::SendResponse<Bytes>,
) {
    let engine = engine.clone();
    let tls_facts = tls_facts.clone();
    tokio::spawn(async move {
        if let Err(e) = handle_stream(engine, http_req, respond, peer, tls_facts).await {
            tracing::debug!(%peer, error = %e, "h2 stream ended with error");
        }
    });
}

async fn handle_stream(
    engine: Arc<FlowEngine>,
    req: http::Request<h2::RecvStream>,
    mut respond: h2::server::SendResponse<Bytes>,
    peer: SocketAddr,
    tls_facts: Option<TlsFacts>,
) -> AetherResult<()> {
    let (parts, body_recv) = req.into_parts();

    // ── model request from h2 semantics (ADR-003: the quadruple maps 1:1) ──
    let uri = parts.uri.clone();
    let scheme = uri.scheme_str().unwrap_or("https").to_string();
    let authority = match uri.authority() {
        Some(a) => a.to_string(),
        None => {
            return refuse(&mut respond, 400, "h2 request missing :authority").await;
        }
    };
    let (path, query) = match uri.path_and_query().map(|pq| pq.as_str()) {
        Some(pq) => match pq.split_once('?') {
            Some((p, q)) => (p.to_string(), Some(q.to_string())),
            None => (pq.to_string(), None),
        },
        None => ("/".to_string(), None),
    };
    let host = match crate::wire::split_authority(&authority) {
        Ok((host, port)) => Url {
            scheme: scheme.clone(),
            port,
            host,
            path: path.clone(),
            query: query.clone(),
            fragment: None,
        },
        Err(_) => {
            return refuse(&mut respond, 400, "bad :authority").await;
        }
    };
    if host.path.is_empty() {
        // (path_and_query always yields at least "/")
    }

    let method = Method::parse(parts.method.as_str());
    let mut request = Request {
        method,
        version: HttpVersion::Http2,
        url: host,
        headers: Headers::new(),
        body: Body::Empty,
    };
    for (name, value) in parts.headers.iter() {
        request.headers.append(
            name.as_str(),
            String::from_utf8_lossy(value.as_bytes()).into_owned(),
        );
    }

    // ── bounded request body (ADR-011) ─────────────────────────────────
    let cap = engine.upstream_cfg.capture_max_body;
    let (body_bytes, overflow) = capture_body(body_recv, cap).await?;
    request.body = wire::body_from_captured(body_bytes);
    if overflow.is_some() {
        // an edited/pipelined request cannot be reconciled with unread
        // upload bytes — refuse loudly, mirroring the h1 413 behavior
        return refuse(
            &mut respond,
            413,
            &format!("h2 request body exceeds capture limit of {cap}"),
        )
        .await;
    }

    let conn = ConnInfo {
        client_addr: peer,
        client_proto: aether_core::model::TransportProto::Tls,
        client_h2: true,
        tls: tls_facts,
    };

    match engine.handle_request(request, conn).await {
        crate::engine::EngineOutcome::Serve {
            response,
            remainder,
        } => {
            let http_resp = to_http_response(&response)?;
            let _is_head_like = response.status.code == 204 || response.status.code == 304;
            let has_cl = response.headers.contains("content-length");
            match respond.send_response(http_resp, false) {
                Ok(mut send) => {
                    let mut payload = Bytes::copy_from_slice(response.body.as_bytes());
                    if let Some(mut rem) = remainder {
                        // captured prefix first, then the streaming tail
                        if send_data_all(&mut send, payload.split_off(0))
                            .await
                            .is_err()
                        {
                            tracing::debug!("h2 response body send failed");
                            send.send_reset(h2::Reason::CANCEL);
                            return Ok(());
                        }
                        let mut chunk = vec![0u8; 16 * 1024];
                        loop {
                            match rem.read(&mut chunk).await {
                                Ok(0) => break,
                                Ok(n) => {
                                    if send_data_all(&mut send, Bytes::copy_from_slice(&chunk[..n]))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                    } else if send_data_all(&mut send, payload.split_off(0))
                        .await
                        .is_err()
                    {
                        tracing::debug!("h2 response body send failed");
                        send.send_reset(h2::Reason::CANCEL);
                        return Ok(());
                    }
                    let _ = has_cl;
                    let _ = send.send_data(Bytes::new(), true);
                }
                Err(e) => {
                    tracing::debug!(error = %e, "h2 send_response failed");
                }
            }
            Ok(())
        }
        crate::engine::EngineOutcome::Dropped { reason } => {
            refuse(&mut respond, 502, &format!("exchange dropped: {reason}")).await
        }
    }
}

/// Sends a minimal JSON error response and closes the stream (the h2
/// equivalent of the h1 error path in `server.rs`).
async fn refuse(
    respond: &mut h2::server::SendResponse<Bytes>,
    status: u16,
    message: &str,
) -> AetherResult<()> {
    let body = serde_json::json!({ "error": { "status": status, "message": message } }).to_string();
    let Ok(resp) = http::Response::builder()
        .status(status)
        .header("content-type", "application/json; charset=utf-8")
        .body(())
    else {
        return Ok(());
    };
    match respond.send_response(resp, false) {
        Ok(mut send) => {
            let _ = send.send_data(Bytes::from(body), true);
        }
        Err(_) => {
            respond.send_reset(h2::Reason::INTERNAL_ERROR);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_core::model::{Method, Url};

    fn h2_request() -> Request {
        let mut r = Request::new(
            Method::Post,
            Url::parse("https://api.example.com/v1/x?a=1").unwrap(),
        );
        r.set_header("content-type", "application/json");
        r.set_header("x-custom", "keep");
        r.set_header("connection", "close"); // must be stripped
        r.body = Body::text("{\"a\":1}");
        r
    }

    #[test]
    fn to_http_request_maps_semantics_and_strips_hop_by_hop() {
        let req = to_http_request(&h2_request()).unwrap();
        assert_eq!(req.method(), http::Method::POST);
        // http::Uri drops the default port — a faithful h2 :authority
        assert_eq!(req.uri().to_string(), "https://api.example.com/v1/x?a=1");
        assert!(
            req.headers().get("connection").is_none(),
            "hop-by-hop stripped"
        );
        assert!(req.headers().get("content-length").is_some());
        assert_eq!(req.headers()["content-length"], "7", "truthful CL");
        assert_eq!(req.headers()["x-custom"], "keep");
    }

    #[test]
    fn request_body_bytes_matches_model_body() {
        assert_eq!(request_body_bytes(&h2_request()).as_ref(), b"{\"a\":1}");
    }

    #[test]
    fn from_http_response_keeps_duplicates_and_strips_hop_by_hop() {
        let builder = http::Response::builder()
            .status(200)
            .header("set-cookie", "a=1")
            .header("set-cookie", "b=2")
            .header("transfer-encoding", "chunked");
        let resp = builder.body(()).unwrap();
        let model = from_http_response(resp).unwrap();
        assert_eq!(model.version, HttpVersion::Http2);
        assert_eq!(model.status.code, 200);
        let mut cookies = model.headers.get_all("set-cookie");
        cookies.sort_unstable();
        assert_eq!(cookies, vec!["a=1", "b=2"], "duplicates preserved");
        assert!(!model.headers.contains("transfer-encoding"));
    }

    #[test]
    fn to_http_response_drops_transfer_encoding() {
        let mut res = Response::new(200).unwrap();
        res.set_header("content-length", "5");
        res.set_header("transfer-encoding", "chunked");
        let out = to_http_response(&res).unwrap();
        assert!(out.headers().get("transfer-encoding").is_none());
        assert_eq!(out.headers()["content-length"], "5");
    }
}

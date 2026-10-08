//! Aether — network engine.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Upstream leg: dial the origin, send the (already hooked/edited) request,
//! read the response with **bounded capture** — bodies above the capture
//! limit are streamed to the client while only a prefix is kept for history
//! (ADR-011). Chunked upstream bodies are decoded fully before the client
//! sees them, so the client leg always receives exact `Content-Length`
//! framing regardless of what the origin spoke.

use aether_core::error::{AetherError, AetherResult};
use aether_core::model::{Request, Response};
use rustls_pki_types::ServerName;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::Duration;
use tokio_rustls::TlsConnector;

use crate::tls;
use crate::wire;

/// Reader positioned right after the captured prefix of a larger body.
pub type Remainder = Box<dyn AsyncRead + Unpin + Send>;

pub struct UpstreamReply {
    pub response: Response,
    /// `Some` when the body exceeded the capture limit: the unparsed rest of
    /// the body, ready to stream to the client.
    pub remainder: Option<Remainder>,
}

/// Wire stream to the origin. (TLS wraps TCP — the 40-byte delta is the
/// whole point of the enum, and one instance lives per upstream request.)
#[allow(clippy::large_enum_variant)]
pub enum UpstreamStream {
    Tcp(TcpStream),
    Tls(tokio_rustls::client::TlsStream<TcpStream>),
}

impl AsyncRead for UpstreamStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            UpstreamStream::Tcp(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            UpstreamStream::Tls(s) => std::pin::Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for UpstreamStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match &mut *self {
            UpstreamStream::Tcp(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            UpstreamStream::Tls(s) => std::pin::Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            UpstreamStream::Tcp(s) => std::pin::Pin::new(s).poll_flush(cx),
            UpstreamStream::Tls(s) => std::pin::Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            UpstreamStream::Tcp(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            UpstreamStream::Tls(s) => std::pin::Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// Engine-facing knobs the upstream needs.
#[derive(Debug, Clone)]
pub struct UpstreamCfg {
    /// Capture limit in bytes (per body). Default 8 MiB (ADR-011).
    pub capture_max_body: usize,
    /// Validate upstream certificates (default true; off = lab mode).
    pub upstream_tls_verify: bool,
    /// `"host"` or `"host:port"` → `"socket://host:port"` dial override
    /// (labs, replay against local mocks).
    pub host_overrides: std::collections::BTreeMap<String, String>,
    pub connect_timeout: Duration,
    pub response_timeout: Duration,
}

impl Default for UpstreamCfg {
    fn default() -> Self {
        Self {
            capture_max_body: 8 * 1024 * 1024,
            upstream_tls_verify: true,
            host_overrides: Default::default(),
            connect_timeout: Duration::from_secs(10),
            response_timeout: Duration::from_secs(120),
        }
    }
}

fn resolve_dial_target(
    host: &str,
    port: u16,
    overrides: &std::collections::BTreeMap<String, String>,
) -> (String, u16) {
    if let Some(target) = overrides
        .get(&format!("{host}:{port}"))
        .or_else(|| overrides.get(host))
    {
        let t = target.strip_prefix("socket://").unwrap_or(target);
        if let Some((h, p)) = t.rsplit_once(':') {
            if let Ok(p) = p.parse::<u16>() {
                let h = h.trim_matches(|c| c == '[' || c == ']');
                return (h.to_string(), p);
            }
        }
    }
    (host.to_string(), port)
}

async fn dial(host: &str, port: u16, cfg: &UpstreamCfg) -> AetherResult<TcpStream> {
    let (dial_host, dial_port) = resolve_dial_target(host, port, &cfg.host_overrides);
    let addr = tokio::net::lookup_host((dial_host.as_str(), dial_port))
        .await
        .map_err(|e| AetherError::network(format!("dns lookup {dial_host}: {e}")))?
        .next()
        .ok_or_else(|| AetherError::network(format!("no addresses for {dial_host}")))?;
    tokio::time::timeout(cfg.connect_timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| AetherError::network(format!("connect timeout to {addr}")))?
        .map_err(|e| AetherError::network(format!("connect {addr}: {e}")))
}

fn server_name_for(host: &str) -> AetherResult<ServerName<'static>> {
    let unbracketed = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = unbracketed.parse::<std::net::IpAddr>() {
        return Ok(ServerName::IpAddress(ip.into()));
    }
    ServerName::try_from(unbracketed.to_string())
        .map_err(|_| AetherError::tls(format!("invalid server name '{host}'")))
}

/// Sends the request upstream and returns the captured response + optional
/// streaming remainder.
pub async fn send_request(req: &Request, cfg: &UpstreamCfg) -> AetherResult<UpstreamReply> {
    let host = req.url.host.clone();
    let port = req.url.effective_port();
    let use_tls = req.url.scheme.eq_ignore_ascii_case("https");

    let tcp = dial(&host, port, cfg).await?;
    let stream = if use_tls {
        let connector = TlsConnector::from(Arc::new(tls::upstream_client_config(
            cfg.upstream_tls_verify,
        )?));
        let server_name = server_name_for(&host)?;
        let tls_stream =
            tokio::time::timeout(cfg.connect_timeout, connector.connect(server_name, tcp))
                .await
                .map_err(|_| AetherError::tls(format!("tls handshake timeout to {host}")))?
                .map_err(|e| AetherError::tls(format!("tls handshake to {host}: {e}")))?;
        UpstreamStream::Tls(tls_stream)
    } else {
        UpstreamStream::Tcp(tcp)
    };

    let bytes = wire::serialize_request(req);
    let mut stream = stream;
    tokio::time::timeout(cfg.response_timeout, async {
        stream.write_all(&bytes).await?;
        stream.flush().await
    })
    .await
    .map_err(|_| AetherError::network("upstream write timeout"))?
    .map_err(|e| AetherError::network(format!("upstream write: {e}")))?;

    let mut reader = BufReader::new(stream);
    let head = tokio::time::timeout(
        cfg.response_timeout,
        wire::read_until_needle(&mut reader, b"\r\n\r\n", wire::MAX_HEAD_BYTES),
    )
    .await
    .map_err(|_| AetherError::network("upstream response head timeout"))?
    .and_then(|o| o.ok_or_else(|| AetherError::network("upstream closed before response head")))?;

    let request_was_head = req.method == aether_core::model::Method::Head;
    let mut response = wire::parse_response_head(&head, request_was_head)?;
    wire::strip_hop_by_hop(&mut response.headers);
    let spec = wire::response_body_spec(&response.headers, response.status.code, request_was_head);

    let (body, remainder) = read_captured(reader, spec, cfg.capture_max_body).await?;
    response.body = wire::body_from_captured(body);
    Ok(UpstreamReply {
        response,
        remainder,
    })
}

/// Reads at most `cap` bytes of a body, returning the captured prefix and —
/// when truncated — a remainder reader positioned exactly after it.
async fn read_captured<R>(
    reader: BufReader<R>,
    spec: wire::BodySpec,
    cap: usize,
) -> AetherResult<(Vec<u8>, Option<Remainder>)>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    match spec {
        wire::BodySpec::Empty => Ok((Vec::new(), None)),
        wire::BodySpec::ContentLength(n) => {
            let mut reader = reader;
            if n <= cap as u64 {
                let mut buf = vec![0u8; n as usize];
                reader
                    .read_exact(&mut buf)
                    .await
                    .map_err(|e| AetherError::network(format!("read body: {e}")))?;
                Ok((buf, None))
            } else {
                let mut captured = vec![0u8; cap];
                reader
                    .read_exact(&mut captured)
                    .await
                    .map_err(|e| AetherError::network(format!("read body: {e}")))?;
                let remaining = n - cap as u64;
                Ok((
                    captured,
                    Some(Box::new(ExactRemainder { reader, remaining })),
                ))
            }
        }
        wire::BodySpec::Chunked => {
            let mut chunked = ChunkedReader::new(reader);
            let mut captured = Vec::new();
            let mut tmp = vec![0u8; 8192];
            loop {
                let n = chunked
                    .read(&mut tmp)
                    .await
                    .map_err(|e| AetherError::network(format!("read chunked body: {e}")))?;
                if n == 0 {
                    return Ok((captured, None)); // fully decoded within cap
                }
                if captured.len() + n > cap {
                    let keep = cap - captured.len();
                    captured.extend_from_slice(&tmp[..keep]);
                    let over = tmp[keep..n].to_vec();
                    return Ok((
                        captured,
                        Some(Box::new(PassthroughPrefix {
                            prefix: over,
                            inner: chunked,
                        })),
                    ));
                }
                captured.extend_from_slice(&tmp[..n]);
            }
        }
        wire::BodySpec::ReadToEnd => {
            let mut reader = reader;
            let mut captured = Vec::new();
            let mut tmp = vec![0u8; 8192];
            loop {
                let n = reader
                    .read(&mut tmp)
                    .await
                    .map_err(|e| AetherError::network(format!("read body: {e}")))?;
                if n == 0 {
                    return Ok((captured, None)); // EOF: fully captured
                }
                if captured.len() + n > cap {
                    let keep = cap - captured.len();
                    captured.extend_from_slice(&tmp[..keep]);
                    let over = tmp[keep..n].to_vec();
                    return Ok((
                        captured,
                        Some(Box::new(PassthroughPrefix {
                            prefix: over,
                            inner: reader,
                        })),
                    ));
                }
                captured.extend_from_slice(&tmp[..n]);
            }
        }
    }
}

/// Streams exactly `remaining` bytes then stops (ContentLength remainder).
struct ExactRemainder<R: AsyncRead + Unpin> {
    reader: BufReader<R>,
    remaining: u64,
}

impl<R: AsyncRead + Unpin> AsyncRead for ExactRemainder<R> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.remaining == 0 {
            return std::task::Poll::Ready(Ok(()));
        }
        let limit = buf
            .remaining()
            .min(this.remaining.min(usize::MAX as u64) as usize);
        let dst = buf.initialize_unfilled_to(limit);
        let mut sub = ReadBuf::new(dst);
        match std::pin::Pin::new(&mut this.reader).poll_read(cx, &mut sub) {
            std::task::Poll::Ready(Ok(())) => {
                let n = sub.filled().len();
                if n == 0 {
                    return std::task::Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "upstream closed mid-body",
                    )));
                }
                this.remaining -= n as u64;
                buf.advance(n);
                std::task::Poll::Ready(Ok(()))
            }
            std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e)),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// First replays bytes that were read past the cap, then passes the inner
/// reader through untouched.
struct PassthroughPrefix<R> {
    prefix: Vec<u8>,
    inner: R,
}

impl<R: AsyncRead + Unpin> AsyncRead for PassthroughPrefix<R> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if !this.prefix.is_empty() {
            let take = buf.remaining().min(this.prefix.len());
            buf.put_slice(&this.prefix[..take]);
            this.prefix.drain(..take);
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

// ---------------------------------------------------------------------------
// chunked decoding as an AsyncRead (byte-wise state machine — safe under
// async cancellation because all progress lives in the struct)
// ---------------------------------------------------------------------------

pub struct ChunkedReader<R: AsyncRead + Unpin> {
    inner: BufReader<R>,
    state: ChunkState,
    line: Vec<u8>,
    remaining_chunk: u64,
}

enum ChunkState {
    SizeLine,
    Data,
    DataCrlf(u8),
    Trailers,
    Done,
}

impl<R: AsyncRead + Unpin> ChunkedReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner: BufReader::new(inner),
            state: ChunkState::SizeLine,
            line: Vec::new(),
            remaining_chunk: 0,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for ChunkedReader<R> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        loop {
            match this.state {
                ChunkState::SizeLine => {
                    let mut byte = [0u8; 1];
                    let mut sub = ReadBuf::new(&mut byte);
                    match std::pin::Pin::new(&mut this.inner).poll_read(cx, &mut sub) {
                        std::task::Poll::Ready(Ok(())) => {
                            if sub.filled().is_empty() {
                                return std::task::Poll::Ready(Err(std::io::Error::new(
                                    std::io::ErrorKind::UnexpectedEof,
                                    "eof in chunk size",
                                )));
                            }
                            let byte = sub.filled()[0];
                            if byte == b'\n' {
                                if this.line.last() == Some(&b'\r') {
                                    this.line.pop();
                                }
                                let text = std::str::from_utf8(&this.line).map_err(|_| {
                                    std::io::Error::new(
                                        std::io::ErrorKind::InvalidData,
                                        "chunk size not utf8",
                                    )
                                })?;
                                let hex = text.split(';').next().unwrap_or("").trim();
                                let size = u64::from_str_radix(hex, 16).map_err(|_| {
                                    std::io::Error::new(
                                        std::io::ErrorKind::InvalidData,
                                        format!("bad chunk size '{hex}'"),
                                    )
                                })?;
                                this.line.clear();
                                this.remaining_chunk = size;
                                this.state = if size == 0 {
                                    ChunkState::Trailers
                                } else {
                                    ChunkState::Data
                                };
                            } else {
                                this.line.push(byte);
                                if this.line.len() > 1024 {
                                    return std::task::Poll::Ready(Err(std::io::Error::new(
                                        std::io::ErrorKind::InvalidData,
                                        "chunk size line too long",
                                    )));
                                }
                            }
                        }
                        std::task::Poll::Ready(Err(e)) => return std::task::Poll::Ready(Err(e)),
                        std::task::Poll::Pending => return std::task::Poll::Pending,
                    }
                }
                ChunkState::Data => {
                    let take = buf
                        .remaining()
                        .min(this.remaining_chunk.min(usize::MAX as u64) as usize);
                    if take == 0 {
                        this.state = ChunkState::DataCrlf(0);
                        continue;
                    }
                    let dst = buf.initialize_unfilled_to(take);
                    let mut sub = ReadBuf::new(dst);
                    match std::pin::Pin::new(&mut this.inner).poll_read(cx, &mut sub) {
                        std::task::Poll::Ready(Ok(())) => {
                            let n = sub.filled().len();
                            if n == 0 {
                                return std::task::Poll::Ready(Err(std::io::Error::new(
                                    std::io::ErrorKind::UnexpectedEof,
                                    "eof inside chunk",
                                )));
                            }
                            this.remaining_chunk -= n as u64;
                            buf.advance(n);
                            if this.remaining_chunk == 0 {
                                this.state = ChunkState::DataCrlf(0);
                            }
                            return std::task::Poll::Ready(Ok(()));
                        }
                        std::task::Poll::Ready(Err(e)) => return std::task::Poll::Ready(Err(e)),
                        std::task::Poll::Pending => return std::task::Poll::Pending,
                    }
                }
                ChunkState::DataCrlf(seen) => {
                    let mut byte = [0u8; 1];
                    let mut sub = ReadBuf::new(&mut byte);
                    match std::pin::Pin::new(&mut this.inner).poll_read(cx, &mut sub) {
                        std::task::Poll::Ready(Ok(())) => {
                            if sub.filled().is_empty() {
                                return std::task::Poll::Ready(Err(std::io::Error::new(
                                    std::io::ErrorKind::UnexpectedEof,
                                    "eof in chunk terminator",
                                )));
                            }
                            let byte = sub.filled()[0];
                            // after chunk data: expect \r then \n
                            if seen == 0 && byte == b'\r' {
                                this.state = ChunkState::DataCrlf(1);
                            } else if seen == 0 && byte == b'\n' {
                                // lenient: bare \n terminator
                                this.state = ChunkState::SizeLine;
                            } else if seen == 1 && byte == b'\n' {
                                this.state = ChunkState::SizeLine;
                            } else {
                                return std::task::Poll::Ready(Err(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    "bad chunk terminator",
                                )));
                            }
                        }
                        std::task::Poll::Ready(Err(e)) => return std::task::Poll::Ready(Err(e)),
                        std::task::Poll::Pending => return std::task::Poll::Pending,
                    }
                }
                ChunkState::Trailers => {
                    let mut byte = [0u8; 1];
                    let mut sub = ReadBuf::new(&mut byte);
                    match std::pin::Pin::new(&mut this.inner).poll_read(cx, &mut sub) {
                        std::task::Poll::Ready(Ok(())) if sub.filled().is_empty() => {
                            this.state = ChunkState::Done
                        }
                        std::task::Poll::Ready(Ok(())) => {
                            let byte = sub.filled()[0];
                            if byte == b'\n' {
                                if this.line.last() == Some(&b'\r') {
                                    this.line.pop();
                                }
                                if this.line.is_empty() {
                                    this.state = ChunkState::Done;
                                }
                                this.line.clear();
                            } else {
                                this.line.push(byte);
                                if this.line.len() > 8192 {
                                    return std::task::Poll::Ready(Err(std::io::Error::new(
                                        std::io::ErrorKind::InvalidData,
                                        "trailer line too long",
                                    )));
                                }
                            }
                        }
                        std::task::Poll::Ready(Err(e)) => return std::task::Poll::Ready(Err(e)),
                        std::task::Poll::Pending => return std::task::Poll::Pending,
                    }
                }
                ChunkState::Done => return std::task::Poll::Ready(Ok(())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_core::model::{Method, Url};

    fn chunked_body() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n");
        v
    }

    #[tokio::test]
    async fn chunked_reader_decodes_to_end() {
        let raw = chunked_body();
        let mut decoder = ChunkedReader::new(raw.as_slice());
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, b"Wikipedia");
    }

    #[tokio::test]
    async fn chunked_reader_with_trailers() {
        let mut raw = chunked_body();
        raw.extend_from_slice(b"X-Trailer: v\r\n\r\n");
        let mut decoder = ChunkedReader::new(raw.as_slice());
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, b"Wikipedia");
    }

    #[tokio::test]
    async fn dial_target_override_is_honored() {
        let mut cfg = UpstreamCfg::default();
        cfg.host_overrides
            .insert("mock.test".into(), "socket://127.0.0.1:1".into());
        let req = Request::new(Method::Get, Url::parse("http://mock.test/x").unwrap());
        let err = match send_request(&req, &cfg).await {
            Ok(_) => panic!("dial to closed port must fail"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("connect"), "{err}");
    }
}

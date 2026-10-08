//! Aether — network engine.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Hand-rolled HTTP/1.1 wire codec (ADR-003 wire truth + ADR-009).
//!
//! Why hand-rolled instead of hyper: a security proxy must see and control
//! the exact bytes — header order, duplicates, chunk framing, EOF-delimited
//! bodies. hyper normalizes aggressively, which is exactly wrong for a
//! research tool. This module is deliberately small and strict:
//! - `\r\n\r\n`-terminated heads (64 KiB cap), no obs-fold tolerance,
//! - `Content-Length` vs `Transfer-Encoding: chunked` (both set ⇒ protocol
//!   error — a classic smuggling signal we refuse to launder),
//! - EOF-delimited responses (HTTP/1.0 origins) handled correctly,
//! - bounded capture: bodies above the capture limit are *streamed through*
//!   while only the prefix is retained for history.

use aether_core::error::{AetherError, AetherResult};
use aether_core::model::{Body, Headers, HttpVersion, Method, Request, Response, StatusCode, Url};

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

/// Maximum request/response head size (request line + headers).
pub const MAX_HEAD_BYTES: usize = 64 * 1024;
/// Maximum size of a single header/chunk-size line.
const MAX_LINE_BYTES: usize = 16 * 1024;

// ---------------------------------------------------------------------------
// hop-by-hop headers (RFC 9110 §7.6.1) — never forwarded, either direction
// ---------------------------------------------------------------------------

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

pub fn strip_hop_by_hop(headers: &mut Headers) {
    // headers named by `Connection:` are hop-by-hop too
    let named: Vec<String> = headers
        .get_all("connection")
        .iter()
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    for name in named {
        headers.remove(&name);
    }
    for name in HOP_BY_HOP {
        headers.remove(name);
    }
}

// ---------------------------------------------------------------------------
// head reading (terminator-scanning, bounded)
// ---------------------------------------------------------------------------

/// Reads bytes until `needle` is seen (needle included in the result).
/// `Ok(None)` = clean EOF before any byte arrived (connection closed).
///
/// Consumption discipline: exactly the bytes up to and including the needle
/// are consumed from the reader — anything after it stays buffered for the
/// next read (bodies follow heads; chunk data follows size lines).
pub async fn read_until_needle<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    needle: &[u8],
    cap: usize,
) -> AetherResult<Option<Vec<u8>>> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    loop {
        let chunk_len;
        {
            let chunk = reader
                .fill_buf()
                .await
                .map_err(|e| AetherError::network(format!("read: {e}")))?;
            if chunk.is_empty() {
                if buf.is_empty() {
                    return Ok(None); // clean close
                }
                return Err(AetherError::protocol("connection closed mid-head"));
            }
            chunk_len = chunk.len();
            buf.extend_from_slice(chunk);
        }

        if let Some(pos) = find(&buf, needle) {
            let keep = pos + needle.len();
            // consume only what we keep; the rest stays in the reader
            let consumed_from_chunk = keep.saturating_sub(buf.len() - chunk_len);
            reader.consume(consumed_from_chunk);
            buf.truncate(keep);
            return Ok(Some(buf));
        }
        reader.consume(chunk_len);

        if buf.len() > cap {
            return Err(AetherError::protocol(format!(
                "head exceeds {cap} byte limit"
            )));
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Reads one `\n`-terminated line (cr-stripped), bounded.
async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> AetherResult<Vec<u8>> {
    let raw = read_until_needle(reader, b"\n", MAX_LINE_BYTES)
        .await?
        .ok_or_else(|| AetherError::protocol("connection closed mid-line"))?;
    let mut line = raw;
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(line)
}

// ---------------------------------------------------------------------------
// request parsing
// ---------------------------------------------------------------------------

fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| {
            matches!(b,
                b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' |
                b'^' | b'_' | b'`' | b'|' | b'~' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z')
        })
}

fn parse_version(s: &str) -> AetherResult<HttpVersion> {
    match s {
        "HTTP/1.1" => Ok(HttpVersion::Http11),
        "HTTP/1.0" => Ok(HttpVersion::Http10),
        other => Err(AetherError::protocol(format!(
            "unsupported HTTP version '{other}' (V0.1 speaks h1; h2 lands in V0.2, ADR-003)"
        ))),
    }
}

/// Parses a raw request head into the domain model.
///
/// - `scheme_fallback`/`authority_fallback` are used on the MITM leg where
///   clients send origin-form requests (`GET /path HTTP/1.1`); on the plain
///   forward-proxy leg clients must use absolute-form and the fallbacks are
///   `None`.
/// - CONNECT arrives here in authority-form (`host:port`) and is modeled as
///   `Request{ method: CONNECT, url: scheme://host:port/ }`.
pub fn parse_request_head(
    head: &[u8],
    scheme_fallback: Option<&str>,
    authority_fallback: Option<&str>,
) -> AetherResult<Request> {
    let text = std::str::from_utf8(head)
        .map_err(|_| AetherError::protocol("request head is not valid UTF-8"))?;
    let mut lines = text.split("\r\n").filter(|l| !l.is_empty());

    let request_line = lines
        .next()
        .ok_or_else(|| AetherError::protocol("empty request head"))?;
    let mut parts = request_line.split(' ');
    let method_raw = parts
        .next()
        .ok_or_else(|| AetherError::protocol("missing method"))?;
    let target = parts
        .next()
        .ok_or_else(|| AetherError::protocol("missing request target"))?;
    let version = parse_version(
        parts
            .next()
            .ok_or_else(|| AetherError::protocol("missing HTTP version"))?,
    )?;
    if parts.next().is_some() {
        return Err(AetherError::protocol("malformed request line"));
    }
    let method = Method::parse(method_raw);
    if method_raw.is_empty() || !method_raw.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(AetherError::protocol("invalid method token"));
    }

    let mut url = match method {
        Method::Connect => {
            // authority-form
            let (host, port) = split_authority(target)?;
            Url {
                scheme: "https".into(),
                host,
                port: Some(port.unwrap_or(443)),
                path: "/".into(),
                query: None,
                fragment: None,
            }
        }
        _ if target.starts_with('/') => {
            let Some(authority) = authority_fallback else {
                return Err(AetherError::protocol(
                    "forward-proxy requests must use absolute-form (GET http://host/path HTTP/1.1)",
                ));
            };
            let (host, port) = split_authority(authority)?;
            let (path, query) = match target.split_once('?') {
                Some((p, q)) => (p, Some(q.to_string())),
                None => (target, None),
            };
            Url {
                scheme: scheme_fallback.unwrap_or("http").to_string(),
                host,
                port,
                path: if path.is_empty() {
                    "/".into()
                } else {
                    path.to_string()
                },
                query,
                fragment: None,
            }
        }
        _ => Url::parse(target)?,
    };
    if url.path.is_empty() {
        url.path = "/".into();
    }

    let mut headers = Headers::new();
    for line in lines {
        if line.starts_with(' ') || line.starts_with('\t') {
            return Err(AetherError::protocol(
                "obs-fold (continued) header lines are rejected (request smuggling vector)",
            ));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| AetherError::protocol(format!("malformed header line: {line}")))?;
        let name = name.trim_end();
        if !is_token(name) {
            return Err(AetherError::protocol(format!(
                "invalid header name: {name}"
            )));
        }
        headers.append(name, value.trim());
    }

    Ok(Request {
        method,
        version,
        url,
        headers,
        body: Body::Empty,
    })
}

/// `host[:port]` → `(host, Option<port>)`; IPv6 brackets preserved in host.
pub fn split_authority(authority: &str) -> AetherResult<(String, Option<u16>)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let Some(end) = rest.find(']') else {
            return Err(AetherError::protocol("unterminated IPv6 authority"));
        };
        let host = format!("[{}]", &rest[..end]);
        let port = rest[end + 1..]
            .strip_prefix(':')
            .map(|p| {
                p.parse::<u16>()
                    .map_err(|_| AetherError::protocol("bad port"))
            })
            .transpose()?;
        return Ok((host, port));
    }
    match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() => {
            let port = p
                .parse::<u16>()
                .map_err(|_| AetherError::protocol(format!("bad port '{p}'")))?;
            Ok((h.to_string(), Some(port)))
        }
        _ => Ok((authority.to_string(), None)),
    }
}

// ---------------------------------------------------------------------------
// body framing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodySpec {
    Empty,
    ContentLength(u64),
    Chunked,
    ReadToEnd,
}

pub fn request_body_spec(headers: &Headers) -> AetherResult<BodySpec> {
    let cl = headers.get("content-length");
    let te = headers.get("transfer-encoding").map(str::to_lowercase);
    match (cl, te) {
        (Some(_), Some(_)) => Err(AetherError::protocol(
            "both Content-Length and Transfer-Encoding present (smuggling vector) — refusing",
        )),
        (Some(v), None) => {
            let n = v
                .trim()
                .parse::<u64>()
                .map_err(|_| AetherError::protocol("bad Content-Length"))?;
            Ok(if n == 0 {
                BodySpec::Empty
            } else {
                BodySpec::ContentLength(n)
            })
        }
        (None, Some(te)) if te.trim().eq_ignore_ascii_case("chunked") => Ok(BodySpec::Chunked),
        (None, Some(te)) => Err(AetherError::protocol(format!(
            "unsupported Transfer-Encoding: {te}"
        ))),
        (None, None) => Ok(BodySpec::Empty),
    }
}

pub fn response_body_spec(headers: &Headers, status: u16, request_was_head: bool) -> BodySpec {
    if request_was_head || status < 200 || status == 204 || status == 304 {
        return BodySpec::Empty;
    }
    if let Some(te) = headers.get("transfer-encoding") {
        if te.to_ascii_lowercase().contains("chunked") {
            return BodySpec::Chunked;
        }
    }
    if let Some(cl) = headers.get("content-length") {
        if let Ok(n) = cl.trim().parse::<u64>() {
            return if n == 0 {
                BodySpec::Empty
            } else {
                BodySpec::ContentLength(n)
            };
        }
    }
    BodySpec::ReadToEnd
}

/// Reads a body to completion (requests, and fully-captured responses).
pub async fn read_body<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    spec: &BodySpec,
    cap: usize,
) -> AetherResult<Vec<u8>> {
    match spec {
        BodySpec::Empty => Ok(Vec::new()),
        BodySpec::ContentLength(n) => {
            if *n > cap as u64 {
                return Err(AetherError::protocol(format!(
                    "body of {n} bytes exceeds capture limit of {cap}"
                )));
            }
            let mut buf = vec![0u8; *n as usize];
            reader
                .read_exact(&mut buf)
                .await
                .map_err(|e| AetherError::network(format!("read body: {e}")))?;
            Ok(buf)
        }
        BodySpec::Chunked => {
            let mut out = Vec::new();
            loop {
                let size_line = read_line(reader).await?;
                let size = parse_chunk_size(&size_line)?;
                if size == 0 {
                    drain_trailers(reader).await?;
                    return Ok(out);
                }
                if out.len() as u64 + size > cap as u64 {
                    return Err(AetherError::protocol(format!(
                        "chunked body exceeds capture limit of {cap}"
                    )));
                }
                let start = out.len();
                out.resize(start + size as usize, 0);
                reader
                    .read_exact(&mut out[start..])
                    .await
                    .map_err(|e| AetherError::network(format!("read chunk: {e}")))?;
                let mut crlf = [0u8; 2];
                reader
                    .read_exact(&mut crlf)
                    .await
                    .map_err(|e| AetherError::network(format!("read chunk terminator: {e}")))?;
            }
        }
        BodySpec::ReadToEnd => {
            let mut out = Vec::new();
            reader
                .take(cap as u64)
                .read_to_end(&mut out)
                .await
                .map_err(|e| AetherError::network(format!("read to end: {e}")))?;
            Ok(out)
        }
    }
}

fn parse_chunk_size(line: &[u8]) -> AetherResult<u64> {
    let text = std::str::from_utf8(line)
        .map_err(|_| AetherError::protocol("chunk size line is not UTF-8"))?;
    let hex = text.split(';').next().unwrap_or("").trim();
    u64::from_str_radix(hex, 16)
        .map_err(|_| AetherError::protocol(format!("bad chunk size '{hex}'")))
}

async fn drain_trailers<R: AsyncBufRead + Unpin>(reader: &mut R) -> AetherResult<()> {
    for _ in 0..256 {
        let line = read_line(reader).await?;
        if line.is_empty() {
            return Ok(());
        }
    }
    Err(AetherError::protocol("trailer section too large"))
}

/// A captured body becomes `Text` exactly when the bytes are valid UTF-8 —
/// otherwise it stays `Binary` (base64 on the control plane). This keeps
/// stored flows human-inspectable, lets text-only rules (body rewrite) fire,
/// and is wire-equivalent for forwarding (`Body::as_bytes`).
pub fn body_from_captured(bytes: Vec<u8>) -> Body {
    match String::from_utf8(bytes) {
        Ok(text) => Body::Text { text },
        Err(e) => Body::Binary {
            bytes: e.into_bytes(),
        },
    }
}

// ---------------------------------------------------------------------------
// response parsing
// ---------------------------------------------------------------------------

/// Parses a raw response head; `request_was_head` applies RFC 9110 HEAD rules.
pub fn parse_response_head(head: &[u8], request_was_head: bool) -> AetherResult<Response> {
    let text = std::str::from_utf8(head)
        .map_err(|_| AetherError::protocol("response head is not valid UTF-8"))?;
    let mut lines = text.split("\r\n").filter(|l| !l.is_empty());

    let status_line = lines
        .next()
        .ok_or_else(|| AetherError::protocol("empty response head"))?;
    let mut parts = status_line.splitn(3, ' ');
    let version = parse_version(
        parts
            .next()
            .ok_or_else(|| AetherError::protocol("missing status line"))?,
    )?;
    let code_str = parts
        .next()
        .ok_or_else(|| AetherError::protocol("missing status code"))?
        .to_string();
    let code = code_str
        .parse::<u16>()
        .map_err(|_| AetherError::protocol(format!("bad status code '{code_str}'")))?;
    let _reason = parts.next(); // reason phrase is advisory; not preserved

    let mut headers = Headers::new();
    for line in lines {
        if line.starts_with(' ') || line.starts_with('\t') {
            return Err(AetherError::protocol(
                "obs-fold in response headers rejected",
            ));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| AetherError::protocol(format!("malformed header line: {line}")))?;
        headers.append(name.trim_end(), value.trim());
    }

    let _ = request_was_head; // used by the caller via response_body_spec
    Ok(Response {
        status: StatusCode::new(code)?,
        version,
        headers,
        body: Body::Empty,
    })
}

// ---------------------------------------------------------------------------
// serialization
// ---------------------------------------------------------------------------

fn reason(code: u16) -> &'static str {
    match code {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        407 => "Proxy Authentication Required",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        418 => "I'm a teapot",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}

/// Serializes the request for the upstream leg: origin-form target,
/// guaranteed `Host`, a **truthful** `Content-Length` (hooks and intercept
/// edits may have changed the body — the header is recomputed, never
/// trusted), and `Connection: close` (V0.1 opens a fresh upstream per
/// request — ADR-009).
pub fn serialize_request(req: &Request) -> Vec<u8> {
    let mut headers = req.headers.clone();
    headers.remove("content-length");
    let mut out = Vec::with_capacity(256 + req.body.len());
    out.extend_from_slice(req.method.as_str().as_bytes());
    out.push(b' ');
    out.extend_from_slice(req.url.path_and_query().as_bytes());
    out.extend_from_slice(b" HTTP/1.1\r\n");

    for h in headers.iter() {
        out.extend_from_slice(h.name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(h.value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if !headers.contains("host") {
        out.extend_from_slice(b"Host: ");
        out.extend_from_slice(req.url.authority().as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if !req.body.is_empty() {
        out.extend_from_slice(format!("Content-Length: {}\r\n", req.body.len()).as_bytes());
    } else if req.headers.contains("content-length") {
        // an explicitly zero-length body keeps its declared framing
        out.extend_from_slice(b"Content-Length: 0\r\n");
    }
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    out.extend_from_slice(req.body.as_bytes());
    out
}

/// Serializes a response for the client leg. `keep_alive` mirrors the client's
/// own preference; bodies are always sent with an exact `Content-Length`
/// (we buffer or de-chunk upstream bodies before serving them).
pub fn serialize_response(res: &Response, keep_alive: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(192 + res.body.len());
    out.extend_from_slice(
        format!(
            "HTTP/1.1 {} {}\r\n",
            res.status.code,
            reason(res.status.code)
        )
        .as_bytes(),
    );
    for h in res.headers.iter() {
        out.extend_from_slice(h.name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(h.value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if !res.headers.contains("content-length") {
        out.extend_from_slice(format!("Content-Length: {}\r\n", res.body.len()).as_bytes());
    }
    out.extend_from_slice(if keep_alive {
        b"Connection: keep-alive\r\n\r\n" as &[u8]
    } else {
        b"Connection: close\r\n\r\n" as &[u8]
    });
    out.extend_from_slice(res.body.as_bytes());
    out
}

/// A bare response with a JSON error body (used for 400/413/502 from the
/// proxy itself).
pub fn error_response(status: u16, message: &str) -> AetherResult<Response> {
    let mut res = Response::new(status)?;
    res.set_header("content-type", "application/json; charset=utf-8");
    res.body = Body::text(
        serde_json::json!({ "error": { "status": status, "message": message } }).to_string(),
    );
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_from(head: &str, body: &[u8]) -> Request {
        let mut r = parse_request_head(head.as_bytes(), None, None).unwrap();
        r.body = Body::binary(body.to_vec());
        r
    }

    #[test]
    fn body_from_captured_splits_on_utf8_validity() {
        assert_eq!(
            super::body_from_captured(b"{\"ok\":true}".to_vec()),
            Body::text("{\"ok\":true}")
        );
        let binary = vec![0x00, 0xff, 0xfe, 0x01];
        assert!(matches!(
            super::body_from_captured(binary.clone()),
            Body::Binary { .. }
        ));
        assert_eq!(super::body_from_captured(Vec::new()), Body::text(""));
    }

    #[test]
    fn parses_absolute_form_with_wire_truth() {
        let r = req_from(
            "POST http://Example.COM:8080/v1/x?b=2&a=1 HTTP/1.1\r\n\
             X-Custom: keep me\r\n\
             set-cookie: a=1\r\n\
             set-cookie: b=2\r\n\
             \r\n",
            b"hello",
        );
        assert_eq!(r.method, Method::Post);
        assert_eq!(r.version, HttpVersion::Http11);
        assert_eq!(r.url.host, "Example.COM");
        assert_eq!(r.url.port, Some(8080));
        assert_eq!(r.url.path, "/v1/x");
        assert_eq!(r.url.query.as_deref(), Some("b=2&a=1"));
        let mut set_cookies = r.headers.get_all("set-cookie");
        set_cookies.sort_unstable();
        assert_eq!(set_cookies, vec!["a=1", "b=2"]);
        assert_eq!(r.headers.get("x-CUSTOM"), Some("keep me"));
        assert_eq!(
            r.headers.iter().next().unwrap().name,
            "X-Custom",
            "original casing"
        );
    }

    #[test]
    fn origin_form_requires_fallback() {
        assert!(parse_request_head(b"GET /x HTTP/1.1\r\n\r\n", None, None).is_err());
        let r = parse_request_head(
            b"GET /x?q=1 HTTP/1.1\r\n\r\n",
            Some("https"),
            Some("a.com:8443"),
        )
        .unwrap();
        assert_eq!(r.url.scheme, "https");
        assert_eq!(r.url.host, "a.com");
        assert_eq!(r.url.port, Some(8443));
        assert_eq!(r.url.path_and_query(), "/x?q=1");
    }

    #[test]
    fn connect_is_modeled_as_authority() {
        let r =
            parse_request_head(b"CONNECT example.com:443 HTTP/1.1\r\n\r\n", None, None).unwrap();
        assert_eq!(r.method, Method::Connect);
        assert_eq!(r.url.host, "example.com");
        assert_eq!(r.url.effective_port(), 443);
    }

    #[test]
    fn rejects_obs_fold_and_bad_versions() {
        assert!(
            parse_request_head(
                b"GET http://a/ HTTP/1.1\r\nX: one\r\n two\r\n\r\n",
                None,
                None
            )
            .is_err()
        );
        assert!(parse_request_head(b"GET http://a/ HTTP/2.0\r\n\r\n", None, None).is_err());
        assert!(parse_request_head(b"GET http://a/ HTTP/1.1 x\r\n\r\n", None, None).is_err());
    }

    #[test]
    fn body_specs_and_smuggling_refusal() {
        let mut h = Headers::new();
        h.append("content-length", "5");
        assert_eq!(request_body_spec(&h).unwrap(), BodySpec::ContentLength(5));
        h.append("transfer-encoding", "chunked");
        assert!(request_body_spec(&h).is_err(), "CL+TE refused");

        let mut h2 = Headers::new();
        h2.append("transfer-encoding", "Chunked");
        assert_eq!(request_body_spec(&h2).unwrap(), BodySpec::Chunked);

        assert_eq!(
            response_body_spec(&Headers::new(), 204, false),
            BodySpec::Empty
        );
        assert_eq!(
            response_body_spec(&Headers::new(), 200, true),
            BodySpec::Empty
        );
        let mut h3 = Headers::new();
        assert_eq!(response_body_spec(&h3, 200, false), BodySpec::ReadToEnd);
        h3.append("content-length", "12");
        assert_eq!(
            response_body_spec(&h3, 200, false),
            BodySpec::ContentLength(12)
        );
    }

    #[tokio::test]
    async fn chunked_decode_roundtrip() {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"4\r\nWiki\r\n5;r=1\r\npedia\r\n0\r\nX-Trailer: t\r\n\r\n");
        raw.extend_from_slice(b"NEXT-REQUEST-HEAD-MUST-SURVIVE");
        let mut reader = tokio::io::BufReader::new(&raw[..]);
        let body = read_body(&mut reader, &BodySpec::Chunked, 1024)
            .await
            .unwrap();
        assert_eq!(body, b"Wikipedia");
        // framing discipline: nothing after the final CRLFCRLF was consumed
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).await.unwrap();
        assert_eq!(rest, b"NEXT-REQUEST-HEAD-MUST-SURVIVE");
    }

    #[test]
    fn hop_by_hop_stripping_includes_connection_named() {
        let mut h = Headers::new();
        h.append("connection", "X-Private, close");
        h.append("X-Private", "secret");
        h.append("keep-alive", "timeout=5");
        h.append("content-type", "text/plain");
        strip_hop_by_hop(&mut h);
        assert!(!h.contains("x-private"));
        assert!(!h.contains("connection"));
        assert!(!h.contains("keep-alive"));
        assert!(h.contains("content-type"));
    }

    #[test]
    fn request_serialization_is_exact() {
        let mut r = Request::post(Url::parse("http://a.com:8080/x?y=1").unwrap());
        r.set_header("X-A", "1");
        r.body = Body::text("payload");
        let bytes = serialize_request(&r);
        let s = String::from_utf8(bytes).unwrap();
        assert_eq!(
            s,
            "POST /x?y=1 HTTP/1.1\r\nX-A: 1\r\nHost: a.com:8080\r\nContent-Length: 7\r\nConnection: close\r\n\r\npayload"
        );
    }

    #[test]
    fn response_serialization_roundtrips() {
        let mut res = Response::new(200).unwrap();
        res.set_header("content-type", "application/json");
        res.body = Body::text("{}");
        let bytes = serialize_response(&res, true);
        let parsed = parse_response_head(&bytes[..bytes.len() - 2], false).unwrap();
        assert_eq!(parsed.status.code, 200);
        assert_eq!(parsed.headers.get("content-type"), Some("application/json"));
    }
}

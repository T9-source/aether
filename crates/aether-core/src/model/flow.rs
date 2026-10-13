//! Flow — the unit of history (primitive #1 continues into primitive #5).
//!
//! One flow = one logical request/response exchange. Client-leg and
//! server-leg transports are recorded separately (ADR-003: they may differ,
//! e.g. h2 client leg downgraded to h1 server leg).
//!
//! Wire facts (ADR-015): the logical model is canonical — never the raw
//! bytes. What we *observed* on each wire and what we actually *forwarded*
//! are recorded as explicit, serializable facts beside the logical model,
//! because the two may legitimately diverge (match & replace, replay,
//! capture-limit re-framing, h2↔h1 translation).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::{Request, Response, WsSession};

pub type FlowId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TransportProto {
    Plaintext,
    Tls,
    Quic,
}

/// How a message body was framed on one wire (ADR-015). `None` (the field
/// default) means "no body / not applicable" — distinct from an empty body
/// declared with `Content-Length: 0`, which is [`Framing::ContentLength`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Framing {
    /// Exact-length framing (`Content-Length`).
    ContentLength,
    /// Chunked transfer coding (`Transfer-Encoding: chunked`).
    Chunked,
    /// EOF-delimited: no declared length, body ends with the connection
    /// (RFC 9110 §6.3) — the only honest choice when re-framing would lie.
    CloseDelimited,
    /// `101 Switching Protocols` — the wire became a tunnel (WebSocket).
    Upgrade,
    /// HTTP/2: no h1 message framing exists; bodies are DATA frames under
    /// flow control.
    H2Streams,
}

/// Compact sidecar the SQLite store persists as one JSON column (ADR-015).
/// `FlowMeta` stays flat for the UI/TS bindings; storage owns the grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[ts(export)]
pub struct WireFacts {
    #[serde(default)]
    pub observed_req: Option<Framing>,
    #[serde(default)]
    pub forwarded_req: Option<Framing>,
    #[serde(default)]
    pub observed_res: Option<Framing>,
    #[serde(default)]
    pub forwarded_res: Option<Framing>,
}

impl WireFacts {
    pub fn from_meta(m: &FlowMeta) -> Self {
        Self {
            observed_req: m.observed_req_framing,
            forwarded_req: m.forwarded_req_framing,
            observed_res: m.observed_res_framing,
            forwarded_res: m.forwarded_res_framing,
        }
    }

    /// `true` when every fact is `None` — stored as SQL `NULL`.
    pub fn is_empty(&self) -> bool {
        self.observed_req.is_none()
            && self.forwarded_req.is_none()
            && self.observed_res.is_none()
            && self.forwarded_res.is_none()
    }
}

/// Best-effort TLS facts recorded for a proxied exchange (ADR-004).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[ts(export)]
pub struct TlsMeta {
    pub version: Option<String>,
    pub cipher: Option<String>,
    pub alpn: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FlowMeta {
    pub created_at_unix_ms: u64,
    pub duration_ms: Option<u64>,
    /// Transport of the client leg (browser/app ↔ Aether).
    pub client_proto: TransportProto,
    /// Transport of the server leg (Aether ↔ origin).
    pub server_proto: TransportProto,
    /// TLS facts for the server leg when it spoke TLS (MITM leg details).
    #[serde(default)]
    pub tls: Option<TlsMeta>,
    /// Ordered, de-duplicated tags (set by hooks — ADR-006 rule #1).
    pub tags: Vec<String>,
    // ── Wire facts (ADR-015) — observed vs forwarded, recorded per leg ──
    /// Framing of the request body **as received from the client**.
    #[serde(default)]
    pub observed_req_framing: Option<Framing>,
    /// Framing of the request body **as we sent it upstream**. May differ
    /// from the observed framing (de-chunking, capture re-framing, h2).
    #[serde(default)]
    pub forwarded_req_framing: Option<Framing>,
    /// Framing of the response body **as received from the origin**.
    #[serde(default)]
    pub observed_res_framing: Option<Framing>,
    /// Framing of the response body **as we served it to the client**.
    #[serde(default)]
    pub forwarded_res_framing: Option<Framing>,
}

impl FlowMeta {
    pub fn now() -> Self {
        Self {
            created_at_unix_ms: unix_millis_now(),
            duration_ms: None,
            client_proto: TransportProto::Plaintext,
            server_proto: TransportProto::Plaintext,
            tls: None,
            tags: Vec::new(),
            observed_req_framing: None,
            forwarded_req_framing: None,
            observed_res_framing: None,
            forwarded_res_framing: None,
        }
    }
}

pub fn unix_millis_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Flow {
    pub id: FlowId,
    pub request: Option<Request>,
    pub response: Option<Response>,
    pub meta: FlowMeta,
    /// WebSocket session record when this flow upgraded to a WS connection
    /// (V0.2, ADR-013). `Some` only after the relay observed messages.
    #[serde(default)]
    pub ws: Option<WsSession>,
}

impl Flow {
    pub fn new(id: FlowId, request: Request) -> Self {
        Self {
            id,
            request: Some(request),
            response: None,
            meta: FlowMeta::now(),
            ws: None,
        }
    }

    /// Adds a tag preserving order, ignoring duplicates (case-insensitive).
    pub fn add_tag(&mut self, tag: impl Into<String>) {
        let tag = tag.into();
        if !self.meta.tags.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
            self.meta.tags.push(tag);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Method, Url};

    #[test]
    fn wire_facts_default_to_none_and_survive_json_roundtrip() {
        let mut f = Flow::new(
            1,
            Request::new(Method::Get, Url::parse("https://a/").unwrap()),
        );
        assert_eq!(f.meta.observed_req_framing, None);
        f.meta.observed_req_framing = Some(Framing::Chunked);
        f.meta.forwarded_req_framing = Some(Framing::ContentLength);
        f.meta.observed_res_framing = Some(Framing::H2Streams);
        f.meta.forwarded_res_framing = Some(Framing::CloseDelimited);
        let json = serde_json::to_string(&f.meta).unwrap();
        assert!(json.contains("\"observed_req_framing\":\"chunked\""));
        // Old records (pre-ADR-015) deserialize with all facts as `None`.
        let bare: FlowMeta = serde_json::from_str(
            "{\"created_at_unix_ms\":1,\"client_proto\":\"plaintext\",\"server_proto\":\"plaintext\",\"tags\":[]}",
        )
        .unwrap();
        assert_eq!(bare.observed_req_framing, None);
    }

    #[test]
    fn tags_dedup_case_insensitively_and_keep_order() {
        let mut f = Flow::new(
            1,
            Request::new(Method::Get, Url::parse("https://a/").unwrap()),
        );
        f.add_tag("API");
        f.add_tag("api");
        f.add_tag("Auth");
        assert_eq!(f.meta.tags, vec!["API", "Auth"]);
    }
}

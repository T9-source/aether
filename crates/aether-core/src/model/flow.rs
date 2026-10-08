//! Flow — the unit of history (primitive #1 continues into primitive #5).
//!
//! One flow = one logical request/response exchange. Client-leg and
//! server-leg transports are recorded separately (ADR-003: they may differ,
//! e.g. h2 client leg downgraded to h1 server leg).

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

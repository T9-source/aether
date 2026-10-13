//! Traffic Model (primitive #1) — protocol-agnostic and serialization-shaped.
//!
//! Per ADR-003 the model speaks h1+h2 semantics from day 1:
//! `method / scheme / authority / path / query` + ordered, duplicate-capable
//! headers + body chunks. Headers are stored as an ordered list so wire truth
//! (repeated `Set-Cookie`, header order) survives.

pub mod body;
pub mod flow;
pub(crate) mod fnv;
pub mod headers;
pub mod request;
pub mod response;
pub mod url;
pub mod ws;

pub use body::Body;
pub use flow::{
    Flow, FlowId, FlowMeta, Framing, TlsMeta, TransportProto, WireFacts, unix_millis_now,
};
pub use headers::{Header, Headers};
pub use request::{HttpVersion, Method, Request};
pub use response::{Response, StatusCode};
pub use url::Url;
pub use ws::{WsDirection, WsKind, WsMessage, WsSession};

//! Request — h1 + h2 semantics in one struct (ADR-003).
//!
//! `method / version / url / ordered headers / body`. The URL carries
//! scheme+authority+path+query, which is the h2 pseudo-header quadruple
//! (`:method :scheme :authority :path`) and maps 1:1 onto an h1 request line.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::fnv::Fnv1a;
use crate::model::{Body, Headers, Url};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum HttpVersion {
    Http10,
    Http11,
    Http2,
    Http3,
}

impl HttpVersion {
    pub(crate) fn tag(self) -> u8 {
        match self {
            Self::Http10 => 10,
            Self::Http11 => 11,
            Self::Http2 => 20,
            Self::Http3 => 30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
    Options,
    Trace,
    Connect,
    /// Security tooling must accept arbitrary methods — nothing is rejected
    /// at the model layer.
    Custom(String),
}

impl Method {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
            Self::Trace => "TRACE",
            Self::Connect => "CONNECT",
            Self::Custom(m) => m,
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "GET" => Self::Get,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "DELETE" => Self::Delete,
            "PATCH" => Self::Patch,
            "HEAD" => Self::Head,
            "OPTIONS" => Self::Options,
            "TRACE" => Self::Trace,
            "CONNECT" => Self::Connect,
            other => Self::Custom(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Request {
    pub method: Method,
    pub version: HttpVersion,
    pub url: Url,
    pub headers: Headers,
    pub body: Body,
}

impl Request {
    pub fn new(method: Method, url: Url) -> Self {
        Self {
            method,
            version: HttpVersion::Http11,
            url,
            headers: Headers::new(),
            body: Body::Empty,
        }
    }

    pub fn get(url: Url) -> Self {
        Self::new(Method::Get, url)
    }

    pub fn post(url: Url) -> Self {
        Self::new(Method::Post, url)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)
    }

    pub fn set_header(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.headers.insert(name, value);
    }

    /// Cheap change detector used by the pipeline to decide whether a hook
    /// modified the request (ADR-006: hooks mutate in place; the pipeline
    /// emits `RequestModified` when the fingerprint shifts).
    pub fn fingerprint(&self) -> u64 {
        let mut h = Fnv1a::new();
        h.write(self.method.as_str().as_bytes());
        h.write(&[self.version.tag()]);
        h.write(self.url.as_url().as_bytes());
        for header in self.headers.iter() {
            h.write(header.name.as_bytes());
            h.write(b"\x00");
            h.write(header.value.as_bytes());
            h.write(b"\x01");
        }
        h.write(&self.body.len().to_le_bytes());
        hash_body_tail(&mut h, self.body.as_bytes());
        h.finish()
    }
}

/// Hash head + total length of a body (bounded work on huge bodies while
/// still catching edits at the edges — front edits are the common replay case).
pub(crate) fn hash_body_tail(h: &mut Fnv1a, bytes: &[u8]) {
    let head = &bytes[..bytes.len().min(4096)];
    h.write(head);
    h.write(&bytes.len().to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_parse_roundtrip() {
        assert_eq!(Method::parse("get"), Method::Get);
        assert_eq!(Method::parse("PROPFIND"), Method::Custom("PROPFIND".into()));
        assert_eq!(Method::Custom("X".into()).as_str(), "X");
    }

    #[test]
    fn fingerprint_changes_on_any_edit() {
        let mut r = Request::get(Url::parse("https://a.com/x").unwrap());
        let base = r.fingerprint();
        r.set_header("A", "1");
        assert_ne!(base, r.fingerprint());
        let with_header = r.fingerprint();
        r.set_header("A", "2"); // same header count, different value
        assert_ne!(with_header, r.fingerprint());
    }

    #[test]
    fn fingerprint_ignores_nothing() {
        // URL edit must be caught too
        let mut r = Request::get(Url::parse("https://a.com/x").unwrap());
        let base = r.fingerprint();
        r.url.path = "/y".into();
        assert_ne!(base, r.fingerprint());
    }
}

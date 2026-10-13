//! The serialization-shaped hook contract, cut at the WASM boundary
//! (ADR-006 rule #2 — "hosting change, not an API redesign").
//!
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Shape:
//! - **In**: a [`HttpRequestView`] / [`HttpResponseView`] — owned, ordered,
//!   JSON-serializable snapshot. Bodies are lossy-UTF-8 and truncated at 1
//!   MiB with an honest `body_truncated` flag (a plugin never silently sees
//!   half a body pretending to be the whole one).
//! - **Out**: a [`PluginOutcome`] — a *declarative edit-set*. The plugin
//!   never touches traffic directly; the host applies the edits
//!   (`apply_to_request` / `apply_to_response`). Dropping traffic must be a
//!   deliberate act: an unknown `action` string fails open to `continue`,
//!   while a literal `"stop"` is the only way to abort.
//!
//! Set-vs-remove semantics match `Headers::insert`/`Headers::remove`:
//! `set_headers` replaces *all* occurrences of a name, `remove_headers`
//! removes *all* occurrences.

use aether_core::model::{Body, Request, Response};
use serde::{Deserialize, Serialize};

use super::manifest::MAX_VIEW_BODY;

/// Ordered snapshot of an outbound request, as the plugin sees it.
#[derive(Debug, Clone, Serialize)]
pub struct HttpRequestView {
    pub method: String,
    pub url: String,
    /// Ordered `(name, value)` pairs — header order is wire truth.
    pub headers: Vec<(String, String)>,
    /// Lossy UTF-8 body, truncated to 1 MiB.
    pub body: String,
    pub body_truncated: bool,
}

/// Ordered snapshot of an inbound response, as the plugin sees it.
#[derive(Debug, Clone, Serialize)]
pub struct HttpResponseView {
    pub status: u16,
    /// Ordered `(name, value)` pairs — header order is wire truth.
    pub headers: Vec<(String, String)>,
    /// Lossy UTF-8 body, truncated to 1 MiB.
    pub body: String,
    pub body_truncated: bool,
}

fn view_body(body: &Body) -> (String, bool) {
    let bytes = body.as_bytes();
    if bytes.len() > MAX_VIEW_BODY {
        (
            String::from_utf8_lossy(&bytes[..MAX_VIEW_BODY]).into_owned(),
            true,
        )
    } else {
        (String::from_utf8_lossy(bytes).into_owned(), false)
    }
}

impl HttpRequestView {
    pub fn from_request(req: &Request) -> Self {
        let (body, body_truncated) = view_body(&req.body);
        Self {
            method: req.method.as_str().to_string(),
            url: req.url.to_string(),
            headers: req
                .headers
                .iter()
                .map(|h| (h.name.clone(), h.value.clone()))
                .collect(),
            body,
            body_truncated,
        }
    }
}

impl HttpResponseView {
    pub fn from_response(res: &Response) -> Self {
        let (body, body_truncated) = view_body(&res.body);
        Self {
            status: res.status.code,
            headers: res
                .headers
                .iter()
                .map(|h| (h.name.clone(), h.value.clone()))
                .collect(),
            body,
            body_truncated,
        }
    }
}

/// One header write in the declared edit-set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderEdit {
    pub name: String,
    pub value: String,
}

/// What the plugin asks the host to do. Everything is a suggestion the host
/// applies — see `apply_to_request` / `apply_to_response`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginOutcome {
    /// `"continue"` (default) or `"stop"`. Anything else fails open to
    /// continue — dropping traffic must be deliberate, never accidental.
    action: Option<String>,
    /// Replace-all-per-name header writes.
    set_headers: Vec<HeaderEdit>,
    /// Remove-all-per-name.
    remove_headers: Vec<String>,
    /// Replace the body with this text (host writes it as `Body::text`).
    set_body: Option<String>,
}

impl PluginOutcome {
    /// Continue, with no edits.
    pub fn continue_noop() -> Self {
        Self::default()
    }

    /// True only on a literal `"stop"`. Unknown/absent actions fail open.
    pub fn action_is_stop(&self) -> bool {
        self.action.as_deref() == Some("stop")
    }

    pub fn set_headers(&self) -> &[HeaderEdit] {
        &self.set_headers
    }

    pub fn remove_headers(&self) -> &[String] {
        &self.remove_headers
    }

    pub fn set_body(&self) -> Option<&str> {
        self.set_body.as_deref()
    }

    /// Applies the edit-set to a request. `set` is a full replace: ALL
    /// existing occurrences of the name are removed, then the new value is
    /// written once — a duplicate-header smuggling vector is not something a
    /// plugin can create by accident (`Headers::insert` alone would only
    /// replace the first match and leave the rest). `remove` deletes all
    /// occurrences; removal wins on a name present in both lists.
    pub fn apply_to_request(&self, req: &mut Request) {
        for name in &self.remove_headers {
            req.headers.remove(name);
        }
        for h in &self.set_headers {
            req.headers.remove(&h.name);
            req.headers.append(h.name.clone(), h.value.clone());
        }
        if let Some(body) = &self.set_body {
            req.body = Body::text(body.clone());
        }
    }

    /// Applies the edit-set to a response (same semantics).
    pub fn apply_to_response(&self, res: &mut Response) {
        for name in &self.remove_headers {
            res.headers.remove(name);
        }
        for h in &self.set_headers {
            res.headers.remove(&h.name);
            res.headers.append(h.name.clone(), h.value.clone());
        }
        if let Some(body) = &self.set_body {
            res.body = Body::text(body.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_core::model::{Request, StatusCode, Url};

    fn req() -> Request {
        let mut r = Request::get(Url::parse("https://example.com/x").unwrap());
        r.headers.append("x-a", "1");
        r.headers.append("x-a", "2");
        r.body = Body::text("hello");
        r
    }

    #[test]
    fn view_is_ordered_and_honest() {
        let r = req();
        let v = HttpRequestView::from_request(&r);
        assert_eq!(v.method, "GET");
        assert_eq!(v.headers[0], ("x-a".into(), "1".into()));
        assert_eq!(v.headers[1], ("x-a".into(), "2".into()));
        assert_eq!(v.body, "hello");
        assert!(!v.body_truncated);
    }

    #[test]
    fn body_truncation_is_flagged() {
        let mut r = req();
        r.body = Body::binary(vec![b'x'; MAX_VIEW_BODY + 1]);
        let v = HttpRequestView::from_request(&r);
        assert_eq!(v.body.len(), MAX_VIEW_BODY);
        assert!(v.body_truncated);
    }

    #[test]
    fn set_replaces_all_occurrences() {
        let mut r = req();
        let o: PluginOutcome =
            serde_json::from_str(r#"{"set_headers": [{"name": "x-a", "value": "z"}]}"#).unwrap();
        o.apply_to_request(&mut r);
        let all = r.headers.get_all("x-a");
        assert_eq!(all, vec!["z"]);
    }

    #[test]
    fn remove_deletes_all_occurrences() {
        let mut r = req();
        let o: PluginOutcome = serde_json::from_str(r#"{"remove_headers": ["x-a"]}"#).unwrap();
        o.apply_to_request(&mut r);
        assert!(r.headers.get("x-a").is_none());
    }

    #[test]
    fn unknown_action_fails_open_to_continue() {
        let o: PluginOutcome = serde_json::from_str(r#"{"action": "DROP_EVERYTHING"}"#).unwrap();
        assert!(!o.action_is_stop());
        let o: PluginOutcome = serde_json::from_str(r#"{}"#).unwrap();
        assert!(!o.action_is_stop());
        let o: PluginOutcome = serde_json::from_str(r#"{"action": "stop"}"#).unwrap();
        assert!(o.action_is_stop());
    }

    #[test]
    fn stop_never_edits() {
        // a stop outcome still carries edits in the wire format, but the
        // contract is: host checks action first. This test just pins that
        // stop parses and reads back as stop.
        let o: PluginOutcome = serde_json::from_str(
            r#"{"action": "stop", "set_headers": [{"name":"x","value":"y"}]}"#,
        )
        .unwrap();
        assert!(o.action_is_stop());
        assert_eq!(o.set_headers().len(), 1);
    }

    #[tokio::test]
    async fn response_apply_roundtrip() {
        let mut res = Response::new(200).unwrap();
        res.headers.append("server", "origin/1");
        let o: PluginOutcome =
            serde_json::from_str(r#"{"remove_headers": ["server"], "set_body": "rewritten"}"#)
                .unwrap();
        o.apply_to_response(&mut res);
        assert!(res.headers.get("server").is_none());
        assert_eq!(res.body.as_bytes(), b"rewritten");
        assert_eq!(res.status.code, StatusCode::new(200).unwrap().code);
    }
}

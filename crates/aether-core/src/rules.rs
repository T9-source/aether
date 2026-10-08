//! Match & replace rules — the *serialized* hook layer (ADR-006 #1/#2).
//!
//! A `RuleSpec` is pure data: JSON in (control plane / rules.json), a compiled
//! [`RuleHook`] out. This is the proof that the hook boundary is
//! serialization-shaped: a UI or a future WASM plugin can author traffic
//! logic without recompiling the daemon.
//!
//! Engine contract (read by `aether-http`):
//! - `AddTag` accumulates into `ctx.data[CTX_TAGS_KEY]` (JSON array of strings).
//! - `DropFlow` returns `HookAction::Stop` with no response payload.
//! - `RespondWith` stashes a JSON payload into `ctx.data[CTX_RESPOND_WITH_KEY]`
//!   and returns `Stop` — the engine short-circuits with that response.

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::context::{RequestContext, Scope};
use crate::error::{AetherError, AetherResult};
use crate::hooks::{Hook, HookAction, HookResult};
use crate::model::{Body, Request, Response, StatusCode};

/// Where a rule applies. `Both` runs it in the request *and* response stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RuleStage {
    #[default]
    Request,
    Response,
    Both,
}

/// What a rule does to matching traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum RuleAction {
    SetRequestHeader {
        name: String,
        value: String,
    },
    RemoveRequestHeader {
        name: String,
    },
    SetResponseHeader {
        name: String,
        value: String,
    },
    RemoveResponseHeader {
        name: String,
    },
    /// Regex rewrite over `path + "?" + query` (h2 `:path` semantics).
    ReplaceRequestPath {
        pattern: String,
        replacement: String,
    },
    /// Regex rewrite over the request body — **text bodies only**; binary
    /// bodies are skipped untouched (corrupting binaries is not a feature).
    ReplaceRequestBody {
        pattern: String,
        replacement: String,
    },
    ReplaceResponseBody {
        pattern: String,
        replacement: String,
    },
    AddTag {
        tag: String,
    },
    /// Abort the exchange (client leg gets a 502 from the proxy).
    DropFlow,
    /// Mock short-circuit — the request never reaches the origin.
    RespondWith {
        status: u16,
        content_type: Option<String>,
        body: String,
    },
}

/// Pure-data rule definition — the JSON surface for match & replace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RuleSpec {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub stage: RuleStage,
    /// Host globs that must match (`*` crosses dots; case-insensitive).
    #[serde(default)]
    pub host_include: Vec<String>,
    /// Host globs that must NOT match.
    #[serde(default)]
    pub host_exclude: Vec<String>,
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// Exact method match, case-insensitive.
    #[serde(default)]
    pub method: Option<String>,
    pub action: RuleAction,
}

fn default_true() -> bool {
    true
}

/// Context keys the engine consumes after the pipeline ran (see module docs).
pub const CTX_TAGS_KEY: &str = "aether.tags";
pub const CTX_RESPOND_WITH_KEY: &str = "aether.respond_with";

/// A compiled rule — [`Hook`] implementation generated from a [`RuleSpec`].
pub struct RuleHook {
    spec: RuleSpec,
    scope: Scope,
    re_path: Option<Regex>,
    re_req_body: Option<Regex>,
    re_resp_body: Option<Regex>,
}

impl RuleHook {
    /// Compiles + validates the spec up front so bad regexes fail at
    /// *registration*, not on the hot path.
    pub fn from_spec(spec: RuleSpec) -> AetherResult<Self> {
        let compile = |pattern: &str| -> AetherResult<Regex> {
            Regex::new(pattern)
                .map_err(|e| AetherError::invalid_input(format!("invalid regex '{pattern}': {e}")))
        };
        let (re_path, re_req_body, re_resp_body) = match &spec.action {
            RuleAction::ReplaceRequestPath { pattern, .. } => (Some(compile(pattern)?), None, None),
            RuleAction::ReplaceRequestBody { pattern, .. } => (None, Some(compile(pattern)?), None),
            RuleAction::ReplaceResponseBody { pattern, .. } => {
                (None, None, Some(compile(pattern)?))
            }
            RuleAction::RespondWith { status, .. } => {
                StatusCode::new(*status)?;
                (None, None, None)
            }
            _ => (None, None, None),
        };
        if spec.name.trim().is_empty() {
            return Err(AetherError::invalid_input("rule name must not be empty"));
        }
        Ok(Self {
            scope: Scope {
                // empty include list = "any host" (same as Scope::all())
                include: if spec.host_include.is_empty() {
                    vec!["*".to_string()]
                } else {
                    spec.host_include.clone()
                },
                exclude: spec.host_exclude.clone(),
            },
            spec,
            re_path,
            re_req_body,
            re_resp_body,
        })
    }

    pub fn spec(&self) -> &RuleSpec {
        &self.spec
    }

    fn applies_to(&self, method: &str, host: &str, path: &str) -> bool {
        if let Some(m) = &self.spec.method {
            if !m.eq_ignore_ascii_case(method) {
                return false;
            }
        }
        if let Some(p) = &self.spec.path_prefix {
            if !path.starts_with(p.as_str()) {
                return false;
            }
        }
        self.scope.matches_host(host)
    }

    /// Regex replacement with standard capture-group expansion (`$1`, `${name}`);
    /// a literal `$` is written `$$`.
    fn replace_text(re: &Regex, replacement: &str, text: &str) -> String {
        re.replace_all(text, replacement).into_owned()
    }
}

#[async_trait]
impl Hook for RuleHook {
    fn name(&self) -> &str {
        &self.spec.name
    }

    async fn on_request(&self, ctx: &mut RequestContext, req: &mut Request) -> HookResult {
        if !self.spec.enabled
            || !matches!(self.spec.stage, RuleStage::Request | RuleStage::Both)
            || !self.applies_to(req.method.as_str(), &req.url.host, &req.url.path)
        {
            return Ok(HookAction::Continue);
        }
        match &self.spec.action {
            RuleAction::SetRequestHeader { name, value } => {
                req.set_header(name.clone(), value.clone());
            }
            RuleAction::RemoveRequestHeader { name } => {
                req.headers.remove(name);
            }
            RuleAction::ReplaceRequestPath { replacement, .. } => {
                let Some(re) = &self.re_path else {
                    return Ok(HookAction::Continue);
                };
                let target = req.url.path_and_query();
                let new = Self::replace_text(re, replacement, &target);
                match new.split_once('?') {
                    Some((p, q)) => {
                        req.url.path = p.to_string();
                        req.url.query = Some(q.to_string());
                    }
                    None => {
                        req.url.path = new;
                        req.url.query = None;
                    }
                }
                if req.url.path.is_empty() {
                    req.url.path = "/".into();
                }
            }
            RuleAction::ReplaceRequestBody { replacement, .. } => {
                let Some(re) = &self.re_req_body else {
                    return Ok(HookAction::Continue);
                };
                if let Body::Text { text } = &mut req.body {
                    *text = Self::replace_text(re, replacement, text);
                }
            }
            RuleAction::AddTag { tag } => push_ctx_tag(ctx, tag),
            RuleAction::DropFlow => {
                return Ok(HookAction::Stop);
            }
            RuleAction::RespondWith {
                status,
                content_type,
                body,
            } => {
                ctx.set_data(
                    CTX_RESPOND_WITH_KEY,
                    serde_json::json!({
                        "status": status,
                        "content_type": content_type,
                        "body": body,
                    }),
                );
                return Ok(HookAction::Stop);
            }
            // response-side actions are no-ops in the request stage
            _ => {}
        }
        Ok(HookAction::Continue)
    }

    async fn on_response(&self, ctx: &mut RequestContext, res: &mut Response) -> HookResult {
        if !self.spec.enabled || !matches!(self.spec.stage, RuleStage::Response | RuleStage::Both) {
            return Ok(HookAction::Continue);
        }
        match &self.spec.action {
            RuleAction::SetResponseHeader { name, value } => {
                res.set_header(name.clone(), value.clone());
            }
            RuleAction::RemoveResponseHeader { name } => {
                res.headers.remove(name);
            }
            RuleAction::ReplaceResponseBody { replacement, .. } => {
                let Some(re) = &self.re_resp_body else {
                    return Ok(HookAction::Continue);
                };
                if let Body::Text { text } = &mut res.body {
                    *text = Self::replace_text(re, replacement, text);
                }
            }
            RuleAction::AddTag { tag } => {
                // `Both` runs this per stage — `push_ctx_tag` dedups.
                push_ctx_tag(ctx, tag);
            }
            _ => {}
        }
        Ok(HookAction::Continue)
    }
}

fn push_ctx_tag(ctx: &mut RequestContext, tag: &str) {
    let entry = ctx
        .data
        .entry(CTX_TAGS_KEY.to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    if let serde_json::Value::Array(items) = entry {
        if !items.iter().any(|v| v.as_str() == Some(tag)) {
            items.push(serde_json::Value::String(tag.to_string()));
        }
    }
}

/// Tags accumulated by `AddTag` rules during the pipeline run.
pub fn drain_ctx_tags(ctx: &mut RequestContext) -> Vec<String> {
    match ctx.data.remove(CTX_TAGS_KEY) {
        Some(serde_json::Value::Array(items)) => items
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// A `RespondWith` short-circuit left in the context by a rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RespondWith {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: String,
}

pub fn take_respond_with(ctx: &mut RequestContext) -> Option<RespondWith> {
    let value = ctx.data.remove(CTX_RESPOND_WITH_KEY)?;
    serde_json::from_value(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Method, Url};

    fn ctx() -> RequestContext {
        RequestContext::new(1, crate::context::TrafficSource::Proxy)
    }

    fn req(url: &str) -> Request {
        Request::new(Method::Get, Url::parse(url).unwrap())
    }

    #[tokio::test]
    async fn set_and_remove_headers() {
        let set = RuleHook::from_spec(serde_json::from_value(serde_json::json!({
            "name": "stamp", "action": {"kind": "set_request_header", "name": "x-stamp", "value": "yes"}
        })).unwrap())
        .unwrap();
        let mut c = ctx();
        let mut r = req("https://a.com/");
        set.on_request(&mut c, &mut r).await.unwrap();
        assert_eq!(r.header("x-stamp"), Some("yes"));

        let rm = RuleHook::from_spec(
            serde_json::from_value(serde_json::json!({
                "name": "strip", "action": {"kind": "remove_request_header", "name": "x-stamp"}
            }))
            .unwrap(),
        )
        .unwrap();
        rm.on_request(&mut c, &mut r).await.unwrap();
        assert_eq!(r.header("x-stamp"), None);
    }

    #[tokio::test]
    async fn path_regex_rewrite() {
        let hook = RuleHook::from_spec(serde_json::from_value(serde_json::json!({
            "name": "v1->v2",
            "action": {"kind": "replace_request_path", "pattern": "^/v1/", "replacement": "/v2/"}
        })).unwrap())
        .unwrap();
        let mut c = ctx();
        let mut r = req("https://a.com/v1/users?id=7");
        hook.on_request(&mut c, &mut r).await.unwrap();
        assert_eq!(r.url.path, "/v2/users");
        assert_eq!(r.url.query.as_deref(), Some("id=7"));
    }

    #[tokio::test]
    async fn body_replace_is_text_only() {
        let hook = RuleHook::from_spec(serde_json::from_value(serde_json::json!({
            "name": "r", "action": {"kind": "replace_request_body", "pattern": "secret", "replacement": "***"}
        })).unwrap())
        .unwrap();
        let mut c = ctx();

        let mut r = req("https://a.com/");
        r.body = Body::text("a secret here");
        hook.on_request(&mut c, &mut r).await.unwrap();
        assert_eq!(r.body.as_bytes(), b"a *** here");

        let mut rb = req("https://a.com/");
        rb.body = Body::binary(vec![0xDE, 0xAD]);
        hook.on_request(&mut c, &mut rb).await.unwrap();
        assert_eq!(rb.body.as_bytes(), &[0xDE, 0xAD], "binary untouched");
    }

    #[tokio::test]
    async fn drop_and_respond_with_stop_the_pipeline() {
        let drop = RuleHook::from_spec(
            serde_json::from_value(serde_json::json!({
                "name": "block", "action": {"kind": "drop_flow"}
            }))
            .unwrap(),
        )
        .unwrap();
        let mut c = ctx();
        let mut r = req("https://a.com/");
        assert_eq!(
            drop.on_request(&mut c, &mut r).await.unwrap(),
            HookAction::Stop
        );

        let mock = RuleHook::from_spec(serde_json::from_value(serde_json::json!({
            "name": "mock", "action": {"kind": "respond_with", "status": 418, "content_type": "text/plain", "body": "teapot"}
        })).unwrap())
        .unwrap();
        let mut c2 = ctx();
        let mut r2 = req("https://a.com/");
        assert_eq!(
            mock.on_request(&mut c2, &mut r2).await.unwrap(),
            HookAction::Stop
        );
        let rw = take_respond_with(&mut c2).unwrap();
        assert_eq!(rw.status, 418);
        assert_eq!(rw.body, "teapot");
    }

    #[tokio::test]
    async fn tags_accumulate_and_drain() {
        let tag = RuleHook::from_spec(
            serde_json::from_value(serde_json::json!({
                "name": "t", "stage": "both", "action": {"kind": "add_tag", "tag": "api"}
            }))
            .unwrap(),
        )
        .unwrap();
        let mut c = ctx();
        let mut r = req("https://a.com/");
        let mut res = Response::new(200).unwrap();
        tag.on_request(&mut c, &mut r).await.unwrap();
        tag.on_response(&mut c, &mut res).await.unwrap();
        tag.on_response(&mut c, &mut res).await.unwrap(); // dedup
        assert_eq!(drain_ctx_tags(&mut c), vec!["api"]);
    }

    #[test]
    fn scope_and_matcher() {
        let hook = RuleHook::from_spec(RuleSpec {
            name: "api-only".into(),
            enabled: true,
            stage: RuleStage::Request,
            host_include: vec!["*.example.com".into()],
            host_exclude: vec!["admin.example.com".into()],
            path_prefix: Some("/v1".into()),
            method: Some("get".into()),
            action: RuleAction::AddTag { tag: "x".into() },
        })
        .unwrap();
        assert!(hook.applies_to("GET", "api.example.com", "/v1/users"));
        assert!(!hook.applies_to("POST", "api.example.com", "/v1/users"));
        assert!(!hook.applies_to("GET", "admin.example.com", "/v1/users"));
        assert!(!hook.applies_to("GET", "api.example.com", "/v2/users"));
        assert!(!hook.applies_to("GET", "evil.com", "/v1/users"));
    }

    #[test]
    fn invalid_regex_fails_at_registration() {
        let spec: RuleSpec = serde_json::from_value(serde_json::json!({
            "name": "bad", "action": {"kind": "replace_request_path", "pattern": "((", "replacement": ""}
        }))
        .unwrap();
        assert!(RuleHook::from_spec(spec).is_err());
    }

    #[test]
    fn spec_json_roundtrip() {
        let spec: RuleSpec = serde_json::from_value(serde_json::json!({
            "name": "r", "enabled": false, "stage": "response",
            "action": {"kind": "set_response_header", "name": "x", "value": "y"}
        }))
        .unwrap();
        assert!(!spec.enabled);
        assert_eq!(spec.stage, RuleStage::Response);
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(serde_json::from_str::<RuleSpec>(&json).unwrap(), spec);
    }
}

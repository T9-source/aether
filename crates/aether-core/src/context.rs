//! `RequestContext` — the capability carrier (ADR-006 rule #4).
//!
//! Every hook receives exactly what the context grants: scope, capabilities
//! and a scratch `data` map. **No hook ever reaches global state.** Future
//! sandboxing = replacing the capability map with brokered handles — a
//! mechanical change, not a redesign.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::model::FlowId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[ts(export)]
pub struct Scope {
    /// Wildcard host patterns; `*` matches any sequence (incl. dots).
    /// A flow is in scope iff it matches at least one include and no exclude.
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl Scope {
    pub fn all() -> Self {
        Self {
            include: vec!["*".to_string()],
            exclude: Vec::new(),
        }
    }

    pub fn matches_host(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.');
        self.include.iter().any(|p| wildcard_match(p, host))
            && !self.exclude.iter().any(|p| wildcard_match(p, host))
    }

    /// Convenience constructor: include `patterns`, nothing excluded.
    pub fn include(patterns: &[&str]) -> Self {
        Self {
            include: patterns.iter().map(|s| s.to_string()).collect(),
            exclude: Vec::new(),
        }
    }
}

/// Iterative glob matcher supporting `*` (any sequence) and `?` (single char).
/// Case-insensitive on hosts per DNS convention.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    wildcard_match_inner(pattern.as_bytes(), text.as_bytes(), |p, t| {
        p.eq_ignore_ascii_case(t)
    })
}

fn wildcard_match_inner(pattern: &[u8], text: &[u8], eq: impl Fn(&u8, &u8) -> bool) -> bool {
    // classic two-star backtracking algorithm
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut star_t) = (None::<usize>, 0usize);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'*' || eq(&pattern[p], &text[t])) {
            if pattern[p] == b'*' {
                star = Some(p);
                star_t = t;
                p += 1;
            } else {
                p += 1;
                t += 1;
            }
        } else if let Some(s) = star {
            p = s + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TrafficSource {
    /// Live traffic entering through the proxy.
    #[default]
    Proxy,
    /// Re-driven from history (Repeater / automation).
    Replay,
    /// Generated from scratch (Fuzzer / request composer / OpenAPI).
    Generator,
    /// Launched by an automation workflow.
    Automation,
}

/// What the current stage is *allowed* to do. Phase 0 models the vocabulary;
/// the daemon starts enforcing it when subsystems exist (storage, findings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[ts(export)]
pub struct Capabilities {
    pub read_history: bool,
    pub write_history: bool,
    pub modify_traffic: bool,
    pub network_access: bool,
    pub emit_findings: bool,
}

impl Capabilities {
    /// Sensible defaults for the live proxy path.
    pub fn proxy_defaults() -> Self {
        Self {
            read_history: true,
            write_history: true,
            modify_traffic: true,
            network_access: true,
            emit_findings: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RequestContext {
    pub flow_id: FlowId,
    pub source: TrafficSource,
    pub scope: Scope,
    pub capabilities: Capabilities,
    /// Free-form scratch space shared between hooks of one run. Serializable
    /// by construction — this map is exactly the shape a future plugin broker
    /// will forward across the WASM boundary (ADR-006 rule #2).
    #[ts(type = "{ [key: string]: unknown }")]
    pub data: serde_json::Map<String, Value>,
}

impl RequestContext {
    pub fn new(flow_id: FlowId, source: TrafficSource) -> Self {
        Self {
            flow_id,
            source,
            scope: Scope::all(),
            capabilities: Capabilities::default(),
            data: serde_json::Map::new(),
        }
    }

    pub fn set_data(&mut self, key: impl Into<String>, value: Value) {
        self.data.insert(key.into(), value);
    }

    pub fn get_data(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    pub fn in_scope(&self, host: &str) -> bool {
        self.scope.matches_host(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_wildcards() {
        let s = Scope::include(&["*.example.com"]);
        assert!(s.matches_host("api.example.com"));
        assert!(s.matches_host("a.b.example.com"));
        assert!(!s.matches_host("example.com")); // star eats at least one char
        assert!(!s.matches_host("badexample.com"));

        let exact = Scope::include(&["example.com"]);
        assert!(exact.matches_host("example.com"));
        assert!(!exact.matches_host("api.example.com"));

        let mut s = Scope::all();
        s.exclude.push("evil.com".into());
        assert!(s.matches_host("good.com"));
        assert!(!s.matches_host("evil.com"));
    }

    #[test]
    fn context_data_roundtrip() {
        let mut ctx = RequestContext::new(1, TrafficSource::Proxy);
        ctx.set_data("classified", Value::String("api".into()));
        assert_eq!(
            ctx.get_data("classified").and_then(|v| v.as_str()),
            Some("api")
        );
    }
}

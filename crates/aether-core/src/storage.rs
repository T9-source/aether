//! Storage port (ADR-005) — the daemon persists flow history behind this trait.
//!
//! The **contract** lives in the domain core; the SQLite implementation lives in
//! `aether-store-sqlite` (the core must never know about SQL, ADR-005). Two
//! non-negotiable parts of the contract:
//!
//! 1. **Keyset pagination** — `FlowQuery.before/after` cursors only. No offsets:
//!    a 100k-flow history must paginate in O(limit) per page.
//! 2. **Full-fidelity records** — flows round-trip exactly (ordered headers,
//!    binary bodies as base64) because `Flow`/`Request`/`Response` are the same
//!    serializable shapes used everywhere else.
//!
//! [`MemoryStorage`] is a correct in-process implementation used by tests, by
//! `aether serve --memory`, and as the reference for backend conformance tests.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;
use ts_rs::TS;

use crate::error::AetherResult;
use crate::model::{Flow, FlowId, TransportProto};

/// Compact row shape for history lists — no bodies, no header maps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FlowSummary {
    pub id: FlowId,
    pub created_at_unix_ms: u64,
    pub method: String,
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub query: Option<String>,
    pub status: Option<u16>,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub duration_ms: Option<u64>,
    pub client_proto: TransportProto,
    pub server_proto: TransportProto,
    pub tags: Vec<String>,
}

impl FlowSummary {
    pub fn from_flow(flow: &Flow) -> Self {
        let (method, scheme, host, port, path, query, request_bytes) = flow
            .request
            .as_ref()
            .map(|r| {
                (
                    r.method.as_str().to_string(),
                    r.url.scheme.clone(),
                    r.url.host.clone(),
                    r.url.effective_port(),
                    r.url.path.clone(),
                    r.url.query.clone(),
                    r.body.len() as u64,
                )
            })
            .unwrap_or_else(|| {
                (
                    String::new(),
                    String::new(),
                    String::new(),
                    80,
                    "/".into(),
                    None,
                    0,
                )
            });
        let (status, response_bytes) = flow
            .response
            .as_ref()
            .map(|r| (Some(r.status.code), r.body.len() as u64))
            .unwrap_or((None, 0));
        Self {
            id: flow.id,
            created_at_unix_ms: flow.meta.created_at_unix_ms,
            method,
            scheme,
            host,
            port,
            path,
            query,
            status,
            request_bytes,
            response_bytes,
            duration_ms: flow.meta.duration_ms,
            client_proto: flow.meta.client_proto,
            server_proto: flow.meta.server_proto,
            tags: flow.meta.tags.clone(),
        }
    }

    /// True when the flow was dropped before a response was served.
    pub fn is_dropped(&self) -> bool {
        self.tags.iter().any(|t| t.eq_ignore_ascii_case("dropped"))
    }
}

/// Keyset-paginated history query (ADR-005). Cursors are flow ids: page
/// "newer" reads `after = <cursor>`, page "older" reads `before = <cursor>`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FlowQuery {
    pub before: Option<FlowId>,
    pub after: Option<FlowId>,
    pub limit: u32,
    /// Glob filter on host (`*.example.com`, `api.*`) — case-insensitive.
    pub host: Option<String>,
    /// Exact method filter, case-insensitive.
    pub method: Option<String>,
}

impl FlowQuery {
    pub fn limit(&self) -> u32 {
        self.limit.clamp(1, 500)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FlowPage {
    pub items: Vec<FlowSummary>,
    /// Pass as `before` to fetch the next (older) page.
    pub next_cursor: Option<FlowId>,
    /// Pass as `after` to fetch the previous (newer) page.
    pub prev_cursor: Option<FlowId>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FlowStats {
    pub total: u64,
    pub completed: u64,
    pub dropped: u64,
}

/// The storage contract (ADR-005). Implementations must be crash-safe enough
/// for a dev tool and must preserve `Flow` fidelity exactly.
#[async_trait]
pub trait Storage: Send + Sync {
    /// Reserve the next monotonic flow id (the engine needs it *before* the
    /// flow is complete — events carry it from the first byte).
    async fn alloc_id(&self) -> AetherResult<FlowId>;

    /// Insert or replace a full flow record (in-flight or completed).
    async fn put_flow(&self, flow: &Flow) -> AetherResult<()>;

    /// Batch insert/replace. Backends should implement this transactionally
    /// (history import, replay, benchmarks); the default just loops.
    async fn put_flows(&self, flows: &[Flow]) -> AetherResult<()> {
        for flow in flows {
            self.put_flow(flow).await?;
        }
        Ok(())
    }

    async fn get_flow(&self, id: FlowId) -> AetherResult<Option<Flow>>;

    /// Keyset-paginated, newest-first history.
    async fn list_flows(&self, query: FlowQuery) -> AetherResult<FlowPage>;

    /// Full-text-ish search over method/host/path/query/body. Backends may
    /// implement with FTS5, LIKE, or linear scan — the contract is substring
    /// semantics, newest-first.
    async fn search_flows(&self, query: &str, limit: u32) -> AetherResult<Vec<FlowSummary>>;

    async fn flow_stats(&self) -> AetherResult<FlowStats>;
}

/// Correct in-memory implementation (tests, `--memory`, conformance reference).
#[derive(Default)]
pub struct MemoryStorage {
    inner: Mutex<MemInner>,
}

#[derive(Default)]
struct MemInner {
    next: FlowId,
    flows: BTreeMap<FlowId, Flow>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Storage for MemoryStorage {
    async fn alloc_id(&self) -> AetherResult<FlowId> {
        let mut g = self.inner.lock().expect("memory storage lock");
        g.next += 1;
        Ok(g.next)
    }

    async fn put_flow(&self, flow: &Flow) -> AetherResult<()> {
        self.inner
            .lock()
            .expect("memory storage lock")
            .flows
            .insert(flow.id, flow.clone());
        Ok(())
    }

    async fn get_flow(&self, id: FlowId) -> AetherResult<Option<Flow>> {
        Ok(self
            .inner
            .lock()
            .expect("memory storage lock")
            .flows
            .get(&id)
            .cloned())
    }

    async fn list_flows(&self, query: FlowQuery) -> AetherResult<FlowPage> {
        let g = self.inner.lock().expect("memory storage lock");
        let limit = query.limit() as usize;
        let mut ids: Vec<FlowId> = g
            .flows
            .values()
            .filter(|f| {
                if let Some(before) = query.before {
                    if f.id >= before {
                        return false;
                    }
                }
                if let Some(after) = query.after {
                    if f.id <= after {
                        return false;
                    }
                }
                if let Some(m) = &query.method {
                    let req_method = f.request.as_ref().map(|r| r.method.as_str()).unwrap_or("");
                    if !req_method.eq_ignore_ascii_case(m) {
                        return false;
                    }
                }
                if let Some(pattern) = &query.host {
                    let host = f
                        .request
                        .as_ref()
                        .map(|r| r.url.host.as_str())
                        .unwrap_or("");
                    if !crate::context::wildcard_match(pattern, host) {
                        return false;
                    }
                }
                true
            })
            .map(|f| f.id)
            .collect();
        ids.sort_unstable_by(|a, b| b.cmp(a)); // newest first

        let total_matching = ids.len();
        let page_ids: Vec<FlowId> = ids.iter().take(limit).copied().collect();
        let items: Vec<FlowSummary> = page_ids
            .iter()
            .filter_map(|id| g.flows.get(id).map(FlowSummary::from_flow))
            .collect();
        let next_cursor = if total_matching > limit {
            items.last().map(|s| s.id)
        } else {
            None
        };
        let prev_cursor = if query.before.is_some() || query.after.is_some() {
            items.first().map(|s| s.id)
        } else {
            None
        };
        Ok(FlowPage {
            items,
            next_cursor,
            prev_cursor,
        })
    }

    async fn search_flows(&self, query: &str, limit: u32) -> AetherResult<Vec<FlowSummary>> {
        let needle = query.to_lowercase();
        let g = self.inner.lock().expect("memory storage lock");
        let mut hits: Vec<FlowSummary> = g
            .flows
            .values()
            .filter(|f| flow_contains_needle(f, &needle))
            .map(FlowSummary::from_flow)
            .collect();
        hits.sort_by_key(|s| std::cmp::Reverse(s.id));
        hits.truncate(limit.clamp(1, 500) as usize);
        Ok(hits)
    }

    async fn flow_stats(&self) -> AetherResult<FlowStats> {
        let g = self.inner.lock().expect("memory storage lock");
        let mut stats = FlowStats::default();
        for f in g.flows.values() {
            stats.total += 1;
            if f.response.is_some() {
                stats.completed += 1;
            }
            if f.meta
                .tags
                .iter()
                .any(|t| t.eq_ignore_ascii_case("dropped"))
            {
                stats.dropped += 1;
            }
        }
        Ok(stats)
    }
}

fn flow_contains_needle(flow: &Flow, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut hay = String::new();
    if let Some(req) = &flow.request {
        hay.push_str(&req.method.as_str().to_lowercase());
        hay.push(' ');
        hay.push_str(&req.url.as_url().to_lowercase());
        hay.push(' ');
        hay.push_str(&req.body.display_text().to_lowercase());
        hay.push(' ');
    }
    if let Some(res) = &flow.response {
        hay.push_str(&res.body.display_text().to_lowercase());
    }
    hay.contains(needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Body, Method, Request, Response, Url};

    fn flow(id: FlowId, host: &str, status: Option<u16>, dropped: bool) -> Flow {
        let mut f = Flow::new(
            id,
            Request::new(
                Method::Get,
                Url::parse(&format!("https://{host}/p?id={id}")).unwrap(),
            ),
        );
        f.request.as_mut().unwrap().body = Body::text(format!("body-{id}"));
        if let Some(code) = status {
            f.response = Some(Response::new(code).unwrap());
        }
        if dropped {
            f.add_tag("dropped");
        }
        f
    }

    #[tokio::test]
    async fn alloc_put_get_roundtrip() {
        let s = MemoryStorage::new();
        let id = s.alloc_id().await.unwrap();
        assert_eq!(id, 1);
        let f = flow(id, "a.com", Some(200), false);
        s.put_flow(&f).await.unwrap();
        let got = s.get_flow(id).await.unwrap().unwrap();
        assert_eq!(got, f);
        assert!(s.get_flow(99).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn keyset_pagination_walks_all_without_dupes() {
        let s = MemoryStorage::new();
        for i in 1..=25u64 {
            s.put_flow(&flow(i, "a.com", Some(200), false))
                .await
                .unwrap();
        }
        let mut seen = Vec::new();
        let mut cursor: Option<FlowId> = None;
        loop {
            let page = s
                .list_flows(FlowQuery {
                    before: cursor,
                    limit: 10,
                    ..Default::default()
                })
                .await
                .unwrap();
            seen.extend(page.items.iter().map(|s| s.id));
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        assert_eq!(seen.len(), 25);
        assert_eq!(seen, (1..=25).rev().collect::<Vec<_>>(), "newest first");
    }

    #[tokio::test]
    async fn filters_and_stats() {
        let s = MemoryStorage::new();
        s.put_flow(&flow(1, "api.a.com", Some(200), false))
            .await
            .unwrap();
        s.put_flow(&flow(2, "web.b.com", None, true)).await.unwrap();
        let page = s
            .list_flows(FlowQuery {
                host: Some("*.a.com".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].host, "api.a.com");

        let stats = s.flow_stats().await.unwrap();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.completed, 1);
        assert_eq!(stats.dropped, 1);
    }

    #[tokio::test]
    async fn search_hits_body_and_url() {
        let s = MemoryStorage::new();
        s.put_flow(&flow(1, "a.com", Some(200), false))
            .await
            .unwrap();
        s.put_flow(&flow(2, "b.com", Some(404), false))
            .await
            .unwrap();
        let hits = s.search_flows("body-2", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 2);
    }
}

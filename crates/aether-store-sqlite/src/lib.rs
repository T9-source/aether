//! Aether storage — SQLite via sqlx (runtime API, no macros).
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! ADR-005 contract: WAL journal, keyset pagination (cursors only, no
//! offsets), full-fidelity flow records (serialized `Request`/`Response`
//! JSON — base64 bodies, ordered headers), FTS5 search **with graceful
//! degradation** to metadata LIKE search when the bundled SQLite lacks FTS5.

use aether_core::error::{AetherError, AetherResult};
use aether_core::model::{Flow, FlowId};
use aether_core::storage::{FlowPage, FlowQuery, FlowStats, FlowSummary, Storage};
use async_trait::async_trait;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow, SqliteSynchronous,
};
use sqlx::{Pool, QueryBuilder, Row, Sqlite};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub struct SqliteStore {
    pool: Pool<Sqlite>,
    fts: bool,
    next_id: AtomicU64,
}

const FLOW_COLUMNS: &str = "id, created_at, completed, dropped, method, scheme, host, port, \
     path, query, status, request_bytes, response_bytes, duration_ms, \
     client_proto, server_proto, tags_json, request_json, response_json, tls_json";

impl SqliteStore {
    /// Opens (creating if needed) a store at `path`. `:memory:` uses a
    /// single-connection pool so the in-memory database is shared.
    pub async fn open(path: &Path) -> AetherResult<Self> {
        let is_memory = path.as_os_str() == ":memory:";
        let mut opts = SqliteConnectOptions::new();
        opts = if is_memory {
            opts.filename(":memory:")
        } else {
            std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))
                .map_err(|e| AetherError::storage(format!("create data dir: {e}")))?;
            opts.filename(path)
        }
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            .max_connections(if is_memory { 1 } else { 4 })
            .connect_with(opts)
            .await
            .map_err(|e| AetherError::storage(format!("open sqlite {path:?}: {e}")))?;

        let mut store = Self {
            pool,
            fts: false,
            next_id: AtomicU64::new(0),
        };
        store.migrate().await?;
        store.fts = store.probe_fts5().await;
        if store.fts {
            tracing::info!("FTS5 available — full-text flow search enabled");
        } else {
            tracing::warn!("FTS5 unavailable — falling back to metadata-only search");
        }
        let max: i64 = sqlx::query("SELECT COALESCE(MAX(id), 0) AS m FROM flows")
            .fetch_one(&store.pool)
            .await
            .map_err(db)?
            .get("m");
        store.next_id.store(max as u64, Ordering::SeqCst);
        Ok(store)
    }

    async fn migrate(&self) -> AetherResult<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS flows (
                id              INTEGER PRIMARY KEY,
                created_at      INTEGER NOT NULL,
                completed       INTEGER NOT NULL DEFAULT 0,
                dropped         INTEGER NOT NULL DEFAULT 0,
                method          TEXT NOT NULL,
                scheme          TEXT NOT NULL,
                host            TEXT NOT NULL,
                port            INTEGER NOT NULL,
                path            TEXT NOT NULL,
                query           TEXT,
                status          INTEGER,
                request_bytes   INTEGER NOT NULL DEFAULT 0,
                response_bytes  INTEGER NOT NULL DEFAULT 0,
                duration_ms     INTEGER,
                client_proto    TEXT NOT NULL DEFAULT 'plaintext',
                server_proto    TEXT NOT NULL DEFAULT 'plaintext',
                tags_json       TEXT NOT NULL DEFAULT '[]',
                request_json    TEXT,
                response_json   TEXT,
                tls_json        TEXT
            )"#,
        )
        .execute(&self.pool)
        .await
        .map_err(db)?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_flows_host ON flows(host)")
            .execute(&self.pool)
            .await
            .map_err(db)?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_flows_method ON flows(method)")
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn probe_fts5(&self) -> bool {
        sqlx::query(
            "CREATE VIRTUAL TABLE IF NOT EXISTS flows_fts USING fts5(text, flow_id UNINDEXED)",
        )
        .execute(&self.pool)
        .await
        .is_ok()
    }

    async fn index_flow(&self, flow: &Flow) {
        if !self.fts {
            return;
        }
        let text = fts_text(flow);
        let _ = sqlx::query("DELETE FROM flows_fts WHERE rowid = ?1")
            .bind(flow.id as i64)
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("INSERT INTO flows_fts (rowid, text) VALUES (?1, ?2)")
            .bind(flow.id as i64)
            .bind(&text)
            .execute(&self.pool)
            .await;
    }
}

fn fts_text(flow: &Flow) -> String {
    let mut out = String::new();
    if let Some(req) = &flow.request {
        out.push_str(req.method.as_str());
        out.push(' ');
        out.push_str(&req.url.as_url());
        out.push(' ');
        for h in req.headers.iter() {
            out.push_str(&h.name);
            out.push(':');
            out.push_str(&h.value);
            out.push(' ');
        }
        out.push_str(&req.body.display_text());
        out.push(' ');
    }
    if let Some(res) = &flow.response {
        out.push_str(&res.body.display_text());
    }
    out
}

fn db(e: sqlx::Error) -> AetherError {
    AetherError::storage(format!("sqlite: {e}"))
}

fn serde_err(e: serde_json::Error) -> AetherError {
    AetherError::storage(format!("record decode: {e}"))
}

fn flow_to_row(flow: &Flow) -> AetherResult<RowValues> {
    let summary = FlowSummary::from_flow(flow);
    let completed = flow.response.is_some();
    let dropped = summary.is_dropped();
    Ok(RowValues {
        id: flow.id as i64,
        created_at: flow.meta.created_at_unix_ms as i64,
        completed: completed as i64,
        dropped: dropped as i64,
        method: summary.method.clone(),
        scheme: summary.scheme.clone(),
        host: summary.host.clone(),
        port: summary.port as i64,
        path: summary.path.clone(),
        query: summary.query.clone(),
        status: summary.status.map(|s| s as i64),
        request_bytes: summary.request_bytes as i64,
        response_bytes: summary.response_bytes as i64,
        duration_ms: flow.meta.duration_ms.map(|d| d as i64),
        client_proto: serde_json::to_string(&flow.meta.client_proto).map_err(serde_err)?,
        server_proto: serde_json::to_string(&flow.meta.server_proto).map_err(serde_err)?,
        tags_json: serde_json::to_string(&flow.meta.tags).map_err(serde_err)?,
        request_json: flow
            .request
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(serde_err)?,
        response_json: flow
            .response
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(serde_err)?,
        tls_json: flow
            .meta
            .tls
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(serde_err)?,
    })
}

#[allow(dead_code)]
struct RowValues {
    id: i64,
    created_at: i64,
    completed: i64,
    dropped: i64,
    method: String,
    scheme: String,
    host: String,
    port: i64,
    path: String,
    query: Option<String>,
    status: Option<i64>,
    request_bytes: i64,
    response_bytes: i64,
    duration_ms: Option<i64>,
    client_proto: String,
    server_proto: String,
    tags_json: String,
    request_json: Option<String>,
    response_json: Option<String>,
    tls_json: Option<String>,
}

fn row_to_summary(row: &SqliteRow) -> AetherResult<FlowSummary> {
    let tags: Vec<String> = serde_json::from_str(row.try_get::<&str, _>("tags_json").map_err(db)?)
        .map_err(serde_err)?;
    Ok(FlowSummary {
        id: row.try_get::<i64, _>("id").map_err(db)? as u64,
        created_at_unix_ms: row.try_get::<i64, _>("created_at").map_err(db)? as u64,
        method: row.try_get::<String, _>("method").map_err(db)?,
        scheme: row.try_get::<String, _>("scheme").map_err(db)?,
        host: row.try_get::<String, _>("host").map_err(db)?,
        port: row.try_get::<i64, _>("port").map_err(db)? as u16,
        path: row.try_get::<String, _>("path").map_err(db)?,
        query: row.try_get::<Option<String>, _>("query").map_err(db)?,
        status: row
            .try_get::<Option<i64>, _>("status")
            .map_err(db)?
            .map(|s| s as u16),
        request_bytes: row.try_get::<i64, _>("request_bytes").map_err(db)? as u64,
        response_bytes: row.try_get::<i64, _>("response_bytes").map_err(db)? as u64,
        duration_ms: row
            .try_get::<Option<i64>, _>("duration_ms")
            .map_err(db)?
            .map(|d| d as u64),
        client_proto: serde_json::from_str(row.try_get::<&str, _>("client_proto").map_err(db)?)
            .map_err(serde_err)?,
        server_proto: serde_json::from_str(row.try_get::<&str, _>("server_proto").map_err(db)?)
            .map_err(serde_err)?,
        tags,
    })
}

fn row_to_flow(row: &SqliteRow) -> AetherResult<Flow> {
    let request: Option<aether_core::model::Request> = row
        .try_get::<Option<&str>, _>("request_json")
        .map_err(db)?
        .map(serde_json::from_str)
        .transpose()
        .map_err(serde_err)?;
    let response: Option<aether_core::model::Response> = row
        .try_get::<Option<&str>, _>("response_json")
        .map_err(db)?
        .map(serde_json::from_str)
        .transpose()
        .map_err(serde_err)?;
    let tags: Vec<String> = serde_json::from_str(row.try_get::<&str, _>("tags_json").map_err(db)?)
        .map_err(serde_err)?;
    let tls: Option<aether_core::model::TlsMeta> = row
        .try_get::<Option<&str>, _>("tls_json")
        .map_err(db)?
        .map(serde_json::from_str)
        .transpose()
        .map_err(serde_err)?;
    let client_proto: aether_core::model::TransportProto =
        serde_json::from_str(row.try_get::<&str, _>("client_proto").map_err(db)?)
            .map_err(serde_err)?;
    let server_proto: aether_core::model::TransportProto =
        serde_json::from_str(row.try_get::<&str, _>("server_proto").map_err(db)?)
            .map_err(serde_err)?;

    Ok(Flow {
        id: row.try_get::<i64, _>("id").map_err(db)? as u64,
        request,
        response,
        meta: aether_core::model::FlowMeta {
            created_at_unix_ms: row.try_get::<i64, _>("created_at").map_err(db)? as u64,
            duration_ms: row
                .try_get::<Option<i64>, _>("duration_ms")
                .map_err(db)?
                .map(|d| d as u64),
            client_proto,
            server_proto,
            tls,
            tags,
        },
    })
}

#[async_trait]
impl Storage for SqliteStore {
    async fn alloc_id(&self) -> AetherResult<FlowId> {
        Ok(self.next_id.fetch_add(1, Ordering::SeqCst) + 1)
    }

    async fn put_flow(&self, flow: &Flow) -> AetherResult<()> {
        let v = flow_to_row(flow)?;
        sqlx::query(
            r#"INSERT INTO flows
               (id, created_at, completed, dropped, method, scheme, host, port, path, query,
                status, request_bytes, response_bytes, duration_ms, client_proto, server_proto,
                tags_json, request_json, response_json, tls_json)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)
               ON CONFLICT(id) DO UPDATE SET
                 created_at=excluded.created_at, completed=excluded.completed,
                 dropped=excluded.dropped, method=excluded.method, scheme=excluded.scheme,
                 host=excluded.host, port=excluded.port, path=excluded.path, query=excluded.query,
                 status=excluded.status, request_bytes=excluded.request_bytes,
                 response_bytes=excluded.response_bytes, duration_ms=excluded.duration_ms,
                 client_proto=excluded.client_proto, server_proto=excluded.server_proto,
                 tags_json=excluded.tags_json, request_json=excluded.request_json,
                 response_json=excluded.response_json, tls_json=excluded.tls_json"#,
        )
        .bind(v.id)
        .bind(v.created_at)
        .bind(v.completed)
        .bind(v.dropped)
        .bind(&v.method)
        .bind(&v.scheme)
        .bind(&v.host)
        .bind(v.port)
        .bind(&v.path)
        .bind(&v.query)
        .bind(v.status)
        .bind(v.request_bytes)
        .bind(v.response_bytes)
        .bind(v.duration_ms)
        .bind(&v.client_proto)
        .bind(&v.server_proto)
        .bind(&v.tags_json)
        .bind(&v.request_json)
        .bind(&v.response_json)
        .bind(&v.tls_json)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        self.index_flow(flow).await;
        Ok(())
    }

    async fn put_flows(&self, flows: &[Flow]) -> AetherResult<()> {
        // one transaction for the whole batch — the difference between
        // ~200/s and tens of thousands/s on spinning-everywhere autocommit
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AetherError::storage(format!("begin batch: {e}")))?;
        for flow in flows {
            let v = flow_to_row(flow)?;
            sqlx::query(
                r#"INSERT INTO flows
                   (id, created_at, completed, dropped, method, scheme, host, port, path, query,
                    status, request_bytes, response_bytes, duration_ms, client_proto, server_proto,
                    tags_json, request_json, response_json, tls_json)
                   VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)
                   ON CONFLICT(id) DO UPDATE SET
                     created_at=excluded.created_at, completed=excluded.completed,
                     dropped=excluded.dropped, method=excluded.method, scheme=excluded.scheme,
                     host=excluded.host, port=excluded.port, path=excluded.path, query=excluded.query,
                     status=excluded.status, request_bytes=excluded.request_bytes,
                     response_bytes=excluded.response_bytes, duration_ms=excluded.duration_ms,
                     client_proto=excluded.client_proto, server_proto=excluded.server_proto,
                     tags_json=excluded.tags_json, request_json=excluded.request_json,
                     response_json=excluded.response_json, tls_json=excluded.tls_json"#,
            )
            .bind(v.id)
            .bind(v.created_at)
            .bind(v.completed)
            .bind(v.dropped)
            .bind(&v.method)
            .bind(&v.scheme)
            .bind(&v.host)
            .bind(v.port)
            .bind(&v.path)
            .bind(&v.query)
            .bind(v.status)
            .bind(v.request_bytes)
            .bind(v.response_bytes)
            .bind(v.duration_ms)
            .bind(&v.client_proto)
            .bind(&v.server_proto)
            .bind(&v.tags_json)
            .bind(&v.request_json)
            .bind(&v.response_json)
            .bind(&v.tls_json)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            if self.fts {
                let text = fts_text(flow);
                let _ = sqlx::query("DELETE FROM flows_fts WHERE rowid = ?1")
                    .bind(flow.id as i64)
                    .execute(&mut *tx)
                    .await;
                let _ = sqlx::query("INSERT INTO flows_fts (rowid, text) VALUES (?1, ?2)")
                    .bind(flow.id as i64)
                    .bind(&text)
                    .execute(&mut *tx)
                    .await;
            }
        }
        tx.commit()
            .await
            .map_err(|e| AetherError::storage(format!("commit batch: {e}")))?;
        Ok(())
    }

    async fn get_flow(&self, id: FlowId) -> AetherResult<Option<Flow>> {
        let row = sqlx::query(&format!("SELECT {FLOW_COLUMNS} FROM flows WHERE id = ?1"))
            .bind(id as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?;
        match row {
            Some(r) => Ok(Some(row_to_flow(&r)?)),
            None => Ok(None),
        }
    }

    async fn list_flows(&self, query: FlowQuery) -> AetherResult<FlowPage> {
        let limit = query.limit();
        let mut qb: QueryBuilder<Sqlite> =
            QueryBuilder::new(format!("SELECT {FLOW_COLUMNS} FROM flows WHERE 1=1"));
        if let Some(before) = query.before {
            qb.push(" AND id < ").push_bind(before as i64);
        }
        if let Some(after) = query.after {
            qb.push(" AND id > ").push_bind(after as i64);
        }
        if let Some(pattern) = &query.host {
            let like = pattern.replace('*', "%").replace('?', "_");
            qb.push(" AND host LIKE ").push_bind(like);
        }
        if let Some(m) = &query.method {
            qb.push(" AND UPPER(method) = ").push_bind(m.to_uppercase());
        }

        let ascending = query.after.is_some() && query.before.is_none();
        qb.push(if ascending {
            " ORDER BY id ASC LIMIT "
        } else {
            " ORDER BY id DESC LIMIT "
        });
        qb.push_bind(limit as i64 + 1); // one extra row to detect a next page

        let rows = qb.build().fetch_all(&self.pool).await.map_err(db)?;
        let fetched = rows.len();
        // keep only the rows adjacent to the cursor (fetch order), then flip
        // to display order (newest-first)
        let mut items: Vec<FlowSummary> = rows
            .iter()
            .take(limit as usize)
            .map(row_to_summary)
            .collect::<AetherResult<_>>()?;
        if ascending {
            items.reverse(); // display is always newest-first
        }
        // `fetched` counted BEFORE truncation — that's the whole point of the
        // extra row: it tells us whether another page exists.
        let has_more_newer = ascending && fetched > limit as usize;
        let has_more_older = !ascending && fetched > limit as usize;
        let next_cursor = if has_more_older || (ascending && !items.is_empty()) {
            items.last().map(|s| s.id)
        } else {
            None
        };
        let prev_cursor = if has_more_newer || (!ascending && query.before.is_some()) {
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
        let limit = limit.clamp(1, 500);
        if query.trim().is_empty() {
            let page = self
                .list_flows(FlowQuery {
                    limit,
                    ..Default::default()
                })
                .await?;
            return Ok(page.items);
        }
        if self.fts {
            // quote the user query so FTS5 treats it as a phrase, not syntax
            // (a bare `needle-body-2` would parse `-` as a column operator)
            let phrase = format!("\"{}\"", query.replace('"', "\"\""));
            let rows = sqlx::query(
                "SELECT f.id, f.created_at, f.method, f.scheme, f.host, f.port, f.path, f.query, \
                        f.status, f.request_bytes, f.response_bytes, f.duration_ms, \
                        f.client_proto, f.server_proto, f.tags_json \
                 FROM flows_fts ft JOIN flows f ON f.id = ft.rowid \
                 WHERE flows_fts MATCH ?1 ORDER BY rank LIMIT ?2",
            )
            .bind(phrase)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(db)?;
            return rows.iter().map(row_to_summary).collect();
        }
        // Fallback: metadata-only substring search (documented degradation)
        let needle = format!("%{}%", query.to_lowercase());
        let rows = sqlx::query(
            &format!(
                "SELECT {FLOW_COLUMNS} FROM flows \
                 WHERE LOWER(method || ' ' || scheme || '://' || host || ':' || port || path) LIKE ?1 \
                 ORDER BY id DESC LIMIT ?2"
            ),
        )
        .bind(needle)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(row_to_summary).collect()
    }

    async fn flow_stats(&self) -> AetherResult<FlowStats> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS total, COALESCE(SUM(completed),0) AS completed, \
             COALESCE(SUM(dropped),0) AS dropped FROM flows",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        Ok(FlowStats {
            total: row.try_get::<i64, _>("total").map_err(db)? as u64,
            completed: row.try_get::<i64, _>("completed").map_err(db)? as u64,
            dropped: row.try_get::<i64, _>("dropped").map_err(db)? as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_core::model::{Body, Method, Request, Response, Url};
    use aether_core::storage::MemoryStorage;

    async fn open_tmp() -> (tempfile::TempDir, SqliteStore) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = SqliteStore::open(&dir.path().join("test.db"))
            .await
            .unwrap();
        (dir, store)
    }

    fn flow(id: FlowId, host: &str, status: Option<u16>, dropped: bool) -> Flow {
        let mut f = Flow::new(
            id,
            Request::new(
                Method::Get,
                Url::parse(&format!("https://{host}/p?id={id}")).unwrap(),
            ),
        );
        f.request.as_mut().unwrap().body = Body::text(format!("needle-body-{id}"));
        f.request.as_mut().unwrap().set_header("X-Wire", "keep");
        if let Some(code) = status {
            let mut res = Response::new(code).unwrap();
            res.body = Body::text("response-payload");
            f.response = Some(res);
        }
        if dropped {
            f.add_tag("dropped");
        }
        f
    }

    #[tokio::test]
    async fn full_fidelity_roundtrip() {
        let (_d, store) = open_tmp().await;
        let id = store.alloc_id().await.unwrap();
        let f = flow(id, "a.com", Some(200), false);
        store.put_flow(&f).await.unwrap();
        let got = store.get_flow(id).await.unwrap().unwrap();
        assert_eq!(
            got, f,
            "headers order, body bytes, tags, tls — all preserved"
        );
    }

    #[tokio::test]
    async fn keyset_pagination_walks_everything() {
        let (_d, store) = open_tmp().await;
        for i in 1..=60u64 {
            store
                .put_flow(&flow(i, "a.com", Some(200), false))
                .await
                .unwrap();
        }
        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let page = store
                .list_flows(FlowQuery {
                    before: cursor,
                    limit: 25,
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
        assert_eq!(seen, (1..=60).rev().collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn newer_page_via_after_cursor() {
        let (_d, store) = open_tmp().await;
        for i in 1..=30u64 {
            store
                .put_flow(&flow(i, "a.com", Some(200), false))
                .await
                .unwrap();
        }
        let page = store
            .list_flows(FlowQuery {
                after: Some(20),
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            page.items.iter().map(|s| s.id).collect::<Vec<_>>(),
            (21..=30).rev().collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn host_and_method_filters() {
        let (_d, store) = open_tmp().await;
        store
            .put_flow(&flow(1, "api.a.com", Some(200), false))
            .await
            .unwrap();
        let mut f2 = flow(2, "web.b.com", Some(404), false);
        f2.request = Some(Request::new(
            Method::Post,
            Url::parse("https://web.b.com/p").unwrap(),
        ));
        store.put_flow(&f2).await.unwrap();

        let page = store
            .list_flows(FlowQuery {
                host: Some("*.a.com".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);

        let page = store
            .list_flows(FlowQuery {
                method: Some("post".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, 2);
    }

    #[tokio::test]
    async fn search_finds_body_text() {
        let (_d, store) = open_tmp().await;
        store
            .put_flow(&flow(1, "a.com", Some(200), false))
            .await
            .unwrap();
        store
            .put_flow(&flow(2, "b.com", Some(200), false))
            .await
            .unwrap();
        let hits = store.search_flows("needle-body-2", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 2);
    }

    #[tokio::test]
    async fn stats_count_completed_and_dropped() {
        let (_d, store) = open_tmp().await;
        store
            .put_flow(&flow(1, "a.com", Some(200), false))
            .await
            .unwrap();
        store.put_flow(&flow(2, "a.com", None, true)).await.unwrap();
        let stats = store.flow_stats().await.unwrap();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.completed, 1);
        assert_eq!(stats.dropped, 1);
    }

    /// ADR-005 conformance: the SQLite backend must behave exactly like the
    /// in-memory reference implementation for the pagination contract.
    #[tokio::test]
    async fn matches_memory_storage_contract() {
        let (dir, s) = open_tmp().await; // dir must outlive the store
        let mem: Box<dyn Storage> = Box::new(MemoryStorage::new());
        let sql: Box<dyn Storage> = Box::new(s);
        for i in 1..=35u64 {
            let f = flow(i, "a.com", Some(200), false);
            mem.put_flow(&f).await.unwrap();
            sql.put_flow(&f).await.unwrap();
        }
        let m = mem
            .list_flows(FlowQuery {
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        let s = sql
            .list_flows(FlowQuery {
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(m.items, s.items);
        assert_eq!(m.next_cursor, s.next_cursor);
        drop(sql);
        drop(dir);
    }
}

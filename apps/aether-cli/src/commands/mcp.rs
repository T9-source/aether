//! MCP (Model Context Protocol) server — exposes a running Aether daemon to
//! any MCP client (Claude Desktop, VS Code, Cline, …) over stdio.
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Design invariants:
//! - The daemon owns all state (ADR-001): this server is a *thin client* of
//!   the control plane (ADR-002/010) — it holds nothing, caches nothing.
//! - Read-only by default; mutating tools exist only when the operator passes
//!   `--allow-write` (ADR-008: opt-in, explicit, zero telemetry).
//! - Newline-delimited JSON-RPC 2.0 on stdin/stdout (MCP stdio transport).

use crate::client::ControlClient;
use aether_core::error::AetherResult;
use aether_daemon::ControlRequest;
use serde_json::{Value, json};
use std::sync::{Arc, LazyLock};

pub const PROTOCOL_VERSION: &str = "2024-11-05";
pub const SERVER_NAME: &str = "aether";

/// One control-plane round trip — abstracted so the JSON-RPC layer is
/// unit-testable without a live daemon.
#[async_trait::async_trait]
pub trait Backend: Send + Sync {
    async fn request(&self, req: ControlRequest) -> AetherResult<Value>;
}

/// Live backend: a single multiplexed control connection guarded by a mutex.
pub struct LiveBackend {
    client: tokio::sync::Mutex<ControlClient>,
}

impl LiveBackend {
    pub fn new(client: ControlClient) -> Self {
        Self {
            client: tokio::sync::Mutex::new(client),
        }
    }
}

#[async_trait::async_trait]
impl Backend for LiveBackend {
    async fn request(&self, req: ControlRequest) -> AetherResult<Value> {
        self.client.lock().await.request(req).await
    }
}

/// Server state shared across JSON-RPC calls.
pub struct McpState<B: Backend + 'static> {
    backend: B,
    allow_write: bool,
}

impl<B: Backend + 'static> McpState<B> {
    pub fn new(backend: B, allow_write: bool) -> Self {
        Self {
            backend,
            allow_write,
        }
    }
}

// ---------------------------------------------------------------------------
// Tool catalog
// ---------------------------------------------------------------------------

struct ToolDef {
    name: &'static str,
    description: &'static str,
    /// Lazily-built JSON schema (json! is not const-compatible).
    schema: fn() -> Value,
    write: bool,
}

static TOOLS: LazyLock<Vec<ToolDef>> = LazyLock::new(|| {
    vec![
        ToolDef {
            name: "aether_status",
            description: "Daemon health: version, counters, proxy/control addresses, flow count.",
            schema: || json!({"type": "object", "properties": {}}),
            write: false,
        },
        ToolDef {
            name: "aether_flows_list",
            description: "List captured flows (newest last) with host/method filters and pagination.",
            schema: || {
                json!({
                    "type": "object",
                    "properties": {
                        "limit": {"type": "integer", "description": "max flows to return (default 50)"},
                        "host": {"type": "string", "description": "exact-host filter"},
                        "method": {"type": "string", "description": "HTTP method filter"},
                        "before": {"type": "integer",
                            "description": "keyset pagination: flows older than this id"},
                        "after": {"type": "integer",
                            "description": "keyset pagination: flows newer than this id"}
                    }
                })
            },
            write: false,
        },
        ToolDef {
            name: "aether_flow_get",
            description: "One flow in full detail: headers, bodies, TLS facts.",
            schema: || {
                json!({
                    "type": "object",
                    "properties": {"id": {"type": "integer"}},
                    "required": ["id"]
                })
            },
            write: false,
        },
        ToolDef {
            name: "aether_flows_search",
            description: "Full-text search (FTS5) across captured flows.",
            schema: || {
                json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "limit": {"type": "integer"}
                    },
                    "required": ["query"]
                })
            },
            write: false,
        },
        ToolDef {
            name: "aether_intercept_list",
            description: "List flows currently held by intercept.",
            schema: || json!({"type": "object", "properties": {}}),
            write: false,
        },
        ToolDef {
            name: "aether_intercept_show",
            description: "Dump one held flow as JSON for review.",
            schema: || {
                json!({
                    "type": "object",
                    "properties": {"id": {"type": "integer"}},
                    "required": ["id"]
                })
            },
            write: false,
        },
        ToolDef {
            name: "aether_rules_list",
            description: "List live match & replace rules.",
            schema: || json!({"type": "object", "properties": {}}),
            write: false,
        },
        ToolDef {
            name: "aether_intercept_set",
            description: "Turn intercept hold on/off for new requests and/or responses.",
            schema: || {
                json!({
                    "type": "object",
                    "properties": {
                        "request": {"type": "boolean"},
                        "response": {"type": "boolean"}
                    }
                })
            },
            write: true,
        },
        ToolDef {
            name: "aether_intercept_drop",
            description: "Drop a held flow (client receives 502). Tagged decision, ADR-017.",
            schema: || {
                json!({
                    "type": "object",
                    "properties": {"id": {"type": "integer"}},
                    "required": ["id"]
                })
            },
            write: true,
        },
        ToolDef {
            name: "aether_rules_remove",
            description: "Remove a live rule by id.",
            schema: || {
                json!({
                    "type": "object",
                    "properties": {"id": {"type": "integer"}},
                    "required": ["id"]
                })
            },
            write: true,
        },
    ]
});

fn tools_json(allow_write: bool) -> Value {
    let list: Vec<Value> = TOOLS
        .iter()
        .filter(|t| allow_write || !t.write)
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": (t.schema)(),
            })
        })
        .collect();
    json!({ "tools": list })
}

// ---------------------------------------------------------------------------
// JSON-RPC layer
// ---------------------------------------------------------------------------

/// Handles one incoming JSON-RPC line. Returns `None` for notifications and
/// parse-error-free frames that need no reply; otherwise the reply line.
pub async fn handle_line<B: Backend + 'static>(state: &McpState<B>, line: &str) -> Option<String> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(
                json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {"code": -32700, "message": format!("parse error: {e}")}
                })
                .to_string(),
            );
        }
    };

    // Notifications (no id) never get a reply.
    let id = match msg.get("id") {
        Some(v) if !v.is_null() => v.clone(),
        _ => return None,
    };

    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(json!({}));

    let result = match method {
        "initialize" => Ok(json!({
            // MCP spec: echo the client's version only when we support it;
            // anything else gets OUR latest supported version (never a blind
            // echo of a version we cannot speak).
            "protocolVersion": params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .filter(|v| *v == PROTOCOL_VERSION)
                .unwrap_or(PROTOCOL_VERSION),
            "capabilities": {"tools": {}},
            "serverInfo": {
                "name": SERVER_NAME,
                "version": aether_daemon::VERSION
            }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_json(state.allow_write)),
        "tools/call" => tools_call(state, &params).await,
        other => Err(format!("method not found: {other}")),
    };

    Some(match result {
        Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}).to_string(),
        Err(e) => {
            let code = if method == "tools/call" {
                -32000
            } else {
                -32601
            };
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": code, "message": e}
            })
            .to_string()
        }
    })
}

/// MCP tool result shape: content[] + isError.
fn tool_text(text: String, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error
    })
}

fn arg_u64(args: &Value, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("argument '{key}' must be a non-negative integer")),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(format!("argument '{key}' must be a string")),
    }
}

/// Page-size ceiling for MCP tools: AI clients routinely ask for huge
/// limits ("give me everything") — clamp instead of truncating or erroring.
/// The control plane still owns its own default (50) when no limit is sent.
pub const MAX_PAGE_LIMIT: u64 = 200;

/// `limit` for paged tools: clamped to [`MAX_PAGE_LIMIT`], never truncated
/// and never a hard error — an AI client asking for 10^9 flows gets a page,
/// not a protocol failure.
fn clamp_limit(args: &Value) -> Result<Option<u32>, String> {
    Ok(arg_u64(args, "limit")?.map(|v| v.min(MAX_PAGE_LIMIT) as u32))
}

async fn tools_call<B: Backend + 'static>(
    state: &McpState<B>,
    params: &Value,
) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("tools/call requires a tool name")?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let def = TOOLS
        .iter()
        .find(|t| t.name == name)
        .ok_or_else(|| format!("unknown tool: {name}"))?;

    if def.write && !state.allow_write {
        return Ok(tool_text(
            format!(
                "tool '{name}' is a mutating tool and the server was started read-only — \
                 restart `aether mcp --allow-write` to enable it"
            ),
            true,
        ));
    }

    let req = build_request(name, &args)?;
    match state.backend.request(req).await {
        Ok(v) => Ok(tool_text(pretty(&v), false)),
        Err(e) => Ok(tool_text(e.to_string(), true)),
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

fn build_request(name: &str, args: &Value) -> Result<ControlRequest, String> {
    let req = match name {
        "aether_status" => ControlRequest::StatsGet,
        "aether_flows_list" => ControlRequest::FlowsList {
            limit: clamp_limit(args)?,
            before: arg_u64(args, "before")?,
            after: arg_u64(args, "after")?,
            host: arg_str(args, "host")?.map(str::to_string),
            method: arg_str(args, "method")?.map(str::to_string),
        },
        "aether_flow_get" => ControlRequest::FlowsGet {
            id: arg_u64(args, "id")?.ok_or("missing 'id'")?,
        },
        "aether_flows_search" => ControlRequest::FlowsSearch {
            query: arg_str(args, "query")?
                .ok_or("missing 'query'")?
                .to_string(),
            limit: clamp_limit(args)?,
        },
        "aether_intercept_list" => ControlRequest::InterceptList,
        "aether_intercept_show" => ControlRequest::InterceptShow {
            id: arg_u64(args, "id")?.ok_or("missing 'id'")?,
        },
        "aether_rules_list" => ControlRequest::RulesList,
        "aether_intercept_set" => ControlRequest::InterceptSet {
            request: args.get("request").and_then(Value::as_bool),
            response: args.get("response").and_then(Value::as_bool),
        },
        "aether_intercept_drop" => ControlRequest::InterceptDrop {
            id: arg_u64(args, "id")?.ok_or("missing 'id'")?,
        },
        "aether_rules_remove" => ControlRequest::RulesRemove {
            id: arg_u64(args, "id")?.ok_or("missing 'id'")?,
        },
        other => return Err(format!("unknown tool: {other}")),
    };
    Ok(req)
}

// ---------------------------------------------------------------------------
// stdio loop
// ---------------------------------------------------------------------------

/// Runs the JSON-RPC loop over arbitrary byte streams until EOF or a
/// shutdown signal. Split out from [`run_stdio`] so the loop itself is
/// unit-testable without a real terminal.
pub async fn run_lines<B, R, W>(
    state: &Arc<McpState<B>>,
    input: R,
    mut output: W,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> AetherResult<()>
where
    B: Backend + 'static,
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut lines = BufReader::new(input).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        if l.trim().is_empty() {
                            continue;
                        }
                        if let Some(reply) = handle_line(state, &l).await {
                            let io_err = |e: std::io::Error| {
                                aether_core::error::AetherError::network(format!(
                                    "stdio write: {e}"
                                ))
                            };
                            output.write_all(reply.as_bytes()).await.map_err(io_err)?;
                            output.write_all(b"\n").await.map_err(io_err)?;
                            output.flush().await.map_err(io_err)?;
                        }
                    }
                    Ok(None) => break, // EOF: client hung up
                    Err(e) => {
                        return Err(aether_core::error::AetherError::network(format!(
                            "stdin read: {e}"
                        )));
                    }
                }
            }
            // Shutdown wins even mid-read: without the select, a silent
            // stdin would hold the server hostage regardless of the flag.
            _ = shutdown.changed() => break,
        }
    }
    Ok(())
}

/// Runs the stdio server until EOF or shutdown (Ctrl-C).
pub async fn run_stdio<B: Backend + 'static>(
    state: Arc<McpState<B>>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> AetherResult<()> {
    run_lines(&state, tokio::io::stdin(), tokio::io::stdout(), shutdown).await
}

// ---------------------------------------------------------------------------
// Entry point (called from main.rs)
// ---------------------------------------------------------------------------

pub async fn run(
    control: Option<String>,
    auth: Option<String>,
    allow_write: bool,
) -> AetherResult<()> {
    let data_dir = aether_daemon::default_data_dir();
    let addr = crate::client::resolve_addr(control.as_deref(), &data_dir);
    let auth = crate::client::resolve_token(auth.as_deref(), &data_dir)?;

    let client = ControlClient::connect(&addr, &auth).await?;
    let state = Arc::new(McpState::new(LiveBackend::new(client), allow_write));

    eprintln!(
        "aether mcp: serving {} tools ({}) on stdio — control {}",
        TOOLS.iter().filter(|t| allow_write || !t.write).count(),
        if allow_write {
            "read+write"
        } else {
            "read-only"
        },
        addr
    );

    // Real shutdown wiring: Ctrl-C flips the watch channel, run_lines sees
    // it and exits the loop even while stdin is silent.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = shutdown_tx.send(true);
        }
    });

    run_stdio(state, shutdown_rx).await
}

// ---------------------------------------------------------------------------
// Tests — JSON-RPC semantics without a daemon
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeBackend {
        seen: Mutex<Vec<String>>,
        result: Value,
    }

    #[async_trait::async_trait]
    impl Backend for FakeBackend {
        async fn request(&self, req: ControlRequest) -> AetherResult<Value> {
            let name = match req {
                ControlRequest::StatsGet => "stats",
                ControlRequest::FlowsList { .. } => "flows_list",
                ControlRequest::RulesList => "rules_list",
                _ => "other",
            };
            self.seen.lock().unwrap().push(name.to_string());
            Ok(self.result.clone())
        }
    }

    async fn call<B: Backend + 'static>(state: &McpState<B>, line: &str) -> Option<String> {
        handle_line(state, line).await
    }

    #[tokio::test]
    async fn initialize_echoes_supported_protocol_version() {
        let st = McpState::new(
            FakeBackend {
                seen: Mutex::new(vec![]),
                result: json!({}),
            },
            false,
        );
        let reply = call(
            &st,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize",
                 "params":{"protocolVersion":"2024-11-05"}}"#,
        )
        .await
        .unwrap();
        let v: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(v["result"]["serverInfo"]["name"], "aether");
    }

    #[tokio::test]
    async fn initialize_downgrades_unsupported_protocol_version() {
        // MCP spec: never echo a version we cannot speak — answer with our
        // latest supported one and let the client decide.
        let st = McpState::new(
            FakeBackend {
                seen: Mutex::new(vec![]),
                result: json!({}),
            },
            false,
        );
        for requested in ["2099-01-01", "2025-06-18", "bogus"] {
            let reply = call(
                &st,
                &format!(
                    r#"{{"jsonrpc":"2.0","id":1,"method":"initialize",
                         "params":{{"protocolVersion":"{requested}"}}}}"#
                ),
            )
            .await
            .unwrap();
            let v: Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(
                v["result"]["protocolVersion"], PROTOCOL_VERSION,
                "requested {requested} must not be echoed"
            );
        }
    }

    #[tokio::test]
    async fn notifications_get_no_reply() {
        let st = McpState::new(
            FakeBackend {
                seen: Mutex::new(vec![]),
                result: json!({}),
            },
            false,
        );
        assert!(
            call(
                &st,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
            )
            .await
            .is_none()
        );
    }

    #[tokio::test]
    async fn read_only_hides_write_tools() {
        let ro = McpState::new(
            FakeBackend {
                seen: Mutex::new(vec![]),
                result: json!({}),
            },
            false,
        );
        let rw = McpState::new(
            FakeBackend {
                seen: Mutex::new(vec![]),
                result: json!({}),
            },
            true,
        );
        let ro_reply = call(&ro, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
            .await
            .unwrap();
        let ro_list: Value = serde_json::from_str(&ro_reply).unwrap();
        let rw_reply = call(&rw, r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#)
            .await
            .unwrap();
        let rw_list: Value = serde_json::from_str(&rw_reply).unwrap();
        let names = |v: &Value| -> Vec<String> {
            v["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect()
        };
        let ro_names = names(&ro_list);
        assert_eq!(ro_names.len(), 7);
        assert!(!ro_names.iter().any(|n| n == "aether_intercept_drop"));
        assert_eq!(names(&rw_list).len(), TOOLS.len());
    }

    #[tokio::test]
    async fn tools_call_maps_arguments() {
        let fake = FakeBackend {
            seen: Mutex::new(vec![]),
            result: json!({"flows": []}),
        };
        let st = McpState::new(fake, false);
        let reply = call(
            &st,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call",
                 "params":{"name":"aether_flows_list",
                 "arguments":{"limit":10,"host":"example.com"}}}"#,
        )
        .await
        .unwrap();
        let v: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["result"]["isError"], false);
        assert!(
            v["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("flows")
        );
    }

    #[tokio::test]
    async fn write_tool_blocked_when_read_only() {
        let st = McpState::new(
            FakeBackend {
                seen: Mutex::new(vec![]),
                result: json!({}),
            },
            false,
        );
        let reply = call(
            &st,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call",
                 "params":{"name":"aether_intercept_drop","arguments":{"id":7}}}"#,
        )
        .await
        .unwrap();
        let v: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["result"]["isError"], true);
        assert!(
            v["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("read-only")
        );
    }

    #[tokio::test]
    async fn backend_error_surfaces_as_tool_error_not_rpc_error() {
        struct Failing;
        #[async_trait::async_trait]
        impl Backend for Failing {
            async fn request(&self, _req: ControlRequest) -> AetherResult<Value> {
                Err(aether_core::error::AetherError::network(
                    "control plane closed",
                ))
            }
        }
        let st = McpState::new(Failing, false);
        let reply = call(
            &st,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call",
                 "params":{"name":"aether_status"}}"#,
        )
        .await
        .unwrap();
        let v: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["result"]["isError"], true);
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("control plane closed"));
    }

    #[tokio::test]
    async fn unknown_tool_is_rpc_error() {
        let st = McpState::new(
            FakeBackend {
                seen: Mutex::new(vec![]),
                result: json!({}),
            },
            false,
        );
        let reply = call(
            &st,
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"nope"}}"#,
        )
        .await
        .unwrap();
        let v: Value = serde_json::from_str(&reply).unwrap();
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("unknown tool")
        );
    }

    #[tokio::test]
    async fn bad_json_is_parse_error() {
        let st = McpState::new(
            FakeBackend {
                seen: Mutex::new(vec![]),
                result: json!({}),
            },
            false,
        );
        let reply = call(&st, "not json at all").await.unwrap();
        let v: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn huge_page_limit_is_clamped_not_truncated() {
        struct Capture;
        #[async_trait::async_trait]
        impl Backend for Capture {
            async fn request(&self, req: ControlRequest) -> AetherResult<Value> {
                let limit = match req {
                    ControlRequest::FlowsList { limit, .. } => limit,
                    _ => None,
                };
                Ok(json!({ "limit": limit }))
            }
        }
        let st = McpState::new(Capture, false);
        let reply = call(
            &st,
            r#"{"jsonrpc":"2.0","id":9,"method":"tools/call",
                 "params":{"name":"aether_flows_list",
                 "arguments":{"limit":18446744073709551615}}}"#,
        )
        .await
        .unwrap();
        let v: Value = serde_json::from_str(&reply).unwrap();
        // u64::MAX used to `as u32`-truncate to u32::MAX (4294967295); now it clamps.
        assert_eq!(
            v["result"]["content"][0]["text"].as_str().unwrap(),
            format!("{{\n  \"limit\": {}\n}}", MAX_PAGE_LIMIT)
        );
    }

    #[tokio::test]
    async fn shutdown_breaks_the_loop_while_stdin_is_silent() {
        use tokio::io::duplex;

        let st = Arc::new(McpState::new(
            FakeBackend {
                seen: Mutex::new(vec![]),
                result: json!({}),
            },
            false,
        ));
        let (_client, server_side) = duplex(4096);
        let (_out_tx, out_rx) = duplex(4096);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let server_st = st.clone();
        let server =
            tokio::spawn(
                async move { run_lines(&server_st, server_side, out_rx, shutdown_rx).await },
            );
        // The old run_stdio only checked its flag BETWEEN reads — a silent
        // stdin kept it hostage forever. The select! must win instead.
        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("run_lines ignored the shutdown signal")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let st = McpState::new(
            FakeBackend {
                seen: Mutex::new(vec![]),
                result: json!({}),
            },
            false,
        );
        let reply = call(&st, r#"{"jsonrpc":"2.0","id":8,"method":"resources/list"}"#)
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["error"]["code"], -32601);
    }
}

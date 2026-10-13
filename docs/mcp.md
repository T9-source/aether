# Aether MCP Server

`aether mcp` exposes a running Aether daemon to any
[Model Context Protocol](https://modelcontextprotocol.io) client over stdio:
Claude Desktop, VS Code (Copilot MCP), Cline, Zed, and any other MCP-capable
assistant.

Design invariants (see `docs/adr/`):

- **ADR-001**: the daemon owns all state — the MCP server is a thin control
  plane client. It caches nothing, holds nothing.
- **ADR-008**: read-only by default. Mutating tools (intercept set/drop,
  rules remove) exist only with the explicit `--allow-write` flag. The AI
  reads your traffic; it never touches it unless you hand it the keys.
- **Zero telemetry**: the server talks to your daemon and your stdio. Nothing
  else.

## Tools

Read-only (always available):

| Tool | Maps to |
|---|---|
| `aether_status` | daemon stats, version, addresses |
| `aether_flows_list` | keyset-paginated flow list (host/method filters) |
| `aether_flow_get` | full flow detail (headers, bodies, TLS facts) |
| `aether_flows_search` | FTS5 full-text search across history |
| `aether_intercept_list` | currently held flows |
| `aether_intercept_show` | one held flow as JSON |
| `aether_rules_list` | live match & replace rules |

Mutating (only with `--allow-write`):

| Tool | Maps to |
|---|---|
| `aether_intercept_set` | turn request/response hold on/off |
| `aether_intercept_drop` | drop a held flow (client gets 502) |
| `aether_rules_remove` | remove a live rule |

## Wiring it up

Prerequisite: a running daemon (`aether serve`). The MCP server resolves the
control-plane address and auth the same way the CLI does:
`--control/--auth` flags → `AETHER_CONTROL_ADDR`/`AETHER_TOKEN` env →
`~/.aether/` files → defaults.

Claude Desktop (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "aether": {
      "command": "/path/to/aether",
      "args": ["mcp"]
    }
  }
}
```

VS Code (`settings.json`):

```json
{
  "mcp": {
    "servers": {
      "aether": {
        "type": "stdio",
        "command": "/path/to/aether",
        "args": ["mcp"]
      }
    }
  }
}
```

## Transport

Newline-delimited JSON-RPC 2.0 on stdin/stdout (the MCP stdio transport).
One frame per line; notifications get no reply; tool errors are returned as
`isError: true` content results (the RPC layer stays clean), while protocol
errors use standard JSON-RPC error codes.

## Example session

```
> {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}
< {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"aether","version":"0.2.0"}}}

> {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"aether_flows_search","arguments":{"query":"login","limit":5}}}
< {"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{ …matching flows… }"}],"isError":false}}
```

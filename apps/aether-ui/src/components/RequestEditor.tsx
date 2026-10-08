// Structured + raw request/response editors — the operator's editing surface
// for Intercept (ADR-010 holds) and Replay (ADR-012). Both editors emit the
// same serializable domain shapes the daemon already owns; the Rust wire
// codec stays the authority (it recomputes framing honestly after edits).
import { useEffect, useState } from "react";
import type { Body, Headers, Method, Request, Response } from "../types";
import { bytesToBase64, methodToString, requestToRaw } from "../lib/httpText";

const COMMON_METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
const KNOWN = new Set(COMMON_METHODS.concat(["TRACE", "CONNECT"]).map((m) => m.toLowerCase()));

export function toMethod(m: string): Method {
  const lower = m.trim().toLowerCase();
  return KNOWN.has(lower) ? (lower as Method) : { custom: m.trim().toUpperCase() };
}

export function emptyHeaders(): Headers {
  return { entries: [] };
}

export function textBody(text: string): Body {
  return text.length === 0 ? { kind: "empty" } : { kind: "text", text };
}

// ── Request editor ────────────────────────────────────────────────────────

interface ReqProps {
  value: Request;
  onChange: (r: Request) => void;
}

export function RequestEditor(p: ReqProps) {
  const [mode, setMode] = useState<"structured" | "raw">("structured");
  const [raw, setRaw] = useState(() => requestToRaw(p.value));
  const [rawError, setRawError] = useState<string | null>(null);

  useEffect(() => {
    if (mode === "structured") setRaw(requestToRaw(p.value));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [p.value, mode]);

  const commitRaw = (text: string) => {
    setRaw(text);
    const parsed = parseRawRequest(text);
    if ("error" in parsed) {
      setRawError(parsed.error);
    } else {
      setRawError(null);
      p.onChange(parsed.request);
    }
  };

  return (
    <div className="editor">
      <div className="editor-toolbar">
        <button
          className={`tablet ${mode === "structured" ? "active" : ""}`}
          onClick={() => setMode("structured")}
        >
          structured
        </button>
        <button className={`tablet ${mode === "raw" ? "active" : ""}`} onClick={() => setMode("raw")}>
          raw HTTP
        </button>
      </div>

      {mode === "structured" ? (
        <>
          <div className="editor-line">
            <select
              value={COMMON_METHODS.includes(methodToString(p.value.method)) ? methodToString(p.value.method) : "CUSTOM"}
              onChange={(e) => {
                const m = e.target.value === "CUSTOM" ? "CUSTOM" : e.target.value;
                p.onChange({
                  ...p.value,
                  method: m === "CUSTOM" ? { custom: "GET" } : toMethod(m),
                });
              }}
              aria-label="Method"
            >
              {COMMON_METHODS.map((m) => (
                <option key={m}>{m}</option>
              ))}
              <option value="CUSTOM">custom…</option>
            </select>
            {methodToString(p.value.method) === "CUSTOM" ||
            !COMMON_METHODS.includes(methodToString(p.value.method)) ? (
              <input
                className="w-narrow mono"
                value={methodToString(p.value.method)}
                onChange={(e) => p.onChange({ ...p.value, method: toMethod(e.target.value) })}
                aria-label="Custom method"
              />
            ) : null}
            <select
              value={p.value.url.scheme}
              onChange={(e) => p.onChange({ ...p.value, url: { ...p.value.url, scheme: e.target.value } })}
              aria-label="Scheme"
            >
              <option value="http">http://</option>
              <option value="https">https://</option>
            </select>
            <input
              className="grow mono"
              value={p.value.url.host}
              placeholder="host"
              onChange={(e) => p.onChange({ ...p.value, url: { ...p.value.url, host: e.target.value } })}
              spellCheck={false}
              aria-label="Host"
            />
            <input
              className="w-port mono"
              value={p.value.url.port ?? ""}
              placeholder="port"
              onChange={(e) => {
                const v = e.target.value.trim();
                const port = v === "" ? null : Number(v);
                p.onChange({ ...p.value, url: { ...p.value.url, port: port !== null && !Number.isNaN(port) ? port : null } });
              }}
              spellCheck={false}
              aria-label="Port"
            />
          </div>
          <div className="editor-line">
            <span className="path-slash mono">/</span>
            <input
              className="grow mono"
              value={p.value.url.path.replace(/^\//, "")}
              placeholder="path"
              onChange={(e) => {
                const path = `/${e.target.value.replace(/^\//, "")}`;
                p.onChange({ ...p.value, url: { ...p.value.url, path } });
              }}
              spellCheck={false}
              aria-label="Path"
            />
            <span className="dim">?</span>
            <input
              className="grow mono"
              value={p.value.url.query ?? ""}
              placeholder="query (no leading ?)"
              onChange={(e) =>
                p.onChange({
                  ...p.value,
                  url: { ...p.value.url, query: e.target.value === "" ? null : e.target.value },
                })
              }
              spellCheck={false}
              aria-label="Query"
            />
          </div>

          <HeadersEditor headers={p.value.headers} onChange={(h) => p.onChange({ ...p.value, headers: h })} />
          <BodyEditor body={p.value.body} onChange={(b) => p.onChange({ ...p.value, body: b })} />
        </>
      ) : (
        <>
          <textarea
            className="raw-area mono"
            value={raw}
            onChange={(e) => commitRaw(e.target.value)}
            spellCheck={false}
            rows={16}
          />
          {rawError !== null && <div className="editor-error">{rawError}</div>}
        </>
      )}
    </div>
  );
}

/** Best-effort raw HTTP → Request (editing aid; the daemon re-validates). */
export function parseRawRequest(text: string): { request: Request } | { error: string } {
  const normalized = text.replace(/\r\n/g, "\n");
  const lines = normalized.split("\n");
  if (lines.length === 0 || lines[0].trim() === "") return { error: "empty request" };
  const m = /^([A-Z]+) (\S+) (HTTP\/[\d.]+)$/.exec(lines[0].trim());
  if (m === null) return { error: `bad request line: "${lines[0]}"` };

  const headers: Headers = { entries: [] };
  let i = 1;
  for (; i < lines.length; i++) {
    const line = lines[i];
    if (line === "") {
      i += 1;
      break;
    }
    const idx = line.indexOf(":");
    if (idx === -1) return { error: `bad header line: "${line}"` };
    headers.entries.push({ name: line.slice(0, idx).trim(), value: line.slice(idx + 1).trim() });
  }
  const bodyText = lines.slice(i).join("\n");

  const method = toMethod(m[1]);
  const target = m[2];
  const version = m[3] === "HTTP/1.0" ? "http10" : "http11";

  let scheme = "http";
  let host = "";
  let port: number | null = null;
  let path = "/";
  let query: string | null = null;

  const absolute = /^(https?):\/\/([^/\s]+)(\/.*)?$/.exec(target);
  if (absolute !== null) {
    scheme = absolute[1];
    const hp = absolute[2];
    const pm = /^(?:\[([^\]]+)\]|([^:]+))(?::(\d+))?$/.exec(hp);
    if (pm === null) return { error: `bad authority: "${hp}"` };
    host = pm[1] ?? pm[2] ?? "";
    port = pm[3] !== undefined ? Number(pm[3]) : null;
    const rest = absolute[3] ?? "/";
    const qi = rest.indexOf("?");
    path = qi === -1 ? rest : rest.slice(0, qi);
    query = qi === -1 ? null : rest.slice(qi + 1);
  } else if (target.startsWith("/")) {
    const qi = target.indexOf("?");
    path = qi === -1 ? target : target.slice(0, qi);
    query = qi === -1 ? null : target.slice(qi + 1);
    const h = headers.entries.find((x) => x.name.toLowerCase() === "host");
    if (h !== undefined) {
      const hp = h.value;
      const pm = /^(?:\[([^\]]+)\]|([^:\s]+))(?::(\d+))?$/.exec(hp);
      if (pm !== null) {
        host = pm[1] ?? pm[2] ?? "";
        port = pm[3] !== undefined ? Number(pm[3]) : null;
      }
    }
  } else {
    return { error: `unsupported request target: "${target}" (use origin-form or absolute-form)` };
  }

  if (host === "") return { error: "empty host (set Host header or use absolute-form)" };

  return {
    request: {
      method,
      version,
      url: { scheme, host, port, path, query, fragment: null },
      headers,
      body: textBody(bodyText),
    },
  };
}

// ── Response editor (intercept response holds) ───────────────────────────

interface ResProps {
  value: Response;
  onChange: (r: Response) => void;
}

export function ResponseEditor(p: ResProps) {
  return (
    <div className="editor">
      <div className="editor-line">
        <span className="dim">status</span>
        <input
          className="w-narrow mono"
          value={p.value.status.code}
          onChange={(e) => {
            const code = Number(e.target.value);
            if (!Number.isNaN(code) && code >= 100 && code <= 599) {
              p.onChange({ ...p.value, status: { code } });
            }
          }}
          aria-label="Status code"
        />
      </div>
      <HeadersEditor headers={p.value.headers} onChange={(h) => p.onChange({ ...p.value, headers: h })} />
      <BodyEditor body={p.value.body} onChange={(b) => p.onChange({ ...p.value, body: b })} />
    </div>
  );
}

// ── shared header/body editors ────────────────────────────────────────────

export function HeadersEditor(p: { headers: Headers; onChange: (h: Headers) => void }) {
  const rows = p.headers.entries;
  const set = (i: number, patch: { name?: string; value?: string }) => {
    const next = { entries: rows.map((e, j) => (j === i ? { ...e, ...patch } : e)) };
    p.onChange(next);
  };
  return (
    <div className="headers-edit">
      <div className="section-label">headers ({rows.length})</div>
      {rows.map((h, i) => (
        <div className="editor-line" key={i}>
          <input className="mono h-name" value={h.name} onChange={(e) => set(i, { name: e.target.value })} spellCheck={false} />
          <input className="mono grow" value={h.value} onChange={(e) => set(i, { value: e.target.value })} spellCheck={false} />
          <button
            className="btn ghost tiny"
            title="remove header"
            onClick={() => p.onChange({ entries: rows.filter((_, j) => j !== i) })}
          >
            ×
          </button>
        </div>
      ))}
      <button
        className="btn ghost tiny"
        onClick={() => p.onChange({ entries: [...rows, { name: "", value: "" }] })}
      >
        + add header
      </button>
    </div>
  );
}

export function BodyEditor(p: { body: Body; onChange: (b: Body) => void }) {
  const kind = p.body.kind;
  return (
    <div className="body-edit">
      <div className="section-label">
        body
        <span className="spacer" />
        <button className={`tablet ${kind === "empty" ? "active" : ""}`} onClick={() => p.onChange({ kind: "empty" })}>
          empty
        </button>
        <button className={`tablet ${kind === "text" ? "active" : ""}`} onClick={() => p.onChange({ kind: "text", text: kind === "text" ? p.body.text : "" })}>
          text
        </button>
        <button
          className={`tablet ${kind === "binary" ? "active" : ""}`}
          onClick={() => p.onChange({ kind: "binary", bytes: kind === "binary" ? p.body.bytes : "" })}
          title="base64"
        >
          binary (base64)
        </button>
      </div>
      {kind === "text" && (
        <textarea
          className="raw-area mono"
          value={p.body.text}
          onChange={(e) => p.onChange({ kind: "text", text: e.target.value })}
          spellCheck={false}
          rows={8}
        />
      )}
      {kind === "binary" && (
        <textarea
          className="raw-area mono"
          value={p.body.bytes}
          onChange={(e) => p.onChange({ kind: "binary", bytes: e.target.value })}
          spellCheck={false}
          rows={6}
          placeholder="base64-encoded payload"
        />
      )}
      {kind === "empty" && <div className="dim body-empty">(no body)</div>}
    </div>
  );
}

/** Convenience: build a Body from a UTF-8 string for binary round-trips. */
export function utf8ToBody(bytes: Uint8Array): Body {
  return bytes.length === 0 ? { kind: "empty" } : { kind: "binary", bytes: bytesToBase64(bytes) };
}

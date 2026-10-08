import { useEffect, useMemo, useState } from "react";
import type { ControlClient } from "../lib/ws";
import type { Body, Flow } from "../types";
import { bodyAsText, bodyBytes, methodToString, requestToCurl, requestToRaw, responseToRaw, toHexDump } from "../lib/httpText";
import { fmtBytes, fmtMs, methodClass, pathWithQuery, statusClass } from "../lib/format";

type BodyMode = "pretty" | "raw" | "hex";

interface Props {
  client: ControlClient;
  flowId: number | null;
  onReplay: (flow: Flow) => void;
}

/** Pretty-prints JSON when the body parses as JSON; renders text as-is. */
function prettyOrRaw(text: string): string {
  const t = text.trim();
  if (t.length === 0) return "";
  if (t.startsWith("{") || t.startsWith("[")) {
    try {
      return JSON.stringify(JSON.parse(t), null, 2);
    } catch {
      /* not JSON */
    }
  }
  return text;
}

function contentType(b: Body, headers: { name: string; value: string }[]): string {
  const ct = headers.find((h) => h.name.toLowerCase() === "content-type");
  return ct?.value ?? (b.kind === "binary" ? "application/octet-stream" : "text/plain");
}

export function FlowViewer(props: Props) {
  const [flow, setFlow] = useState<Flow | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState<"request" | "response">("request");
  const [reqMode, setReqMode] = useState<BodyMode>("pretty");
  const [resMode, setResMode] = useState<BodyMode>("pretty");
  const [rawWhole, setRawWhole] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);

  useEffect(() => {
    if (props.flowId === null) {
      setFlow(null);
      return;
    }
    let alive = true;
    setError(null);
    props.client
      .request<Flow>("flows_get", { id: props.flowId })
      .then((f) => {
        if (!alive) return;
        setFlow(f);
        setTab(f.response !== null ? "response" : "request");
      })
      .catch((e) => alive && setError(String(e.message ?? e)));
    return () => {
      alive = false;
    };
  }, [props.client, props.flowId]);

  const copy = (what: string, text: string) => {
    void navigator.clipboard?.writeText(text);
    setCopied(what);
    setTimeout(() => setCopied(null), 1200);
  };

  if (props.flowId === null) {
    return (
      <div className="viewer empty">
        <div className="empty-inner">
          <div className="empty-glyph">◈</div>
          <p>Select a flow to inspect it — headers, bodies, timing, TLS facts.</p>
        </div>
      </div>
    );
  }
  if (error !== null) {
    return <div className="viewer empty">Failed to load flow: {error}</div>;
  }
  if (flow === null) {
    return <div className="viewer empty">Loading…</div>;
  }

  const meta = flow.meta;
  const req = flow.request;
  const res = flow.response;

  return (
    <div className="viewer">
      <div className="viewer-head">
        <span className={`chip method ${methodClass(methodToString(req?.method ?? "get"))}`}>
          {methodToString(req?.method ?? { custom: "GET" })}
        </span>
        <span className="viewer-url mono">
          {req === null ? "(no request)" : `${req.url.scheme}://${req.url.host}${pathWithQuery(req.url.path, req.url.query)}`}
        </span>
        {res !== null && (
          <span className={`chip status ${statusClass(res.status.code)}`}>{res.status.code}</span>
        )}
        <span className="spacer" />
        {req !== null && (
          <>
            <button className="btn ghost" onClick={() => copy("curl", requestToCurl(req))}>
              {copied === "curl" ? "copied ✓" : "copy as cURL"}
            </button>
            <button className="btn accent" onClick={() => props.onReplay(flow)}>
              ↻ Replay
            </button>
          </>
        )}
      </div>

      <div className="viewer-meta">
        <span className="meta-chip">#{flow.id}</span>
        <span className="meta-chip">{meta.client_proto.toUpperCase()} → {meta.server_proto.toUpperCase()}</span>
        {meta.duration_ms !== null && <span className="meta-chip">{fmtMs(meta.duration_ms)}</span>}
        {req !== null && <span className="meta-chip">req {fmtBytes(req.body.kind === "empty" ? 0 : req.body.kind === "text" ? req.body.text.length : req.body.bytes.length)}</span>}
        {res !== null && <span className="meta-chip">res {fmtBytes(res.body.kind === "empty" ? 0 : res.body.kind === "text" ? res.body.text.length : res.body.bytes.length)}</span>}
        {meta.tls !== null && (
          <span className="meta-chip tls">
            TLS {meta.tls.version ?? "?"}
            {meta.tls.alpn !== null ? ` · alpn ${meta.tls.alpn}` : ""}
          </span>
        )}
        {meta.tags.map((t) => (
          <span key={t} className="meta-chip tag">{t}</span>
        ))}
      </div>

      <div className="viewer-tabs">
        <button className={`tab ${tab === "request" ? "active" : ""}`} onClick={() => setTab("request")}>
          Request
        </button>
        <button
          className={`tab ${tab === "response" ? "active" : ""}`}
          onClick={() => setTab("response")}
          disabled={res === null}
        >
          Response {res === null ? "(none)" : ""}
        </button>
        <span className="spacer" />
        <label className="raw-toggle">
          <input type="checkbox" checked={rawWhole} onChange={(e) => setRawWhole(e.target.checked)} />
          raw HTTP message
        </label>
      </div>

      {tab === "request" && req !== null && (
        <MessagePane
          kind="request"
          msg={req}
          mode={reqMode}
          setMode={setReqMode}
          rawWhole={rawWhole}
          rawText={requestToRaw(req)}
          onCopy={copy}
          copied={copied}
        />
      )}
      {tab === "response" && res !== null && (
        <MessagePane
          kind="response"
          msg={res}
          mode={resMode}
          setMode={setResMode}
          rawWhole={rawWhole}
          rawText={responseToRaw(res)}
          onCopy={copy}
          copied={copied}
        />
      )}
    </div>
  );
}

interface MsgProps<M extends { headers: { entries: { name: string; value: string }[] }; body: Body }> {
  kind: "request" | "response";
  msg: M;
  mode: BodyMode;
  setMode: (m: BodyMode) => void;
  rawWhole: boolean;
  rawText: string;
  onCopy: (what: string, text: string) => void;
  copied: string | null;
}

function MessagePane<M extends { headers: { entries: { name: string; value: string }[] }; body: Body }>(
  p: MsgProps<M>,
) {
  const body = p.msg.body;
  const ct = contentType(body, p.msg.headers.entries);
  const isJson = ct.includes("json");
  const text = useMemo(() => bodyAsText(body), [body]);
  const hex = useMemo(() => toHexDump(bodyBytes(body)), [body]);

  if (p.rawWhole) {
    return (
      <div className="pane">
        <div className="pane-toolbar">
          <span className="dim">wire view (headers + body as captured)</span>
          <button className="btn ghost" onClick={() => p.onCopy(`${p.kind}-raw`, p.rawText)}>
            {p.copied === `${p.kind}-raw` ? "copied ✓" : "copy"}
          </button>
        </div>
        <pre className="code">{p.rawText}</pre>
      </div>
    );
  }

  return (
    <div className="pane">
      <div className="section-label">headers ({p.msg.headers.entries.length})</div>
      <table className="headers">
        <tbody>
          {p.msg.headers.entries.map((h, i) => (
            <tr key={`${h.name}:${i}`}>
              <td className="h-name mono">{h.name}</td>
              <td className="h-value mono">{h.value}</td>
            </tr>
          ))}
        </tbody>
      </table>

      <div className="section-label">
        body
        <span className="dim"> · {ct}</span>
        <span className="spacer" />
        <button
          className={`tablet ${p.mode === "pretty" ? "active" : ""}`}
          onClick={() => p.setMode("pretty")}
        >
          pretty
        </button>
        <button className={`tablet ${p.mode === "raw" ? "active" : ""}`} onClick={() => p.setMode("raw")}>
          raw
        </button>
        <button className={`tablet ${p.mode === "hex" ? "active" : ""}`} onClick={() => p.setMode("hex")}>
          hex
        </button>
      </div>

      {body.kind === "empty" && <div className="dim body-empty">(empty body)</div>}

      {body.kind !== "empty" && p.mode === "hex" && <pre className="code hex">{hex}</pre>}

      {body.kind !== "empty" && p.mode !== "hex" && (
        <>
          {body.kind === "binary" && p.mode === "pretty" && (
            <div className="dim body-note">binary body — showing hex view (pretty is text-only)</div>
          )}
          <pre className={`code ${isJson ? "json" : ""}`}>
            {p.mode === "pretty"
              ? body.kind === "binary"
                ? toHexDump(bodyBytes(body))
                : prettyOrRaw(text)
              : text}
          </pre>
        </>
      )}
    </div>
  );
}

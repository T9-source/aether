import { useCallback, useEffect, useState } from "react";
import type { ControlClient } from "../lib/ws";
import type { Request } from "../types";
import { RequestEditor } from "./RequestEditor";
import { pathWithQuery } from "../lib/format";

interface Props {
  client: ControlClient;
  /** Prefill from a flow selected in the viewer ("↻ Replay" button). */
  seed: Request | null;
  /** Bumped by the panel after each send so History catches new flows. */
  onSent: (flowId: number) => void;
  onOpenInHistory: (flowId: number) => void;
}

interface ReplayRecord {
  flowId: number;
  at: string;
  target: string;
}

function cloneRequest(r: Request): Request {
  return JSON.parse(JSON.stringify(r)) as Request;
}

function blankRequest(): Request {
  return {
    method: "get",
    version: "http11",
    url: { scheme: "https", host: "example.com", port: null, path: "/", query: null, fragment: null },
    headers: {
      entries: [
        { name: "host", value: "example.com" },
        { name: "user-agent", value: "aether-replay/0.1" },
        { name: "accept", value: "*/*" },
      ],
    },
    body: { kind: "empty" },
  };
}

export function ReplayPanel(props: Props) {
  const [draft, setDraft] = useState<Request>(() => (props.seed !== null ? cloneRequest(props.seed) : blankRequest()));
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [recent, setRecent] = useState<ReplayRecord[]>([]);

  // new seed (from viewer) replaces the draft
  useEffect(() => {
    if (props.seed !== null) setDraft(cloneRequest(props.seed));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.seed]);

  const send = useCallback(async () => {
    setSending(true);
    setError(null);
    try {
      const r = await props.client.request<{ flow_id: number }>("flows_replay", { request: draft });
      const rec: ReplayRecord = {
        flowId: r.flow_id,
        at: new Date().toLocaleTimeString(),
        target: `${draft.url.scheme}://${draft.url.host}${pathWithQuery(draft.url.path, draft.url.query)}`,
      };
      setRecent((prev) => [rec, ...prev].slice(0, 25));
      props.onSent(r.flow_id);
    } catch (e) {
      setError(String((e as { message?: string }).message ?? e));
    } finally {
      setSending(false);
    }
  }, [draft, props]);

  return (
    <div className="replay">
      <div className="replay-toolbar">
        <div>
          <div className="section-label">replay — the operator's direct voice</div>
          <div className="dim">
            Runs upstream with <b>no client leg, no pipeline, no intercept</b>; recorded in
            history tagged <span className="chip tag">replay</span>. Edits are honest: framing
            (content-length, chunking) is recomputed by the wire codec, never trusted from the editor.
          </div>
        </div>
        <button className="btn accent" onClick={() => void send()} disabled={sending}>
          {sending ? "sending…" : "▶ Send request"}
        </button>
      </div>

      {error !== null && <div className="editor-error">{error}</div>}

      <RequestEditor value={draft} onChange={setDraft} />

      <div className="replay-recent">
        <div className="section-label">recent replays</div>
        {recent.length === 0 && <div className="dim" style={{ padding: "0 4px 8px" }}>Nothing sent yet from this panel.</div>}
        {recent.map((r) => (
          <div className="replay-row mono" key={r.flowId}>
            <span className="dim">{r.at}</span>
            <span className="chip method m-replay">#{r.flowId}</span>
            <span className="grow">{r.target}</span>
            <button className="btn ghost tiny" onClick={() => props.onOpenInHistory(r.flowId)}>
              view
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}

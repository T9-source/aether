import { useCallback, useEffect, useReducer, useState } from "react";
import type { ControlClient } from "../lib/ws";
import type { AetherEvent, DecisionDto, HeldRow, Request, Response } from "../types";
import { heldReducer, heldSummary } from "../state/held";
import { RequestEditor, ResponseEditor } from "./RequestEditor";

interface Props {
  client: ControlClient;
  refreshTick: number;
  onOpenInHistory: (flowId: number) => void;
}

interface ToggleState {
  request: boolean;
  response: boolean;
}

export function InterceptPanel(props: Props) {
  const [toggles, setToggles] = useState<ToggleState>({ request: false, response: false });
  const [held, dispatch] = useReducer(heldReducer, [] as HeldRow[]);
  const [selected, setSelected] = useState<{ flowId: number; direction: "request" | "response" } | null>(null);
  const [draft, setDraft] = useState<Request | null>(null);
  const [respDraft, setRespDraft] = useState<Response | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  // initial sync (and after reconnects / refresh ticks)
  useEffect(() => {
    let alive = true;
    props.client
      .request<{ request: boolean; response: boolean }>("intercept_set", {})
      .then((t) => alive && setToggles(t))
      .catch(() => {});
    props.client
      .request<HeldRow[]>("intercept_list")
      .then((rows) => alive && dispatch({ t: "set", rows }))
      .catch(() => {});
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.client, props.refreshTick]);

  // live queue
  useEffect(() => {
    const onEvent = (e: AetherEvent) => {
      if (e.type === "intercept_hold") {
        // payload details arrive via intercept_show; the row summary is enough here
        dispatch({
          t: "hold",
          row: {
            flow_id: Number(e.flow_id),
            direction: e.direction,
            method: null,
            host: null,
            path: null,
            status: null,
          },
        });
      } else if (e.type === "intercept_released") {
        dispatch({ t: "released", flow_id: Number(e.flow_id) });
        setSelected((s) => (s !== null && s.flowId === Number(e.flow_id) ? null : s));
      }
    };
    return props.client.onEvent(onEvent);
  }, [props.client]);

  // enrich unknown rows + load the selected payload
  useEffect(() => {
    const unknown = held.find((r) => r.method === null && r.host === null && r.status === null);
    if (unknown === undefined) return;
    let alive = true;
    props.client
      .request<HeldRow[]>("intercept_list")
      .then((rows) => alive && dispatch({ t: "set", rows }))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [held, props.client]);

  const select = useCallback(
    async (row: HeldRow) => {
      setSelected({ flowId: row.flow_id, direction: row.direction });
      setError(null);
      try {
        const payload = await props.client.request<{ request: Request } | { response: Response }>(
          "intercept_show",
          { id: row.flow_id },
        );
        if ("request" in payload) {
          setDraft(payload.request);
          setRespDraft(null);
        } else {
          setDraft(null);
          setRespDraft(payload.response);
        }
      } catch (e) {
        setError(String((e as { message?: string }).message ?? e));
      }
    },
    [props.client],
  );

  const setToggle = async (which: keyof ToggleState, on: boolean) => {
    try {
      const t = await props.client.request<{ request: boolean; response: boolean }>("intercept_set", {
        [which]: on,
      });
      setToggles(t);
    } catch (e) {
      setError(String((e as { message?: string }).message ?? e));
    }
  };

  const decide = async (action: "forward" | "drop") => {
    if (selected === null) return;
    let decision: DecisionDto;
    if (action === "drop") {
      decision = { action: "drop" };
    } else if (selected.direction === "request" && draft !== null) {
      decision = { action: "forward", payload: { request: draft } };
    } else if (selected.direction === "response" && respDraft !== null) {
      decision = { action: "forward", payload: { response: respDraft } };
    } else {
      // response forward: reload untouched payload and forward it
      try {
        const payload = await props.client.request<{ response: Response }>("intercept_show", {
          id: selected.flowId,
        });
        decision = { action: "forward", payload: { response: payload.response } };
      } catch (e) {
        setError(String((e as { message?: string }).message ?? e));
        return;
      }
    }
    try {
      await props.client.request("intercept_release", { id: selected.flowId, decision });
      setNote(
        action === "drop"
          ? `flow #${selected.flowId} dropped`
          : `flow #${selected.flowId} forwarded`,
      );
      setSelected(null);
      setDraft(null);
      setRespDraft(null);
      setTimeout(() => setNote(null), 2500);
    } catch (e) {
      setError(String((e as { message?: string }).message ?? e));
    }
  };

  const selectedRow = held.find(
    (r) => selected !== null && r.flow_id === selected.flowId && r.direction === selected.direction,
  );

  return (
    <div className="intercept">
      <div className="intercept-toolbar">
        <div className={`toggle-card ${toggles.request ? "on" : ""}`}>
          <div>
            <div className="toggle-title">Requests</div>
            <div className="dim">hold every request before it leaves</div>
          </div>
          <button
            className={`switch ${toggles.request ? "on" : ""}`}
            onClick={() => void setToggle("request", !toggles.request)}
            aria-label="toggle request intercept"
          >
            <span className="knob" />
          </button>
        </div>
        <div className={`toggle-card ${toggles.response ? "on" : ""}`}>
          <div>
            <div className="toggle-title">Responses</div>
            <div className="dim">hold every response before it returns</div>
          </div>
          <button
            className={`switch ${toggles.response ? "on" : ""}`}
            onClick={() => void setToggle("response", !toggles.response)}
            aria-label="toggle response intercept"
          >
            <span className="knob" />
          </button>
        </div>
        <div className="intercept-note">
          {note !== null && <span className="ok-note">{note}</span>}
          {error !== null && <span className="editor-error">{error}</span>}
        </div>
      </div>

      <div className="intercept-split">
        <div className="held-list">
          <div className="section-label">held queue ({held.length})</div>
          {held.length === 0 && (
            <div className="empty small">
              Nothing held.{" "}
              {toggles.request || toggles.response
                ? "Send traffic through the proxy — held exchanges appear here in real time."
                : "Turn on Requests or Responses above to start holding traffic."}
            </div>
          )}
          {held.map((r) => (
            <button
              key={`${r.flow_id}:${r.direction}`}
              className={`held-row mono ${selectedRow === r ? "selected" : ""}`}
              onClick={() => void select(r)}
            >
              {heldSummary(r)}
            </button>
          ))}
        </div>

        <div className="held-detail">
          {selected === null && <div className="empty small">Select a held exchange to edit it.</div>}
          {selected !== null && (
            <>
              <div className="section-label">
                editing held {selected.direction} · flow #{selected.flowId}
              </div>
              {selected.direction === "request" && draft !== null && (
                <RequestEditor value={draft} onChange={setDraft} />
              )}
              {selected.direction === "response" && respDraft !== null && (
                <ResponseEditor value={respDraft} onChange={setRespDraft} />
              )}
              <div className="editor-actions">
                <button
                  className="btn accent"
                  onClick={() => void decide("forward")}
                  disabled={
                    (selected.direction === "request" && draft === null) ||
                    (selected.direction === "response" && respDraft === null)
                  }
                >
                  ▶ Forward
                </button>
                <button className="btn danger" onClick={() => void decide("drop")}>
                  ✕ Drop
                </button>
                <button className="btn ghost" onClick={() => props.onOpenInHistory(selected.flowId)}>
                  view in history
                </button>
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

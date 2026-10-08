import { useEffect, useState } from "react";
import type { ControlClient } from "../lib/ws";
import type { RuleAction, RuleSpec } from "../types";

interface Props {
  client: ControlClient;
  refreshTick: number;
}

// Daemon shape: RuleEntry { id, #[serde(flatten)] spec } → the wire object is
// the flat RuleSpec plus the id (no nested "spec" key).
interface RuleRow extends RuleSpec {
  id: number;
}

const ACTIONS: { value: RuleAction["kind"]; label: string }[] = [
  { value: "set_request_header", label: "set request header" },
  { value: "remove_request_header", label: "remove request header" },
  { value: "set_response_header", label: "set response header" },
  { value: "remove_response_header", label: "remove response header" },
  { value: "replace_request_path", label: "rewrite request path (regex)" },
  { value: "replace_request_body", label: "rewrite request body (regex)" },
  { value: "replace_response_body", label: "rewrite response body (regex)" },
  { value: "add_tag", label: "add tag" },
  { value: "drop_flow", label: "drop flow" },
  { value: "respond_with", label: "mock response" },
];

function actionSummary(a: RuleAction): string {
  switch (a.kind) {
    case "set_request_header":
      return `${a.name}: ${a.value}`;
    case "remove_request_header":
      return a.name;
    case "set_response_header":
      return `${a.name}: ${a.value}`;
    case "remove_response_header":
      return a.name;
    case "replace_request_path":
      return `s/${a.pattern}/${a.replacement}/`;
    case "replace_request_body":
      return `s/${a.pattern}/${a.replacement}/`;
    case "replace_response_body":
      return `s/${a.pattern}/${a.replacement}/`;
    case "add_tag":
      return a.tag;
    case "drop_flow":
      return "abort exchange";
    case "respond_with":
      return `${a.status} ${a.content_type ?? ""} (${a.body.length} B)`;
  }
}

interface DraftState {
  name: string;
  stage: "request" | "response" | "both";
  hostInclude: string;
  hostExclude: string;
  pathPrefix: string;
  method: string;
  kind: RuleAction["kind"];
  name_: string;
  value_: string;
  pattern: string;
  replacement: string;
  tag: string;
  status: number;
  contentType: string;
  body: string;
}

const BLANK: DraftState = {
  name: "",
  stage: "request",
  hostInclude: "",
  hostExclude: "",
  pathPrefix: "",
  method: "",
  kind: "set_request_header",
  name_: "",
  value_: "",
  pattern: "",
  replacement: "",
  tag: "",
  status: 200,
  contentType: "application/json",
  body: "{}",
};

function draftToSpec(d: DraftState): RuleSpec {
  let action: RuleAction;
  switch (d.kind) {
    case "set_request_header":
    case "set_response_header":
      action = { kind: d.kind, name: d.name_, value: d.value_ };
      break;
    case "remove_request_header":
    case "remove_response_header":
      action = { kind: d.kind, name: d.name_ };
      break;
    case "replace_request_path":
    case "replace_request_body":
    case "replace_response_body":
      action = { kind: d.kind, pattern: d.pattern, replacement: d.replacement };
      break;
    case "add_tag":
      action = { kind: "add_tag", tag: d.tag };
      break;
    case "drop_flow":
      action = { kind: "drop_flow" };
      break;
    case "respond_with":
      action = {
        kind: "respond_with",
        status: d.status,
        content_type: d.contentType === "" ? null : d.contentType,
        body: d.body,
      };
  }
  const split = (s: string) =>
    s
      .split(",")
      .map((x) => x.trim())
      .filter((x) => x.length > 0);
  return {
    name: d.name.trim() === "" ? "unnamed rule" : d.name.trim(),
    enabled: true,
    stage: d.stage,
    host_include: split(d.hostInclude),
    host_exclude: split(d.hostExclude),
    path_prefix: d.pathPrefix.trim() === "" ? null : d.pathPrefix.trim(),
    method: d.method.trim() === "" ? null : d.method.trim(),
    action,
  };
}

export function RulesPanel(props: Props) {
  const [rules, setRules] = useState<RuleRow[]>([]);
  const [draft, setDraft] = useState<DraftState>(BLANK);
  const [showForm, setShowForm] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const reload = () => {
    props.client
      .request<RuleRow[]>("rules_list")
      .then(setRules)
      .catch((e) => setError(String(e.message ?? e)));
  };

  useEffect(() => {
    reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.client, props.refreshTick]);

  const add = async () => {
    setError(null);
    try {
      await props.client.request("rules_add", { spec: draftToSpec(draft) });
      setDraft(BLANK);
      setShowForm(false);
      reload();
    } catch (e) {
      setError(String((e as { message?: string }).message ?? e));
    }
  };

  const toggle = async (row: RuleRow) => {
    await props.client
      .request("rules_set_enabled", { id: row.id, enabled: !row.enabled })
      .catch((e) => setError(String(e.message ?? e)));
    reload();
  };

  const remove = async (row: RuleRow) => {
    await props.client
      .request("rules_remove", { id: row.id })
      .catch((e) => setError(String(e.message ?? e)));
    reload();
  };

  return (
    <div className="rules">
      <div className="rules-toolbar">
        <div>
          <div className="section-label">match &amp; replace — the serialized hook layer</div>
          <div className="dim">
            Rules are pure data (ADR-006): authored over the control plane, compiled into hooks
            by the daemon, persisted in <code>rules.json</code>. Same engine, zero recompiles.
          </div>
        </div>
        <button className="btn accent" onClick={() => setShowForm((s) => !s)}>
          {showForm ? "close" : "+ new rule"}
        </button>
      </div>

      {error !== null && <div className="editor-error">{error}</div>}

      {showForm && (
        <div className="rule-form">
          <div className="editor-line">
            <input
              className="grow mono"
              placeholder="rule name"
              value={draft.name}
              onChange={(e) => setDraft({ ...draft, name: e.target.value })}
              spellCheck={false}
            />
            <select value={draft.stage} onChange={(e) => setDraft({ ...draft, stage: e.target.value as DraftState["stage"] })}>
              <option value="request">stage: request</option>
              <option value="response">stage: response</option>
              <option value="both">stage: both</option>
            </select>
          </div>
          <div className="editor-line">
            <input
              className="grow mono"
              placeholder="host include globs (comma-sep, e.g. *.api.example.com)"
              value={draft.hostInclude}
              onChange={(e) => setDraft({ ...draft, hostInclude: e.target.value })}
              spellCheck={false}
            />
            <input
              className="grow mono"
              placeholder="host exclude globs"
              value={draft.hostExclude}
              onChange={(e) => setDraft({ ...draft, hostExclude: e.target.value })}
              spellCheck={false}
            />
          </div>
          <div className="editor-line">
            <input
              className="grow mono"
              placeholder="path prefix (e.g. /api/v2)"
              value={draft.pathPrefix}
              onChange={(e) => setDraft({ ...draft, pathPrefix: e.target.value })}
              spellCheck={false}
            />
            <input
              className="grow mono"
              placeholder="method (blank = any)"
              value={draft.method}
              onChange={(e) => setDraft({ ...draft, method: e.target.value })}
              spellCheck={false}
            />
            <select
              value={draft.kind}
              onChange={(e) => setDraft({ ...draft, kind: e.target.value as RuleAction["kind"] })}
            >
              {ACTIONS.map((a) => (
                <option key={a.value} value={a.value}>
                  {a.label}
                </option>
              ))}
            </select>
          </div>

          {(draft.kind === "set_request_header" ||
            draft.kind === "set_response_header" ||
            draft.kind === "remove_request_header" ||
            draft.kind === "remove_response_header") && (
            <div className="editor-line">
              <input className="mono" placeholder="header name" value={draft.name_} onChange={(e) => setDraft({ ...draft, name_: e.target.value })} spellCheck={false} />
              {(draft.kind === "set_request_header" || draft.kind === "set_response_header") && (
                <input className="mono grow" placeholder="header value" value={draft.value_} onChange={(e) => setDraft({ ...draft, value_: e.target.value })} spellCheck={false} />
              )}
            </div>
          )}

          {(draft.kind === "replace_request_path" ||
            draft.kind === "replace_request_body" ||
            draft.kind === "replace_response_body") && (
            <div className="editor-line">
              <input className="mono" placeholder="regex pattern" value={draft.pattern} onChange={(e) => setDraft({ ...draft, pattern: e.target.value })} spellCheck={false} />
              <input className="mono grow" placeholder="replacement" value={draft.replacement} onChange={(e) => setDraft({ ...draft, replacement: e.target.value })} spellCheck={false} />
            </div>
          )}

          {draft.kind === "add_tag" && (
            <div className="editor-line">
              <input className="mono" placeholder="tag" value={draft.tag} onChange={(e) => setDraft({ ...draft, tag: e.target.value })} spellCheck={false} />
            </div>
          )}

          {draft.kind === "respond_with" && (
            <div className="editor-line">
              <input className="mono w-narrow" placeholder="status" value={draft.status} onChange={(e) => setDraft({ ...draft, status: Number(e.target.value) || 200 })} spellCheck={false} />
              <input className="mono" placeholder="content-type" value={draft.contentType} onChange={(e) => setDraft({ ...draft, contentType: e.target.value })} spellCheck={false} />
              <textarea className="raw-area mono grow" rows={3} value={draft.body} onChange={(e) => setDraft({ ...draft, body: e.target.value })} spellCheck={false} />
            </div>
          )}

          <div className="rule-preview">
            <span className="dim">wire spec:</span>
            <code className="mono">{JSON.stringify(draftToSpec(draft))}</code>
          </div>

          <div className="editor-actions">
            <button className="btn accent" onClick={() => void add()}>
              add rule
            </button>
            <button className="btn ghost" onClick={() => setDraft(BLANK)}>
              reset
            </button>
          </div>
        </div>
      )}

      <div className="rules-list">
        {rules.length === 0 && <div className="empty small">No rules yet — add one to rewrite, tag, drop or mock traffic.</div>}
        {rules.map((r) => (
          <div className="rule-row" key={r.id}>
            <button
              className={`switch small ${r.enabled ? "on" : ""}`}
              onClick={() => void toggle(r)}
              aria-label="toggle rule"
            >
              <span className="knob" />
            </button>
            <span className="rule-id dim">#{r.id}</span>
            <span className="rule-name">{r.name}</span>
            <span className="chip">{r.stage}</span>
            <span className="dim rule-matcher">
              {[r.host_include.join(", "), r.path_prefix ?? "", r.method ?? ""]
                .filter((x) => x !== "")
                .join(" · ") || "any traffic"}
            </span>
            <span className="rule-action mono">{r.action.kind}: {actionSummary(r.action)}</span>
            <button className="btn ghost tiny" onClick={() => void remove(r)}>
              delete
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}

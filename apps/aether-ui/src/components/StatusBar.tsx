import { useEffect, useState } from "react";
import type { ControlClient } from "../lib/ws";
import { fmtUptime } from "../lib/format";
import type { AetherEvent, StatsInfo } from "../types";

interface Props {
  client: ControlClient;
  stats: StatsInfo | null;
  ticker: AetherEvent[];
  proxyAddr: string | null;
}

function describe(e: AetherEvent): string {
  switch (e.type) {
    case "flow_started":
      return `flow #${e.flow_id} started`;
    case "request_received":
      return `#${e.flow_id} request received`;
    case "request_modified":
      return `#${e.flow_id} request modified by ${e.by}`;
    case "request_forwarded":
      return `#${e.flow_id} request forwarded`;
    case "response_received":
      return `#${e.flow_id} response ${e.status}`;
    case "response_forwarded":
      return `#${e.flow_id} response forwarded`;
    case "flow_completed":
      return `#${e.flow_id} completed`;
    case "flow_dropped":
      return `#${e.flow_id} dropped (${e.reason})`;
    case "intercept_hold":
      return `#${e.flow_id} held (${e.direction})`;
    case "intercept_released":
      return `#${e.flow_id} released (${e.decision})`;
    case "pass_through":
      return `pass-through: ${e.authority}`;
    case "error_occurred":
      return `error: ${e.message}`;
    case "hook_executed":
      return `hook ${e.hook} → ${e.duration_ms}ms`;
    case "finding_created":
      return `finding: ${e.title}`;
    default:
      return "";
  }
}

export function StatusBar(props: Props) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);

  const last = props.ticker[0];
  const uptime = props.stats !== null ? fmtUptime(props.stats.uptime_ms + (now % 1000)) : "—";

  const exportCa = async () => {
    try {
      const r = await props.client.request<{ pem: string }>("ca_export_pem");
      const blob = new Blob([r.pem], { type: "application/x-pem-file" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = "aether-ca.pem";
      a.click();
      URL.revokeObjectURL(url);
    } catch {
      /* status bar stays quiet; export is a convenience */
    }
  };

  return (
    <div className="statusbar">
      <span className="sb-item">
        <span className="sb-label">proxy</span>
        <span className="mono">{props.proxyAddr ?? "—"}</span>
        <span className="dim"> (set as HTTP(S) proxy in your client)</span>
      </span>
      <span className="sb-sep" />
      <span className="sb-item">
        <span className="sb-label">flows</span>
        {props.stats === null ? "—" : `${props.stats.flows.total.toLocaleString()} total · ${props.stats.flows.completed.toLocaleString()} done · ${props.stats.flows.dropped.toLocaleString()} dropped`}
      </span>
      <span className="sb-sep" />
      <span className="sb-item">
        <span className="sb-label">held</span>
        {props.stats?.held ?? 0}
      </span>
      <span className="sb-sep" />
      <span className="sb-item">
        <span className="sb-label">rules</span>
        {props.stats?.rules ?? 0}
      </span>
      <span className="sb-sep" />
      <span className="sb-item">
        <span className="sb-label">uptime</span>
        {uptime}
      </span>
      <span className="sb-sep" />
      <span className="sb-item ticker mono dim" title="latest daemon event">
        {last !== undefined ? `⚡ ${describe(last)}` : "no events yet"}
      </span>
      <span className="spacer" />
      <button className="btn ghost tiny" onClick={() => void exportCa()} title="download the MITM root CA (install it in your browser/OS to trust intercepted TLS)">
        ⤓ CA certificate
      </button>
      <span className="sb-item dim mono">{props.stats !== null ? `v${props.stats.version}` : ""}</span>
    </div>
  );
}

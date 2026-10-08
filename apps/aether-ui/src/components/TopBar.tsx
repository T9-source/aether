import type { ConnStatus } from "../lib/ws";

interface Props {
  status: ConnStatus;
  detail?: string;
  version: string;
  onDisconnect: () => void;
}

const STATUS_TEXT: Record<ConnStatus, string> = {
  idle: "offline",
  connecting: "connecting…",
  connected: "connected",
  reconnecting: "reconnecting…",
  "auth-failed": "auth failed",
  closed: "offline",
};

export function TopBar(props: Props) {
  const dot =
    props.status === "connected"
      ? "ok"
      : props.status === "auth-failed"
        ? "err"
        : "warn";
  return (
    <div className="topbar">
      <div className="brand">
        <svg viewBox="0 0 32 32" width="22" height="22" aria-hidden>
          <defs>
            <linearGradient id="tg" x1="0" y1="0" x2="1" y2="1">
              <stop offset="0" stopColor="#4cc9f0" />
              <stop offset="1" stopColor="#b58cff" />
            </linearGradient>
          </defs>
          <rect width="32" height="32" rx="7" fill="#0b0e14" />
          <path d="M16 5 L26 16 L16 27 L6 16 Z" fill="none" stroke="url(#tg)" strokeWidth="2.4" strokeLinejoin="round" />
          <circle cx="16" cy="16" r="3.2" fill="url(#tg)" />
        </svg>
        <span className="brand-name">aether</span>
        {props.version !== "" && <span className="brand-version dim">v{props.version}</span>}
      </div>
      <span className="spacer" />
      <div className={`conn ${dot}`} title={props.detail ?? ""}>
        <span className="conn-dot" />
        {STATUS_TEXT[props.status]}
      </div>
      <button className="btn ghost tiny" onClick={props.onDisconnect}>
        disconnect
      </button>
    </div>
  );
}

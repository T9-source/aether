import { useState } from "react";
import { CaWizard } from "./CaWizard";

export interface ConnectProps {
  initialUrl: string;
  /** daemon auth value, prefilled from a deep link or a saved session */
  initialAuth: string;
  busy: boolean;
  error: string | null;
  /** set when this connect attempt came from a saved session / deep link */
  autoNote: string | null;
  /** called with (controlWsUrl, auth) on submit */
  onConnect: (url: string, auth: string) => void;
}

/**
 * First-run screen: point the console at a local daemon and present the
 * daemon auth value. The one-click path (`aether serve` prints a deep link
 * containing both values) lands here pre-filled and auto-connects —
 * see lib/conn.ts.
 */
export function Connect(props: ConnectProps) {
  const [url, setUrl] = useState(props.initialUrl);
  const [auth, setAuth] = useState(props.initialAuth);
  const [showCa, setShowCa] = useState(false);

  return (
    <div className="connect-wrap">
      <div className="connect-card">
        <div className="connect-logo" aria-hidden>
          <svg viewBox="0 0 32 32" width="52" height="52">
            <defs>
              <linearGradient id="lg" x1="0" y1="0" x2="1" y2="1">
                <stop offset="0" stopColor="#4cc9f0" />
                <stop offset="1" stopColor="#b58cff" />
              </linearGradient>
            </defs>
            <rect width="32" height="32" rx="7" fill="#0b0e14" />
            <path
              d="M16 5 L26 16 L16 27 L6 16 Z"
              fill="none"
              stroke="url(#lg)"
              strokeWidth="2.4"
              strokeLinejoin="round"
            />
            <circle cx="16" cy="16" r="3.2" fill="url(#lg)" />
          </svg>
        </div>
        <h1>Aether Console</h1>
        <p className="connect-sub">
          local-first · developer-first · AI-native API security proxy
        </p>

        {props.autoNote !== null && (
          <div className="connect-auto" data-testid="auto-note">
            {props.autoNote}
          </div>
        )}

        <form
          onSubmit={(e) => {
            e.preventDefault();
            props.onConnect(url.trim(), auth.trim());
          }}
        >
          <label className="field">
            <span>Daemon control plane</span>
            <input
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="ws://127.0.0.1:9099"
              spellCheck={false}
              autoFocus
            />
          </label>
          <label className="field">
            <span>Auth token</span>
            <input
              value={auth}
              onChange={(e) => setAuth(e.target.value)}
              placeholder="paste the daemon auth value from `aether serve`"
              spellCheck={false}
              type="password"
            />
          </label>
          {props.error !== null && <div className="connect-error">{props.error}</div>}
          <button className="btn primary wide" disabled={props.busy || auth.trim() === ""}>
            {props.busy ? "Connecting…" : "Connect"}
          </button>
        </form>

        <button className="linklike" onClick={() => setShowCa(!showCa)} data-testid="ca-toggle">
          {showCa ? "▾" : "▸"} HTTPS setup (CA) — 60 seconds
        </button>
        {showCa && <CaWizard caPath={null} onDone={() => setShowCa(false)} />}

        <div className="connect-help">
          <h3>Where is the auth value?</h3>
          <ul>
            <li>
              printed by <code>aether serve</code> at startup — or open the
              <b> one-click console link </b> it prints, and you're already connected;
            </li>
            <li>
              stored in <code>~/.aether/token</code> (0600, daemon-owned).
            </li>
          </ul>
          <p>
            The console is a thin client: history, intercept state and rules live in the
            daemon (ADR-001) — closing this tab never touches a running capture.
          </p>
        </div>
      </div>
    </div>
  );
}

import { useState } from "react";

export interface ConnectProps {
  initialUrl: string;
  initialToken: string;
  busy: boolean;
  error: string | null;
  onConnect: (url: string, token: string) => void;
}

/** First-run screen: point the console at a local daemon and present the token. */
export function Connect(props: ConnectProps) {
  const [url, setUrl] = useState(props.initialUrl);
  const [token, setToken] = useState(props.initialToken);

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

        <form
          onSubmit={(e) => {
            e.preventDefault();
            props.onConnect(url.trim(), token.trim());
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
              value={token}
              onChange={(e) => setToken(e.target.value)}
              placeholder="paste the token from `aether serve`"
              spellCheck={false}
              type="password"
            />
          </label>
          {props.error !== null && <div className="connect-error">{props.error}</div>}
          <button className="btn primary wide" disabled={props.busy || token.trim() === ""}>
            {props.busy ? "Connecting…" : "Connect"}
          </button>
        </form>

        <div className="connect-help">
          <h3>Where is the token?</h3>
          <ul>
            <li>
              printed by <code>aether serve</code> at startup, and
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

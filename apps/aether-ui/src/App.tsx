import { useCallback, useEffect, useRef, useState } from "react";
import { Connect } from "./components/Connect";
import { TopBar } from "./components/TopBar";
import { HistoryTable, type HistoryHandle } from "./components/HistoryTable";
import { FlowViewer } from "./components/FlowViewer";
import { InterceptPanel } from "./components/InterceptPanel";
import { ReplayPanel } from "./components/ReplayPanel";
import { RulesPanel } from "./components/RulesPanel";
import { StatusBar } from "./components/StatusBar";
import { useDaemon } from "./hooks/useDaemon";
import { ControlClient } from "./lib/ws";
import type { Request } from "./types";

type View = "history" | "intercept" | "replay" | "rules";

const LS_URL = "aether.console.url";
const LS_TOKEN = "aether.console.token";

function initialConn(): { url: string; token: string; auto: boolean } {
  const q = new URLSearchParams(window.location.search);
  const url = q.get("ws") ?? localStorage.getItem(LS_URL) ?? "ws://127.0.0.1:9099";
  const token = q.get("token") ?? localStorage.getItem(LS_TOKEN) ?? "";
  return { url, token, auto: q.get("ws") !== null && q.get("token") !== null };
}

export default function App() {
  const init = useRef(initialConn()).current;
  const [client, setClient] = useState<ControlClient | null>(null);
  const [token] = useState(init.token);
  const [busy, setBusy] = useState(false);
  const [connError, setConnError] = useState<string | null>(null);

  const [view, setView] = useState<View>("history");
  const [flowId, setFlowId] = useState<number | null>(null);
  const [replaySeed, setReplaySeed] = useState<Request | null>(null);
  const historyHandle = useRef<HistoryHandle | null>(null);

  const daemon = useDaemon(client);

  const doConnect = useCallback(
    (u: string, t: string) => {
      setBusy(true);
      setConnError(null);
      const c = new ControlClient(u, t);
      setClient(c); // wire up status/events immediately (useDaemon watches it)
      c.connect()
        .then(() => {
          localStorage.setItem(LS_URL, u);
          localStorage.setItem(LS_TOKEN, t);
          setBusy(false);
        })
        .catch((e) => {
          setBusy(false);
          setConnError(String(e?.message ?? "connection failed"));
          setClient(null);
        });
    },
    [],
  );

  // autoconnect when both params are present (automation / demo mode)
  useEffect(() => {
    if (init.auto && init.token !== "") doConnect(init.url, init.token);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const disconnect = () => {
    client?.close();
    setClient(null);
    setFlowId(null);
  };

  if (client === null) {
    return (
      <Connect
        initialUrl={init.url}
        initialToken={token}
        busy={busy}
        error={connError}
        onConnect={doConnect}
      />
    );
  }

  const registerHandle = (h: HistoryHandle) => {
    historyHandle.current = h;
  };

  const openInHistory = (id: number) => {
    setView("history");
    setFlowId(id);
    // let the view mount/refresh before revealing
    setTimeout(() => {
      if (!historyHandle.current?.reveal(id)) setFlowId(id);
    }, 60);
  };

  const nav: { id: View; label: string; badge?: number }[] = [
    { id: "history", label: "History" },
    { id: "intercept", label: "Intercept", badge: daemon.stats?.held ?? 0 },
    { id: "replay", label: "Replay" },
    { id: "rules", label: "Rules" },
  ];

  return (
    <div className="app">
      <TopBar
        status={daemon.status}
        detail={daemon.statusDetail}
        version={daemon.version}
        onDisconnect={disconnect}
      />
      <div className="body">
        <nav className="nav">
          {nav.map((n) => (
            <button
              key={n.id}
              className={`nav-item ${view === n.id ? "active" : ""}`}
              onClick={() => setView(n.id)}
            >
              {n.label}
              {n.badge !== undefined && n.badge > 0 && <span className="badge">{n.badge}</span>}
            </button>
          ))}
        </nav>

        <main className="main">
          {view === "history" && (
            <div className="history-split">
              <HistoryTable
                client={client}
                refreshTick={daemon.refreshTick}
                selectedId={flowId}
                onSelect={setFlowId}
                registerHandle={registerHandle}
              />
              <FlowViewer
                client={client}
                flowId={flowId}
                onReplay={(flow) => {
                  if (flow.request !== null) {
                    setReplaySeed(flow.request);
                    setView("replay");
                  }
                }}
              />
            </div>
          )}
          {view === "intercept" && (
            <InterceptPanel
              client={client}
              refreshTick={daemon.refreshTick}
              onOpenInHistory={openInHistory}
            />
          )}
          {view === "replay" && (
            <ReplayPanel
              client={client}
              seed={replaySeed}
              onSent={() => daemon.bumpRefresh()}
              onOpenInHistory={openInHistory}
            />
          )}
          {view === "rules" && <RulesPanel client={client} refreshTick={daemon.refreshTick} />}
        </main>
      </div>
      <StatusBar client={client} stats={daemon.stats} ticker={daemon.ticker} proxyAddr={daemon.stats?.proxy_addr ?? null} />
    </div>
  );
}

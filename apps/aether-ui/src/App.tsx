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
import { initialConn as resolveConn, scrubCredentials, type ConnInit } from "./lib/conn";
import type { Request } from "./types";

type View = "history" | "intercept" | "replay" | "rules";

// Audit LOW: the daemon auth value moves to sessionStorage — it no longer
// survives the browser session (a shared machine, an XSS, a sync extension).
// The URL may stay in localStorage (it is not a secret); any saved auth from
// the localStorage era is honored once, then rewritten into sessionStorage.
const connStore = {
  getUrl: () => sessionStorage.getItem("aether.console.url") ?? localStorage.getItem("aether.console.url"),
  getAuth: () =>
    sessionStorage.getItem("aether.console.auth") ??
    localStorage.getItem("aether.console.auth") ??
    localStorage.getItem("aether.console.token"),
};

function resolveInitialConn(): ConnInit {
  return resolveConn(window.location.search, connStore);
}

export default function App() {
  const init = useRef(resolveInitialConn()).current;
  const [client, setClient] = useState<ControlClient | null>(null);
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
          sessionStorage.setItem("aether.console.url", u);
          sessionStorage.setItem("aether.console.auth", t);
          // wipe any pre-migration secret left in localStorage
          localStorage.removeItem("aether.console.auth");
          localStorage.removeItem("aether.console.token");
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

  // one-click reconnect: deep link or a previously saved session
  useEffect(() => {
    if (init.auto) {
      const cleaned = scrubCredentials(window.location.search);
      window.history.replaceState(null, "", window.location.pathname + cleaned);
      doConnect(init.url, init.auth);
    }
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
        initialAuth={init.auth}
        busy={busy}
        error={connError}
        autoNote={init.auto ? "Reconnecting to the daemon from your last session…" : null}
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

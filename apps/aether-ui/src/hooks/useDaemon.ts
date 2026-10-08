// App-level daemon lifecycle: connection status, stats polling, live event
// feed, and a `refreshTick` that views can watch to reload after reconnects.
import { useCallback, useEffect, useRef, useState } from "react";
import { ControlClient, type ConnStatus } from "../lib/ws";
import type { AetherEvent, StatsInfo } from "../types";

const STATS_POLL_MS = 5000;
const TICKER_EVENTS = 60;

export function useDaemon(client: ControlClient | null) {
  const [status, setStatus] = useState<ConnStatus>("idle");
  const [statusDetail, setStatusDetail] = useState<string | undefined>(undefined);
  const [version, setVersion] = useState<string>("");
  const [stats, setStats] = useState<StatsInfo | null>(null);
  const [ticker, setTicker] = useState<AetherEvent[]>([]);
  const [refreshTick, setRefreshTick] = useState(0);
  const clientRef = useRef<ControlClient | null>(client);
  clientRef.current = client;

  // status subscription
  useEffect(() => {
    if (!client) return;
    return client.onStatus((s, detail) => {
      setStatus(s);
      setStatusDetail(detail);
      if (s === "connected" && client.hello) {
        setVersion(client.hello.version);
        setRefreshTick((t) => t + 1);
      }
    });
  }, [client]);

  // event subscription (live feed)
  useEffect(() => {
    if (!client) return;
    return client.onEvent((e) => {
      setTicker((prev) => {
        const next = [e, ...prev];
        next.length = Math.min(next.length, TICKER_EVENTS);
        return next;
      });
    });
  }, [client]);

  // stats polling + immediate refresh after events that change counters
  useEffect(() => {
    if (!client || status !== "connected") return;
    let alive = true;
    const pull = () => {
      clientRef.current
        ?.request<StatsInfo>("stats_get")
        .then((s) => {
          if (alive) setStats(s);
        })
        .catch(() => {
          /* status bar shows connectivity via `status` */
        });
    };
    pull();
    const id = setInterval(pull, STATS_POLL_MS);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, [client, status, refreshTick]);

  const request = useCallback(
    <T,>(op: string, params: Record<string, unknown> = {}): Promise<T> => {
      const c = clientRef.current;
      if (!c) return Promise.reject({ kind: "offline", message: "not connected" });
      return c.request<T>(op, params);
    },
    [],
  );

  return {
    status,
    statusDetail,
    version,
    stats,
    ticker,
    refreshTick,
    request,
    bumpRefresh: () => setRefreshTick((t) => t + 1),
  };
}

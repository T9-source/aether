import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ControlClient } from "../lib/ws";
import type { AetherEvent, FlowSummary, FlowPage } from "../types";
import { fmtBytes, fmtClock, fmtMs, methodClass, pathWithQuery, statusClass } from "../lib/format";
import { useVirtual } from "../hooks/useVirtual";

const ROW_H = 30;
const PAGE = 200;

export interface HistoryHandle {
  /** Select + reveal a flow (used by Replay results). Returns false when not loaded. */
  reveal: (id: number) => boolean;
}

interface Props {
  client: ControlClient;
  refreshTick: number;
  selectedId: number | null;
  onSelect: (id: number) => void;
  registerHandle: (h: HistoryHandle) => void;
}

const METHODS = ["", "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/** The UI-side twin of Rust's `FlowSummary::is_dropped` (tags carry the verdict). */
function isDropped(r: FlowSummary): boolean {
  return r.tags.some((t) => t.toLowerCase() === "dropped");
}

export function HistoryTable(props: Props) {
  const { client, selectedId, onSelect } = props;
  const [rows, setRows] = useState<FlowSummary[]>([]);
  const [filters, setFilters] = useState({ method: "", host: "" });
  const [search, setSearch] = useState("");
  const [searchLive, setSearchLive] = useState(""); // debounced
  const [newCount, setNewCount] = useState(0);
  const [cursor, setCursor] = useState<number | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const rowsRef = useRef(rows);
  rowsRef.current = rows;
  const newestIdRef = useRef<number | null>(rows.length > 0 ? rows[0].id : null);
  newestIdRef.current = rows.length > 0 ? rows[0].id : null;
  const atTopRef = useRef(true);
  const cursorRef = useRef(cursor);
  cursorRef.current = cursor;
  const searchLiveRef = useRef(searchLive);
  searchLiveRef.current = searchLive;

  /**
   * Live-prepend anchor: remember (rowId, oldIndex) before newer rows are
   * inserted above it; after commit, re-anchor so the viewport never jumps.
   */
  const anchorRef = useRef<{ id: number; oldIndex: number } | null>(null);
  useLayoutEffect(() => {
    const a = anchorRef.current;
    if (a === null) return;
    anchorRef.current = null;
    const newIdx = rowsRef.current.findIndex((r) => r.id === a.id);
    if (newIdx !== -1 && newIdx !== a.oldIndex) {
      const el = scrollRef.current;
      if (el) el.scrollTop += (newIdx - a.oldIndex) * ROW_H;
    }
  }, [rows]);

  const { scrollRef, onScroll, range, scrollToRow } = useVirtual(rows.length, ROW_H, 10);

  const isSearch = searchLive.trim().length > 0;

  // ── data loading ──────────────────────────────────────────────────────
  const loadFirstPage = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      if (searchLiveRef.current.trim()) {
        const hits = await client.request<FlowSummary[]>("flows_search", {
          query: searchLiveRef.current.trim(),
          limit: 300,
        });
        setRows(hits);
        setCursor(null);
      } else {
        const page = await client.request<FlowPage>("flows_list", {
          limit: PAGE,
          method: filters.method || undefined,
          host: filters.host || undefined,
        });
        setRows(page.items);
        setCursor(page.next_cursor ?? null);
      }
      setNewCount(0);
    } catch (e) {
      setError(String((e as { message?: string }).message ?? e));
    } finally {
      setLoading(false);
    }
  }, [client, filters]);

  useEffect(() => {
    void loadFirstPage();
  }, [loadFirstPage, props.refreshTick]);

  // debounced search box → searchLive
  useEffect(() => {
    const id = setTimeout(() => setSearchLive(search), 250);
    return () => clearTimeout(id);
  }, [search]);

  const loadOlder = useCallback(async () => {
    const c = cursorRef.current;
    if (c === null || loading || searchLiveRef.current.trim()) return;
    setLoading(true);
    try {
      const page = await client.request<FlowPage>("flows_list", {
        limit: PAGE,
        before: c,
        method: filters.method || undefined,
        host: filters.host || undefined,
      });
      setRows((prev) => [...prev, ...page.items]);
      setCursor(page.next_cursor ?? null);
    } catch (e) {
      setError(String((e as { message?: string }).message ?? e));
    } finally {
      setLoading(false);
    }
  }, [client, filters, loading]);

  // ── live updates (keyset catch-up — the bus tells us *when*, storage tells
  // us *what*: no polling, no unbounded client-side accumulation) ─────────
  useEffect(() => {
    const catchUp = async () => {
      if (searchLiveRef.current.trim()) return; // search results are a snapshot
      const after = newestIdRef.current;
      if (after === null) return;
      try {
        const page = await client.request<FlowPage>("flows_list", {
          limit: 100,
          after,
          method: filters.method || undefined,
          host: filters.host || undefined,
        });
        if (page.items.length === 0) return;
        const first = rowsRef.current[0];
        if (first !== undefined) {
          anchorRef.current = { id: first.id, oldIndex: 0 };
        }
        setRows((prev) => [...page.items, ...prev]);
      } catch {
        /* transient — next event retries */
      }
    };
    const onEvent = (e: AetherEvent) => {
      const interesting =
        e.type === "flow_completed" ||
        e.type === "flow_dropped" ||
        e.type === "response_received";
      if (!interesting) return;
      if (atTopRef.current) {
        void catchUp();
      } else {
        setNewCount((n) => n + 1);
      }
    };
    return client.onEvent(onEvent);
  }, [client, filters]);

  // track "at top" + infinite scroll downward (older pages)
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const handle = () => {
      atTopRef.current = el.scrollTop < 80;
      if (el.scrollTop + el.clientHeight >= el.scrollHeight - ROW_H * 12) {
        void loadOlder();
      }
    };
    el.addEventListener("scroll", handle, { passive: true });
    return () => el.removeEventListener("scroll", handle);
  }, [scrollRef, loadOlder]);

  // ── imperative handle: Replay → reveal flow ───────────────────────────
  const revealRef = useRef<(id: number) => boolean>(() => false);
  revealRef.current = (id: number) => {
    const idx = rowsRef.current.findIndex((r) => r.id === id);
    if (idx === -1) return false;
    onSelect(id);
    scrollToRow(idx);
    return true;
  };
  const { registerHandle } = props;
  useEffect(() => {
    registerHandle({ reveal: (id: number) => revealRef.current(id) });
  }, [registerHandle]);

  const slice = useMemo(
    () => rows.slice(range.start, range.end),
    [rows, range.start, range.end],
  );

  return (
    <div className="history">
      <div className="history-toolbar">
        <input
          className="search"
          placeholder="Search method, host, path, body… (full-text)"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          spellCheck={false}
        />
        <select
          value={filters.method}
          onChange={(e) => setFilters((f) => ({ ...f, method: e.target.value }))}
          aria-label="Method filter"
        >
          {METHODS.map((m) => (
            <option key={m} value={m}>
              {m === "" ? "all methods" : m}
            </option>
          ))}
        </select>
        <input
          className="host-filter"
          placeholder="host glob e.g. *.api.example.com"
          value={filters.host}
          onChange={(e) => setFilters((f) => ({ ...f, host: e.target.value }))}
          spellCheck={false}
        />
        <span className="history-count">
          {rows.length.toLocaleString()} loaded{cursor !== null ? " · more below" : ""}
        </span>
      </div>

      <div className="history-head">
        <span className="c-id">#</span>
        <span className="c-time">time</span>
        <span className="c-method">method</span>
        <span className="c-host">host</span>
        <span className="c-path">path</span>
        <span className="c-status">status</span>
        <span className="c-ms">took</span>
        <span className="c-size">size</span>
      </div>

      <div className="history-scroll" ref={scrollRef} onScroll={onScroll}>
        {rows.length === 0 && !loading && (
          <div className="empty">
            {isSearch
              ? "No flows match this search."
              : "No flows yet — point a client at the proxy listener (status bar → proxy), or open Replay and send the first request."}
          </div>
        )}
        {rows.length > 0 && <div style={{ height: range.padTop }} />}
        {slice.map((r) => (
          <div
            key={r.id}
            className={`flow-row ${selectedId === r.id ? "selected" : ""} ${
              isDropped(r) ? "dropped" : ""
            }`}
            style={{ height: ROW_H }}
            onClick={() => onSelect(r.id)}
            data-flow-id={r.id}
          >
            <span className="c-id">{r.id}</span>
            <span className="c-time dim">{fmtClock(r.created_at_unix_ms)}</span>
            <span className="c-method">
              <span className={`chip method ${methodClass(r.method)}`}>{r.method}</span>
            </span>
            <span className="c-host dim" title={r.host}>
              {r.host}
            </span>
            <span className="c-path mono" title={pathWithQuery(r.path, r.query)}>
              {pathWithQuery(r.path, r.query)}
            </span>
            <span className="c-status">
              {isDropped(r) ? (
                <span className="chip status s-drop">DROP</span>
              ) : (
                <span className={`chip status ${statusClass(r.status)}`}>
                  {r.status === 101 ? "WS" : (r.status ?? "…")}
                </span>
              )}
            </span>
            <span className="c-ms dim">{fmtMs(r.duration_ms)}</span>
            <span className="c-size dim">{fmtBytes(r.request_bytes + r.response_bytes)}</span>
          </div>
        ))}
        {rows.length > 0 && <div style={{ height: range.padBottom }} />}
      </div>

      {newCount > 0 && (
        <button
          className="jump-pill"
          onClick={() => {
            setNewCount(0);
            void loadFirstPage();
            const el = scrollRef.current;
            if (el) el.scrollTop = 0;
          }}
        >
          {newCount} new flow{newCount === 1 ? "" : "s"} — jump to latest
        </button>
      )}
      {error !== null && <div className="history-error">{error}</div>}
      {loading && rows.length === 0 && <div className="empty">Loading…</div>}
    </div>
  );
}

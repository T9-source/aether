// Control-plane client (ADR-002 / ADR-010, protocol v1).
//
// Frame grammar (from crates/aether-daemon/src/control.rs):
//   →  {"op":"auth","params":{"token":...}}            first frame, always
//   →  {"op":"<op>","params":{...}}                    requests
//   ←  {"type":"response","ok":true,"result":...}      replies, strictly FIFO
//   ←  {"type":"response","ok":false,"error":{...}}    failures, strictly FIFO
//   ←  {"type":"event","event":{...AetherEvent...}}    server-pushed, async
//
// The daemon dispatches one op at a time per connection, therefore replies
// arrive in request order — matching them FIFO needs no correlation ids.
// The client is transport-injected so tests can drive it without a browser.
import type { AetherEvent, Hello } from "../types";

export type ConnStatus =
  | "idle"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "auth-failed"
  | "closed";

export interface ControlError {
  kind: string;
  message: string;
}

/** Minimal shape of the browser WebSocket — injectable for tests. */
export interface WSLike {
  send(data: string): void;
  close(): void;
  onopen: (() => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: (() => void) | null;
  onerror: (() => void) | null;
}

interface Pending {
  resolve: (value: unknown) => void;
  reject: (err: ControlError) => void;
}

const MAX_RETRIES = 12;

export class ControlClient {
  private ws: WSLike | null = null;
  private pending: Pending[] = [];
  private eventHandlers = new Set<(e: AetherEvent) => void>();
  private statusHandlers = new Set<(s: ConnStatus, detail?: string) => void>();
  private closedByUs = false;
  private retries = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;

  hello: Hello | null = null;

  constructor(
    readonly url: string,
    readonly token: string,
    private openSocket: (url: string) => WSLike = (u) =>
      new WebSocket(u) as unknown as WSLike,
  ) {}

  // ── lifecycle ─────────────────────────────────────────────────────────

  /** Opens the socket and performs the mandatory auth handshake. */
  connect(): Promise<Hello> {
    this.closedByUs = false;
    return new Promise<Hello>((resolve, reject) => {
      let settled = false;
      const ws = this.openSocket(this.url);
      this.ws = ws;

      const failOnce = (err: ControlError) => {
        if (!settled) {
          settled = true;
          reject(err);
        }
      };

      ws.onopen = () => {
        ws.send(JSON.stringify({ op: "auth", params: { token: this.token } }));
      };

      ws.onmessage = (ev) => {
        let frame: Record<string, unknown>;
        try {
          frame = JSON.parse(String(ev.data)) as Record<string, unknown>;
        } catch {
          return; // non-JSON frame: ignore (never happens with our daemon)
        }
        if (frame.type === "response" && !settled) {
          // the auth reply itself
          settled = true;
          if (frame.ok === true) {
            this.hello = frame.result as Hello;
            this.retries = 0;
            this.emitStatus("connected");
            resolve(this.hello);
          } else {
            const e = frame.error as ControlError | undefined;
            this.emitStatus("auth-failed", e?.message);
            reject(e ?? { kind: "auth", message: "authentication failed" });
          }
          return;
        }
        this.handleFrame(frame);
      };

      ws.onerror = () => {
        failOnce({ kind: "network", message: "connection failed" });
      };

      ws.onclose = () => {
        this.ws = null;
        if (!settled) {
          settled = true;
          this.emitStatus("reconnecting");
          reject({ kind: "network", message: "socket closed during handshake" });
        }
        this.scheduleReconnect();
      };
    });
  }

  close(): void {
    this.closedByUs = true;
    if (this.reconnectTimer !== null) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
    const ws = this.ws;
    this.ws = null;
    this.failAllPending({ kind: "closed", message: "client closed" });
    ws?.close();
    this.emitStatus("closed");
  }

  private scheduleReconnect(): void {
    if (this.closedByUs) return;
    if (this.retries >= MAX_RETRIES) {
      this.emitStatus("closed", "gave up after repeated failures");
      this.failAllPending({ kind: "network", message: "connection lost" });
      return;
    }
    const delay = Math.min(250 * 2 ** this.retries, 3000);
    this.retries += 1;
    this.emitStatus("reconnecting");
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect().catch(() => {
        /* retry loop continues via onclose */
      });
    }, delay);
  }

  // ── requests (FIFO) ───────────────────────────────────────────────────

  request<T>(op: string, params: Record<string, unknown> = {}): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      if (!this.ws) {
        reject({ kind: "offline", message: "not connected" });
        return;
      }
      this.pending.push({
        resolve: resolve as (v: unknown) => void,
        reject,
      });
      // Mirror Rust serde's externally-tagged enum exactly: struct variants
      // carry "params", unit variants (stats_get, rules_list, …) never do.
      const frame: Record<string, unknown> = { op };
      if (Object.keys(params).length > 0) frame.params = params;
      try {
        this.ws.send(JSON.stringify(frame));
      } catch (e) {
        this.pending.pop();
        reject({ kind: "network", message: `send failed: ${String(e)}` });
      }
    });
  }

  private handleFrame(frame: Record<string, unknown>): void {
    if (frame.type === "response") {
      const p = this.pending.shift();
      if (!p) return; // stray reply (e.g. after a reconnect) — drop
      if (frame.ok === true) {
        p.resolve(frame.result);
      } else {
        const e = frame.error as ControlError | undefined;
        p.reject(e ?? { kind: "unknown", message: "unknown error" });
      }
      return;
    }
    if (frame.type === "event") {
      const event = frame.event as AetherEvent | undefined;
      if (event) {
        for (const fn of this.eventHandlers) fn(event);
      }
    }
  }

  private failAllPending(err: ControlError): void {
    const waiting = this.pending.splice(0);
    for (const p of waiting) p.reject(err);
  }

  // ── subscriptions ─────────────────────────────────────────────────────

  onEvent(fn: (e: AetherEvent) => void): () => void {
    this.eventHandlers.add(fn);
    return () => this.eventHandlers.delete(fn);
  }

  onStatus(fn: (s: ConnStatus, detail?: string) => void): () => void {
    this.statusHandlers.add(fn);
    return () => this.statusHandlers.delete(fn);
  }

  private emitStatus(s: ConnStatus, detail?: string): void {
    for (const fn of this.statusHandlers) fn(s, detail);
  }
}

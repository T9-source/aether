import { describe, expect, it } from "vitest";
import { ControlClient, type WSLike } from "../src/lib/ws";

/** Deterministic fake socket: records sent frames, lets tests push replies. */
class FakeSocket implements WSLike {
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  closed = false;

  send(data: string): void {
    this.sent.push(data);
  }
  close(): void {
    this.closed = true;
  }
  // test helpers
  open() {
    this.onopen?.();
  }
  reply(obj: unknown) {
    this.onmessage?.({ data: JSON.stringify(obj) });
  }
  die() {
    this.onclose?.();
  }
}

function makeClient() {
  const sockets: FakeSocket[] = [];
  const client = new ControlClient("ws://x", "sekrit", () => {
    const s = new FakeSocket();
    sockets.push(s);
    return s;
  });
  return { client, sockets };
}

const HELLO = { type: "response", ok: true, result: { version: "0.1.0", server_time_unix_ms: 1 } };

describe("ControlClient", () => {
  it("authenticates with the token as the first frame", async () => {
    const { client, sockets } = makeClient();
    const p = client.connect();
    sockets[0].open();
    expect(JSON.parse(sockets[0].sent[0])).toEqual({ op: "auth", params: { token: "sekrit" } });
    sockets[0].reply(HELLO);
    const hello = await p;
    expect(hello.version).toBe("0.1.0");
  });

  it("rejects connect on auth failure and does not retry silently", async () => {
    const { client, sockets } = makeClient();
    const p = client.connect();
    sockets[0].open();
    sockets[0].reply({ type: "response", ok: false, error: { kind: "auth", message: "mismatch" } });
    await expect(p).rejects.toMatchObject({ kind: "auth" });
  });

  it("matches responses to requests strictly FIFO", async () => {
    const { client, sockets } = makeClient();
    const conn = client.connect();
    sockets[0].open();
    sockets[0].reply(HELLO);
    await conn;

    const r1 = client.request<{ n: number }>("a");
    const r2 = client.request<{ n: number }>("b");
    const r3 = client.request<{ n: number }>("c");
    // replies arrive in request order (daemon is sequential per connection)
    sockets[0].reply({ type: "response", ok: true, result: { n: 1 } });
    sockets[0].reply({ type: "response", ok: true, result: { n: 2 } });
    sockets[0].reply({ type: "response", ok: false, error: { kind: "x", message: "boom" } });

    expect((await r1).n).toBe(1);
    expect((await r2).n).toBe(2);
    await expect(r3).rejects.toMatchObject({ kind: "x" });
  });

  it("dispatches server events to subscribers", async () => {
    const { client, sockets } = makeClient();
    const conn = client.connect();
    sockets[0].open();
    sockets[0].reply(HELLO);
    await conn;

    const seen: string[] = [];
    const off = client.onEvent((e) => seen.push(e.type));
    sockets[0].reply({ type: "event", event: { type: "flow_completed", flow_id: 9 } });
    expect(seen).toEqual(["flow_completed"]);
    off();
    sockets[0].reply({ type: "event", event: { type: "flow_completed", flow_id: 10 } });
    expect(seen).toEqual(["flow_completed"]);
  });

  it("fails pending requests when the client is closed", async () => {
    const { client, sockets } = makeClient();
    const conn = client.connect();
    sockets[0].open();
    sockets[0].reply(HELLO);
    await conn;

    const r = client.request("stats_get");
    client.close();
    await expect(r).rejects.toMatchObject({ kind: "closed" });
  });

  it("rejects requests made while offline", async () => {
    const { client } = makeClient();
    await expect(client.request("ping")).rejects.toMatchObject({ kind: "offline" });
  });

  it("omits params for unit-variant ops (serde enum parity)", async () => {
    const { client, sockets } = makeClient();
    const conn = client.connect();
    sockets[0].open();
    sockets[0].reply(HELLO);
    await conn;

    const r = client.request("stats_get");
    // frame sent with NO params key at all — the daemon's serde rejects
    // `params: {}` on unit variants ("invalid type: map, expected unit variant")
    const lastSent = () => sockets[0].sent[sockets[0].sent.length - 1];
    expect(lastSent()).toBe('{"op":"stats_get"}');
    sockets[0].reply({
      type: "response",
      ok: true,
      result: { flows: { total: 0, completed: 0, dropped: 0 }, held: 0, rules: 0, uptime_ms: 1, version: "0.1.0", proxy_addr: "127.0.0.1:8888" },
    });
    await r;

    const r2 = client.request("flows_list", { limit: 5 });
    const sent = JSON.parse(lastSent());
    expect(sent).toEqual({ op: "flows_list", params: { limit: 5 } });
    sockets[0].reply({ type: "response", ok: true, result: { items: [], next_cursor: null, prev_cursor: null } });
    await r2;
  });
});

import { describe, expect, it } from "vitest";
import { heldReducer, heldSummary } from "../src/state/held";
import type { HeldRow } from "../src/types";

const row = (id: number, direction: "request" | "response" = "request"): HeldRow => ({
  flow_id: id,
  direction,
  method: direction === "request" ? "GET" : null,
  host: "api.example.com",
  path: "/v2/things",
  status: direction === "response" ? 200 : null,
});

describe("heldReducer", () => {
  it("adds holds without duplicates and removes on release", () => {
    let state = heldReducer([], { t: "hold", row: row(1) });
    state = heldReducer(state, { t: "hold", row: row(1) });
    expect(state).toHaveLength(1);
    state = heldReducer(state, { t: "hold", row: row(2, "response") });
    expect(state).toHaveLength(2);
    state = heldReducer(state, { t: "released", flow_id: 1 });
    expect(state.map((r) => r.flow_id)).toEqual([2]);
  });

  it("set replaces the whole queue (reconnect resync)", () => {
    let state = heldReducer([row(1)], { t: "hold", row: row(2) });
    state = heldReducer(state, { t: "set", rows: [row(7)] });
    expect(state.map((r) => r.flow_id)).toEqual([7]);
  });

  it("summary mirrors the CLI shape", () => {
    expect(heldSummary(row(3))).toContain("#3");
    expect(heldSummary(row(3))).toMatch(/^REQ/);
    expect(heldSummary(row(3, "response"))).toMatch(/^RES/);
  });
});

describe("request raw parsing (editing aid)", () => {
  it("round-trips through parseRawRequest", async () => {
    const { parseRawRequest } = await import("../src/components/RequestEditor");
    const raw = [
      "POST https://api.example.com:8443/v2/submit?x=1 HTTP/1.1",
      "host: api.example.com:8443",
      "content-type: application/json",
      "",
      '{"a":1}',
    ].join("\r\n");
    const parsed = parseRawRequest(raw);
    if ("error" in parsed) throw new Error(parsed.error);
    const r = parsed.request;
    expect(r.method).toBe("post");
    expect(r.url).toMatchObject({ scheme: "https", host: "api.example.com", port: 8443, path: "/v2/submit", query: "x=1" });
    expect(r.headers.entries).toHaveLength(2);
    expect(r.body).toEqual({ kind: "text", text: '{"a":1}' });
  });

  it("origin-form borrows the host from the Host header", async () => {
    const { parseRawRequest } = await import("../src/components/RequestEditor");
    const parsed = parseRawRequest("GET /health HTTP/1.1\r\nhost: up.stream:8080\r\n\r\n");
    if ("error" in parsed) throw new Error(parsed.error);
    expect(parsed.request.url).toMatchObject({ scheme: "http", host: "up.stream", port: 8080, path: "/health" });
  });

  it("rejects garbage with a useful message", async () => {
    const { parseRawRequest } = await import("../src/components/RequestEditor");
    expect(parseRawRequest("nonsense")).toHaveProperty("error");
    expect(parseRawRequest("GET nope HTTP/1.1\r\n\r\n")).toHaveProperty("error");
  });
});

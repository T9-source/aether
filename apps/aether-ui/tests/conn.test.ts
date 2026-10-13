import { describe, it, expect } from "vitest";
import { initialConn, scrubCredentials, DEFAULT_URL, type ConnStore } from "../src/lib/conn";

const store = (url: string | null, auth: string | null): ConnStore => ({
  getUrl: () => url,
  getAuth: () => auth,
});

describe("connection bootstrap", () => {
  it("first run: no query, no saved session → defaults, no auto-connect", () => {
    const c = initialConn("", store(null, null));
    expect(c).toEqual({ url: DEFAULT_URL, auth: "", auto: false });
  });

  it("deep link with ws+auth → auto-connect", () => {
    const c = initialConn("?ws=ws://127.0.0.1:9099&auth=abc123", store(null, null));
    expect(c).toEqual({ url: "ws://127.0.0.1:9099", auth: "abc123", auto: true });
  });

  it("saved session → auto-connect with saved values", () => {
    const c = initialConn("", store("ws://127.0.0.1:9099", "saved456"));
    expect(c).toEqual({ url: "ws://127.0.0.1:9099", auth: "saved456", auto: true });
  });

  it("deep link wins over saved session", () => {
    const c = initialConn("?ws=ws://localhost:1111&auth=deep", store("ws://x", "saved"));
    expect(c).toEqual({ url: "ws://localhost:1111", auth: "deep", auto: true });
  });

  it("H1: attacker deep link (ws only, no auth) must NOT inherit the saved token", () => {
    // The attack: victim has a saved session; attacker sends
    // http://console/?ws=ws://attacker.example:1337 — the console must not
    // authenticate to attacker.example with the daemon token.
    const c = initialConn("?ws=ws://attacker.example:1337", store("ws://127.0.0.1:9099", "supersecret"));
    expect(c).toEqual({ url: "ws://attacker.example:1337", auth: "", auto: false });
  });

  it("same-daemon link (saved URL, no auth param) → reconnect allowed", () => {
    const c = initialConn("?ws=ws://127.0.0.1:9099", store("ws://127.0.0.1:9099", "saved456"));
    expect(c).toEqual({ url: "ws://127.0.0.1:9099", auth: "saved456", auto: true });
  });

  it("url-only deep link with no saved session → no auto-connect, empty auth", () => {
    const c = initialConn("?ws=ws://localhost:1111", store(null, null));
    expect(c).toEqual({ url: "ws://localhost:1111", auth: "", auto: false });
  });

  it("auth-only deep link → default url, no auto-connect (half link)", () => {
    const c = initialConn("?auth=abc", store(null, null));
    expect(c).toEqual({ url: DEFAULT_URL, auth: "", auto: false });
  });

  it("auth-only deep link with saved session does not leak the saved token either", () => {
    // The attacker-controlled URL is the default here, but the principle
    // holds: an auth-less query never inherits a saved value.
    const c = initialConn("?auth=abc", store("ws://evil.example", "saved"));
    expect(c.auth).toBe("");
    expect(c.auto).toBe(false);
  });

  it("empty query string behaves like absent", () => {
    expect(initialConn("?", store(null, null)).auto).toBe(false);
  });
});

describe("scrubCredentials (pure)", () => {
  it("removes ws and auth, keeps the rest", () => {
    expect(scrubCredentials("?ws=ws://x&auth=abc&keep=1")).toBe("?keep=1");
  });

  it("returns '' when nothing remains", () => {
    expect(scrubCredentials("?ws=ws://x&auth=abc")).toBe("");
  });

  it("returns the input untouched when there are no credential params", () => {
    expect(scrubCredentials("?keep=1")).toBe("?keep=1");
  });

  it("empty search is a no-op", () => {
    expect(scrubCredentials("")).toBe("");
  });
});

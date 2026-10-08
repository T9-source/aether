import { describe, expect, it } from "vitest";
import { fmtBytes, fmtClock, fmtMs, fmtUptime, methodClass, pathWithQuery, statusClass, truncate } from "../src/lib/format";

describe("fmtBytes", () => {
  it("stays exact below 1KiB", () => {
    expect(fmtBytes(0)).toBe("0 B");
    expect(fmtBytes(1023)).toBe("1023 B");
  });
  it("scales to KB/MB/GB", () => {
    expect(fmtBytes(1024)).toBe("1.0 KB");
    expect(fmtBytes(1536)).toBe("1.5 KB");
    expect(fmtBytes(5 * 1024 * 1024)).toBe("5.0 MB");
    expect(fmtBytes(3 * 1024 ** 3)).toBe("3.0 GB");
  });
  it("handles null/undefined", () => {
    expect(fmtBytes(null)).toBe("—");
    expect(fmtBytes(undefined)).toBe("—");
  });
});

describe("fmtMs", () => {
  it("renders ms below 1s and seconds above", () => {
    expect(fmtMs(812)).toBe("812 ms");
    expect(fmtMs(1240)).toBe("1.24 s");
    expect(fmtMs(95000)).toBe("1m 35s");
  });
  it("renders missing as dash", () => {
    expect(fmtMs(null)).toBe("—");
  });
});

describe("fmtClock / fmtUptime", () => {
  it("clock uses local time with millis", () => {
    const t = Date.UTC(2026, 0, 2, 3, 4, 5, 678);
    const s = fmtClock(t);
    expect(s).toMatch(/^\d{2}:\d{2}:\d{2}\.\d{3}$/);
  });
  it("uptime formats m:s and h:m:s", () => {
    expect(fmtUptime(65_000)).toBe("01:05");
    expect(fmtUptime(3_723_000)).toBe("01:02:03");
  });
});

describe("status/method classes", () => {
  it("maps status bands", () => {
    expect(statusClass(200)).toBe("s-2x");
    expect(statusClass(302)).toBe("s-3x");
    expect(statusClass(404)).toBe("s-4x");
    expect(statusClass(503)).toBe("s-5x");
    expect(statusClass(null)).toBe("s-pending");
  });
  it("maps methods", () => {
    expect(methodClass("GET")).toBe("m-get");
    expect(methodClass("post")).toBe("m-post");
    expect(methodClass("PROPFIND")).toBe("m-other");
  });
});

describe("misc", () => {
  it("pathWithQuery appends query when present", () => {
    expect(pathWithQuery("/a", "b=1")).toBe("/a?b=1");
    expect(pathWithQuery("/a", null)).toBe("/a");
  });
  it("truncate keeps ellipsis within max", () => {
    expect(truncate("abcdef", 4)).toBe("abc…");
    expect(truncate("ab", 4)).toBe("ab");
  });
});

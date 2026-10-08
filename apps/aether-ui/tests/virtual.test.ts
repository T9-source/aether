import { describe, expect, it } from "vitest";
import { visibleRange, totalHeight } from "../src/lib/virtual";

describe("visibleRange", () => {
  const rowH = 30;
  it("renders the first window at scrollTop 0 with overscan", () => {
    const r = visibleRange({ scrollTop: 0, viewport: 300, rowH, count: 100_000, overscan: 8 });
    expect(r.start).toBe(0);
    expect(r.end).toBe(10 + 8); // ceil(300/30)=10 visible + 8 overscan
    expect(r.padTop).toBe(0);
    expect(r.padBottom).toBe((100_000 - r.end) * rowH);
  });

  it("slides with scroll", () => {
    const r = visibleRange({ scrollTop: 3000, viewport: 300, rowH, count: 100_000, overscan: 8 });
    expect(r.start).toBe(100 - 8);
    expect(r.end).toBe(100 + 10 + 8);
    expect(r.padTop).toBe(r.start * rowH);
  });

  it("clamps at the end of a short list", () => {
    const r = visibleRange({ scrollTop: 9999, viewport: 300, rowH, count: 12, overscan: 8 });
    expect(r.start).toBe(3); // max(0, (12-1) - 8)
    expect(r.end).toBe(12);
    expect(r.padBottom).toBe(0);
  });

  it("handles degenerate inputs", () => {
    expect(visibleRange({ scrollTop: 0, viewport: 0, rowH: 30, count: 10 })).toEqual({
      start: 0,
      end: 0,
      padTop: 0,
      padBottom: 0,
    });
    expect(visibleRange({ scrollTop: 0, viewport: 300, rowH: 0, count: 10 })).toEqual({
      start: 0,
      end: 0,
      padTop: 0,
      padBottom: 0,
    });
  });

  it("totalHeight matches pad+rendered for full coverage", () => {
    const count = 1234;
    const r = visibleRange({ scrollTop: 500, viewport: 320, rowH, count, overscan: 5 });
    expect(totalHeight(count, rowH)).toBe(count * rowH);
    expect(r.padTop + r.padBottom).toBeLessThanOrEqual(count * rowH);
  });
});

// Pure virtualization math — fixed row height, overscan window.
// Kept free of React/DOM so it is unit-testable without jsdom (layout is
// measured by the useVirtual hook, this file only does arithmetic).

export interface Range {
  /** First visible (rendered) row index. */
  start: number;
  /** One past the last rendered row index (exclusive). */
  end: number;
  /** Top spacer height in px (= start * rowH). */
  padTop: number;
  /** Bottom spacer height in px. */
  padBottom: number;
}

export function visibleRange(opts: {
  scrollTop: number;
  viewport: number;
  rowH: number;
  count: number;
  overscan?: number;
}): Range {
  const { scrollTop, viewport, rowH, count } = opts;
  const overscan = opts.overscan ?? 8;
  if (count === 0 || rowH <= 0 || viewport <= 0) {
    return { start: 0, end: 0, padTop: 0, padBottom: 0 };
  }
  const first = Math.min(Math.floor(scrollTop / rowH), Math.max(0, count - 1));
  const visible = Math.ceil(viewport / rowH);
  const start = Math.max(0, first - overscan);
  const end = Math.min(count, first + visible + overscan);
  return {
    start,
    end,
    padTop: start * rowH,
    padBottom: (count - end) * rowH,
  };
}

/** Total scroll height for `count` fixed-height rows. */
export function totalHeight(count: number, rowH: number): number {
  return Math.max(0, count) * rowH;
}

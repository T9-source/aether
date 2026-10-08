// React binding for the pure virtual math in lib/virtual.ts.
import { useCallback, useEffect, useRef, useState } from "react";
import { visibleRange, type Range } from "../lib/virtual";

export function useVirtual(count: number, rowH: number, overscan = 8) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewport, setViewport] = useState(600);

  const onScroll = useCallback(() => {
    const el = scrollRef.current;
    if (el) setScrollTop(el.scrollTop);
  }, []);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const measure = () => setViewport(el.clientHeight);
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const range: Range = visibleRange({ scrollTop, viewport, rowH, count, overscan });

  /** Scrolls so that row `i` is visible; used by "jump to flow". */
  const scrollToRow = useCallback(
    (i: number) => {
      const el = scrollRef.current;
      if (!el) return;
      const top = i * rowH;
      if (top < el.scrollTop) el.scrollTop = top;
      else if (top + rowH > el.scrollTop + el.clientHeight)
        el.scrollTop = top + rowH - el.clientHeight;
    },
    [rowH],
  );

  return { scrollRef, onScroll, range, scrollToRow };
}

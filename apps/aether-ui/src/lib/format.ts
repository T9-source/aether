// Formatting helpers — pure, testable, locale-stable (the UI is a dev tool:
// fixed en-style numeric formatting keeps columns machine-comparable).

export function fmtBytes(n: number | null | undefined): string {
  if (n === null || n === undefined) return "—";
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n;
  let u = -1;
  do {
    v /= 1024;
    u += 1;
  } while (v >= 1024 && u < units.length - 1);
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[u]}`;
}

export function fmtMs(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return "—";
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(2)} s`;
  const m = Math.floor(ms / 60_000);
  const s = Math.round((ms % 60_000) / 1000);
  return `${m}m ${s}s`;
}

/** HH:MM:SS.mmm in local time — the density a flow list needs. */
export function fmtClock(unixMs: number): string {
  const d = new Date(unixMs);
  const p = (x: number, w = 2) => String(x).padStart(w, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(
    d.getMilliseconds(),
    3,
  )}`;
}

export function fmtUptime(ms: number): string {
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const p = (x: number) => String(x).padStart(2, "0");
  return h > 0 ? `${p(h)}:${p(m)}:${p(sec)}` : `${p(m)}:${p(sec)}`;
}

export function statusClass(status: number | null | undefined): string {
  if (status === null || status === undefined) return "s-pending";
  if (status < 200) return "s-1x";
  if (status < 300) return "s-2x";
  if (status < 400) return "s-3x";
  if (status < 500) return "s-4x";
  return "s-5x";
}

export function methodClass(method: string): string {
  switch (method.toUpperCase()) {
    case "GET":
      return "m-get";
    case "POST":
      return "m-post";
    case "PUT":
      return "m-put";
    case "PATCH":
      return "m-patch";
    case "DELETE":
      return "m-delete";
    case "HEAD":
      return "m-head";
    case "OPTIONS":
      return "m-options";
    default:
      return "m-other";
  }
}

export function truncate(s: string, max: number): string {
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}

/** `path?query` — the human-facing request target. */
export function pathWithQuery(path: string, query: string | null): string {
  return query ? `${path}?${query}` : path;
}

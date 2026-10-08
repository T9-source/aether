// Intercept queue state — a pure reducer so live event application is
// unit-testable without React. The panel feeds control-plane events in and
// renders the queue out; `set` resyncs from `intercept_list` after reconnects.
import type { HeldRow } from "../types";

export type HeldAction =
  | { t: "set"; rows: HeldRow[] }
  | { t: "hold"; row: HeldRow }
  | { t: "released"; flow_id: number };

export function heldReducer(state: HeldRow[], a: HeldAction): HeldRow[] {
  switch (a.t) {
    case "set":
      return a.rows;
    case "hold": {
      if (state.some((r) => r.flow_id === a.row.flow_id && r.direction === a.row.direction)) {
        return state;
      }
      return [...state, a.row];
    }
    case "released":
      return state.filter((r) => r.flow_id !== a.flow_id);
  }
}

/** One-line summary for queue rows (mirrors the CLI's intercept list). */
export function heldSummary(r: HeldRow): string {
  const dir = r.direction === "request" ? "REQ" : "RES";
  const line =
    r.direction === "request"
      ? `${r.method ?? "?"} ${r.host ?? "?"}${r.path ?? "/"}`
      : `${r.status ?? "?"} ${r.host ?? "?"}${r.path ?? "/"}`;
  return `${dir}  #${r.flow_id}  ${line}`;
}

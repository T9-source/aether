export interface ConnInit {
  url: string;
  /** daemon auth value (hex, printed by `aether serve`) */
  auth: string;
  /** true when the console should connect on first render without a click. */
  auto: boolean;
}

export interface ConnStore {
  getUrl(): string | null;
  getAuth(): string | null;
}

export const DEFAULT_URL = "ws://127.0.0.1:9099";

/**
 * Connection bootstrap resolution order:
 *   1. explicit query params (?ws=…&auth=…) — the one-click link printed by `aether serve`
 *   2. saved session (localStorage) — returning users reconnect instantly
 *   3. defaults — first run shows the Connect screen
 *
 * Auto-connect fires when an auth value is available AND the source is either
 * the deep link or a previously saved session. A bare default URL never
 * auto-connects (nothing to connect to yet).
 */
export function initialConn(search: string, store: ConnStore): ConnInit {
  const q = new URLSearchParams(search);
  const qUrl = q.get("ws");
  const qAuth = q.get("auth");

  const lsUrl = store.getUrl();
  const lsAuth = store.getAuth();

  // Security invariant (audit H1): the saved auth value may only ever be
  // reused with the exact URL it was saved for. A deep link that supplies
  // `ws` without `auth` must NEVER inherit the saved auth — otherwise a
  // crafted link (?ws=ws://attacker.example) makes the console silently
  // authenticate to the attacker's server with the daemon token (full
  // traffic read, rule edit, replay).
  const deepLink = qUrl !== null && qAuth !== null;
  const savedSession =
    qUrl === null && qAuth === null && lsUrl !== null && lsAuth !== null;
  // Same-URL continuation: the link names the daemon we already have a
  // session for — the saved pair may reconnect.
  const sameDaemon = qUrl !== null && qAuth === null && qUrl === lsUrl && lsAuth !== null;

  const auto = deepLink || savedSession || sameDaemon;

  let url: string;
  let auth: string;
  if (deepLink) {
    url = qUrl;
    auth = qAuth;
  } else if (savedSession || sameDaemon) {
    url = qUrl ?? lsUrl;
    auth = lsAuth;
  } else {
    // Unknown URL without its own auth: nothing is auto-filled, nothing is
    // sent anywhere. The user types the auth value by hand on this screen.
    url = qUrl ?? lsUrl ?? DEFAULT_URL;
    auth = "";
  }

  return { url, auth, auto };
}

/**
 * Strip credentials (ws/auth params) from a query string. Pure: returns the
 * new search string ("" when nothing remains) — the caller applies it to
 * history.
 */
export function scrubCredentials(search: string): string {
  if (search === "") return "";
  const q = new URLSearchParams(search);
  if (!q.has("ws") && !q.has("auth")) return search;
  q.delete("ws");
  q.delete("auth");
  const qs = q.toString();
  return qs ? `?${qs}` : "";
}

/** Clipboard helper that never throws (clipboard can be blocked); resolves to success. */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

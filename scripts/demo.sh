#!/usr/bin/env bash
# demo.sh — the Aether birth moment, reproducible end-to-end, minus secrets.
# Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
#
# Proves, on one machine, in ~30 seconds:
#   1. headless daemon start (ADR-001)
#   2. TLS MITM of a real HTTPS origin with zero certificate errors
#      (ADR-004: the client trusts ONLY the Aether CA)
#   3. history recorded + searchable (ADR-005)
#   4. CLI intercept: hold → edit → forward (ADR-001/002: the control plane
#      is a first-class interface — MILESTONE-0 criterion 6)
#
# Everything runs against a loopback origin started by this script — no
# external network, no tokens left behind, no root, no CA installed into
# the OS trust store (curl is pointed at the CA explicitly).
# Requires: curl, openssl, python3, jq (jq is optional; python3 is enough).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(dirname "$HERE")"                       # repo root (aether/)
WORK="$(mktemp -d)"
DAEMON_PID=""
ORIGIN_PID=""
cleanup() {
  [ -n "$ORIGIN_PID" ] && kill "$ORIGIN_PID" 2>/dev/null || true
  [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

AETHER="${AETHER_BIN:-$ROOT/target/debug/aether}"
command -v curl >/dev/null || { echo "curl is required"; exit 1; }
command -v openssl >/dev/null || { echo "openssl is required"; exit 1; }
command -v python3 >/dev/null || { echo "python3 is required"; exit 1; }
[ -x "$AETHER" ] || { echo "build first:  cargo build -p aether-cli"; exit 1; }

say() { printf '\n\033[1;36m── %s\033[0m\n' "$*"; }

say "0/5 local HTTPS origin (self-signed — stands in for the real internet)"
openssl req -x509 -newkey rsa:2048 -nodes -keyout "$WORK/origin.key" \
  -out "$WORK/origin.crt" -days 1 -subj "/CN=demo.local" \
  -addext "subjectAltName=DNS:demo.local,IP:127.0.0.1" \
  -addext "basicConstraints=critical,CA:FALSE" 2>/dev/null
python3 - "$WORK" <<'PY' &
import os, ssl, http.server, sys
work = sys.argv[1]

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = f'{{"origin":"aether-demo","path":"{self.path}"}}'.encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):  # keep the demo output clean
        pass

ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(os.path.join(work, "origin.crt"),
                    os.path.join(work, "origin.key"))
srv = http.server.ThreadingHTTPServer(("127.0.0.1", 8443), H)
srv.socket = ctx.wrap_socket(srv.socket, server_side=True)
print("origin ready on https://127.0.0.1:8443", flush=True)
srv.serve_forever()
PY
ORIGIN_PID=$!
sleep 1

say "1/5 headless daemon (the UI can die — the proxy doesn't care: ADR-001)"
export AETHER_DATA_DIR="$WORK/data"
# host_overrides: demo.local exists only in this lab, so the daemon's dial
# layer maps it onto the loopback origin (feature showcase, not a hack).
cat > "$WORK/config.toml" <<TOML
# lab mode: the demo origin is self-signed by this very script (a real
# browser-facing origin needs no such waiver — see docs/LIMITATIONS.md).
# NB: top-level keys must precede any [section] header in TOML.
upstream_tls_verify = false

[proxy]
listen = "127.0.0.1:8899"

[control]
listen = "127.0.0.1:9098"

[storage]
backend = "memory"

[host_overrides]
"demo.local:8443" = "socket://127.0.0.1:8443"
TOML
"$AETHER" serve --config "$WORK/config.toml" \
  > "$WORK/daemon.log" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 50); do
  [ -f "$AETHER_DATA_DIR/proxy.addr" ] && break
  sleep 0.1
done
TOKEN="$(cat "$AETHER_DATA_DIR/token")"
echo "proxy   : $(cat "$AETHER_DATA_DIR/proxy.addr")"
echo "control : $(cat "$AETHER_DATA_DIR/control.addr")"

say "2/5 CA export (the 'install into your browser' step, automated)"
"$AETHER" ca export --out "$WORK/aether-ca.pem" --token "$TOKEN" >/dev/null
echo "CA written to $WORK/aether-ca.pem (client trusts ONLY Aether)"

say "3/5 real HTTPS through the proxy — zero certificate errors"
RESP="$(curl -s --proxy http://127.0.0.1:8899 --cacert "$WORK/aether-ca.pem" \
  --resolve demo.local:8443:127.0.0.1 "https://demo.local:8443/hello?via=aether")"
echo "origin said: $RESP"
echo "$RESP" | grep -q aether-demo || { echo "MITM round-trip FAILED"; exit 1; }

say "4/5 history recorded + full-text searchable"
"$AETHER" flows list --limit 3 --token "$TOKEN"
"$AETHER" flows search "aether-demo" --limit 3 --token "$TOKEN" | head -12

say "5/5 CLI intercept — hold, edit, forward (the control plane is real)"
"$AETHER" intercept set --request true --token "$TOKEN" >/dev/null
curl -s --proxy http://127.0.0.1:8899 --cacert "$WORK/aether-ca.pem" \
  --resolve demo.local:8443:127.0.0.1 "https://demo.local:8443/intercepted?step=1" \
  -o "$WORK/client.out" --max-time 20 &
CLIENT_PID=$!
sleep 1
HELD_ID="$("$AETHER" intercept ls --json --token "$TOKEN" \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)[0]["flow_id"])')"
echo "held flow  : $HELD_ID"
"$AETHER" intercept show "$HELD_ID" --token "$TOKEN" > "$WORK/held.txt"
python3 - "$WORK/held.txt" "$WORK/payload.json" <<'PY'
import json, sys
raw = open(sys.argv[1]).read()
# `intercept show` appends a usage hint after the JSON — keep only the JSON
d = json.loads(raw[raw.index("{"):raw.rindex("}") + 1])
req = d["request"]
req["url"]["path"] = "/intercepted"
req["url"]["query"] = "edited=by-cli&proof=aether"
json.dump({"request": req}, open(sys.argv[2], "w"))
PY
"$AETHER" intercept release "$HELD_ID" --from-file "$WORK/payload.json" --token "$TOKEN" >/dev/null
wait "$CLIENT_PID" || true
echo "origin saw : $(cat "$WORK/client.out")"
grep -q "edited=by-cli" "$WORK/client.out" \
  || { echo "intercept edit did not reach the origin"; exit 1; }
"$AETHER" intercept set --request false --token "$TOKEN" >/dev/null

say "teardown (kill the daemon — everything above already happened)"
echo
echo "birth moment reproduced. Milestone-0 criteria exercised: 1, 3 (lab), 6, 9."

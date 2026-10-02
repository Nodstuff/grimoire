#!/usr/bin/env bash
# TaisceKit against REAL scratch daemons: a LOCAL-mode one on 7515 and a
# SERVER-mode one on 7516 (--public-url http://localhost:7516), signed in
# headlessly with the daemon's softpasskey example. Temp dirs only: never
# ~/.grimoire, never port 7425. Daemons are killed on exit.
#
# Usage: apple/scripts/integration.sh [path to the grimoire checkout]
#   (default: the main checkout; it needs target/release/taisce and
#   target/release/examples/softpasskey, built with
#   `cargo build --release -p taisce && cargo build --release -p taisce --example softpasskey`;
#   GRIMOIRE_BIN / SOFTPASSKEY_BIN point at binaries built elsewhere)
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="${1:-${GRIMOIRE_REPO:-$HOME/personal/knowledge-system}}"
BIN="${GRIMOIRE_BIN:-$REPO/target/release/taisce}"
SOFTPASSKEY="${SOFTPASSKEY_BIN:-$REPO/target/release/examples/softpasskey}"
LOCAL_PORT=7515
SERVER_PORT=7516

for f in "$BIN" "$SOFTPASSKEY"; do
  [[ -x "$f" ]] || { echo "missing $f (build it in $REPO)" >&2; exit 1; }
done

ROOT="$(mktemp -d "${TMPDIR:-/tmp}/taisce-it.XXXXXX")"
PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  [[ "${KEEP:-}" == 1 ]] && echo "kept $ROOT" || rm -rf "$ROOT"
}
trap cleanup EXIT

for port in $LOCAL_PORT $SERVER_PORT; do
  if lsof -iTCP:$port -sTCP:LISTEN -t >/dev/null 2>&1; then
    echo "port $port is busy" >&2; exit 1
  fi
done

start() { # name port [extra global args...]
  local name=$1 port=$2; shift 2
  mkdir -p "$ROOT/$name"
  GRIMOIRE_IDENTITY_FILE="$ROOT/$name/identity.key" \
    "$BIN" --db "$ROOT/$name/ks.db" --port "$port" "$@" serve >"$ROOT/$name/daemon.log" 2>&1 &
  PIDS+=($!)
}

wait_for() { # url expected-status
  for _ in $(seq 1 100); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "$1" || true)
    [[ "$code" == "$2" ]] && return 0
    sleep 0.2
  done
  echo "timed out waiting for $1 ($2)" >&2; return 1
}

start local $LOCAL_PORT
start server $SERVER_PORT --public-url "http://localhost:$SERVER_PORT"
wait_for "http://127.0.0.1:$LOCAL_PORT/api/docs" 200
wait_for "http://localhost:$SERVER_PORT/.well-known/oauth-authorization-server" 200

# a one-time enrollment link (the CLI writes the db directly; WAL lets the daemon run on)
ENROLL=$(GRIMOIRE_IDENTITY_FILE="$ROOT/server/identity.key" \
  "$BIN" --db "$ROOT/server/ks.db" --public-url "http://localhost:$SERVER_PORT" auth enroll | head -1)
[[ "$ENROLL" == http* ]] || { echo "no enrollment link: $ENROLL" >&2; exit 1; }

cd "$HERE/TaisceKit"
set +e
TAISCE_IT_URL="http://127.0.0.1:$LOCAL_PORT" \
TAISCE_IT_AUTH_URL="http://localhost:$SERVER_PORT" \
TAISCE_IT_ENROLL_URL="$ENROLL" \
TAISCE_IT_SOFTPASSKEY="$SOFTPASSKEY" \
  swift test --no-parallel --filter 'LocalIntegrationTests|ServerIntegrationTests' 2>&1 | tee "$ROOT/swift-test.log" \
  | grep -E '^(✔|✘|↳)|Test run|error:'
status=${PIPESTATUS[0]}
set -e
if [[ $status -ne 0 ]]; then
  # the daemon logs next to its db; stdout/stderr catch startup failures
  for name in server local; do
    echo "--- $name daemon (tail)"; tail -20 "$ROOT/$name/daemon.log" "$ROOT/$name"/*.log 2>/dev/null
  done
fi
exit $status

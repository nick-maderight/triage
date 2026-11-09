#!/bin/sh
set -eu

TRIAGE_BIN=${TRIAGE_BIN:-${CARGO_TARGET_DIR:-target}/debug/triage}
RUN_DIR=$(mktemp -d "${TMPDIR:-/tmp}/triage-smoke.XXXXXX")
DB=$RUN_DIR/triage.db
PORT=${TRIAGE_SMOKE_PORT:-7391}
URL=http://127.0.0.1:$PORT
MAX_ATTEMPTS=${TRIAGE_SMOKE_ATTEMPTS:-50}
export TRIAGE_URL=$URL
server_pid=
cleanup() {
  if [ -n "$server_pid" ]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$RUN_DIR"
}
trap cleanup EXIT INT TERM
# The pane tracker follows real tmux panes, and an agent finishing a turn on
# this machine would inject a card into the throwaway db mid-run. A failing
# tmux stub keeps the smoke server's queue to the events the script posts.
STUB_DIR=$RUN_DIR/tmux-stub
mkdir -p "$STUB_DIR"
printf '%s\n' '#!/bin/sh' 'exit 1' > "$STUB_DIR/tmux"
chmod 755 "$STUB_DIR/tmux"
export DSH_HOST_FILE="$RUN_DIR/no-dsh-host.json"
PATH="$STUB_DIR:$PATH" "$TRIAGE_BIN" serve --db "$DB" --listen "127.0.0.1:$PORT" --no-notify >/dev/null 2>&1 &
server_pid=$!
server_owns_port() {
  listeners=$(ss -Hlnpt "sport = :$PORT" 2>/dev/null) || return 1
  case "$listeners" in
    *"pid=$server_pid,"*|*"pid=$server_pid)"*) return 0 ;;
  esac
  return 1
}

attempt=0
until server_owns_port && curl --max-time 1 -fsS "$URL/v1/queue" >/dev/null 2>&1; do
  if ! kill -0 "$server_pid" 2>/dev/null; then
    printf '%s\n' 'server exited before becoming ready' >&2
    exit 1
  fi
  attempt=$((attempt + 1))
  if [ "$attempt" -ge "$MAX_ATTEMPTS" ]; then
    printf '%s\n' 'server did not become ready' >&2
    exit 1
  fi
  sleep 0.1
done

"$TRIAGE_BIN" finding --source test://smoke --title 'Smoke finding' --id "smoke-$$" >/dev/null
queue=$(curl -fsS "$URL/v1/queue")
printf '%s' "$queue" | jq -e '.cards | length == 1' >/dev/null
card_id=$(printf '%s' "$queue" | jq -r '.cards[0].id | @uri')
curl -fsS -X POST "$URL/v1/cards/$card_id/skip" >/dev/null
curl -fsS "$URL/v1/queue" | jq -e '.cards | length == 0' >/dev/null
